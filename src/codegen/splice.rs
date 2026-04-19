//! L_splice: paste-inline a precompiled helper body at a call site rather
//! than emitting `Call(idx)`. See `crates/tscc/docs/design-emit-architecture.md`.
//!
//! Authoring side lives in `helpers/`: `tscc_inline! { fn ... }` emits the
//! function plus a custom-section marker that flips
//! `PrecompiledFunc::is_inline = true`. The build-time extractor also
//! records `local_sites`, `has_return`, `has_br_table` so this module is a
//! pure byte-rewriter at runtime — no wasmparser dependency, no opcode
//! table to maintain.
//!
//! ## POC subset (this file)
//!
//! Implements the minimum needed to migrate `__hash_fx_i32` from L_helper to
//! L_splice as the first port target:
//!
//! - Helper params allocated as caller-side locals; reverse `LocalSet` to
//!   pop them off the stack in correct order.
//! - Wrapping `Block` with an inferred result type (Empty or one ValType).
//! - Body bytes pasted with local indices renumbered to caller-side, plus
//!   the existing `Call`/`CallIndirect`/`GlobalGet/Set` index rewrites that
//!   `precompiled::build_function` already does for L_helper.
//! - Trailing function-terminator `End` byte stripped (we emit our own
//!   block-closing `End`).
//!
//! Explicitly NOT yet supported (errors out clearly):
//!
//! - `Return` opcodes (need rewrite to `Br(0)` against the wrapping block).
//! - `BrTable`.
//! - Helpers with declared locals beyond their params.
//! - Multi-result helpers.
//!
//! All slated for the production hardening step; the first port target
//! doesn't need any of them.

use wasm_encoder::{BlockType, Instruction, ValType};

use crate::error::CompileError;
use crate::types::WasmType;

use super::func::FuncContext;
use super::precompiled::{PrecompiledFunc, RewritePlan, write_uleb128};

/// Paste-inline `pf` at the current emit point. Args must already be on the
/// stack in left-to-right order — same convention as a `Call(idx)` would
/// expect. After return, the helper's single result (if any) sits on top of
/// the stack, again matching what `Call` would leave behind.
pub fn splice_inline_call(
    ctx: &mut FuncContext,
    pf: &PrecompiledFunc,
    plan: &RewritePlan<'_>,
) -> Result<(), CompileError> {
    debug_assert!(pf.is_inline, "splice_inline_call invoked on non-inline helper");

    let helper_name = pf.name.unwrap_or("<unnamed>");
    if !pf.locals.is_empty() {
        return Err(CompileError::codegen(format!(
            "splicer POC subset: helper `{helper_name}` has declared locals \
             beyond params (production splicer required)"
        )));
    }
    if pf.results.len() > 1 {
        return Err(CompileError::codegen(format!(
            "splicer POC subset: helper `{helper_name}` is multi-result \
             (production splicer required)"
        )));
    }
    if pf.has_return {
        return Err(CompileError::codegen(format!(
            "splicer POC subset: helper `{helper_name}` contains `return` \
             (production splicer required)"
        )));
    }
    if pf.has_br_table {
        return Err(CompileError::codegen(format!(
            "splicer POC subset: helper `{helper_name}` contains `br_table` \
             (production splicer required)"
        )));
    }

    // 1. Allocate one caller-side local per param.
    let param_locals: Vec<u32> = pf
        .params
        .iter()
        .map(|vt| ctx.alloc_local(wasm_type_from_val_type(*vt)))
        .collect();

    // 2. Pop args off stack into those locals. Top of stack is the rightmost
    //    arg, so iterate in reverse so local[0] gets the leftmost arg.
    for &local_idx in param_locals.iter().rev() {
        ctx.push(Instruction::LocalSet(local_idx));
    }

    // 3. Open the wrapping block. `Return` (when supported) will become
    //    `Br(0)` to this block. Block result type matches the helper's
    //    declared return.
    let block_type = match pf.results.first() {
        None => BlockType::Empty,
        Some(vt) => BlockType::Result(*vt),
    };
    ctx.push(Instruction::Block(block_type));

    // 4. Paste the rewritten body.
    let rewritten = rewrite_body(pf, plan, &param_locals, helper_name)?;
    ctx.push_raw_bytes(rewritten);

    // 5. Close the wrapping block. Result (if any) is now on top of stack.
    ctx.push(Instruction::End);
    Ok(())
}

