//! Hashing and equality helpers for `Map<K, V>` and `Set<T>` keys.
//!
//! Implementations live in `helpers/src/hash.rs` and are pulled into tscc's
//! output via the precompiled-wasm pipeline, same as the string helpers.
//! This module only owns the tscc-side names + signatures so that
//! `string_builtins::register_string_helpers` can merge them into the single
//! helper-registration pass.
//!
//! There is intentionally no scanner hook — user TS source never mentions
//! these names. They are pulled in by Map/Set codegen (Phase C+), which adds
//! the relevant names to the `used` set before calling `register_string_helpers`.

use crate::types::WasmType;

/// Names of all hash/equality runtime helpers, in registration order. Kept in
/// sync with `helpers/src/hash.rs`. Order matters for deterministic emission
/// when the whole set is pulled in.
#[allow(dead_code)] // Consumed by Map/Set codegen in Phase C.
pub const HASH_HELPER_NAMES: &[&str] = &[
    "__hash_fx_i32",
    "__hash_fx_f64",
    "__hash_fx_bool",
    "__hash_fx_ptr",
    "__hash_xxh3_str",
    "__key_eq_f64",
];

/// Signature tuple matching `string_builtins::HelperSig`. Kept local so the
/// two modules can stay decoupled; `register_string_helpers` flattens both
/// into a single Vec at call time.
type HelperSig = (&'static str, Vec<(String, WasmType)>, WasmType);

/// Signatures for the six hash helpers. Matches the `extern "C"` signatures
/// in `helpers/src/hash.rs` exactly — the build extractor uses those
/// signatures as ground truth when it synthesizes `PRECOMPILED_FUNCS`, so any
/// drift here surfaces as a registration-time type mismatch.
pub fn hash_helper_sigs() -> Vec<HelperSig> {
    vec![
        (
            "__hash_fx_i32",
            vec![("v".into(), WasmType::I32)],
            WasmType::I32,
        ),
        (
            "__hash_fx_f64",
            vec![("v".into(), WasmType::F64)],
            WasmType::I32,
        ),
        (
            "__hash_fx_bool",
            vec![("v".into(), WasmType::I32)],
            WasmType::I32,
        ),
        (
            "__hash_fx_ptr",
            vec![("p".into(), WasmType::I32)],
            WasmType::I32,
        ),
        (
            "__hash_xxh3_str",
            vec![("s".into(), WasmType::I32)],
            WasmType::I32,
        ),
        (
            "__key_eq_f64",
            vec![("a".into(), WasmType::F64), ("b".into(), WasmType::F64)],
            WasmType::I32,
        ),
    ]
}
