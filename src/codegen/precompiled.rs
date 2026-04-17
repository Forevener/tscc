//! Precompiled WASM helpers.
//!
//! The `helpers/` crate compiles to a `wasm32-unknown-unknown` cdylib.
//! `build.rs` extracts every internal function from that module — both
//! exported helpers (e.g. `__str_slice`) and compiler-inserted internals
//! (e.g. `memcpy` shims) — along with their call-site offsets.
//!
//! At codegen time, tscc registers a contiguous run of function indices
//! for the functions it needs, then rewrites each body's `call` immediates
//! to point at those indices. This lets helper Rust code call other
//! functions freely and use `copy_nonoverlapping` / builtin intrinsics.

use wasm_encoder::{Function, ValType};

include!(concat!(env!("OUT_DIR"), "/precompiled_helpers.rs"));

/// Find the bundle index of an exported helper by name.
pub fn find_export(name: &str) -> Option<usize> {
    PRECOMPILED_FUNCS
        .iter()
        .position(|f| f.name == Some(name))
}

/// Transitive closure over the call graph: starting from `seeds` (bundle
/// indices), return every bundle index reachable via `call` instructions,
/// including the seeds themselves.
pub fn transitive_closure(seeds: &[usize]) -> Vec<usize> {
    let mut in_set = vec![false; PRECOMPILED_FUNCS.len()];
    let mut stack: Vec<usize> = seeds.to_vec();
    for &s in seeds {
        in_set[s] = true;
    }
    while let Some(idx) = stack.pop() {
        for cs in PRECOMPILED_FUNCS[idx].call_sites {
            let callee = cs.original_callee as usize;
            if !in_set[callee] {
                in_set[callee] = true;
                stack.push(callee);
            }
        }
    }
    (0..PRECOMPILED_FUNCS.len()).filter(|i| in_set[*i]).collect()
}

/// Build a `Function` for the precompiled helper at bundle index `idx`,
/// rewriting every `call` immediate to the tscc-level index from `index_map`
/// (indexed by bundle position — use `u32::MAX` as a sentinel for unregistered
/// entries; hitting one here is a bug in the caller).
pub fn build_function(idx: usize, index_map: &[u32]) -> Function {
    let pf = &PRECOMPILED_FUNCS[idx];
    let mut out = Vec::with_capacity(pf.body.len());
    let mut cursor = 0usize;
    for cs in pf.call_sites {
        // Copy template bytes through the 0x10 `call` opcode byte, then
        // substitute a freshly encoded uleb128 for the tscc function index.
        let imm_start = cs.byte_offset as usize + 1;
        out.extend_from_slice(&pf.body[cursor..imm_start]);
        let new_idx = index_map[cs.original_callee as usize];
        assert!(
            new_idx != u32::MAX,
            "precompiled helper at {idx} calls unregistered bundle idx {}",
            cs.original_callee
        );
        write_uleb128(&mut out, new_idx);
        cursor = imm_start + cs.uleb128_len as usize;
    }
    out.extend_from_slice(&pf.body[cursor..]);
    let mut f = Function::new(pf.locals.iter().copied());
    f.raw(out);
    f
}

fn write_uleb128(out: &mut Vec<u8>, mut v: u32) {
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
