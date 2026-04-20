//! L_splice: paste-inline a precompiled helper body at a call site rather
//! than emitting `Call(idx)`. See `crates/tscc/docs/design-emit-architecture.md`.
//!
//! Authoring side lives in `helpers/`: `tscc_inline! { fn ... }` emits the
//! function plus a custom-section marker that flips
//! `PrecompiledFunc::is_inline = true`. The build-time extractor also
//! records `local_sites`, `return_sites` (each with its control-frame
//! nesting depth), and `has_br_table`, so this module stays a pure byte-
//! rewriter at runtime — no wasmparser dependency, no opcode table to
//! maintain.
//!
//! ## What the splicer handles
//!
//! - Helper params and declared locals both allocated as caller-side
//!   locals; a local-index map covers the full `[0, total_locals)` range.
//! - Reverse `LocalSet` sequence to pop args off the stack into the
//!   caller-side param slots.
//! - Wrapping `Block` with a signature matching the helper's results:
//!   `Empty` for void, `Result(T)` for one result, and a tscc-registered
//!   `() -> results` function type for multi-result.
//! - Body bytes pasted with local indices renumbered, `Call` /
//!   `CallIndirect` / `GlobalGet/Set` immediates rewritten to tscc
//!   indices, and each helper `Return` rewritten to `Br(block_depth)`
//!   targeting the wrapping block (which replaces the helper's function
//!   frame).
//! - Helper-internal `Br` / `BrIf` immediates are left untouched:
//!   `Br(D)` at nesting depth `D` originally meant "branch out of the
//!   function frame" and after wrapping means "branch to the wrapping
//!   block" — the byte is unchanged because the depth count from that
//!   site is also unchanged (the wrapping block sits where the function
//!   frame did).
//! - Trailing function-terminator `End` byte stripped (we emit our own
//!   block-closing `End`).
//!
//! ## Still not supported
//!
//! - `BrTable`: would need per-site branch-depth rewriting for any arm
//!   that targets the helper's function frame. No current port needs it.

use wasm_encoder::{BlockType, Instruction, ValType};

use crate::error::CompileError;
use crate::types::WasmType;

use super::func::FuncContext;
use super::precompiled::{PrecompiledFunc, RewritePlan, write_uleb128};

/// Paste-inline `pf` at the current emit point. Args must already be on the
/// stack in left-to-right order — same convention as a `Call(idx)` would
/// expect. After return, the helper's result values (zero, one, or many)
/// sit on top of the stack, matching what `Call` would leave behind.
pub fn splice_inline_call(
    ctx: &mut FuncContext,
    pf: &PrecompiledFunc,
    plan: &RewritePlan<'_>,
) -> Result<(), CompileError> {
    debug_assert!(pf.is_inline, "splice_inline_call invoked on non-inline helper");

    let helper_name = pf.name.unwrap_or("<unnamed>");
    if pf.has_br_table {
        return Err(CompileError::codegen(format!(
            "splicer: helper `{helper_name}` contains `br_table`, which is \
             not supported yet"
        )));
    }

    // 1. Build the full local-index map: [0, params.len()) covers params,
    //    [params.len(), total) covers the helper's declared locals. Every
    //    slot gets a fresh caller-side local.
    let mut local_map: Vec<u32> = Vec::with_capacity(pf.params.len());
    for vt in pf.params {
        local_map.push(ctx.alloc_local(wasm_type_from_val_type(*vt)));
    }
    for &(count, vt) in pf.locals {
        for _ in 0..count {
            local_map.push(ctx.alloc_local(wasm_type_from_val_type(vt)));
        }
    }

    // 2. Pop args off the stack into the param slots. Top of stack is the
    //    rightmost arg, so iterate in reverse — local_map[0] receives the
    //    leftmost arg.
    for &local_idx in local_map[..pf.params.len()].iter().rev() {
        ctx.push(Instruction::LocalSet(local_idx));
    }

    // 3. Open the wrapping block. Its signature matches the helper's result
    //    list. For multi-result helpers this needs a registered function
    //    type (block signature `() -> results` since params are already
    //    stored in locals by the time the block opens).
    let block_type = match pf.results.len() {
        0 => BlockType::Empty,
        1 => BlockType::Result(pf.results[0]),
        _ => {
            let type_idx = ctx
                .module_ctx
                .get_or_add_type_sig(Vec::new(), pf.results.to_vec());
            BlockType::FunctionType(type_idx)
        }
    };
    ctx.push(Instruction::Block(block_type));

    // 4. Paste the rewritten body.
    let rewritten = rewrite_body(pf, plan, &local_map, helper_name)?;
    ctx.push_raw_bytes(rewritten);

    // 5. Close the wrapping block. Result(s), if any, are on top of stack.
    ctx.push(Instruction::End);
    Ok(())
}

