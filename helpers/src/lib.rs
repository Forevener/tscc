#![no_std]

pub mod arena;
mod string;

pub use string::__str_lastIndexOf;
pub use string::__str_slice;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}
