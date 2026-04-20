//! Precompiled WASM helpers.
//!
//! The `helpers/` crate compiles to a `wasm32-unknown-unknown` cdylib.
//! `build.rs` extracts the complete closure of what's needed to link those
//! helpers into tscc's output module: function imports, types, declared
//! globals, Data and Table/Element sections, every internal function, and
//! per-body offsets for `call`, `call_indirect`, and `global.get/set`
//! instructions. At codegen time tscc registers the pieces it needs and
//! rewrites those immediates byte-by-byte to point at the merged tscc
//! indices.

use wasm_encoder::Function;

#[allow(dead_code)]
mod generated {
    use wasm_encoder::ValType;
    include!(concat!(env!("OUT_DIR"), "/precompiled_helpers.rs"));
}

pub use generated::*;

/// Plan for rewriting a helper body's indices when splicing it into tscc's
/// output. Every `original_*` value a helper instruction could reference
/// (function, type, global) has a mapping entry here.
pub struct RewritePlan<'a> {
    /// Helper full-space function index → tscc function index.
    /// `[0, import_count)` = helper imports; `[import_count, ..)` = internal
    /// functions in `PRECOMPILED_FUNCS` order.
    pub func_index_map: &'a [u32],
    /// Helper type index → tscc type index.
    pub type_index_map: &'a [u32],
    /// Helper global index → tscc global index.
    pub global_index_map: &'a [u32],
    /// Destination table index for helper `call_indirect`s.
    pub helper_table_index: u32,
}

/// Find the bundle index of an exported L_helper by name. Skips L_splice
/// (inline) helpers — those have no bundle slot of their own; `find_inline`
/// is the only correct way to reach them.
pub fn find_export(name: &str) -> Option<usize> {
    PRECOMPILED_FUNCS
        .iter()
        .position(|f| !f.is_inline && f.name == Some(name))
}

/// Find an inline (L_splice) helper by name. Returns `None` for unknown names
/// or for helpers authored as regular L_helper functions.
pub fn find_inline(name: &str) -> Option<&'static PrecompiledFunc> {
    PRECOMPILED_FUNCS
        .iter()
        .find(|f| f.is_inline && f.name == Some(name))
}

/// Transitive closure over the `call` call graph, starting from `seeds`
/// (bundle indices, i.e. post-import-adjusted indices into
/// `PRECOMPILED_FUNCS`). Includes the seeds. Imports don't contribute to the
/// closure — they are resolved separately at registration time.
pub fn transitive_closure(seeds: &[usize]) -> Vec<usize> {
    let import_count = PRECOMPILED_IMPORTS.len() as u32;
    let mut in_set = vec![false; PRECOMPILED_FUNCS.len()];
    let mut stack: Vec<usize> = seeds.to_vec();
    for &s in seeds {
        in_set[s] = true;
    }
    while let Some(idx) = stack.pop() {
        for cs in PRECOMPILED_FUNCS[idx].call_sites {
            // Skip import callees — they land in tscc's function space via
            // a different registration path.
            if cs.original_callee < import_count {
                continue;
            }
            let callee_bundle = (cs.original_callee - import_count) as usize;
            if !in_set[callee_bundle] {
                in_set[callee_bundle] = true;
                stack.push(callee_bundle);
            }
        }
    }
    (0..PRECOMPILED_FUNCS.len()).filter(|i| in_set[*i]).collect()
}

/// Extend a seed set with functions reachable only via helpers' element
/// segments (rustc's vtables / trait objects). Triggered when any selected
/// function has a `call_indirect`. Returns (expanded set, uses_call_indirect).
pub fn expand_with_dynamic_dispatch(seeds: Vec<usize>) -> (Vec<usize>, bool) {
    let uses_indirect = seeds
        .iter()
        .any(|&i| !PRECOMPILED_FUNCS[i].call_indirect_sites.is_empty());
    if !uses_indirect || PRECOMPILED_ELEMENTS.is_empty() {
        return (seeds, uses_indirect);
    }
    let import_count = PRECOMPILED_IMPORTS.len() as u32;
    let mut in_set = vec![false; PRECOMPILED_FUNCS.len()];
    for &s in &seeds {
        in_set[s] = true;
    }
    let mut extra: Vec<usize> = Vec::new();
    for seg in PRECOMPILED_ELEMENTS {
        for &fi in seg.func_indices {
            if fi < import_count {
                // Element points at an import; its registration is separate.
                continue;
            }
            let bundle_idx = (fi - import_count) as usize;
            if !in_set[bundle_idx] {
                in_set[bundle_idx] = true;
                extra.push(bundle_idx);
            }
        }
    }
    let mut stack = extra;
    while let Some(idx) = stack.pop() {
        for cs in PRECOMPILED_FUNCS[idx].call_sites {
            if cs.original_callee < import_count {
                continue;
            }
            let callee_bundle = (cs.original_callee - import_count) as usize;
            if !in_set[callee_bundle] {
                in_set[callee_bundle] = true;
                stack.push(callee_bundle);
            }
        }
    }
    let closed = (0..PRECOMPILED_FUNCS.len())
        .filter(|i| in_set[*i])
        .collect();
    (closed, uses_indirect)
}