/// Take `pf.body` (which ends in the function-terminator `End` byte) and
/// produce a byte sequence with all relocatable immediates rewritten and the
/// terminator stripped. The output is suitable to paste between a wrapping
/// `Block` and its closing `End`.
fn rewrite_body(
    pf: &PrecompiledFunc,
    plan: &RewritePlan<'_>,
    param_locals: &[u32],
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

    enum Edit {
        Local { new_idx: u32, imm_end: u32 },
        Call { new_callee: u32, imm_end: u32 },
        CallIndirect { new_type: u32, new_table: u32, imm_end: u32 },
        Global { new_idx: u32, imm_end: u32 },
    }

    let mut edits: Vec<(u32, u32, Edit)> = Vec::with_capacity(
        pf.local_sites.len()
            + pf.call_sites.len()
            + pf.call_indirect_sites.len()
            + pf.global_sites.len(),
    );

    for ls in pf.local_sites {
        if (ls.original_idx as usize) >= param_locals.len() {
            return Err(CompileError::codegen(format!(
                "splicer POC subset: helper `{helper_name}` references local {} \
                 beyond param count {}",
                ls.original_idx,
                param_locals.len()
            )));
        }
        let new_idx = param_locals[ls.original_idx as usize];
        let imm_start = ls.byte_offset + 1;
        let imm_end = imm_start + ls.uleb128_len as u32;
        edits.push((ls.byte_offset, imm_start, Edit::Local { new_idx, imm_end }));
    }
    for cs in pf.call_sites {
        let imm_start = cs.byte_offset + 1;
        let imm_end = imm_start + cs.uleb128_len as u32;
        let new_callee = plan.func_index_map[cs.original_callee as usize];
        debug_assert!(new_callee != u32::MAX);
        edits.push((cs.byte_offset, imm_start, Edit::Call { new_callee, imm_end }));
    }
    for cs in pf.call_indirect_sites {
        let imm_start = cs.byte_offset + 1;
        let type_imm_end = imm_start + cs.type_uleb_len as u32;
        let imm_end = type_imm_end + cs.table_uleb_len as u32;
        let new_type = plan.type_index_map[cs.original_type_idx as usize];
        debug_assert!(new_type != u32::MAX);
        edits.push((
            cs.byte_offset,
            imm_start,
            Edit::CallIndirect {
                new_type,
                new_table: plan.helper_table_index,
                imm_end,
            },
        ));
    }
    for gs in pf.global_sites {
        let imm_start = gs.byte_offset + 1;
        let imm_end = imm_start + gs.uleb128_len as u32;
        let new_idx = plan.global_index_map[gs.original_global_idx as usize];
        debug_assert!(new_idx != u32::MAX);
        edits.push((gs.byte_offset, imm_start, Edit::Global { new_idx, imm_end }));
    }

    edits.sort_by_key(|(off, _, _)| *off);

    let mut out = Vec::with_capacity(body_no_end.len());
    let mut cursor = 0usize;
    for (_off, imm_start, edit) in edits {
        out.extend_from_slice(&body_no_end[cursor..imm_start as usize]);
        let imm_end = match edit {
            Edit::Local { new_idx, imm_end } => {
                write_uleb128(&mut out, new_idx);
                imm_end
            }
            Edit::Call { new_callee, imm_end } => {
                write_uleb128(&mut out, new_callee);
                imm_end
            }
            Edit::CallIndirect { new_type, new_table, imm_end } => {
                write_uleb128(&mut out, new_type);
                write_uleb128(&mut out, new_table);
                imm_end
            }
            Edit::Global { new_idx, imm_end } => {
                write_uleb128(&mut out, new_idx);
                imm_end
            }
        };
        cursor = imm_end as usize;
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
        assert!(!pf.has_return);
        assert!(!pf.has_br_table);
        assert_eq!(pf.local_sites.len(), 1, "expected one LocalGet site");
        assert_eq!(pf.local_sites[0].original_idx, 0);
    }

    #[test]
    fn extractor_leaves_l_helper_inline_false() {
        let pf = find_named("__hash_fx_i32");
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
        // Helper has 1 param; passing zero param locals must error.
        let err = rewrite_body(pf, &plan, &[], "test").unwrap_err();
        assert!(format!("{err:?}").contains("beyond param count"));
    }
}
