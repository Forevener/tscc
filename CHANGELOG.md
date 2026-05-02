# Changelog

All notable changes to tscc are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Pre-1.0 minor bumps may include breaking changes.

## [Unreleased]

### Added

- Trivial-iterator inlining — when a user iterable's iterator is a single-cursor walk over a backing `Array<T>` field (canonical `cursor: i32`/`buf: Array<T>` pair, no `return()`/`throw()`, buffer field constructor-only-write, `next()` matches the canonical `value-then-bump` shape), `for..of` rewrites against the underlying array directly. No `next()` call survives in the wasm at the call site; output matches the protocol path byte-for-byte. AST-level peephole inside `emit_for_of`; falls back to the protocol path on any heuristic mismatch.
- `Array<T>` class fields participate in member-access codegen — `this.<F>[i]` reads, `this.<F>.length`, `this.<F>` chained through HOFs (`for..of`, `.filter(...)`, etc.) all work without a temp-local detour. `ClassLayout` carries per-field array-element type / element-class metadata populated at class registration; `resolve_expr_array_elem` / `resolve_expr_array_elem_class` route through it for `StaticMemberExpression` receivers.
- User-defined iterables — `[Symbol.iterator](): It` on a class participates in `for..of` when `It` declares `next(): { value: T; done: boolean }`. Computed-key parsing for the well-known `Symbol.iterator` (other computed keys still error); canonical internal name `@@iterator`; bare `Symbol` and `Symbol.X`-as-value rejected with a precise hint. Detection walks the parent chain so subclasses inherit iterability without re-declaring. Polymorphic dispatch via vtable for both the iterable's `[Symbol.iterator]()` and the iterator's `next()` when the receiver class is polymorphic. `iterator.return()` runs on `break` and on early function-return through the loop (innermost first for nested loops); normal completion (`done=true`) skips it per spec; the result is dropped, so `return(): void` and `return(): { value: T; done: boolean }` are interchangeable. `iterator.throw()` is rejected with a deferred-feature hint pointing at the exceptions roadmap. Built-in `for..of` over `Array<T>` / typed arrays remains unchanged.
- `Object.keys` / `Object.values` / `Object.entries` on shape-typed objects. Layouts are known at compile time, so field names lower to a constant `Array<string>` and value loads to per-field memory reads at the recorded offset. `values` rejects shapes whose fields don't share a `BoundType` (`{a: number, b: string}` errors with a "mixed types" diagnostic — same typed-subset stance as primitive unions). `entries` materializes a fresh tuple `[string, T]` per row; the tuple shape must already be in the registry, which happens automatically when the receiver is annotated (`const e: [string, number][] = Object.entries(p)`). Element-WasmType / element-class tracking propagates through `resolve_expr_array_elem` so the result chains into HOFs (`Object.values(p).map(...)`) and `for..of`.
- `Map.prototype.entries` / `Set.prototype.entries` — finish the trio with `keys` / `values`. Each row of the insertion chain writes a freshly arena-allocated tuple (`[K, V]` for Map; `[T, T]` for Set per ES spec) into an `Array<__Tuple$K$V>`. Tuple shapes are pre-registered per Map / Set instantiation alongside hash-table layouts, so the synthetic `__Tuple$...` class is in the registry by the time codegen runs; if user code already wrote `[K, V]` syntactically the fingerprint dedupes back into the same shape. Element-class tracking propagates through `resolve_expr_array_elem_class`, so `m.entries().filter(...)` carries the pair shape through HOFs and `for..of`.
- `Map.prototype.keys` / `.values` and `Set.prototype.keys` / `.values` — materialize the existing insertion-order chain (the same `head_idx → next_insert` walk that powers `forEach`) into a freshly arena-allocated `Array<K>` / `Array<V>` / `Array<T>`. Per ES spec, `Set.keys` aliases `Set.values`. Result composes as a regular `Array<T>` so it threads through HOFs (`m.keys().filter(...)`) and `for..of`. Capacity snaps to `size`; length patches in to the post-walk index. Element-class tracking propagates through `resolve_expr_array_elem_class` for class-typed columns.
- Coercion constructors `String(x)`, `Number(x)`, `Boolean(x)` — global call form. `String` reuses the existing `__str_from_i32` / `__str_from_f64` paths and emits static `"true"` / `"false"` for boolean literals and detectable boolean expressions (`!x`, `a === b`, `a && b`, …); `Number` widens i32 → f64, returns f64 identity, and routes string operands through `parseFloat` (NaN on parse failure); `Boolean` is fully inline — `(x === x) && (x !== 0)` for f64 (filters NaN and ±0), `len !== 0` for strings (the null-guard's zero bytes give `Boolean(null) → false` for free), and `x !== 0` for everything else. User-declared `function String/Number/Boolean(...)` (or classes with those names) shadow the built-ins.
- Typed arrays — `Int32Array`, `Float64Array`, `Uint8Array`. Fixed-element-width pseudo-classes with an 8-byte `[len][buf_ptr]` header. Construction from length (zero-filled), array literal, `Array<T>`, or another typed array; `T.of(...items)`, `T.from(src)`, `T.from(src, mapFn)`. Indexed read/write (including compound `+=` / `-=` / `*=` / `/=`); `length`, `byteLength`, static `BYTES_PER_ELEMENT`; `for..of`. Methods: `at`, `indexOf`, `lastIndexOf`, `includes`, `join`, `slice` (independent copy), `subarray` (true aliasing view — mutations propagate to parent), `fill`, `set` (cross-kind element-wise widen/narrow, same-kind `memory.copy`), `reverse`, `sort` (numeric default + explicit comparator), `copyWithin`. HOFs: `forEach`, `map`, `filter`, `reduce`, `reduceRight`, `find`, `findIndex`, `findLast`, `findLastIndex`, `some`, `every` — `map` / `filter` return same-kind typed arrays so chained HOFs (`filter→map→reduce`) carry kind through every step. `Uint8Array` reads zero-extend (`i32.load8_u`); stores wrap modulo 256 (`i32.store8` truncates the low 8 bits — applies to indexed writes, literal-init, `set` from `Array<i32>`, and HOF stores). Composes as a class field, generic argument (`Array<Int32Array>`), function param, and return type. `Float32Array` and the other widths are deferred — see `roadmap.md`.
- Discriminated object unions (`type Shape = { kind: 'a'; ... } | { kind: 'b'; ... }`) with narrowing on `===` / `!==` / `==` / `!=` guards; shared-field member access on the un-narrowed union. Multi-variant `else` branches refine to a sub-union; direct unannotated literals (`{ kind: 'circle', r: 1 }`) flow into a union slot without `as` casts; `switch` narrows each `case` body and the `default` body.
- Class unions (`type Pet = Cat | Dog`) with `instanceof` narrowing — positive and negative branches both refine; inheritance-aware (`instanceof Animal` matches every descendant in the union); exhaustive `instanceof` chains compose with `: never` for compile-time exhaustiveness. Class union members must be polymorphic (participate in an inheritance hierarchy); the diagnostic on a leaf-only union steers users to add a common base. Methods shared across every variant (typically inherited from a common ancestor at the same vtable slot) dispatch polymorphically on the un-narrowed union via `call_indirect`. Mixed shape + class unions (`type U = SomeShape | SomeClass`) compose under `instanceof` narrowing and shared-field access; method calls on a mixed union produce a targeted "shapes have no methods" diagnostic.
- Exhaustiveness checking via `: never` — assigning a still-inhabited value to a `: never` slot fails with a diagnostic that lists the un-handled variants. The standard `assertNever(x: never)` pattern compiles when every variant is reached.
- Same-WasmType literal unions and literal field types — string (`'red' | 'green' | 'blue'`), integer (`0 | 1 | 2`), boolean, and `f64` (`0.5 | 1.5 | 2.5`). Pure-`f64`-literal unions resolve to `WasmType::F64` end-to-end: locals, function params/returns, class fields/methods, arrow params, array elements (`Half[]`), `as`-casts (`0.5 as Half`), and generic arguments (both inline `Box<0.5 | 1.5>` and named `Box<Half>`). Mixed-WasmType unions (`string | number`) remain unsupported by design — see the discriminated-wrapper workaround in the diagnostic.
- Unions as generic type arguments — `Array<Shape>`, user generics like `Box<Shape>`, and inline `Box<A | B>`.
- Object literals and structural types (`type`, `interface`, shorthand, spread, destructuring, width-coercion, single `extends`).
- Tuples (`[T, U]`, literal indexing, destructuring, named elements, nested, `Array<[T, U]>`).
- Generic object types (`type Pair<T, U> = {...}`, `interface Box<T> { ... }`).
- `String.fromCharCode(...)` — variadic.
- `String.fromCodePoint(...)` — variadic UTF-8.
- `String.raw\`...\`` tagged template literals.
- `Array.from({length: n}, mapFn)` sequence-generation form.
- `Array.prototype.sort()` / `toSorted()` no-comparator form (numeric default).
- `Array.prototype.indexOf` / `lastIndexOf` / `includes` accept `fromIndex`.
- `Array.prototype.concat` is variadic.
- `Array.prototype.toReversed` / `toSorted` / `toSpliced` / `with` (ES2023 immutable-array wave).
- `Array.of(...items)`.
- `Array.from(src)` / `Array.from(src, mapFn)`.
- `Array.prototype.shift` / `unshift` / `reduceRight` / `copyWithin`.
- `String.prototype.localeCompare`.
- `Number.prototype.toString(radix)`.

## [0.0.1] — unreleased

Initial release. Typed static subset of TypeScript, compiled AOT to deterministic WebAssembly with arena allocation. See `README.md` for the supported subset and built-in surface.

### Added

- Language core: classes, arrays, generics (monomorphized), `Map<K, V>`, `Set<T>`, closures, destructuring, nullable types, template literals.
- Math / Number / String / Array / Globals built-ins — full surface in `README.md`.
- CLI (`tscc compile`), library API (`tscc::compile`), DWARF v4 debug info.
- Arena / bump allocation, null-guard, intrinsics, host import declarations.

### Changed

- `toFixed` / `toPrecision` / `toExponential` round half-away-from-zero per ES § 21.1.3.3 (originally inherited Rust's half-to-even).

[Unreleased]: https://github.com/forevener/tscc/compare/v0.0.1...HEAD
[0.0.1]: https://github.com/forevener/tscc/releases/tag/v0.0.1
