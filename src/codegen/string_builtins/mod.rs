mod convert;
mod split_join;
mod transform;
use std::collections::HashSet;

use oxc_ast::ast::*;
use wasm_encoder::{Function, Instruction, MemArg};

use super::module::ModuleContext;
use crate::types::WasmType;

/// String header size: 4 bytes for the length field.
pub const STRING_HEADER_SIZE: i32 = 4;

/// Names of all string runtime helpers, in registration order.
/// Note: `__str_concat` was removed in favor of inline fused allocation
/// (see `emit_fused_string_chain` in codegen/expr.rs), which avoids the N-1
/// intermediate allocations of the chained-call approach.
/// Note: `__str_eq` and `__str_cmp` are not listed here — they're L_splice
/// helpers living in `helpers/src/inline.rs`, and the splicer pastes their
/// bodies at each call site rather than registering them as real functions.
pub const STRING_HELPER_NAMES: &[&str] = &[
    "__str_indexOf",
    "__str_slice",
    "__str_startsWith",
    "__str_endsWith",
    "__str_includes",
    "__str_toLower",
    "__str_toUpper",
    "__str_trim",
    "__str_trimStart",
    "__str_trimEnd",
    "__str_from_i32",
    "__str_from_f64",
    "__str_split",
    "__str_replace",
    "__str_parseInt",
    "__str_parseFloat",
    "__str_fromCharCode",
    "__str_repeat",
    "__str_padStart",
    "__str_padEnd",
    "__str_concat",
    "__str_replaceAll",
    "__str_toFixed",
    "__str_toPrecision",
    "__str_toExponential",
    "__str_lastIndexOf",
];

