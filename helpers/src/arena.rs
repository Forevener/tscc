// Arena access via a FUNCTION IMPORT.
//
// Function imports with `#[link(wasm_import_module = ...)]` are honored by
// rustc+wasm-ld on stable, whereas `extern static` data symbols are not
// (they compile to `i32.load/store at address 0` regardless). Exposing the
// arena as a function makes the dependency explicit and lets tscc wire it
// through to its own `__arena_alloc`.

#[link(wasm_import_module = "env")]
unsafe extern "C" {
    fn __tscc_arena_alloc(size: u32) -> u32;
}

/// Bump-allocate `size` bytes from the arena. Returns pointer to allocated region.
#[inline(always)]
pub unsafe fn alloc(size: u32) -> u32 {
    unsafe { __tscc_arena_alloc(size) }
}
