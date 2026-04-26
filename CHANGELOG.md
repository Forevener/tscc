# Changelog

All notable changes to tscc are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Pre-1.0 minor bumps may include breaking changes.

## [Unreleased]

### Added

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
