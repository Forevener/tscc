pub(crate) mod classes;
pub(crate) mod codegen;
pub mod error;
pub(crate) mod parse;
pub(crate) mod types;

use error::CompileError;

/// Behavior when the arena allocator exceeds available linear memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArenaOverflow {
    /// Call `memory.grow` when the arena exceeds the current memory size (default).
    /// Safest option — allocations never fail as long as the host allows growth.
    #[default]
    Grow,
    /// Emit an `unreachable` trap on overflow.
    /// Useful for debugging or when deterministic behavior on OOM is desired.
    Trap,
    /// No overflow check — the host is responsible for sizing memory correctly.
    /// Fastest option; matches the original behavior.
    Unchecked,
}

pub struct CompileOptions {
    pub host_module: String,
    pub memory_pages: u32,
    /// Emit DWARF debug info and WASM name section for source-level debugging.
    pub debug: bool,
    /// Source filename (used in DWARF debug info).
    pub filename: String,
    /// Behavior when the arena allocator runs out of linear memory.
    pub arena_overflow: ArenaOverflow,
}

impl Default for CompileOptions {
    fn default() -> Self {
        CompileOptions {
            host_module: "host".to_string(),
            memory_pages: 1,
            debug: false,
            filename: "input.ts".to_string(),
            arena_overflow: ArenaOverflow::default(),
        }
    }
}

pub fn compile(source: &str, options: &CompileOptions) -> Result<Vec<u8>, CompileError> {
    let allocator = oxc_allocator::Allocator::default();
    let program = parse::parse(&allocator, source)?;
    codegen::compile_module(
        &program,
        &options.host_module,
        options.memory_pages,
        source,
        options.debug,
        &options.filename,
        options.arena_overflow,
    )
}
