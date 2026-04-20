//! L_splice helpers — paste-inlined at every call site rather than emitted as
//! real WASM functions. See `crates/tscc/docs/design-emit-architecture.md`.
//!
//! Authoring uses the `tscc_inline!` macro from this crate's root, which
//! emits the function plus a 1-byte custom-section marker that the build-time
//! extractor reads to set `PrecompiledFunc::is_inline = true`.

// Trivial passthrough — kept as a regression smoke for the marker + extractor
// + splicer wiring, since it exercises the smallest possible body shape.
tscc_inline! {
    pub extern "C" fn __inline_test_passthrough(x: i32) -> i32 {
        x
    }
}

// FxHash over an i32 key. First L_helper→L_splice port: this is called once
// per inner probe iteration on every Map<i32, V> lookup, so removing the
// `Call` boundary recovers the 12-14% per-call cost the kernel-bench measured.
// Body is the inlined `fx_round_u64` round (one `i64.mul` after a u32→u64
// widen) — no calls, no globals, no locals beyond the param. The splicer's
// POC subset handles exactly this shape.
tscc_inline! {
    pub extern "C" fn __hash_fx_i32(v: i32) -> i32 {
        (v as u32 as u64).wrapping_mul(crate::hash::FX_K) as i32
    }
}

// SameValueZero equality for f64 keys: NaN === NaN, +0 === -0. Called once
// per probe iteration on every Map<f64, V> lookup that hits an OCCUPIED
// bucket, so the same call-boundary argument that motivated `__hash_fx_i32`
// applies here. Body is one `if/else/end` over `is_nan`, no `return`, no
// extra locals — fits the splicer's POC subset.
//
// `is_nan` lowers to `v != v` on wasm32 (the IEEE NaN-detect idiom) — no
// libcall, no global, no extra locals. `i32::from(bool)` is a no-op at the
// WASM level since bool *is* i32 in our ABI.
tscc_inline! {
    pub extern "C" fn __key_eq_f64(a: f64, b: f64) -> i32 {
        if a.is_nan() {
            i32::from(b.is_nan())
        } else {
            i32::from(a == b)
        }
    }
}

// FxHash over an f64 Map key — called at the same density as `__key_eq_f64`
// on every Map<f64, V> lookup, so the same call-boundary argument applies.
// NaN inputs canonicalize to a fixed quiet-NaN bit pattern, and IEEE-equal
// zeros (+0 / -0) collapse to 0; both invariants must match `__key_eq_f64`'s
// equivalence classes so that lookups and stores agree on what counts as the
// "same key". Body stays inside the splicer's subset: no calls, at most a
// couple of if/else frames, no `return`.
tscc_inline! {
    pub extern "C" fn __hash_fx_f64(v: f64) -> i32 {
        let bits = if v.is_nan() {
            crate::hash::CANONICAL_NAN_BITS
        } else if v == 0.0 {
            // IEEE equality sees +0 == -0, so this branch swallows both zeros.
            0
        } else {
            v.to_bits()
        };
        bits.wrapping_mul(crate::hash::FX_K) as i32
    }
}

// FxHash over a pointer (arena pointer treated as u32 identity) Map key —
// same per-probe-iteration cost as the other hash helpers on every
// Map<ClassRef, V> lookup, so the same call-boundary argument applies. Bump
// allocators produce pointers whose low bits are all aligned-to-8-or-more and
// FxHash's single-round mixer doesn't fully decorrelate those patterns; a
// Fibonacci-hash multiplicative pre-mix spreads them across the 32-bit space
// before the hasher runs. LTO ICF doesn't apply here (inline helpers are
// paste-inlined, not registered as real functions), so the pre-mix is kept
// purely for hash-quality reasons. Body stays inside the splicer's subset:
// pure arithmetic, no calls, no extra locals, no `return`, no branches.
tscc_inline! {
    pub extern "C" fn __hash_fx_ptr(p: i32) -> i32 {
        const FIB_MIX: u32 = 0x9E37_79B9;
        let premixed = (p as u32).wrapping_mul(FIB_MIX);
        (premixed as u64).wrapping_mul(crate::hash::FX_K) as i32
    }
}

// string == string for Map<string, V> equality. Called once per probe
// iteration on every Map<string, V> lookup that hits an OCCUPIED bucket, so
// the same call-boundary argument that motivated the hash helpers applies.
// The body exercises production-hardening features the earlier ports
// didn't: an early `return` inside an `if` frame (length-mismatch fast
// path), declared locals beyond the params (for the length reads and
// byte-slice pointers), and a `Call` to rustc's memcmp (the slice-equality
// lowering for `&[u8] == &[u8]`). The spliced body keeps that memcmp Call —
// only the outer `__str_eq` Call boundary is removed.
tscc_inline! {
    pub extern "C" fn __str_eq(a: u32, b: u32) -> i32 {
        unsafe {
            let al = crate::string::str_len(a);
            let bl = crate::string::str_len(b);
            if al != bl {
                return 0;
            }
            if crate::string::str_bytes(a, al) == crate::string::str_bytes(b, bl) {
                1
            } else {
                0
            }
        }
    }
}

// Lexicographic byte compare. Returns -1 / 0 / 1. Called once per `<` /
// `<=` / `>` / `>=` string expression (see `expr/string.rs`), so splicing it
// removes the outer `Call` boundary on every string comparison. Body shape
// mirrors `__str_eq`: read lengths, delegate prefix-compare to rustc's Ord
// impl for `[u8]` (which lowers to a memcmp Call + length tie-break), cast
// the resulting `Ordering` to `i32`. `Ordering` is `#[repr(i8)]` with
// discriminants -1/0/1, so `as i32` is a sign-extending cast with no
// branches. The internal memcmp Call stays in the spliced body — only the
// outer `__str_cmp` boundary is removed.
tscc_inline! {
    pub extern "C" fn __str_cmp(a: u32, b: u32) -> i32 {
        unsafe {
            let al = crate::string::str_len(a);
            let bl = crate::string::str_len(b);
            let ap = crate::string::str_bytes(a, al);
            let bp = crate::string::str_bytes(b, bl);
            ap.cmp(bp) as i32
        }
    }
}

// Golden-test helper exercising the production-hardening features together:
//
// - An early `return` inside an `if` branch → tests Return → Br(depth) rewrite
//   at a non-zero control-frame depth (the return sits inside one `If` frame).
// - A `while` loop with mutable accumulator + counter → forces rustc to emit
//   declared locals beyond the single param, exercising the multi-local
//   renumbering pass.
// - Nested block/loop/br_if (the lowering of `while`) → confirms that
//   helper-internal branches stay correct after the splicer wraps the body
//   in an outer block (no byte rewrite of their depth immediates).
//
// `sum_below(n) = 0 + 1 + ... + (n-1)` for n > 0, and 0 otherwise.
tscc_inline! {
    pub extern "C" fn __inline_test_sum_below(n: i32) -> i32 {
        if n <= 0 {
            return 0;
        }
        let mut acc: i32 = 0;
        let mut i: i32 = 0;
        while i < n {
            acc = acc.wrapping_add(i);
            i = i.wrapping_add(1);
        }
        acc
    }
}
