// Single integration test binary. All test files live under `tests/it/` and
// are attached here via `#[path]` — the test crate's root is `tests/it.rs`,
// so naive `mod X;` resolves relative to `tests/`, not `tests/it/`. Using
// `#[path]` keeps the submodule files in one directory next to their
// shared `common/` helper while still producing a single linked binary.

#[path = "it/common/mod.rs"]
mod common;

#[path = "it/arrays.rs"]
mod arrays;
#[path = "it/classes.rs"]
mod classes;
#[path = "it/closures.rs"]
mod closures;
#[path = "it/control_flow.rs"]
mod control_flow;
#[path = "it/debug.rs"]
mod debug;
#[path = "it/errors.rs"]
mod errors;
#[path = "it/generics.rs"]
mod generics;
#[path = "it/hashers.rs"]
mod hashers;
#[path = "it/inheritance.rs"]
mod inheritance;
#[path = "it/maps.rs"]
mod maps;
#[path = "it/math.rs"]
mod math;
#[path = "it/objects.rs"]
mod objects;
#[path = "it/sets.rs"]
mod sets;
#[path = "it/splicer.rs"]
mod splicer;
#[path = "it/strings.rs"]
mod strings;
#[path = "it/tuples.rs"]
mod tuples;
#[path = "it/unions.rs"]
mod unions;
