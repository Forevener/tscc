# tscc — TypeScript → WebAssembly AOT compiler

**Status:** pre-release (0.0.1).

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
- **Typed arrays:** `Int32Array`, `Float64Array`, `Uint8Array` — fixed-element-width buffers with the standard immutable / mutable / HOF method surface. `subarray` is a true aliasing view (mutations propagate to the parent); `slice` is an independent copy. `Uint8Array` reads zero-extend; stores wrap modulo 256 via `i32.store8`. Composes as a class field, generic arg, function param/return, and through chained HOFs (`filter→map→reduce` carries kind through every step).
- **Nullable:** `T | null`, with `null` represented as pointer 0.
- **Const enums.**
- **Generics (Phase A):** explicit type-arg form — `class Box<T>`, `class Pair<K, V>`, `function identity<T>(x: T): T`. Monomorphized per instantiation.
- **Maps:** `Map<K, V>` with open-addressing hash tables, specialized per concrete `(K, V)` pair.
- **Object literals / structural types:** `type P = { x: number; y: number }`, `interface` (with single `extends`), inline `{x: number}` annotations, shorthand `{ x, y }`, spread `{ ...a, x: 1 }`, destructuring. Anonymous and named shapes share one synthetic-class registry — `{x, y}` and `{y, x}` are the same shape. Structural width-coercion on assignment (zero-copy when layouts align, field-pick copy otherwise). `readonly` accepted as a no-op; `?:` optional fields deferred with union types.
- **Tuples:** `[string, number]`, `t[0]` / `t[1]` literal-indexed access, destructuring `const [a, b] = t`, named-member syntax `[x: number, y: number]` (labels discarded). Dynamic `t[i]` is rejected — use `Array<T>` when slots share a type.
- **Generic shapes:** `type Pair<T, U> = { first: T; second: U }` monomorphizes alongside generic classes. `Pair<i32, f64>`, `Pair<Box<i32>, string>`, etc.
- **Unions:** discriminated object unions (`type Shape = { kind: 'a'; ... } | { kind: 'b'; ... }`), class unions (`type Pet = Cat | Dog`), mixed shape + class unions (`type U = SomeShape | SomeClass`), and same-WasmType literal unions — string (`'red' | 'green' | 'blue'`), integer (`0 | 1 | 2`), boolean, and `f64` (`0.5 | 1.5 | 2.5`). Narrowing on `===` / `!==` guards refines the binding inside the guarded branch — including multi-variant `else` branches and every `case` body of a `switch` (the `default` clause sees a sub-union of the un-handled members). For class and mixed unions, `instanceof` narrows symmetrically (positive and negative branches both refine; inheritance-aware — `instanceof Parent` matches every descendant in the union). Class union members must be polymorphic (participate in an `extends` chain) so every value carries a vtable pointer for runtime narrowing; the diagnostic on a leaf-only union suggests adding a common base. Shared-field access (`sh.kind`, `pet.hp`) works on the un-narrowed union when every variant declares the field at the same offset; shared-method dispatch (`pet.greet()`) works when every variant has the method at the same vtable slot — typically when it's inherited from a common ancestor. Mixed unions narrow via `instanceof` for the class side; kind-discriminator narrowing (`m.kind === '…'`) is supported only on shape-only unions because classes don't carry the discriminator field. Direct unannotated literals flow into a union slot without `as` casts. Unions compose as generic arguments (`Array<Shape>`, `Box<A | B>`). Mixed-WasmType primitive unions (`string | number`, `i32 | f64`) are deliberately excluded — use a discriminated wrapper such as `{tag:'num'; n: f64} | {tag:'str'; s: string}`.
- **Exhaustiveness via `: never`:** assigning a still-inhabited value to a `: never` slot fails with a diagnostic that lists the un-handled variants, so the standard `assertNever(x: never)` pattern in a `switch` `default` doubles as a compile-time exhaustiveness check.
- **Literal types:** string / integer / boolean literals as field annotations (`kind: 'circle'`, `code: 1`, `flag: true`). Initializers validated against the tag at compile time.

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

### User-defined iterables

- A class declaring `[Symbol.iterator](): It` participates in `for..of` when `It` declares `next(): { value: T; done: boolean }`. `[Symbol.iterator]` is the only computed key recognized; other computed keys still error. Self-iterable (`[Symbol.iterator]() { return this; }`) and separate iterator-class shapes both work; subclasses inherit iterability without re-declaring.
- `iterator.return()` runs on `break` and on early function-return through the loop (innermost first for nested loops); normal completion (`done=true`) skips it per spec. Its declared return type is ignored — `return(): void` and `return(): { value: T; done: boolean }` are interchangeable for cleanup.
- `iterator.throw()` is rejected with a deferred-feature error — gated on the long-term exceptions roadmap.
- Built-in `for..of` over `Array<T>` / typed arrays is unchanged (lowers via direct desugaring, not the protocol path).

