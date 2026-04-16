// Arena pointer access via a WASM global.
//
// We declare __arena_ptr as an extern static, which compiles to a WASM
// global import. When the function body is extracted by build.rs, the
// global.get/global.set instructions reference this global's index.
// Since it's the only imported global, it gets index 0 — matching
// tscc's output where __arena_ptr is also global 0.

unsafe extern "C" {
    #[link_name = "__arena_ptr"]
    static mut ARENA_PTR: u32;
}

/// Bump-allocate `size` bytes from the arena. Returns pointer to allocated region.
#[inline(always)]
pub unsafe fn alloc(size: u32) -> u32 {
    unsafe {
        let ptr = ARENA_PTR;
        ARENA_PTR = ptr + size;
        ptr
    }
}
