//! Hashing and equality helpers for Map / Set keys.
//!
//! FxHash handles the integer-shape keys (i32, f64 bit pattern, bool, arena
//! pointer): fast, non-cryptographic, and byte-exact across builds. xxh3
//! handles string keys — FxHash's rotate-multiply-xor degrades sharply on
//! correlated inputs, which real workloads routinely produce (e.g. prefix-
//! heavy tag names).
//!
//! **Platform independence.** The `rustc-hash` crate's `FxHasher` is a type
//! alias that resolves to a 32-bit state on 32-bit targets and a 64-bit
//! state on 64-bit targets — using it would make wasm32 outputs differ from
//! native test hosts. Instead we inline the single-round FxHash mix directly
//! (one rotate, one xor, one multiply), which is byte-identical on any
//! target. The test suite reimplements the same round for reference checks.
//!
//! **NaN canonicalization** (f64 keys): all NaN bit patterns hash to the same
//! value and compare equal to one another via `__key_eq_f64`. Matches JS
//! SameValueZero — any two NaNs are the same Map/Set key. Implementation:
//! substitute a fixed quiet-NaN bit pattern before hashing, and branch to
//! true when both sides are NaN in `__key_eq_f64`.
//!
//! **±0 equivalence**: +0 and -0 must be the same key. We collapse them to
//! `0_u64` before hashing; `__key_eq_f64` inherits their equivalence from
//! IEEE equality (`+0.0 == -0.0` is true).

/// Fixed quiet-NaN bit pattern used to canonicalize all NaN f64 keys.
pub(crate) const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

/// FxHash multiplier — the constant used by rustc's internal FxHasher.
pub(crate) const FX_K: u64 = 0x517c_c1b7_2722_0a95;

/// One FxHash round over a 64-bit word, starting from zero state. With an
/// all-zeros starting state the `rotate_left(5) ^ word` step reduces to just
/// `word`, so the effective operation is a single wrapping multiply by `FX_K`
/// — matches the first `write_u64` round of `FxHasher64`.
#[inline(always)]
fn fx_round_u64(word: u64) -> u64 {
    word.wrapping_mul(FX_K)
}

// `__hash_fx_i32`, `__hash_fx_f64`, and `__hash_fx_ptr` all live in
// `helpers/src/inline.rs` as L_splice helpers — each is called once per
// Map<K, V> probe iteration, so the splicer pastes their bodies inline rather
// than paying the `Call` boundary cost. See the design doc's "L_splice"
// section for why these in particular.

/// FxHash over a bool key. The tscc ABI passes bool as i32 (0 or 1); any
/// non-zero input is normalized to 1 so sloppy callers still hash
/// deterministically.
#[unsafe(no_mangle)]
pub extern "C" fn __hash_fx_bool(v: i32) -> i32 {
    let word = if v == 0 { 0_u64 } else { 1_u64 };
    fx_round_u64(word) as i32
}

/// xxh3-64 over a tscc-format string `[len: u32][bytes...]`. Returns the low
/// 32 bits of the 64-bit digest — sufficient after modulo-bucket reduction.
#[unsafe(no_mangle)]
pub extern "C" fn __hash_xxh3_str(s: u32) -> i32 {
    unsafe {
        let len = (s as *const u32).read() as usize;
        let bytes = core::slice::from_raw_parts((s + 4) as *const u8, len);
        xxhash_rust::xxh3::xxh3_64(bytes) as i32
    }
}

// `__key_eq_f64` lives in `helpers/src/inline.rs` as an L_splice helper —
// same per-probe-iteration argument as `__hash_fx_i32`. See inline.rs.