/// Register all string runtime helper functions in the module.
/// Must be called after Pass 2 (all user functions registered), before Pass 3 (codegen).
type HelperSig = (&'static str, Vec<(String, WasmType)>, WasmType);

/// Output of helper registration, carrying the state needed by the matching
/// body-emission pass (`compile_string_helpers`). Threads through `module.rs`
/// because `compile_string_helpers` runs later, with immutable access to `ctx`.
///
/// `Clone` so a copy can be stashed on `ModuleContext` for method-body codegen
/// (which needs the rewrite-plan slices to drive the L_splice splicer) while
/// the original keeps flowing through `compile_string_helpers` /
/// `assemble_module`. The fields are all `Vec<u32>` / primitives — cheap.
#[derive(Clone)]
pub struct HelperRegistration {
    /// Bundle indices that WERE registered, in registration (= emission) order.
    pub registered_bundle: Vec<usize>,
    /// Combined helper-space → tscc function index map. Length =
    /// `PRECOMPILED_IMPORTS.len() + PRECOMPILED_FUNCS.len()`; entries for
    /// import slots come first and map to synthesized/user-provided tscc
    /// functions, then bundle slots follow. The Element-section emitter in
    /// `sections.rs` indexes this directly with helper full-space funcrefs.
    pub func_index_map: Vec<u32>,
    /// Parallel to `precompiled::PRECOMPILED_TYPES`: tscc type index for each
    /// helper type referenced by some registered `call_indirect`.
    pub type_index_map: Vec<u32>,
    /// Parallel to `precompiled::PRECOMPILED_GLOBALS`: tscc global index for
    /// each helper global referenced by some registered `global.get/set`.
    pub global_index_map: Vec<u32>,
    /// True iff any registered helper uses `call_indirect`.
    pub requires_table: bool,
    /// True iff the bundle contributed any Data segments to the output.
    pub requires_data: bool,
    /// Destination table index for helper `call_indirect`s.
    pub helper_table_index: u32,
    /// Inline (L_splice) helper names that the caller asked to expose via the
    /// export table. Each gets a synthesized one-line wrapper function: take
    /// the params, splice the inline body, return the result. The host can
    /// then call them through wasmtime exactly like an L_helper export.
    /// Used by `tests/hashers.rs` and similar — the wrapper is the only way
    /// to make an inline helper observable from outside the splicer.
    pub exposed_inline_wrappers: Vec<InlineWrapperReg>,
}

/// Registration metadata for one synthesized inline-helper wrapper. Used by
/// `compile_string_helpers` to emit the matching body in the same order the
/// names were registered (so func indices stay aligned).
#[derive(Clone)]
pub struct InlineWrapperReg {
    pub name: &'static str,
    pub params: Vec<(String, WasmType)>,
    pub ret: WasmType,
}

pub fn register_string_helpers(
    ctx: &mut ModuleContext,
    used: &HashSet<String>,
    exposed: &HashSet<String>,
) -> HelperRegistration {
    let mut helpers: Vec<HelperSig> = vec![
        // Note: `__str_eq` and `__str_cmp` are omitted on purpose — they're
        // L_splice helpers (see `helpers/src/inline.rs`). The splicer pastes
        // their bodies at each call site. The only residual tscc-side wiring
        // is ensuring memcmp (each body's slice-eq/slice-cmp Call target)
        // stays seeded when either name is in `used` — handled in the Phase
        // 2 seed walk below.
        //
        // __str_indexOf(haystack: i32, needle: i32) -> i32
        (
            "__str_indexOf",
            vec![
                ("haystack".into(), WasmType::I32),
                ("needle".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_lastIndexOf(haystack: i32, needle: i32) -> i32
        (
            "__str_lastIndexOf",
            vec![
                ("haystack".into(), WasmType::I32),
                ("needle".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_slice(s: i32, start: i32, end: i32) -> i32
        (
            "__str_slice",
            vec![
                ("s".into(), WasmType::I32),
                ("start".into(), WasmType::I32),
                ("end".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_startsWith(s: i32, prefix: i32) -> i32
        (
            "__str_startsWith",
            vec![
                ("s".into(), WasmType::I32),
                ("prefix".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_endsWith(s: i32, suffix: i32) -> i32
        (
            "__str_endsWith",
            vec![
                ("s".into(), WasmType::I32),
                ("suffix".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_includes(s: i32, search: i32) -> i32
        (
            "__str_includes",
            vec![
                ("s".into(), WasmType::I32),
                ("search".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_toLower(s: i32) -> i32
        (
            "__str_toLower",
            vec![("s".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_toUpper(s: i32) -> i32
        (
            "__str_toUpper",
            vec![("s".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_trim(s: i32) -> i32
        (
            "__str_trim",
            vec![("s".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_trimStart(s: i32) -> i32
        (
            "__str_trimStart",
            vec![("s".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_trimEnd(s: i32) -> i32
        (
            "__str_trimEnd",
            vec![("s".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_from_i32(n: i32) -> i32
        (
            "__str_from_i32",
            vec![("n".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_from_f64(n: f64) -> i32
        (
            "__str_from_f64",
            vec![("n".into(), WasmType::F64)],
            WasmType::I32,
        ),
        // __str_split(s: i32, delim: i32) -> i32 (returns Array<string> pointer)
        (
            "__str_split",
            vec![("s".into(), WasmType::I32), ("delim".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_replace(s: i32, search: i32, replacement: i32) -> i32
        (
            "__str_replace",
            vec![
                ("s".into(), WasmType::I32),
                ("search".into(), WasmType::I32),
                ("replacement".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_parseInt(s: i32) -> i32
        (
            "__str_parseInt",
            vec![("s".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_parseFloat(s: i32) -> f64
        (
            "__str_parseFloat",
            vec![("s".into(), WasmType::I32)],
            WasmType::F64,
        ),
        // __str_fromCharCode(code: i32) -> i32
        (
            "__str_fromCharCode",
            vec![("code".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_repeat(s: i32, count: i32) -> i32
        (
            "__str_repeat",
            vec![("s".into(), WasmType::I32), ("count".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_padStart(s: i32, targetLen: i32, fill: i32) -> i32
        (
            "__str_padStart",
            vec![
                ("s".into(), WasmType::I32),
                ("targetLen".into(), WasmType::I32),
                ("fill".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_padEnd(s: i32, targetLen: i32, fill: i32) -> i32
        (
            "__str_padEnd",
            vec![
                ("s".into(), WasmType::I32),
                ("targetLen".into(), WasmType::I32),
                ("fill".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_concat(a: i32, b: i32) -> i32 — runtime 2-string concat for
        // Array.join. String `+` goes through emit_fused_string_chain and does
        // NOT use this helper.
        (
            "__str_concat",
            vec![("a".into(), WasmType::I32), ("b".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_replaceAll(s: i32, search: i32, replacement: i32) -> i32
        (
            "__str_replaceAll",
            vec![
                ("s".into(), WasmType::I32),
                ("search".into(), WasmType::I32),
                ("replacement".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_toFixed(n: f64, digits: i32) -> i32
        (
            "__str_toFixed",
            vec![("n".into(), WasmType::F64), ("digits".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_toPrecision(n: f64, precision: i32) -> i32
        (
            "__str_toPrecision",
            vec![
                ("n".into(), WasmType::F64),
                ("precision".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_toExponential(n: f64, digits: i32) -> i32
        (
            "__str_toExponential",
            vec![
                ("n".into(), WasmType::F64),
                ("digits".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
    ];

    // Hash/equality helpers for Map/Set keys. The precompiled bodies live
    // alongside the string helpers in `tscc_helpers.wasm`; they share the
    // same registration path via `precompiled::find_export`.
    helpers.extend(super::hash_builtins::hash_helper_sigs());

    // Split registration into two phases so precompiled helpers can live in a
    // contiguous block after all hand-written ones — this keeps
    // `compile_string_helpers` emission order matching registration order,
    // even when the bundle pulls in extra functions via transitive closure.
    let mut precompiled_sigs: std::collections::HashMap<
        String,
        (Vec<(String, WasmType)>, WasmType),
    > = std::collections::HashMap::new();

    // Phase 1: hand-written helpers register by name immediately; precompiled
    // exports have their signatures stashed for phase 3 (the bundle may pull
    // them in via closure even if not directly `used`).
    for (name, params, ret) in helpers {
        if super::precompiled::find_export(name).is_some() {
            precompiled_sigs.insert(name.to_string(), (params, ret));
        } else if used.contains(name) {
            ctx.register_func(name, &params, ret, false).unwrap();
        }
    }

    // Phase 2: seed the bundle from used precompiled exports, walk the call
    // graph, then expand with dynamic dispatch. Inline (L_splice) helpers are
    // never registered as bundle functions (the splicer pastes their bodies
    // at each call site), BUT when an inline helper's body has its own
    // `call` instructions, those callees land in the spliced output and must
    // still be registered in tscc's function space — otherwise the splicer's
    // byte rewrite maps them to `u32::MAX`. So we seed each inline helper's
    // call-site targets alongside the regular seeds and let
    // `transitive_closure` do the rest.
    let import_count = super::precompiled::PRECOMPILED_IMPORTS.len() as u32;
    let mut seeds: Vec<usize> = Vec::new();
    for (i, pf) in super::precompiled::PRECOMPILED_FUNCS.iter().enumerate() {
        match (pf.is_inline, pf.name) {
            (false, Some(name)) if used.contains(name) => seeds.push(i),
            (true, Some(name)) if used.contains(name) => {
                for cs in pf.call_sites {
                    if cs.original_callee >= import_count {
                        seeds.push((cs.original_callee - import_count) as usize);
                    }
                }
            }
            _ => {}
        }
    }
    let direct_closure = super::precompiled::transitive_closure(&seeds);
    let (registered_bundle, requires_table) =
        super::precompiled::expand_with_dynamic_dispatch(direct_closure);

    // Tscc needs __arena_ptr to exist if any helper in the bundle got pulled
    // in — the helper-arena-alloc shim (registered below) bumps it, and
    // helpers' string allocations all flow through that shim.
    if !registered_bundle.is_empty() {
        ctx.init_arena();
    }

    // Phase 3a: register helper function imports. The helper bundle's import
    // table lists the `(module, name, signature)` triples that its internal
    // functions `call` into. Each one needs a tscc function in the same
    // signature shape — some we know how to synthesize (the arena shim),
    // others would be user-provided host imports (not supported yet).
    let num_imports = super::precompiled::PRECOMPILED_IMPORTS.len();
    let num_bundle = super::precompiled::PRECOMPILED_FUNCS.len();
    let mut func_index_map = vec![u32::MAX; num_imports + num_bundle];

    for (import_idx, imp) in super::precompiled::PRECOMPILED_IMPORTS.iter().enumerate() {
        let tscc_idx = match (imp.module, imp.name) {
            ("env", "__tscc_arena_alloc") => register_helper_arena_alloc(ctx),
            _ => panic!(
                "helper wasm imports {}::{} — no tscc handler registered for it",
                imp.module, imp.name
            ),
        };
        func_index_map[import_idx] = tscc_idx;
    }

    // Phase 3b: register each bundle (internal) function. Exports go through
    // `register_func`; internals use `register_raw_func` (supports any
    // ValType including i64). `exposed` is a test/debug hook that forces a
    // named export on specific helpers so the host can call them directly.
    for &bundle_idx in &registered_bundle {
        let pf = &super::precompiled::PRECOMPILED_FUNCS[bundle_idx];
        let tscc_idx = if let Some(name) = pf.name {
            let (params, ret) = precompiled_sigs
                .get(name)
                .unwrap_or_else(|| panic!("no WasmType signature registered for precompiled export '{name}'"));
            let is_export = exposed.contains(name);
            ctx.register_func(name, params, *ret, is_export).unwrap()
        } else {
            let synthetic = format!("__helper_internal_{bundle_idx}");
            ctx.register_raw_func(&synthetic, pf.params.to_vec(), pf.results.to_vec())
        };
        func_index_map[num_imports + bundle_idx] = tscc_idx;
    }

    // Phase 4: map every call_indirect type referenced by registered helpers
    // into tscc's merged type section.
    let mut type_index_map = vec![u32::MAX; super::precompiled::PRECOMPILED_TYPES.len()];
    for &bundle_idx in &registered_bundle {
        let pf = &super::precompiled::PRECOMPILED_FUNCS[bundle_idx];
        for cs in pf.call_indirect_sites {
            let helper_ty = cs.original_type_idx as usize;
            if type_index_map[helper_ty] == u32::MAX {
                let t = &super::precompiled::PRECOMPILED_TYPES[helper_ty];
                let tscc_ty = ctx.get_or_add_type_sig(t.params.to_vec(), t.results.to_vec());
                type_index_map[helper_ty] = tscc_ty;
            }
        }
    }

    // Phase 5: register every helper global that any registered body
    // references via `global.get/set`. Each becomes a new internal tscc
    // global with the rustc-emitted init value (stack pointer, __data_end,
    // __heap_base, …). They don't enter `ctx.globals` (which is reserved for
    // named tscc-visible globals) — we track them only by index.
    let mut global_index_map =
        vec![u32::MAX; super::precompiled::PRECOMPILED_GLOBALS.len()];
    for &bundle_idx in &registered_bundle {
        let pf = &super::precompiled::PRECOMPILED_FUNCS[bundle_idx];
        for gs in pf.global_sites {
            let helper_g = gs.original_global_idx as usize;
            if global_index_map[helper_g] == u32::MAX {
                let g = &super::precompiled::PRECOMPILED_GLOBALS[helper_g];
                global_index_map[helper_g] = register_raw_helper_global(ctx, g);
            }
        }
    }

    let helper_table_index = if requires_table { 1 } else { 0 };
    let requires_data =
        !registered_bundle.is_empty() && super::precompiled::has_data();

    // Phase 6: synthesize a wrapper for each inline (L_splice) helper that
    // the caller asked to expose via the export table. Inline helpers have no
    // bundle slot of their own, so without a wrapper they're unreachable from
    // outside the splicer. The wrapper body is emitted in
    // `compile_string_helpers` once the rewrite plan is fully built.
    //
    // Iterate `exposed` in sorted order: `HashSet<String>::iter()` is seeded
    // by `RandomState` and reshuffles across process instances, which would
    // leak into wrapper registration order and thus into the emitted wasm's
    // function-index space. Sorting the set before iterating is the only
    // place this non-determinism can enter codegen — every other HashSet
    // access is either `.contains()` or writes into another HashSet.
    let mut exposed_sorted: Vec<&String> = exposed.iter().collect();
    exposed_sorted.sort();
    let mut exposed_inline_wrappers: Vec<InlineWrapperReg> = Vec::new();
    for name in exposed_sorted {
        if let Some(pf) = super::precompiled::find_inline(name) {
            let helper_name = pf
                .name
                .expect("find_inline only returns named precompiled helpers");
            let params = inline_wrapper_params(pf, helper_name);
            let ret = inline_wrapper_ret(pf, helper_name);
            ctx.register_func(helper_name, &params, ret, true).unwrap();
            exposed_inline_wrappers.push(InlineWrapperReg {
                name: helper_name,
                params,
                ret,
            });
        }
    }

    HelperRegistration {
        registered_bundle,
        func_index_map,
        type_index_map,
        global_index_map,
        requires_table,
        requires_data,
        helper_table_index,
        exposed_inline_wrappers,
    }
}

/// Convert a precompiled inline helper's `ValType` params into the
/// `(name, WasmType)` shape `register_func` wants. Supports the same param
/// types the splicer's `wasm_type_from_val_type` does (i32, f64).
fn inline_wrapper_params(
    pf: &super::precompiled::PrecompiledFunc,
    helper_name: &str,
) -> Vec<(String, WasmType)> {
    pf.params
        .iter()
        .enumerate()
        .map(|(i, vt)| (format!("p{i}"), val_type_to_wasm(*vt, helper_name)))
        .collect()
}

/// Convert a precompiled inline helper's single result `ValType` to `WasmType`.
/// Multi-result helpers are rejected — none currently exist in inline form.
fn inline_wrapper_ret(
    pf: &super::precompiled::PrecompiledFunc,
    helper_name: &str,
) -> WasmType {
    match pf.results {
        [] => WasmType::Void,
        [vt] => val_type_to_wasm(*vt, helper_name),
        _ => panic!(
            "inline helper `{helper_name}` returns multiple values — \
             splicer wrapper synthesis only supports 0 or 1 results"
        ),
    }
}

fn val_type_to_wasm(vt: wasm_encoder::ValType, helper_name: &str) -> WasmType {
    match vt {
        wasm_encoder::ValType::I32 => WasmType::I32,
        wasm_encoder::ValType::F64 => WasmType::F64,
        other => panic!(
            "inline helper `{helper_name}` uses unsupported wrapper type {other:?}"
        ),
    }
}

/// Synthesize a tscc function `__helper_arena_alloc(size: i32) -> i32` that
/// services every helper's `env::__tscc_arena_alloc` import call with a plain
/// bump on tscc's `__arena_ptr`. No overflow check — helper allocations are
/// short-lived within a single host call, matching the arena lifecycle. If
/// the host has opted in to checked growth via `ArenaOverflow != Unchecked`,
/// the existing `__arena_alloc` function already provides the checked
/// variant; we could route there, but it has an extra branch helpers don't
/// need for their own allocations.
fn register_helper_arena_alloc(ctx: &mut ModuleContext) -> u32 {
    use wasm_encoder::{Function, Instruction};

    // Register the function signature and get an index.
    let params = vec![("size".to_string(), WasmType::I32)];
    let func_idx = ctx
        .register_func("__helper_arena_alloc", &params, WasmType::I32, false)
        .unwrap();

    // Build the body: ptr = arena_ptr; arena_ptr += size; return ptr.
    let arena_idx = ctx
        .arena_ptr_global
        .expect("__arena_ptr global must be initialized before helper registration");
    let mut func = Function::new(vec![(1, wasm_encoder::ValType::I32)]);
    let size_param = 0u32;
    let ret_local = 1u32;

    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ret_local));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(size_param));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(ret_local));
    func.instruction(&Instruction::End);

    // Stash the body — it gets emitted alongside the other helper bodies in
    // `compile_string_helpers` via the `helper_arena_alloc_body` field on
    // `HelperRegistration`. For now, we keep the emission path in
    // `compile_string_helpers` and return just the func_idx.
    ctx.helper_arena_alloc_body
        .replace(Some(func));
    func_idx
}

/// Register a tscc global mirroring a rustc-emitted helper global (stack
/// pointer, `__data_end`, `__heap_base`). Uses a synthetic name so it never
/// collides with user-visible globals. Returns the tscc global index.
fn register_raw_helper_global(
    ctx: &mut ModuleContext,
    g: &super::precompiled::DeclaredGlobal,
) -> u32 {
    let idx = ctx.next_global_index_internal();
    let name = format!("__helper_global_{idx}");
    // For the emission pass, tscc expects:
    //  - entry in `ctx.globals` keyed by name (for mutability lookup)
    //  - entry in `ctx.global_inits` at position `idx`
    //  - entry in `ctx.mutable_globals` if mutable (matched by name)
    let ty = match g.val_type {
        wasm_encoder::ValType::I32 => WasmType::I32,
        wasm_encoder::ValType::F64 => WasmType::F64,
        // Other wasm types don't have a WasmType equivalent; globals of those
        // types would need raw plumbing. Rustc never emits them for our
        // helpers today, so error loudly if it ever starts.
        other => panic!(
            "unsupported helper global type {:?} (only i32/f64 are handled)",
            other
        ),
    };
    ctx.globals.insert(name.clone(), (idx, ty));
    let init = match g.init {
        super::precompiled::GlobalInit::I32(v) => crate::codegen::module::GlobalInit::I32(v),
        super::precompiled::GlobalInit::I64(v) => crate::codegen::module::GlobalInit::I64(v),
        super::precompiled::GlobalInit::F64(v) => crate::codegen::module::GlobalInit::F64(v),
        super::precompiled::GlobalInit::F32(_) => {
            panic!("f32 helper global init not yet supported")
        }
    };
    ctx.global_inits.push(init);
    if g.mutable {
        ctx.mutable_globals.insert(name);
    }
    idx
}

/// Compile bodies for the string helpers that were registered.
/// Emits hand-written helpers first (in `STRING_HELPER_NAMES` order, skipping
/// precompiled exports) and then the precompiled bundle in registration order,
/// matching exactly what `register_string_helpers` did.
pub fn compile_string_helpers(
    ctx: &ModuleContext,
    used: &HashSet<String>,
    reg: &HelperRegistration,
) -> Vec<Function> {
    let arena_idx = ctx.arena_ptr_global.unwrap();
    let mut out: Vec<Function> = Vec::new();

    for name in STRING_HELPER_NAMES {
        if !used.contains(*name) {
            continue;
        }
        if super::precompiled::find_export(name).is_some() {
            continue;
        }
        out.push(compile_helper(name, arena_idx));
    }

    // Emit the helper-arena-alloc shim body (registered during
    // `register_string_helpers` via &mut ctx, body stashed on ctx).
    if let Some(body) = ctx.helper_arena_alloc_body.borrow_mut().take() {
        out.push(body);
    }

    let plan = super::precompiled::RewritePlan {
        func_index_map: &reg.func_index_map,
        type_index_map: &reg.type_index_map,
        global_index_map: &reg.global_index_map,
        helper_table_index: reg.helper_table_index,
    };
    for &bundle_idx in &reg.registered_bundle {
        out.push(super::precompiled::build_function(bundle_idx, &plan));
    }

    // Inline-helper exposure wrappers. Order matches the registration order
    // in `register_string_helpers` Phase 6 — must stay aligned because the
    // wasm function-index space is positional.
    for wrapper in &reg.exposed_inline_wrappers {
        out.push(compile_inline_wrapper(ctx, wrapper, &plan));
    }

    out
}

/// Build the body of a synthesized inline-helper wrapper: load each param,
/// splice the inline helper's body, return the result. Lives outside any TS
/// source, so source map / error spans pass `""`.
fn compile_inline_wrapper(
    ctx: &ModuleContext,
    wrapper: &InlineWrapperReg,
    plan: &super::precompiled::RewritePlan<'_>,
) -> Function {
    use super::func::FuncContext;
    use wasm_encoder::Instruction;

    let pf = super::precompiled::find_inline(wrapper.name).unwrap_or_else(|| {
        panic!(
            "inline helper `{}` registered as wrapper but not found in PRECOMPILED_FUNCS",
            wrapper.name
        )
    });
    let mut fctx = FuncContext::new(ctx, &wrapper.params, wrapper.ret, "");
    for i in 0..wrapper.params.len() as u32 {
        fctx.push(Instruction::LocalGet(i));
    }
    super::splice::splice_inline_call(&mut fctx, pf, plan).unwrap_or_else(|e| {
        panic!(
            "splice failed for inline-helper wrapper `{}`: {e:?}",
            wrapper.name
        )
    });
    let (func, _source_map) = fctx.finish();
    func
}

/// Pre-codegen AST scan that returns the set of string runtime helpers the program
/// will actually call. Used to register only what's needed (tree-shaking).
///
/// Conservative by design: the scanner lacks type info, so it over-approximates for
/// `+`, `==`/`!=`, and comparison operators by enabling the matching helper whenever
/// the program contains both such an operator and at least one string-like literal.
/// Under-approximation would crash at codegen (get_func unwrap), so we prefer slight
/// bloat over correctness holes.
pub fn collect_used_helpers(program: &Program<'_>) -> HashSet<String> {
    let mut s = Scanner::default();
    for stmt in &program.body {
        s.walk_stmt(stmt);
    }
    s.into_set()
}

#[derive(Default)]
struct Scanner {
    has_string_literal: bool,
    has_template_with_expr: bool,
    has_plus: bool,
    has_eq_op: bool,
    has_cmp_op: bool,
    method_names: HashSet<String>,
    identifier_calls: HashSet<String>,
    has_string_from_char_code: bool,
}

impl Scanner {
    fn into_set(self) -> HashSet<String> {
        let mut used = HashSet::new();
        let add = |n: &str, set: &mut HashSet<String>| {
            set.insert(n.to_string());
        };

        // Template literals with interpolated expressions coerce each expression to a
        // string via __str_from_i32 / __str_from_f64. The concat itself is fused inline
        // (see emit_fused_string_chain in codegen/expr.rs) and does NOT call __str_concat.
        if self.has_template_with_expr {
            add("__str_from_i32", &mut used);
            add("__str_from_f64", &mut used);
        }

        // Strings can also enter the program via `String.fromCharCode` or string-returning
        // methods (slice, toLowerCase, etc.). If any such source exists, treat the program
        // as "has strings" for operator-based helper inclusion.
        let string_returning_methods = [
            "slice",
            "substring",
            "toLowerCase",
            "toUpperCase",
            "trim",
            "trimStart",
            "trimEnd",
            "replace",
            "replaceAll",
            "repeat",
            "padStart",
            "padEnd",
            "concat",
        ];
        let has_string_source = self.has_string_literal
            || self.has_string_from_char_code
            || string_returning_methods
                .iter()
                .any(|m| self.method_names.contains(*m));

        // `+` with strings present: enable the coercion helpers so numeric operands
        // can be formatted. The concat is fused inline — no __str_concat call.
        if has_string_source && self.has_plus {
            add("__str_from_i32", &mut used);
            add("__str_from_f64", &mut used);
        }

        if has_string_source && self.has_eq_op {
            add("__str_eq", &mut used);
        }
        if has_string_source && self.has_cmp_op {
            add("__str_cmp", &mut used);
        }

        if self.has_string_from_char_code {
            add("__str_fromCharCode", &mut used);
        }

        let method_map: &[(&str, &str)] = &[
            ("indexOf", "__str_indexOf"),
            ("lastIndexOf", "__str_lastIndexOf"),
            ("includes", "__str_includes"),
            ("startsWith", "__str_startsWith"),
            ("endsWith", "__str_endsWith"),
            ("slice", "__str_slice"),
            ("substring", "__str_slice"),
            ("toLowerCase", "__str_toLower"),
            ("toUpperCase", "__str_toUpper"),
            ("trim", "__str_trim"),
            ("trimStart", "__str_trimStart"),
            ("trimEnd", "__str_trimEnd"),
            ("split", "__str_split"),
            ("replace", "__str_replace"),
            ("replaceAll", "__str_replaceAll"),
            ("repeat", "__str_repeat"),
            ("padStart", "__str_padStart"),
            ("padEnd", "__str_padEnd"),
        ];
        for (method, helper) in method_map {
            if self.method_names.contains(*method) {
                add(helper, &mut used);
            }
        }

        // Number.prototype.toString() needs the coercion helpers.
        if self.method_names.contains("toString") {
            add("__str_from_i32", &mut used);
            add("__str_from_f64", &mut used);
        }

        // Number.prototype.toFixed(digits) needs the dedicated helper.
        if self.method_names.contains("toFixed") {
            add("__str_toFixed", &mut used);
        }

        // Number.prototype.toPrecision(digits) needs the dedicated helper.
        // The no-arg form routes directly to __str_from_f64 at codegen time
        // (ES § 21.1.3.5: `toPrecision()` is `toString()`), so pull that
        // helper in too — the dependency is invisible to transitive closure.
        if self.method_names.contains("toPrecision") {
            add("__str_toPrecision", &mut used);
            add("__str_from_f64", &mut used);
        }

        // Number.prototype.toExponential(digits) needs the dedicated helper.
        if self.method_names.contains("toExponential") {
            add("__str_toExponential", &mut used);
        }

        // `parseInt` / `parseFloat` can appear as bare identifiers OR as
        // `Number.parseInt` / `Number.parseFloat` (ES6 aliases). Method-name
        // presence is conservative — accepts any `.parseInt(...)` call.
        if self.identifier_calls.contains("parseInt") || self.method_names.contains("parseInt") {
            add("__str_parseInt", &mut used);
        }
        if self.identifier_calls.contains("parseFloat") || self.method_names.contains("parseFloat")
        {
            add("__str_parseFloat", &mut used);
        }

        // Array.join: runtime-loop concatenation needs __str_concat plus the
        // numeric stringifiers for non-string element arrays. Over-includes
        // for string-element arrays — cheap in tree-shaking terms.
        if self.method_names.contains("join") {
            add("__str_concat", &mut used);
            add("__str_from_i32", &mut used);
            add("__str_from_f64", &mut used);
        }

        // String.prototype.concat(other) — also needs the runtime 2-string
        // helper. Over-includes for the rare case of `[].concat` on arrays.
        if self.method_names.contains("concat") && has_string_source {
            add("__str_concat", &mut used);
        }

        used
    }

    fn walk_stmt(&mut self, stmt: &Statement<'_>) {
        match stmt {
            Statement::ExpressionStatement(s) => self.walk_expr(&s.expression),
            Statement::BlockStatement(b) => {
                for s in &b.body {
                    self.walk_stmt(s);
                }
            }
            Statement::IfStatement(s) => {
                self.walk_expr(&s.test);
                self.walk_stmt(&s.consequent);
                if let Some(alt) = &s.alternate {
                    self.walk_stmt(alt);
                }
            }
            Statement::WhileStatement(s) => {
                self.walk_expr(&s.test);
                self.walk_stmt(&s.body);
            }
            Statement::DoWhileStatement(s) => {
                self.walk_expr(&s.test);
                self.walk_stmt(&s.body);
            }
            Statement::ForStatement(s) => {
                if let Some(init) = &s.init {
                    match init {
                        ForStatementInit::VariableDeclaration(d) => self.walk_var_decl(d),
                        _ => self.walk_expr(init.to_expression()),
                    }
                }
                if let Some(test) = &s.test {
                    self.walk_expr(test);
                }
                if let Some(update) = &s.update {
                    self.walk_expr(update);
                }
                self.walk_stmt(&s.body);
            }
            Statement::ForOfStatement(s) => {
                if let ForStatementLeft::VariableDeclaration(d) = &s.left {
                    self.walk_var_decl(d);
                }
                self.walk_expr(&s.right);
                self.walk_stmt(&s.body);
            }
            Statement::ForInStatement(s) => {
                self.walk_expr(&s.right);
                self.walk_stmt(&s.body);
            }
            Statement::SwitchStatement(s) => {
                self.walk_expr(&s.discriminant);
                for case in &s.cases {
                    if let Some(t) = &case.test {
                        self.walk_expr(t);
                    }
                    for s in &case.consequent {
                        self.walk_stmt(s);
                    }
                }
            }
            Statement::ReturnStatement(s) => {
                if let Some(arg) = &s.argument {
                    self.walk_expr(arg);
                }
            }
            Statement::ThrowStatement(s) => self.walk_expr(&s.argument),
            Statement::TryStatement(s) => {
                for st in &s.block.body {
                    self.walk_stmt(st);
                }
                if let Some(h) = &s.handler {
                    for st in &h.body.body {
                        self.walk_stmt(st);
                    }
                }
                if let Some(f) = &s.finalizer {
                    for st in &f.body {
                        self.walk_stmt(st);
                    }
                }
            }
            Statement::LabeledStatement(s) => self.walk_stmt(&s.body),
            Statement::VariableDeclaration(d) => self.walk_var_decl(d),
            Statement::FunctionDeclaration(f) => {
                if let Some(body) = &f.body {
                    for st in &body.statements {
                        self.walk_stmt(st);
                    }
                }
            }
            Statement::ClassDeclaration(c) => {
                for element in &c.body.body {
                    if let ClassElement::MethodDefinition(m) = element
                        && let Some(body) = &m.value.body
                    {
                        for st in &body.statements {
                            self.walk_stmt(st);
                        }
                    }
                }
            }
            Statement::ExportDefaultDeclaration(e) => {
                if let ExportDefaultDeclarationKind::FunctionDeclaration(f) = &e.declaration
                    && let Some(body) = &f.body
                {
                    for st in &body.statements {
                        self.walk_stmt(st);
                    }
                }
            }
            Statement::ExportNamedDeclaration(e) => {
                if let Some(Declaration::FunctionDeclaration(f)) = &e.declaration
                    && let Some(body) = &f.body
                {
                    for st in &body.statements {
                        self.walk_stmt(st);
                    }
                }
                if let Some(Declaration::VariableDeclaration(d)) = &e.declaration {
                    self.walk_var_decl(d);
                }
            }
            _ => {}
        }
    }

    fn walk_var_decl(&mut self, d: &VariableDeclaration<'_>) {
        for decl in &d.declarations {
            if let Some(init) = &decl.init {
                self.walk_expr(init);
            }
        }
    }

    fn walk_expr(&mut self, expr: &Expression<'_>) {
        match expr {
            Expression::StringLiteral(_) => {
                self.has_string_literal = true;
            }
            Expression::TemplateLiteral(t) => {
                self.has_string_literal = true;
                if !t.expressions.is_empty() {
                    self.has_template_with_expr = true;
                }
                for e in &t.expressions {
                    self.walk_expr(e);
                }
            }
            Expression::BinaryExpression(b) => {
                use oxc_ast::ast::BinaryOperator as Op;
                match b.operator {
                    Op::Addition => self.has_plus = true,
                    Op::Equality | Op::Inequality | Op::StrictEquality | Op::StrictInequality => {
                        self.has_eq_op = true
                    }
                    Op::LessThan | Op::LessEqualThan | Op::GreaterThan | Op::GreaterEqualThan => {
                        self.has_cmp_op = true
                    }
                    _ => {}
                }
                self.walk_expr(&b.left);
                self.walk_expr(&b.right);
            }
            Expression::LogicalExpression(l) => {
                self.walk_expr(&l.left);
                self.walk_expr(&l.right);
            }
            Expression::UnaryExpression(u) => self.walk_expr(&u.argument),
            Expression::UpdateExpression(_) => {}
            Expression::AssignmentExpression(a) => {
                self.walk_expr(&a.right);
            }
            Expression::ConditionalExpression(c) => {
                self.walk_expr(&c.test);
                self.walk_expr(&c.consequent);
                self.walk_expr(&c.alternate);
            }
            Expression::CallExpression(c) => self.walk_call(c),
            Expression::NewExpression(n) => {
                for a in &n.arguments {
                    self.walk_expr(a.to_expression());
                }
            }
            Expression::ArrayExpression(a) => {
                for el in &a.elements {
                    if let ArrayExpressionElement::SpreadElement(s) = el {
                        self.walk_expr(&s.argument);
                    } else if let Some(e) = el.as_expression() {
                        self.walk_expr(e);
                    }
                }
            }
            Expression::ObjectExpression(o) => {
                for prop in &o.properties {
                    if let ObjectPropertyKind::ObjectProperty(p) = prop {
                        self.walk_expr(&p.value);
                    }
                }
            }
            Expression::ParenthesizedExpression(p) => self.walk_expr(&p.expression),
            Expression::ChainExpression(c) => match &c.expression {
                ChainElement::CallExpression(call) => self.walk_call(call),
                ChainElement::StaticMemberExpression(m) => self.walk_expr(&m.object),
                ChainElement::ComputedMemberExpression(m) => {
                    self.walk_expr(&m.object);
                    self.walk_expr(&m.expression);
                }
                _ => {}
            },
            Expression::StaticMemberExpression(m) => self.walk_expr(&m.object),
            Expression::ComputedMemberExpression(m) => {
                self.walk_expr(&m.object);
                self.walk_expr(&m.expression);
            }
            Expression::ArrowFunctionExpression(a) => {
                for st in &a.body.statements {
                    self.walk_stmt(st);
                }
            }
            Expression::TSAsExpression(a) => self.walk_expr(&a.expression),
            Expression::SequenceExpression(s) => {
                for e in &s.expressions {
                    self.walk_expr(e);
                }
            }
            _ => {}
        }
    }

    fn walk_call(&mut self, call: &CallExpression<'_>) {
        match &call.callee {
            Expression::StaticMemberExpression(m) => {
                let method = m.property.name.as_str();
                self.method_names.insert(method.to_string());
                if let Expression::Identifier(obj) = &m.object
                    && obj.name.as_str() == "String"
                    && method == "fromCharCode"
                {
                    self.has_string_from_char_code = true;
                }
                self.walk_expr(&m.object);
            }
            Expression::Identifier(ident) => {
                self.identifier_calls
                    .insert(ident.name.as_str().to_string());
            }
            other => self.walk_expr(other),
        }
        for arg in &call.arguments {
            self.walk_expr(arg.to_expression());
        }
    }
}

fn compile_helper(name: &str, arena_idx: u32) -> Function {
    match name {
        "__str_toLower" => transform::build_str_to_lower(arena_idx),
        "__str_toUpper" => transform::build_str_to_upper(arena_idx),
        "__str_trim" => transform::build_str_trim_impl(arena_idx, true, true),
        "__str_trimStart" => transform::build_str_trim_impl(arena_idx, true, false),
        "__str_trimEnd" => transform::build_str_trim_impl(arena_idx, false, true),
        "__str_from_i32" => convert::build_str_from_i32(arena_idx),
        "__str_split" => split_join::build_str_split(arena_idx),
        "__str_replace" => transform::build_str_replace(arena_idx),
        "__str_parseInt" => convert::build_str_parse_int(),
        "__str_fromCharCode" => convert::build_str_from_char_code(arena_idx),
        "__str_repeat" => transform::build_str_repeat(arena_idx),
        "__str_padStart" => transform::build_str_pad_start(arena_idx),
        "__str_padEnd" => transform::build_str_pad_end(arena_idx),
        "__str_concat" => transform::build_str_concat(arena_idx),
        "__str_replaceAll" => transform::build_str_replace_all(arena_idx),
        _ => unreachable!("unknown string helper: {name}"),
    }
}

pub(super) fn mem_load_i32(offset: u64) -> Instruction<'static> {
    Instruction::I32Load(MemArg {
        offset,
        align: 2,
        memory_index: 0,
    })
}

pub(super) fn mem_store_i32(offset: u64) -> Instruction<'static> {
    Instruction::I32Store(MemArg {
        offset,
        align: 2,
        memory_index: 0,
    })
}

pub(super) fn mem_load8_u(offset: u64) -> Instruction<'static> {
    Instruction::I32Load8U(MemArg {
        offset,
        align: 0,
        memory_index: 0,
    })
}

pub(super) fn mem_store8(offset: u64) -> Instruction<'static> {
    Instruction::I32Store8(MemArg {
        offset,
        align: 0,
        memory_index: 0,
    })
}

/// Emit: (byte == 32 || byte == 9 || byte == 10 || byte == 13) → i32 on stack
pub(super) fn emit_is_whitespace(func: &mut Function, byte_local: u32) {
    func.instruction(&Instruction::LocalGet(byte_local));
    func.instruction(&Instruction::I32Const(32)); // space
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::LocalGet(byte_local));
    func.instruction(&Instruction::I32Const(9)); // tab
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::LocalGet(byte_local));
    func.instruction(&Instruction::I32Const(10)); // LF
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::LocalGet(byte_local));
    func.instruction(&Instruction::I32Const(13)); // CR
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::I32Or);
}
