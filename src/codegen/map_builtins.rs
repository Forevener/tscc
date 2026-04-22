//! Compiler-owned `Map<K, V>` support.
//!
//! Map is not a user-declared class — tscc synthesizes a `ClassLayout` for
//! each concrete `(K, V)` pair seen in user source. Collection rides on
//! Phase A's generic instantiation walker (see `generics::collect_instantiations`),
//! which records a `MapInstantiation` whenever it sees `Map<K, V>` in a type
//! annotation or `new Map<K, V>()`. Registration happens in a dedicated pass
//! in `compile_module` before class topo-sort. Method dispatch
//! (`get`/`set`/`has`/…) will land in a future `expr/map.rs` dispatcher.
//!
//! Object layout in linear memory (all fields are `i32`, uniform alignment):
//!
//! | offset | field        | purpose                                          |
//! |--------|--------------|--------------------------------------------------|
//! | 0      | buckets_ptr  | pointer to bucket array (0 when unallocated)     |
//! | 4      | size         | count of occupied entries                        |
//! | 8      | capacity     | allocated bucket count (always a power of two)   |
//! | 12     | head_idx     | first-inserted bucket index (-1 when empty)      |
//! | 16     | tail_idx     | last-inserted bucket index  (-1 when empty)      |
//!
//! The bucket array itself is allocated lazily on first `set()` (Step 2+).

use std::collections::{HashMap, HashSet};

use crate::error::CompileError;
use crate::types::{BoundType, WasmType};

use super::classes::{ClassLayout, ClassRegistry};
use super::hash_table::{align_up, bound_align, bound_size};

/// Source-level name used to trigger Map recognition in type annotations and
/// `new` expressions.
pub const MAP_BASE: &str = "Map";

/// Number of type arguments `Map` expects (`K`, `V`).
pub const MAP_ARITY: usize = 2;

/// Field layout of a Map header object. Order fixes offsets; all fields are
/// `i32` so no alignment padding is needed between them.
pub const MAP_FIELDS: &[(&str, WasmType)] = &[
    ("buckets_ptr", WasmType::I32),
    ("size", WasmType::I32),
    ("capacity", WasmType::I32),
    ("head_idx", WasmType::I32),
    ("tail_idx", WasmType::I32),
];

/// One concrete use of `Map<K, V>` discovered in user source.
///
/// `key_ty` and `value_ty` ride along from the collector so later steps can
/// size buckets, route hashing (FxHash for i32/f64/bool/class refs, xxh3 for
/// string), and emit per-monomorphization method bodies without re-parsing
/// the mangled name.
#[derive(Debug, Clone)]
pub struct MapInstantiation {
    pub mangled_name: String,
    pub key_ty: BoundType,
    pub value_ty: BoundType,
}

/// Everything `emit_new_map` and the method dispatcher need to know about a
/// single `Map<K, V>` monomorphization. Stored in `ModuleContext::map_info`
/// keyed on `mangled_name`.
#[derive(Debug, Clone)]
pub struct MapInfo {
    pub key_ty: BoundType,
    pub value_ty: BoundType,
    pub bucket: BucketLayout,
}

/// Mangled Map class name for a given `(K, V)` pair, e.g. `Map$string$i32`.
pub fn mangle_map_name(key_ty: &BoundType, value_ty: &BoundType) -> String {
    format!(
        "{MAP_BASE}${}${}",
        key_ty.mangle_token(),
        value_ty.mangle_token()
    )
}

/// Return `true` when `name` refers to the compiler-owned `Map` template.
pub fn is_map_base(name: &str) -> bool {
    name == MAP_BASE
}

/// Per-monomorphization bucket layout. All offsets are byte offsets from the
/// start of the bucket; `total_size` is padded to `max(alignof(K), alignof(V),
/// 4)` so an array of buckets stays aligned.
///
/// Bucket layout in memory:
///
/// ```text
/// +-- 0 ------------ state: u8
/// |   (pad)
/// +-- key_offset --- key:   K
/// +-- next_offset -- next_insert: i32
/// +-- prev_offset -- prev_insert: i32
/// +-- value_offset - value: V
/// +-- total_size --  (next bucket starts here)
/// ```
#[derive(Debug, Clone, Copy)]
pub struct BucketLayout {
    pub state_offset: u32,
    pub key_offset: u32,
    pub next_offset: u32,
    pub prev_offset: u32,
    pub value_offset: u32,
    pub total_size: u32,
}

