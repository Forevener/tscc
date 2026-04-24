# Changelog

All notable changes to tscc are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Pre-1.0 minor bumps may include breaking changes.

## [Unreleased]

### Added

- **Object literals / structural types** — landed 2026-04-22.
  - `type P = { x: number; y: number }` and `interface P { ... }` lower to the same synthetic class; both names resolve to a single layout.
  - Anonymous literals `{ x: 1, y: 2 }` fingerprint-match to a named shape when the field set aligns, or get a mangled `__ObjLit$...` name otherwise. Reorders (`{y, x}` ↔ `{x, y}`) dedupe to one shape — first-declaration wins layout.
  - Field access `p.x`, reassignment `p.x = v`, and destructuring `const { x, y } = p` all work through the existing class-ref machinery.
  - **Structural width-coercion** (Phase C): assigning a wider source to a narrower target is allowed; zero-copy when the target's fields sit at the source's offsets, field-pick copy otherwise.
  - **Interface `extends`** (single-parent only): parent fields prefix the child's layout; shadowing is rejected; multi-parent, circular, and class-as-parent all error with phase-scoped messages.
  - **Excess / missing-property checks** on literals bound to a named type (TS-style `"object literal may only specify known properties"` wording).
- **Tuples** — landed 2026-04-22 (Phase D). `[string, number]` as a synthetic class with positional `_0, _1, ...` fields; `t[0]` literal-indexed access; `const [a, b] = t` destructuring (with holes and partial-prefix); tuple-typed function parameters and returns; `Array<[i32, i32]>` elements. Dynamic `t[i]` is rejected — use `Array<T>` when slots share a type.
- **Phase E polish** — landed 2026-04-22.
  - **Shorthand properties** (`{ x, y }`): desugars to `{ x: x, y: y }` — inference rules mirror the expanded form exactly (a standalone `{ x, y }` without an annotation still needs one, same as non-shorthand).
  - **Object spread** (`{ ...a, x: 1 }`): evaluates each spread-source pointer and each explicit RHS into a local in source order; later writes win per field (matching TS). Source fields outside the target layout are silently dropped; missing target fields error. Untyped spreads require an explicit target annotation.
  - **Named tuple elements** (`[x: T, y: U]`): labels accepted and discarded — identity stays positional, so the shape dedupes with the bare `[T, U]` form.
  - **`readonly`** on shape fields: accepted as a no-op (no mutation checks yet; documentation only).
  - **Generic object types**: `type Pair<T, U> = { first: T; second: U }` / `interface Box<T> { value: T }` monomorphize alongside generic classes. Each instantiation `Pair<i32, f64>` registers as a concrete synthetic class under the mangled name `Pair$i32$f64`. Fixed-point recursion handles nested generics (`Pair<Box<i32>, Box<f64>>`).
  - **Optional `?:` fields** and **intersection types** stay deferred — clear errors now reference the union-types wave in the roadmap.
