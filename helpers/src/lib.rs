#![no_std]

/// Mark a helper as L_splice (paste-inlined at every call site instead of
/// emitted as a real WASM function). Authoring form is identical to a regular
/// `#[unsafe(no_mangle)] pub extern "C" fn`; the macro additionally emits a
/// 1-byte custom-section marker `.custom_section.tscc_inline.<name>` that
/// tscc's helper extractor reads to flip `PrecompiledFunc::is_inline` true.
///
/// Marker is 1 byte (not zero-sized) because lld DCE's empty `#[used]` statics
/// even with `#[link_section]`. Verified on rustc 1.94 + lld via
/// `dev/tmp/splicer-marker-probe/`.
#[macro_export]
macro_rules! tscc_inline {
    (
        $(#[$attr:meta])*
        pub extern "C" fn $name:ident ( $($args:tt)* ) $(-> $ret:ty)? $body:block
    ) => {
        #[unsafe(no_mangle)]
        $(#[$attr])*
        pub extern "C" fn $name($($args)*) $(-> $ret)? $body

        const _: () = {
            #[unsafe(link_section = concat!(".custom_section.tscc_inline.", stringify!($name)))]
            #[used]
            static _MARKER: [u8; 1] = [0];
        };
    };
}

pub mod arena;
mod hash;
mod inline;
mod string;

pub use inline::__inline_test_passthrough;

pub use hash::__hash_fx_bool;
pub use hash::__hash_fx_f64;
pub use hash::__hash_fx_i32;
pub use hash::__hash_fx_ptr;
pub use hash::__hash_xxh3_str;
pub use hash::__key_eq_f64;
pub use string::__str_cmp;
pub use string::__str_endsWith;
pub use string::__str_eq;
pub use string::__str_from_f64;
pub use string::__str_includes;
pub use string::__str_indexOf;
pub use string::__str_lastIndexOf;
pub use string::__str_parseFloat;
pub use string::__str_slice;
pub use string::__str_startsWith;
pub use string::__str_toExponential;
pub use string::__str_toFixed;
pub use string::__str_toPrecision;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}