impl BucketLayout {
    pub fn compute(key_ty: &BoundType, value_ty: &BoundType) -> Self {
        let key_align = bound_align(key_ty);
        let value_align = bound_align(value_ty);
        let state_offset = 0;
        let key_offset = align_up(state_offset + 1, key_align);
        let next_offset = align_up(key_offset + bound_size(key_ty), 4);
        let prev_offset = next_offset + 4;
        let value_offset = align_up(prev_offset + 4, value_align);
        let unpadded_end = value_offset + bound_size(value_ty);
        let bucket_align = key_align.max(value_align).max(4);
        let total_size = align_up(unpadded_end, bucket_align);
        BucketLayout {
            state_offset,
            key_offset,
            next_offset,
            prev_offset,
            value_offset,
            total_size,
        }
    }
}

/// Name of the hash runtime helper that covers a given key type. Strings ride
/// on xxh3 (better distribution for prefix-heavy inputs); everything else hits
/// FxHash via the width-specific entry point.
pub fn hash_helper_for(key_ty: &BoundType) -> &'static str {
    match key_ty {
        BoundType::I32 => "__hash_fx_i32",
        BoundType::F64 => "__hash_fx_f64",
        BoundType::Bool => "__hash_fx_bool",
        BoundType::Str => "__hash_xxh3_str",
        BoundType::Class(_) => "__hash_fx_ptr",
    }
}

/// Name of the equality helper a key type dispatches to, or `None` when
/// inline `I32Eq` suffices (integer-shape keys and class identity).
pub fn equality_helper_for(key_ty: &BoundType) -> Option<&'static str> {
    match key_ty {
        BoundType::F64 => Some("__key_eq_f64"),
        BoundType::Str => Some("__str_eq"),
        BoundType::I32 | BoundType::Bool | BoundType::Class(_) => None,
    }
}

/// Runtime helpers the emitted method bodies will reference for `insts`.
/// Consumed by `compile_module` to seed `used_string_helpers` before
/// `register_string_helpers` runs — keeps registration tree-shaken when the
/// program doesn't use Maps and correct when it does.
///
/// Inline (L_splice) helpers are included alongside L_helper ones. They have
/// no bundle slot themselves (the splicer pastes their bodies at each call
/// site), but `register_string_helpers` consults the same `used` set to
/// decide which inline helpers' Call targets need to be registered in tscc's
/// function space — without that, an inline helper whose body calls
/// `memcmp` (e.g. `__str_eq`) would splice out a `Call(u32::MAX)`.
pub fn required_runtime_helpers(insts: &[MapInstantiation]) -> HashSet<String> {
    let mut out = HashSet::new();
    for inst in insts {
        out.insert(hash_helper_for(&inst.key_ty).to_string());
        if let Some(name) = equality_helper_for(&inst.key_ty) {
            out.insert(name.to_string());
        }
    }
    out
}

/// Synthesize a `ClassLayout` for a `Map<K, V>` instantiation and insert it
/// into `registry`. Step 1 establishes only the header; no methods are
/// registered yet — `emit_new` will allocate the header via arena bump and
/// leave fields at their arena-zero default (which is correct for `size` /
/// `capacity` / `buckets_ptr`; head/tail default to 0, not -1, and will be
/// fixed up once Step 2 adds a real constructor).
pub fn register_map_layout(
    registry: &mut ClassRegistry,
    mangled_name: &str,
) -> Result<(), CompileError> {
    if registry.get(mangled_name).is_some() {
        return Ok(());
    }
    let mut fields: Vec<(String, u32, WasmType)> = Vec::with_capacity(MAP_FIELDS.len());
    let mut field_map: HashMap<String, (u32, WasmType)> = HashMap::new();
    let mut own_field_names: HashSet<String> = HashSet::new();
    let mut offset: u32 = 0;
    for &(name, ty) in MAP_FIELDS {
        let width: u32 = match ty {
            WasmType::F64 => 8,
            _ => 4,
        };
        offset = (offset + width - 1) & !(width - 1);
        fields.push((name.to_string(), offset, ty));
        field_map.insert(name.to_string(), (offset, ty));
        own_field_names.insert(name.to_string());
        offset += width;
    }
    let size = if offset == 0 { 0 } else { (offset + 7) & !7 };

    registry.classes.insert(
        mangled_name.to_string(),
        ClassLayout {
            name: mangled_name.to_string(),
            size,
            fields,
            field_map,
            field_class_types: HashMap::new(),
            field_string_types: HashSet::new(),
            methods: HashMap::new(),
            parent: None,
            is_polymorphic: false,
            vtable_methods: Vec::new(),
            vtable_method_map: HashMap::new(),
            vtable_offset: 0,
            own_field_names,
        },
    );
    Ok(())
}
