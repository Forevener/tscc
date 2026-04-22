//! Compiler-owned `Map<K, V>` identity — source-level name, arity, and
//! mangling. The shared register-layout / helper-selection / instantiation
//! and info types all live in `hash_table.rs`; this module keeps only the
//! Map-specific vocabulary that distinguishes it from `Set<T>`. Collection
//! happens in `generics::collect_instantiations` via
//! `HashTableInstantiation`; method dispatch lands in `expr/map.rs`.

use crate::types::BoundType;

/// Source-level name used to trigger Map recognition in type annotations and
/// `new` expressions.
pub const MAP_BASE: &str = "Map";

/// Number of type arguments `Map` expects (`K`, `V`).
pub const MAP_ARITY: usize = 2;

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
