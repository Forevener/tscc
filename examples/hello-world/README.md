# hello-world

Minimal end-to-end demo: a TypeScript script compiled by tscc, loaded into wasmtime, wired to a single host import, and called from Rust.

## Running

From the tscc crate root:

```
cargo run --example hello-world
```

Expected output:

```
[guest] hello from tscc!
[guest] this string crossed the WASM boundary via a declared host import.
[guest] seven squared is 49
```

## Files

- `hello.ts` — the guest script. Declares `log`, exports `main()`.
- `main.rs` — the host. Compiles `hello.ts`, provides `host.log`, calls `main`.

## Why there's no `console.log`

tscc is a **sandboxed** compiler: the guest has no ambient I/O, no global `console`, no filesystem, no network. The *only* way out of the WASM boundary is through an import the host has explicitly linked.

So instead of `console.log("hello")`, the pattern is:

```ts
// In the guest script — the host decides this call is allowed.
declare function log(s: string): void;

log("hello");
```

```rust
// In the host — whatever `log` does (stdout, an in-game chat panel,
// a replay recorder, a security audit log) is the host's call.
linker.func_wrap("host", "log", |caller, ptr: i32| {
    // read the string at `ptr` out of guest memory
})?;
```

This is a design choice, not a missing feature:

- **Security.** In game scripting, AI-agent plugins, and untrusted-user-code contexts, the host needs to control what side effects the guest can produce. Explicit imports make the attack surface auditable — every capability the guest has is visible in the `declare function` lines and in the host's `Linker`. Nothing is implicit.
- **Determinism.** tscc is designed for replay- and consensus-style systems (game ticks, lockstep networking, reproducible builds). An ambient `console` would be a hidden side channel and its behavior would depend on the host runtime. Explicit host imports make every effect a documented part of the interface.
- **Portability.** There's no single `console` across wasmtime, wasmer, browsers, V8, and `wasm3` — the "right" logging API depends on the host. Letting each host wire its own `log` keeps tscc out of that decision.

## Adding more capabilities

The same pattern scales:

- `declare function now(): f64;` — host provides a clock.
- `declare function random(): f64;` — host provides PRNG state (or `Math.random`, which tscc already routes through a host import on demand).
- `declare function store_i32(offset: i32, value: i32): void;` — host-managed persistent buffer.

Each capability is opt-in on both sides, and each is visible in code review.

## Memory layout note

In the host's `log` wrapper you'll see the string read as `[u32 little-endian length][utf8 bytes]`. That's tscc's string representation in linear memory:

- Offset 0..4: length as little-endian `u32`.
- Offset 4..4+len: UTF-8 payload (no NUL terminator).

Arrays have an extra capacity field (`[u32 len][u32 cap][elements]`) but the principle is the same — the host reads the header, then the payload.
