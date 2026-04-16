pub(crate) mod array_builtins;
pub(crate) mod classes;
pub(crate) mod dwarf;
pub(crate) mod expr;
pub(crate) mod func;
pub(crate) mod math_builtins;
pub(crate) mod module;
pub(crate) mod precompiled;
mod sections;
mod stmt;
pub(crate) mod string_builtins;
pub(crate) mod wasm_types;

pub(crate) use module::compile_module;
