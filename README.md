# tscc — TypeScript → WebAssembly AOT compiler

**Status:** pre-release (0.0.1). 571 tests, 0 clippy warnings.

A standalone TypeScript-to-WebAssembly ahead-of-time compiler. Compiles a typed static subset of TypeScript to deterministic, GC-free WebAssembly suitable for game scripting, plugins, sandboxed user code, and replay- or consensus-style systems where same-input-same-output is load-bearing.

- **Arena allocation, no GC.** Each plugin call runs against a fresh bump arena; the host resets the pointer after the call returns. `new`, `filter`, `map` each cost one `i32.add`.
- **Full closures.** First-class, with proper capture and auto-boxing for mutated variables. No "broken closures" footgun.
- **Deterministic output.** Same source → same WASM bytes → same execution. Rustc pin + committed `Cargo.lock` on the runtime-helpers crate means output is byte-exact across machines.
- **Typed static subset.** No `any`, no `eval`, no prototype chains, no `async`/`await`. `==` and `===` are identical (no coercion).
- **No built-in I/O.** Every import is explicit via `declare function`. Hosts decide what the guest can call.
- **Small.** Single crate, no LLVM, no Binaryen. Depends on [oxc](https://github.com/oxc-project/oxc) for parsing and [wasm-encoder](https://github.com/bytecodealliance/wasm-tools) for output.

## Quickstart

Install from this repo (not yet on crates.io during pre-release):

```
cargo install --path crates/tscc
```

Compile a script:

```
tscc compile player.ts                # → player.wasm
tscc compile player.ts -o build/p.wasm
tscc compile script.ts --host-module env --memory-pages 4
tscc compile script.ts --debug        # emits DWARF + WASM name section
```

Or use the library directly:

```rust
use tscc::{compile, CompileOptions};

let source = std::fs::read_to_string("player.ts")?;
let opts = CompileOptions {
    host_module: "host".into(),
    memory_pages: 1,
    debug: false,
    filename: "player.ts".into(),
    arena_overflow: tscc::ArenaOverflow::Grow,
    expose_helpers: Default::default(),
};
let wasm_bytes = compile(&source, &opts)?;
```

Run the resulting module with any WebAssembly runtime (wasmtime, wasmer, V8-via-wasm-pack, …). The module exports `memory` and a `__arena_ptr` global for the host to reset between calls.

## Why tscc

tscc has: full closures, arena allocation (no GC), deterministic output, complete destructuring, explicit host imports, first-class scope handling via the oxc parser. The typed-static subset choice is what makes the rest of the pipeline cheap enough to fit in one crate.

## Supported TypeScript subset

### Types

- **Primitives:** `i32`, `f64`, `bool` — plus the aliases `number` (= `f64`), `int` (= `i32`).
- **Strings:** arena-allocated, with a tree-shaken set of runtime helpers for the standard string methods.
- **Classes:** arena-allocated structs, single inheritance, vtable-based polymorphic dispatch, override validation.
- **Arrays:** `Array<T>` / `T[]`, growable with capacity doubling. Literal syntax `[a, b, c]` and spread `[a, ...xs, b]`.
- **Nullable:** `T | null`, with `null` represented as pointer 0.
- **Const enums.**
- **Generics (Phase A):** explicit type-arg form — `class Box<T>`, `class Pair<K, V>`, `function identity<T>(x: T): T`. Monomorphized per instantiation.
- **Maps:** `Map<K, V>` with open-addressing hash tables, specialized per concrete `(K, V)` pair.
- **Object literals / structural types:** `type P = { x: number; y: number }`, `interface` (with single `extends`), inline `{x: number}` annotations, shorthand `{ x, y }`, spread `{ ...a, x: 1 }`, destructuring. Anonymous and named shapes share one synthetic-class registry — `{x, y}` and `{y, x}` are the same shape. Structural width-coercion on assignment (zero-copy when layouts align, field-pick copy otherwise). `readonly` accepted as a no-op; `?:` optional fields deferred with union types.
- **Tuples:** `[string, number]`, `t[0]` / `t[1]` literal-indexed access, destructuring `const [a, b] = t`, named-member syntax `[x: number, y: number]` (labels discarded). Dynamic `t[i]` is rejected — use `Array<T>` when slots share a type.
- **Generic shapes:** `type Pair<T, U> = { first: T; second: U }` monomorphizes alongside generic classes. `Pair<i32, f64>`, `Pair<Box<i32>, string>`, etc.

### Syntax

- `const`, `let` (local and global), type inference from initializers
- `if` / `else`, `for`, `while`, `do..while`, `for..of`, `switch` / `case`, `break`, `continue`
- Functions, arrow functions, methods, constructors
- Object and array destructuring, including nested and rest patterns
- Optional chaining (`?.`), nullish coalescing (`??`), ternary (`? :`)
- Type casts: `f64(x)`, `i32(x)`, `x as f64`, `x as i32`
- `===` / `!==` (identical to `==` / `!=`)
- `undefined` keyword (= `null` = 0)
- Implicit void return

### Classes & inheritance

- Fields with typed offsets, constructors with auto-store
- Methods (static dispatch), `this` keyword
- `extends`, `super()`, `super.method()`, vtable-based polymorphic dispatch
- Override validation

### Closures

- First-class: store in variables, pass to and return from functions
- Proper capture with auto-boxing for mutated variables
- Inline fast path for array HOFs (no `call_indirect` overhead)

### Built-in APIs

**Math:** `PI`, `E`, `LN2`, `LN10`, `LOG2E`, `LOG10E`, `SQRT2`, `SQRT1_2`, `abs`, `floor`, `ceil`, `trunc`, `nearest`, `round`, `sqrt`, `min`, `max`, `sign`, `hypot`, `copysign`, `fround`, `clz32`, `imul`, plus 20 transcendentals via host import (`sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `exp`, `log`, `pow`, `cbrt`, `log2`, `log10`, `sinh`, `cosh`, `tanh`, `expm1`, `log1p`, `asinh`, `acosh`, `atanh`).

**Number:** `MAX_SAFE_INTEGER`, `MIN_SAFE_INTEGER`, `MAX_VALUE`, `MIN_VALUE`, `EPSILON`, `POSITIVE_INFINITY`, `NEGATIVE_INFINITY`, `NaN`, `isNaN`, `isFinite`, `isInteger`, `isSafeInteger`, `parseInt`, `parseFloat`. Instance methods: `toString()`, `toFixed(d)`, `toPrecision(p)`, `toExponential(d)` — all JS-spec-conformant (half-away-from-zero rounding per ES § 21.1.3.3).

**String:** `at`, `charAt`, `charCodeAt`, `codePointAt`, `indexOf`, `lastIndexOf`, `includes`, `startsWith`, `endsWith`, `slice`, `substring`, `toLowerCase`, `toUpperCase`, `trim`, `trimStart`, `trimEnd`, `split`, `replace`, `replaceAll`, `repeat`, `padStart`, `padEnd`, `concat`. Template literals with string-chain fusion; `+`-chain concat optimization.

**Array:** `push`, `pop`, `indexOf`, `lastIndexOf`, `includes`, `reverse`, `at`, `fill`, `slice`, `concat`, `join`, `splice`, `isArray` (compile-time). HOFs with `(value, index)` callbacks: `filter`, `map`, `forEach`, `reduce`, `sort` (merge sort), `find`, `findIndex`, `findLast`, `findLastIndex`, `some`, `every`.

**Globals:** `NaN`, `Infinity`, `isNaN()`, `isFinite()`.

## Memory model

- Arena / bump allocation, reset per plugin call by the host.
- Configurable overflow behavior: `Grow` (default, uses `memory.grow`), `Trap`, `Unchecked`.
- First 8 bytes reserved as a null guard.
- `load_i32` / `load_f64` / `store_i32` / `store_f64` intrinsics for direct buffer access.
- `__static_alloc(size)` for compile-time constant offsets.

## Host interface

- `declare function name(params): ret` declares a WASM import under the configured module (default `"host"`).
- `export function` exports to the host.
- `memory` and the `__arena_ptr` global are exported so the host can reset the arena between calls.

## Tooling

- CLI: `tscc compile input.ts [-o output.wasm] [--host-module name] [--memory-pages n] [--debug] [--arena-overflow grow|trap|unchecked]`.
- Library: `tscc::compile(source, &options) -> Result<Vec<u8>, CompileError>`.
- Error reporting with source locations and caret snippets.
- DWARF v4 debug info (`--debug`): source-level debugging in wasmtime.
- String helper tree-shaking: unused helpers get `unreachable` stubs.

## Out of scope (for now)

| Feature | Reason |
|---|---|
| Full ECMAScript conformance | Typed static subset by design. |
| `eval()` / dynamic code | Incompatible with AOT compilation. |
| `async` / `await` (JS-style) | Synchronous tick model; host decides scheduling. |
| Prototype chains / `Proxy` / `Symbol` | No dynamic metaprogramming in a static compiler. |
| Module bundling / `import` | One script = one compilation unit; host provides shared code. |
| `flat` / `flatMap` | Needs nested array type propagation; low priority. |

## License

Dual-licensed under either of:

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option. Matches the Rust-ecosystem default; compatible with both oxc (MIT) and wasm-encoder (Apache-2.0).