- `String.fromCharCode(...codes)` is now **variadic** — previously only the 1-argument form was accepted. Each code is stored as a single byte (low 8 bits); tscc strings are UTF-8 byte sequences, so codes above 0xFF are truncated (same rule as the 1-arg form, lifted to N args). Empty-arg form returns `""`.
- `String.fromCodePoint(...codePoints)` — variadic UTF-8 encoding, 1-4 bytes per code point. Allocates worst-case `N*4 + 4` bytes, encodes via a new `__utf8_encode_cp` Rust→WASM helper, then rewinds the arena by the unused tail — single allocation, no waste. Out-of-range code points (outside `[0, 0x10FFFF]`) trap fail-loud; the typed subset has no `RangeError`.
- `String.raw\`...\`` tagged template literals — the only supported tag form. Today this reads the same bytes as a regular template literal (tscc's template emitter already uses `quasi.value.raw`); the path is dedicated so a future cooked-string fix to regular templates won't break raw semantics. Unknown tags are rejected at compile time.
- `Array.from({length: n}, mapFn)` — sequence-generation form, recognized as a narrow object-literal pattern (exactly one `length` property; any shorthand / spread / getter / computed key / extra property disqualifies). The map function is required so the element type can be inferred from its return, and each invocation sees `value = 0` since the typed subset has no `undefined`. Explicit `<T>` wins over inference. When general object literals arrive, other shapes will route through the regular object-expression path and this recognizer will keep firing only for the sequence-generation idiom.
- `Array.prototype.sort()` / `toSorted()` now accept **no arguments** and fall back to an inline numeric-order comparator (`-1` / `0` / `+1` from `a < b` / `==` / `a > b`). This diverges from ES — which stringifies and compares lexicographically — but matches what typed-subset users expect, and avoids a per-comparison `ToString` cost. Explicit comparators still work exactly as before.
- `Array.prototype.indexOf` / `lastIndexOf` / `includes` now accept an optional `fromIndex` second argument. Negative values wrap (`+len`); forward search clamps below-range to `0` and lets above-range exit via the existing loop bound; backward search clamps above-range to `len - 1` and lets too-negative values exit via the `i < 0` guard.
- `Array.prototype.concat` is now **variadic** — `a.concat(b, c, d)` concatenates all sources in order via a single arena allocation and one `memory.copy` per source. Previous single-arg behavior is unchanged. Each argument must have the same element type as the receiver (the ES spec's "flatten-one-level and accept non-array values" form isn't supported; that would need variadic type inference beyond the typed subset).
- ES2023 immutable-array wave — non-mutating siblings of `reverse` / `sort` / `splice` / index-assignment. All return a fresh arena allocation; the source is untouched.
  - `Array.prototype.toReversed()` — shallow clone with elements in reverse order.
  - `Array.prototype.toSorted(comparator)` — shallow clone sorted by the same merge-sort path as `sort`. Comparator is required (same rule as `sort`), signature must return i32 or f64.
  - `Array.prototype.toSpliced(start, deleteCount?, ...items)` — fresh copy with the splice applied. Argument semantics match `splice` (1-arg form removes tail from `start`, `deleteCount` clamps to `[0, len - start]`, negative `start` wraps). Returns the NEW array, not the removed items.
  - `Array.prototype.with(index, value)` — shallow clone with `new[index] = value`. Negative indices wrap; out-of-range traps (the typed subset has no `RangeError`, so this matches the spec's intent in a fail-loud way — consistent with `at`).
- `Array.of(...items)` — construct an array from its argument list. Element type comes from an explicit `<T>` when given, otherwise inferred from the first argument (same rule as an array literal).
- `Array.from(src)` / `Array.from(src, mapFn)` — shallow clone or mapped copy of an existing array. The `{length: n}` form is not supported in the typed subset; use `new Array<T>(n).fill(0).map((_, i) => …)` for sequence generation.
- `Array.prototype.shift()` — remove and return the first element; shifts the tail down via a single `memory.copy`. Empty arrays return `0` / `0.0` (matching `pop`).
- `Array.prototype.unshift(...items)` — insert items at the front and return the new length. Reuses `splice`'s grow/in-place fork: in-place when there's spare capacity, copy-and-abandon otherwise (writing the new pointer back to the source identifier).
- `Array.prototype.reduceRight(callback, initialValue)` — mirror of `reduce`, iterating from the last element down to the first.
- `Array.prototype.copyWithin(target, start, end?)` — shallow in-place copy of `[start, end)` to the position beginning at `target`, returning the same array. Negative indices are normalized; the count is capped at `len - target` so the array's length is preserved. Single `memory.copy` handles overlap in either direction.
- `String.prototype.localeCompare(other)` — byte-order lexicographic comparison returning `-1` / `0` / `1`. Reuses the `__str_cmp` L_splice helper that already powers string `<` / `<=` / `>` / `>=`, so there's no new runtime. The ES 22.1.3.10 `locales` / `options` forms are rejected at compile time — honoring them would need ICU and is outside the typed-subset bar.
- `Number.prototype.toString(radix)` — base-R stringification for `radix ∈ [2, 36]`. Radix 10 and the no-arg form route through the existing `__str_from_f64`/`__str_from_i32` path; non-decimal radices go through a new Rust→WASM helper that emits the integer part via repeated division and the fractional part via repeated multiplication (trailing zeros trimmed). Literal radices outside `[2, 36]` are a compile-time error; runtime out-of-range radices fall back to base-10 since tscc has no exception model.

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