/// Take `pf.body` (which ends in the function-terminator `End` byte) and
/// produce a byte sequence with all relocatable immediates rewritten and the
/// terminator stripped. The output is suitable to paste between a wrapping
/// `Block` and its closing `End`.
///
/// `local_map` covers every declared local (params first, then the helper's
/// own locals in declaration order); `local_map[original_idx]` is the
/// caller-side index for that slot.
fn rewrite_body(
    pf: &PrecompiledFunc,
    plan: &RewritePlan<'_>,
    local_map: &[u32],
    helper_name: &str,
) -> Result<Vec<u8>, CompileError> {
    // Body must end with the function-terminator `End` (0x0B). Strip it.
    let body_no_end = match pf.body.split_last() {
        Some((&0x0B, head)) => head,
        _ => {
            return Err(CompileError::codegen(format!(
                "splicer: helper `{helper_name}` body does not end with End byte"
            )));
        }
    };

    // Each edit replaces `body_no_end[replace_start..replace_end]` with a
    // freshly-written byte sequence. For opcodes with an immediate we leave
    // the opcode byte in place and just rewrite its immediate; for `Return`
    // we replace the opcode itself (no immediate originally, but we write a
    // full `Br(depth)` sequence in its place).
    enum Edit {
        Local { new_idx: u32, replace_end: u32 },
        Call { new_callee: u32, replace_end: u32 },
        CallIndirect { new_type: u32, new_table: u32, replace_end: u32 },
        Global { new_idx: u32, replace_end: u32 },
        ReturnToBr { depth: u32, replace_end: u32 },
    }

    let mut edits: Vec<(u32, u32, Edit)> = Vec::with_capacity(
        pf.local_sites.len()
            + pf.call_sites.len()
            + pf.call_indirect_sites.len()
            + pf.global_sites.len()
            + pf.return_sites.len(),
    );

    for ls in pf.local_sites {
        if (ls.original_idx as usize) >= local_map.len() {
            return Err(CompileError::codegen(format!(
                "splicer: helper `{helper_name}` references local {} \
                 beyond declared count {}",
                ls.original_idx,
                local_map.len()
            )));
        }
        let new_idx = local_map[ls.original_idx as usize];
        let imm_start = ls.byte_offset + 1;
        let replace_end = imm_start + ls.uleb128_len as u32;
        edits.push((ls.byte_offset, imm_start, Edit::Local { new_idx, replace_end }));
    }
    for cs in pf.call_sites {
        let imm_start = cs.byte_offset + 1;
        let replace_end = imm_start + cs.uleb128_len as u32;
        let new_callee = plan.func_index_map[cs.original_callee as usize];
        debug_assert!(new_callee != u32::MAX);
        edits.push((cs.byte_offset, imm_start, Edit::Call { new_callee, replace_end }));
    }
    for cs in pf.call_indirect_sites {
        let imm_start = cs.byte_offset + 1;
        let type_imm_end = imm_start + cs.type_uleb_len as u32;
        let replace_end = type_imm_end + cs.table_uleb_len as u32;
        let new_type = plan.type_index_map[cs.original_type_idx as usize];
        debug_assert!(new_type != u32::MAX);
        edits.push((
            cs.byte_offset,
            imm_start,
            Edit::CallIndirect {
                new_type,
                new_table: plan.helper_table_index,
                replace_end,
            },
        ));
    }
    for gs in pf.global_sites {
        let imm_start = gs.byte_offset + 1;
        let replace_end = imm_start + gs.uleb128_len as u32;
        let new_idx = plan.global_index_map[gs.original_global_idx as usize];
        debug_assert!(new_idx != u32::MAX);
        edits.push((gs.byte_offset, imm_start, Edit::Global { new_idx, replace_end }));
    }
    for rs in pf.return_sites {
        // `return` (0x0F) is a single byte. We replace the opcode itself
        // with a full `br <depth>` sequence, so `replace_start` starts at
        // the opcode byte and `replace_end` sits one past it.
        let replace_end = rs.byte_offset + 1;
        edits.push((
            rs.byte_offset,
            rs.byte_offset,
            Edit::ReturnToBr { depth: rs.block_depth, replace_end },
        ));
    }

    edits.sort_by_key(|(off, _, _)| *off);

    let mut out = Vec::with_capacity(body_no_end.len());
    let mut cursor = 0usize;
    for (_off, replace_start, edit) in edits {
        out.extend_from_slice(&body_no_end[cursor..replace_start as usize]);
        let replace_end = match edit {
            Edit::Local { new_idx, replace_end } => {
                write_uleb128(&mut out, new_idx);
                replace_end
            }
            Edit::Call { new_callee, replace_end } => {
                write_uleb128(&mut out, new_callee);
                replace_end
            }
            Edit::CallIndirect { new_type, new_table, replace_end } => {
                write_uleb128(&mut out, new_type);
                write_uleb128(&mut out, new_table);
                replace_end
            }
            Edit::Global { new_idx, replace_end } => {
                write_uleb128(&mut out, new_idx);
                replace_end
            }
            Edit::ReturnToBr { depth, replace_end } => {
                out.push(0x0C); // br opcode
                write_uleb128(&mut out, depth);
                replace_end
            }
        };
        cursor = replace_end as usize;
    }
    out.extend_from_slice(&body_no_end[cursor..]);
    Ok(out)
}