/// True iff the bundle ships any Data segments.
pub fn has_data() -> bool {
    !PRECOMPILED_DATA.is_empty()
}

/// Minimum initial page count needed to fit all helper Data segments.
pub fn required_memory_pages() -> u32 {
    let mut max_end: u32 = 0;
    for seg in PRECOMPILED_DATA {
        let end = seg.offset + seg.bytes.len() as u32;
        if end > max_end {
            max_end = end;
        }
    }
    if max_end == 0 {
        0
    } else {
        max_end.div_ceil(0x10000)
    }
}

/// Build a `Function` for the precompiled helper at bundle index `idx`,
/// rewriting every relocatable immediate via `plan`.
pub fn build_function(idx: usize, plan: &RewritePlan<'_>) -> Function {
    let pf = &PRECOMPILED_FUNCS[idx];

    enum Edit {
        Call {
            callee: u32,
            imm_start: u32,
            imm_end: u32,
        },
        CallIndirect {
            type_idx: u32,
            table_idx: u32,
            imm_start: u32,
            imm_end: u32,
        },
        Global {
            new_idx: u32,
            imm_start: u32,
            imm_end: u32,
        },
    }
    let mut edits: Vec<(u32, Edit)> = Vec::with_capacity(
        pf.call_sites.len() + pf.call_indirect_sites.len() + pf.global_sites.len(),
    );
    for cs in pf.call_sites {
        let imm_start = cs.byte_offset + 1;
        let imm_end = imm_start + cs.uleb128_len as u32;
        let new_callee = plan.func_index_map[cs.original_callee as usize];
        assert!(
            new_callee != u32::MAX,
            "precompiled helper at {idx} calls unregistered func idx {}",
            cs.original_callee
        );
        edits.push((
            cs.byte_offset,
            Edit::Call {
                callee: new_callee,
                imm_start,
                imm_end,
            },
        ));
    }
    for cs in pf.call_indirect_sites {
        let type_imm_start = cs.byte_offset + 1;
        let type_imm_end = type_imm_start + cs.type_uleb_len as u32;
        let table_imm_end = type_imm_end + cs.table_uleb_len as u32;
        let new_type = plan.type_index_map[cs.original_type_idx as usize];
        assert!(
            new_type != u32::MAX,
            "precompiled helper at {idx} call_indirect uses unmapped type {}",
            cs.original_type_idx
        );
        edits.push((
            cs.byte_offset,
            Edit::CallIndirect {
                type_idx: new_type,
                table_idx: plan.helper_table_index,
                imm_start: type_imm_start,
                imm_end: table_imm_end,
            },
        ));
    }
    for gs in pf.global_sites {
        // Opcode byte (0x23 get / 0x24 set) + uleb immediate.
        let imm_start = gs.byte_offset + 1;
        let imm_end = imm_start + gs.uleb128_len as u32;
        let new_idx = plan.global_index_map[gs.original_global_idx as usize];
        assert!(
            new_idx != u32::MAX,
            "precompiled helper at {idx} global.{} uses unmapped global {}",
            if gs.is_set { "set" } else { "get" },
            gs.original_global_idx
        );
        edits.push((
            gs.byte_offset,
            Edit::Global {
                new_idx,
                imm_start,
                imm_end,
            },
        ));
    }
    edits.sort_by_key(|(off, _)| *off);

    let mut out = Vec::with_capacity(pf.body.len());
    let mut cursor = 0usize;
    for (_off, edit) in edits {
        match edit {
            Edit::Call {
                callee,
                imm_start,
                imm_end,
            } => {
                out.extend_from_slice(&pf.body[cursor..imm_start as usize]);
                write_uleb128(&mut out, callee);
                cursor = imm_end as usize;
            }
            Edit::CallIndirect {
                type_idx,
                table_idx,
                imm_start,
                imm_end,
            } => {
                out.extend_from_slice(&pf.body[cursor..imm_start as usize]);
                write_uleb128(&mut out, type_idx);
                write_uleb128(&mut out, table_idx);
                cursor = imm_end as usize;
            }
            Edit::Global {
                new_idx,
                imm_start,
                imm_end,
            } => {
                out.extend_from_slice(&pf.body[cursor..imm_start as usize]);
                write_uleb128(&mut out, new_idx);
                cursor = imm_end as usize;
            }
        }
    }
    out.extend_from_slice(&pf.body[cursor..]);
    let mut f = Function::new(pf.locals.iter().copied());
    f.raw(out);
    f
}

pub(super) fn write_uleb128(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}
