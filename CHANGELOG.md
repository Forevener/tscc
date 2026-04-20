# Changelog

All notable changes to tscc are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Pre-1.0 minor bumps may include breaking changes.

## [Unreleased]

## [0.0.1] — unreleased

Initial public release. Typed static subset of TypeScript, compiled AOT to deterministic WebAssembly with arena allocation. Pre-0.1: further built-in coverage is expected before 0.1.0.

### Added

**Language core**

- Primitives (`i32`, `f64`, `bool`, plus `number`/`int` aliases) with type inference from initializers.
- Classes with fields, methods, constructors, single inheritance, and vtable-based polymorphic dispatch.
- Arrays (`Array<T>` / `T[]`) with capacity-doubling growth.
- Generics Phase A: explicit type-arg form for `class Box<T>`, `class Pair<K, V>`, `function identity<T>(x: T): T`, monomorphized per instantiation. Inference and generic inheritance deferred.
- `Map<K, V>` with open-addressing hash tables, specialized per concrete `(K, V)` pair.
- First-class closures with proper capture and auto-boxing for mutated variables.
- Nullable types (`T | null`), const enums, object / array destructuring (including nested and rest patterns).
- Control flow: `if` / `else`, `for`, `while`, `do..while`, `for..of`, `switch` / `case`, `break`, `continue`.
- Optional chaining (`?.`), nullish coalescing (`??`), ternary.
- Type casts: `f64(x)`, `i32(x)`, `x as f64`, `x as i32`.
- Template literals with string-chain fusion; `+`-chain concat optimization.

**Built-in APIs**

- **Math:** constants (`PI`, `E`, `LN2`, `LN10`, `LOG2E`, `LOG10E`, `SQRT2`, `SQRT1_2`) and methods (`abs`, `floor`, `ceil`, `trunc`, `nearest`, `round`, `sqrt`, `min`, `max`, `sign`, `hypot`, `copysign`, `fround`, `clz32`, `imul`) plus 20 transcendentals via host import.
- **Number:** `MAX_SAFE_INTEGER`, `MIN_SAFE_INTEGER`, `MAX_VALUE`, `MIN_VALUE`, `EPSILON`, `POSITIVE_INFINITY`, `NEGATIVE_INFINITY`, `NaN`, `isNaN`, `isFinite`, `isInteger`, `isSafeInteger`, `parseInt`, `parseFloat`.
- **Number instance methods:** `toString()`, `toFixed(d)`, `toPrecision(p)`, `toExponential(d)` — all JS-spec-conformant (half-away-from-zero rounding per ES § 21.1.3.3, via extended-precision formatting + manual half-up carry).
- **String methods:** `at`, `charAt`, `charCodeAt`, `codePointAt`, `indexOf`, `lastIndexOf`, `includes`, `startsWith`, `endsWith`, `slice`, `substring`, `toLowerCase`, `toUpperCase`, `trim`, `trimStart`, `trimEnd`, `split`, `replace`, `replaceAll`, `repeat`, `padStart`, `padEnd`, `concat`.
- **Array methods:** `push`, `pop`, `indexOf`, `lastIndexOf`, `includes`, `reverse`, `at`, `fill`, `slice`, `concat`, `join`, `splice`, `isArray` (compile-time). HOFs with `(value, index)` callbacks: `filter`, `map`, `forEach`, `reduce`, `sort` (merge sort), `find`, `findIndex`, `findLast`, `findLastIndex`, `some`, `every`.
- **Array literals:** `[a, b, c]` and spread `[a, ...xs, b, ...ys]`. Element type inferred from the first inline element or first spread source; integer literals auto-widen to `f64` when the target element type is `f64`.
- **Globals:** `NaN`, `Infinity`, `isNaN()`, `isFinite()`.

**Tooling**

- CLI (`tscc compile`) with `-o`, `--host-module`, `--memory-pages`, `--debug`, `--arena-overflow`, `--version`, `--help`.
- Library API: `tscc::compile(source, &CompileOptions) -> Result<Vec<u8>, CompileError>`.
- `--arena-overflow {grow|trap|unchecked}` flag controls behavior when arena allocation exceeds linear memory.
- DWARF v4 debug info via `--debug` — source-level debugging in wasmtime.
- Error reporting with source locations and caret snippets.
- String-helper tree-shaking: unused helpers get `unreachable` stubs.

**Memory model & host interface**

- Arena / bump allocation, reset per plugin call by the host.
- First 8 bytes reserved as a null guard.
- `load_i32` / `load_f64` / `store_i32` / `store_f64` intrinsics for direct buffer access.
- `__static_alloc(size)` for compile-time constant offsets.
- `declare function name(params): ret` declares a WASM import under the configured module (default `"host"`).
- `memory` and `__arena_ptr` exported for host reset.

### Changed

- **JS-spec-conformant rounding.** `toFixed`, `toPrecision`, and `toExponential` previously inherited Rust's half-to-even rounding from `{:.*}` / `{:.*e}`. Now all three round half-away-from-zero per ES § 21.1.3.3 step 6, with carry-out renormalization (e.g. `(9.5).toExponential(0)` → `"1e+1"`).

### Internal (not user-visible, included for context)

- Rust→WASM helper pipeline (`helpers/` crate) with reproducibility via pinned rustc + committed `Cargo.lock`. 21+ runtime helpers for strings / hashing / equality / float formatters.
- Three-layer WASM emission architecture (L_helper / L_splice / L_emit). The L_splice splicer is production-complete: it supports `return` at arbitrary control-frame depth (rewritten to `br block_depth` against the wrapping block), helpers with declared locals beyond their params, and multi-result signatures. `br_table` remains unsupported (no current port needs it). Six helpers ship spliced inline: `__hash_fx_i32`, `__hash_fx_f64`, `__hash_fx_ptr`, `__key_eq_f64`, `__str_eq`, `__str_cmp` — each called once per Map<K, V> probe iteration or string compare, where the Call boundary was the dominant per-iteration cost.
- `CompileOptions::expose_helpers` now also covers inline (L_splice) helpers by synthesizing one-line wrapper functions, so inline helpers can be called from the host for regression tests. Used by `tests/splicer.rs` and `tests/hashers.rs`.

[Unreleased]: https://github.com/forevener/tscc/compare/v0.0.1...HEAD
[0.0.1]: https://github.com/forevener/tscc/releases/tag/v0.0.1
