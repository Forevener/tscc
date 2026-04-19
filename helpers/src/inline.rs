//! L_splice helpers — paste-inlined at every call site rather than emitted as
//! real WASM functions. See `crates/tscc/docs/design-emit-architecture.md`.
//!
//! Authoring uses the `tscc_inline!` macro from this crate's root, which
//! emits the function plus a 1-byte custom-section marker that the build-time
//! extractor reads to set `PrecompiledFunc::is_inline = true`.

// Trivial passthrough — exists to validate the macro + extractor end-to-end.
// Will be removed (or kept as a regression smoke) once a real inline helper
// like `__hash_fx_i32` is migrated from L_helper.
tscc_inline! {
    pub extern "C" fn __inline_test_passthrough(x: i32) -> i32 {
        x
    }
}