### Closures

- First-class: store in variables, pass to and return from functions
- Proper capture with auto-boxing for mutated variables
- Inline fast path for array HOFs (no `call_indirect` overhead)

### Built-in APIs

**Math:** `PI`, `E`, `LN2`, `LN10`, `LOG2E`, `LOG10E`, `SQRT2`, `SQRT1_2`, `abs`, `floor`, `ceil`, `trunc`, `nearest`, `round`, `sqrt`, `min`, `max`, `sign`, `hypot`, `copysign`, `fround`, `clz32`, `imul`, plus 20 transcendentals via host import (`sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `exp`, `log`, `pow`, `cbrt`, `log2`, `log10`, `sinh`, `cosh`, `tanh`, `expm1`, `log1p`, `asinh`, `acosh`, `atanh`).

**Number:** `MAX_SAFE_INTEGER`, `MIN_SAFE_INTEGER`, `MAX_VALUE`, `MIN_VALUE`, `EPSILON`, `POSITIVE_INFINITY`, `NEGATIVE_INFINITY`, `NaN`, `isNaN`, `isFinite`, `isInteger`, `isSafeInteger`, `parseInt`, `parseFloat`. Instance methods: `toString()`, `toFixed(d)`, `toPrecision(p)`, `toExponential(d)` — all JS-spec-conformant (half-away-from-zero rounding per ES § 21.1.3.3).

**String:** `at`, `charAt`, `charCodeAt`, `codePointAt`, `indexOf`, `lastIndexOf`, `includes`, `startsWith`, `endsWith`, `slice`, `substring`, `toLowerCase`, `toUpperCase`, `trim`, `trimStart`, `trimEnd`, `split`, `replace`, `replaceAll`, `repeat`, `padStart`, `padEnd`, `concat`. Template literals with string-chain fusion; `+`-chain concat optimization.

**Array:** `push`, `pop`, `indexOf`, `lastIndexOf`, `includes`, `reverse`, `at`, `fill`, `slice`, `concat`, `join`, `splice`, `isArray` (compile-time). HOFs with `(value, index)` callbacks: `filter`, `map`, `forEach`, `reduce`, `sort` (merge sort), `find`, `findIndex`, `findLast`, `findLastIndex`, `some`, `every`.

**TypedArray (`Int32Array`, `Float64Array`, `Uint8Array`):** `length`, `byteLength`, static `BYTES_PER_ELEMENT`. Construction: `new T(n)` (zero-fill), `new T([...])`, `new T(src)` (`Array<T>` or another typed array — copies), `T.of(...items)`, `T.from(src)`, `T.from(src, mapFn)`. Methods: `at`, `indexOf`, `lastIndexOf`, `includes`, `join`, `slice` (copy), `subarray` (view), `fill`, `set` (cross-kind widen/narrow with element-wise loop, same-kind `memory.copy`), `reverse`, `sort` (numeric default + comparator), `copyWithin`. HOFs: `forEach`, `map`, `filter`, `reduce`, `reduceRight`, `find`, `findIndex`, `findLast`, `findLastIndex`, `some`, `every`. `map` / `filter` return same-kind typed arrays.

**Object:** `keys`, `values`, `entries` on shape-typed objects — lowered against the compile-time field set, so the result is a fresh `Array<string>` (keys) or `Array<T>` (values, when fields share a type) or `Array<[string, T]>` (entries, requires the tuple shape to be reachable from the program's annotations). Heterogeneous shapes are rejected with a "mixed types" diagnostic.

**Map / Set:** `size`, `clear`, `has`, `get` (Map), `set` (Map), `add` (Set), `delete`, `forEach`, `keys`, `values`, `entries`. `keys()` / `values()` materialize the insertion-order chain into a fresh `Array<K>` / `Array<V>` (and `Set.keys` is the ES-spec alias of `Set.values`); `entries()` does the same with each row written into a freshly arena-allocated pair (`[K, V]` for Map, `[T, T]` for Set per the ES spec). All three return real `Array<T>` so they compose through HOFs (`m.keys().filter(...)`, `m.entries().forEach(...)`) and `for..of`.

**Globals:** `NaN`, `Infinity`, `isNaN()`, `isFinite()`. Coercion constructors `String(x)`, `Number(x)`, `Boolean(x)` — `String(true)`/`String(false)` and detectable boolean expressions stringify as `"true"`/`"false"`; runtime values whose source-level type is opaque post-emit fall through to the numeric path. User-declared `function String(...)` (or class) shadows the built-in.

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
