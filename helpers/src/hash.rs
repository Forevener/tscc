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
const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

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

/// FxHash over an i32 key. Widens to u64 first so the mixer sees the whole
/// word (matches `write_i32` routing through `write_u32` and then the 64-bit
/// round in `FxHasher64`).
#[unsafe(no_mangle)]
pub extern "C" fn __hash_fx_i32(v: i32) -> i32 {
    fx_round_u64(v as u32 as u64) as i32
}

/// FxHash over an f64 key, with NaN canonicalized and ±0 collapsed.
#[unsafe(no_mangle)]
pub extern "C" fn __hash_fx_f64(v: f64) -> i32 {
    let bits = if v.is_nan() {
        CANONICAL_NAN_BITS
    } else if v == 0.0 {
        // IEEE equality sees +0 == -0, so this branch swallows both zeros.
        0
    } else {
        v.to_bits()
    };
    fx_round_u64(bits) as i32
}

/// FxHash over a bool key. The tscc ABI passes bool as i32 (0 or 1); any
/// non-zero input is normalized to 1 so sloppy callers still hash
/// deterministically.
#[unsafe(no_mangle)]
pub extern "C" fn __hash_fx_bool(v: i32) -> i32 {
    let word = if v == 0 { 0_u64 } else { 1_u64 };
    fx_round_u64(word) as i32
}

/// FxHash over a pointer (arena pointer treated as u32 identity). Bump
/// allocators produce pointers whose low bits are all aligned-to-8-or-more,
/// and FxHash's single-round mixer doesn't fully decorrelate those patterns.
/// A Fibonacci-hash multiplicative pre-mix spreads them across the 32-bit
/// space before the hasher runs. As a side effect this makes the compiled
/// body byte-distinct from `__hash_fx_i32`; without the pre-mix, LTO's
/// identical-code-folding collapses the two into one internal function and
/// the extractor loses one of the two named exports.
#[unsafe(no_mangle)]
pub extern "C" fn __hash_fx_ptr(p: i32) -> i32 {
    const FIB_MIX: u32 = 0x9E37_79B9;
    let premixed = (p as u32).wrapping_mul(FIB_MIX);
    fx_round_u64(premixed as u64) as i32
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

/// SameValueZero equality for f64 keys: NaN === NaN, +0 === -0. All other
/// cases match ordinary IEEE equality. Returns 1 when equal, 0 otherwise.
#[unsafe(no_mangle)]
pub extern "C" fn __key_eq_f64(a: f64, b: f64) -> i32 {
    if a.is_nan() {
        i32::from(b.is_nan())
    } else {
        i32::from(a == b)
    }
}