fn wasm_type_from_val_type(vt: ValType) -> WasmType {
    match vt {
        ValType::I32 => WasmType::I32,
        ValType::F64 => WasmType::F64,
        other => panic!("splicer: unsupported helper param/result type {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::precompiled::PRECOMPILED_FUNCS;

    fn find_named(name: &str) -> &'static PrecompiledFunc {
        PRECOMPILED_FUNCS
            .iter()
            .find(|f| f.name == Some(name))
            .unwrap_or_else(|| panic!("test helper `{name}` not in PRECOMPILED_FUNCS"))
    }

    fn empty_plan() -> RewritePlan<'static> {
        RewritePlan {
            func_index_map: &[],
            type_index_map: &[],
            global_index_map: &[],
            helper_table_index: 0,
        }
    }

    #[test]
    fn extractor_marks_inline_helper() {
        let pf = find_named("__inline_test_passthrough");
        assert!(pf.is_inline, "tscc_inline! marker should set is_inline=true");
        assert!(pf.return_sites.is_empty());
        assert!(!pf.has_br_table);
        assert_eq!(pf.local_sites.len(), 1, "expected one LocalGet site");
        assert_eq!(pf.local_sites[0].original_idx, 0);
    }

    #[test]
    fn extractor_leaves_l_helper_inline_false() {
        // Pick any L_helper — `__hash_fx_bool` stays as a regular precompiled
        // helper since it's rarely called and its inline body would be
        // byte-identical to `__hash_fx_i32` (one wrapping-multiply round on a
        // widened u64), which would invite LTO identical-code-folding.
        let pf = find_named("__hash_fx_bool");
        assert!(!pf.is_inline, "L_helper should not be flagged inline");
        assert!(
            pf.local_sites.is_empty(),
            "L_helper should not pay for splice metadata extraction"
        );
    }

    #[test]
    fn rewrite_body_renumbers_local_to_caller_idx() {
        // __inline_test_passthrough body = `local.get 0; end` = [0x20, 0x00, 0x0B].
        // After stripping End and renumbering local 0 → caller-side 42,
        // expected: [0x20, 0x2A].
        let pf = find_named("__inline_test_passthrough");
        let plan = empty_plan();
        let bytes = rewrite_body(pf, &plan, &[42], "test").unwrap();
        assert_eq!(bytes, vec![0x20, 0x2A], "renumbered local.get bytes");
    }

    #[test]
    fn rewrite_body_renumbers_to_multibyte_uleb() {
        // Local index 200 needs 2 ULEB128 bytes (0xC8 0x01) → tests that the
        // edit machinery handles output longer than the original 1-byte input.
        let pf = find_named("__inline_test_passthrough");
        let plan = empty_plan();
        let bytes = rewrite_body(pf, &plan, &[200], "test").unwrap();
        assert_eq!(bytes, vec![0x20, 0xC8, 0x01]);
    }

    #[test]
    fn rewrite_body_rejects_out_of_range_local() {
        let pf = find_named("__inline_test_passthrough");
        let plan = empty_plan();
        // Helper has 1 param; passing zero locals must error.
        let err = rewrite_body(pf, &plan, &[], "test").unwrap_err();
        assert!(format!("{err:?}").contains("beyond declared count"));
    }
}
