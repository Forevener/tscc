//! Compiler-owned `Set<T>` identity — source-level name, arity, and
//! mangling. Shared machinery (header layout, bucket layout, register,
//! helper selection, instantiation type) lives in `hash_table.rs`;
//! `Set<T>` differs from `Map<K, V>` only in that its bucket carries no
//! value slot. Collection happens in `generics::collect_instantiations`
//! via `HashTableInstantiation`; method dispatch lands in `expr/set.rs`.

use crate::types::BoundType;

/// Source-level name used to trigger Set recognition in type annotations and
/// `new` expressions.
pub const SET_BASE: &str = "Set";

/// Number of type arguments `Set` expects (`T`).
pub const SET_ARITY: usize = 1;

/// Mangled Set class name for a given `T`, e.g. `Set$string`.
pub fn mangle_set_name(elem_ty: &BoundType) -> String {
    format!("{SET_BASE}${}", elem_ty.mangle_token())
}

/// Return `true` when `name` refers to the compiler-owned `Set` template.
pub fn is_set_base(name: &str) -> bool {
    name == SET_BASE
}
