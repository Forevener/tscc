//! Compiler-owned `Set<T>` support.
//!
//! Shares the open-addressing + insertion-chain machinery with `Map<K, V>`,
//! minus the value slot. Each concrete `T` gets its own synthesized
//! `ClassLayout`; the collector in `generics::collect_instantiations` records
//! a `SetInstantiation` whenever it sees `Set<T>` in a type annotation or
//! `new Set<T>()`. Registration happens in a dedicated pass in `compile_module`
//! alongside maps. Method dispatch (`add`/`has`/…) lands in `expr/set.rs`.
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

use std::collections::{HashMap, HashSet};

use crate::error::CompileError;
use crate::types::{BoundType, WasmType};

use super::classes::{ClassLayout, ClassRegistry};
use super::hash_table::{align_up, bound_align, bound_size};

/// Source-level name used to trigger Set recognition in type annotations and
/// `new` expressions.
pub const SET_BASE: &str = "Set";

/// Number of type arguments `Set` expects (`T`).
pub const SET_ARITY: usize = 1;

/// Field layout of a Set header object. Mirrors `MAP_FIELDS` — sets and maps
/// share the same header shape; only the bucket layout differs.
pub const SET_FIELDS: &[(&str, WasmType)] = &[
    ("buckets_ptr", WasmType::I32),
    ("size", WasmType::I32),
    ("capacity", WasmType::I32),
    ("head_idx", WasmType::I32),
    ("tail_idx", WasmType::I32),
];

/// One concrete use of `Set<T>` discovered in user source.
#[derive(Debug, Clone)]
pub struct SetInstantiation {
    pub mangled_name: String,
    pub elem_ty: BoundType,
}

/// Everything `emit_new_set` and the method dispatcher need to know about a
/// single `Set<T>` monomorphization. Stored in `ModuleContext::set_info` keyed
/// on `mangled_name`.
#[derive(Debug, Clone)]
pub struct SetInfo {
    pub elem_ty: BoundType,
    pub bucket: SetBucketLayout,
}

/// Mangled Set class name for a given `T`, e.g. `Set$string`.
pub fn mangle_set_name(elem_ty: &BoundType) -> String {
    format!("{SET_BASE}${}", elem_ty.mangle_token())
}

/// Return `true` when `name` refers to the compiler-owned `Set` template.
pub fn is_set_base(name: &str) -> bool {
    name == SET_BASE
}

/// Per-monomorphization bucket layout. All offsets are byte offsets from the
/// start of the bucket; `total_size` is padded to `max(alignof(T), 4)` so an
/// array of buckets stays aligned.
///
/// Bucket layout in memory:
///
/// ```text
/// +-- 0 ------------ state: u8
/// |   (pad)
/// +-- elem_offset -- elem:  T
/// +-- next_offset -- next_insert: i32
/// +-- prev_offset -- prev_insert: i32
/// +-- total_size --  (next bucket starts here)
/// ```
#[derive(Debug, Clone, Copy)]
pub struct SetBucketLayout {
    pub state_offset: u32,
    pub elem_offset: u32,
    pub next_offset: u32,
    pub prev_offset: u32,
    pub total_size: u32,
}

impl SetBucketLayout {
    pub fn compute(elem_ty: &BoundType) -> Self {
        let elem_align = bound_align(elem_ty);
        let state_offset = 0;
        let elem_offset = align_up(state_offset + 1, elem_align);
        let next_offset = align_up(elem_offset + bound_size(elem_ty), 4);
        let prev_offset = next_offset + 4;
        let unpadded_end = prev_offset + 4;
        let bucket_align = elem_align.max(4);
        let total_size = align_up(unpadded_end, bucket_align);
        SetBucketLayout {
            state_offset,
            elem_offset,
            next_offset,
            prev_offset,
            total_size,
        }
    }
}

/// Runtime helpers the emitted method bodies reference for `insts`. Reuses
/// `map_builtins::hash_helper_for` / `equality_helper_for` — sets and maps
/// dispatch to the same underlying precompiled helpers based on element type.
pub fn required_runtime_helpers(insts: &[SetInstantiation]) -> HashSet<String> {
    let mut out = HashSet::new();
    for inst in insts {
        out.insert(super::map_builtins::hash_helper_for(&inst.elem_ty).to_string());
        if let Some(name) = super::map_builtins::equality_helper_for(&inst.elem_ty) {
            out.insert(name.to_string());
        }
    }
    out
}

/// Synthesize a `ClassLayout` for a `Set<T>` instantiation and insert it into
/// `registry`. Establishes only the header; `emit_new_set` allocates it and
/// wires up the bucket array.
pub fn register_set_layout(
    registry: &mut ClassRegistry,
    mangled_name: &str,
) -> Result<(), CompileError> {
    if registry.get(mangled_name).is_some() {
        return Ok(());
    }
    let mut fields: Vec<(String, u32, WasmType)> = Vec::with_capacity(SET_FIELDS.len());
    let mut field_map: HashMap<String, (u32, WasmType)> = HashMap::new();
    let mut own_field_names: HashSet<String> = HashSet::new();
    let mut offset: u32 = 0;
    for &(name, ty) in SET_FIELDS {
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
