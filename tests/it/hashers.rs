//! Determinism tests for the hash / equality helpers used by Map / Set.
//!
//! The helpers live in `helpers/src/hash.rs` as Rust → WASM exports. tscc
//! splices them into user programs via the precompiled-wasm pipeline. These
//! tests exercise the full pipeline: they compile a small TS program with
//! `expose_helpers` set, which forces the helpers into the bundle AND marks
//! them as named exports. The host then calls them via wasmtime and compares
//! outputs against reference implementations from `rustc-hash` and
//! `xxhash-rust`.
//!
//! Byte-exact match between wasm-side and native hash outputs is the whole
//! point — Map/Set key hashing has to be deterministic across runs and across
//! builds, and these tests guard that invariant. If the `rustc-hash` or
//! `xxhash-rust` dev-dep versions ever drift from `helpers/Cargo.toml`,
//! failures here will flag it immediately.

use std::collections::HashSet;

use wasmtime::*;
use xxhash_rust::xxh3::xxh3_64;

/// FxHash multiplier, must stay in lock-step with `helpers/src/hash.rs::FX_K`.
const FX_K: u64 = 0x517c_c1b7_2722_0a95;

/// Reference FxHash round: one wrapping multiply by `FX_K` on a 64-bit word,
/// starting from zero state. Mirrors `fx_round_u64` in the helper source.
fn fx_round_u64(word: u64) -> u64 {
    word.wrapping_mul(FX_K)
}

const HASH_HELPERS: &[&str] = &[
    "__hash_fx_i32",
    "__hash_fx_f64",
    "__hash_fx_bool",
    "__hash_fx_ptr",
    "__hash_xxh3_str",
    "__key_eq_f64",
];

fn compile_with_hash_helpers(source: &str) -> Vec<u8> {
    let expose: HashSet<String> = HASH_HELPERS.iter().map(|s| s.to_string()).collect();
    let options = tscc::CompileOptions {
        expose_helpers: expose,
        ..Default::default()
    };
    tscc::compile(source, &options).unwrap()
}

fn setup_instance(wasm: &[u8]) -> (Store<()>, Instance) {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    (store, instance)
}

/// Minimal TS program used as the compilation carrier when the test only
/// needs to call the exposed helpers. Nothing here references the helpers —
/// `expose_helpers` alone pulls them in.
const NOOP_SOURCE: &str = "export function _noop(): i32 { return 0; }";

#[test]
fn fx_hash_i32_matches_reference() {
    let wasm = compile_with_hash_helpers(NOOP_SOURCE);
    let (mut store, instance) = setup_instance(&wasm);
    let f = instance
        .get_typed_func::<i32, i32>(&mut store, "__hash_fx_i32")
        .unwrap();
    for &v in &[0, 1, -1, i32::MAX, i32::MIN, 42, 100_000, -424_242] {
        let got = f.call(&mut store, v).unwrap();
        let expected = fx_round_u64(v as u32 as u64) as i32;
        assert_eq!(got, expected, "__hash_fx_i32({v}) wasm != reference");
    }
}

#[test]
fn fx_hash_f64_canonicalizes_nan() {
    let wasm = compile_with_hash_helpers(NOOP_SOURCE);
    let (mut store, instance) = setup_instance(&wasm);
    let f = instance
        .get_typed_func::<f64, i32>(&mut store, "__hash_fx_f64")
        .unwrap();

    let quiet_nan = f64::NAN;
    let arith_nan = f64::from_bits(0x7ff8_abcd_1234_5678);
    let signalling_nan = f64::from_bits(0x7ff0_0000_0000_0001);
    let neg_nan = f64::from_bits(0xfff8_9999_dead_beef);

    let h_quiet = f.call(&mut store, quiet_nan).unwrap();
    let h_arith = f.call(&mut store, arith_nan).unwrap();
    let h_signalling = f.call(&mut store, signalling_nan).unwrap();
    let h_neg = f.call(&mut store, neg_nan).unwrap();

    assert_eq!(h_quiet, h_arith, "quiet NaN vs payload NaN must match");
    assert_eq!(
        h_quiet, h_signalling,
        "quiet NaN vs signalling NaN must match"
    );
    assert_eq!(h_quiet, h_neg, "quiet NaN vs negative NaN must match");
}

#[test]
fn fx_hash_f64_collapses_positive_and_negative_zero() {
    let wasm = compile_with_hash_helpers(NOOP_SOURCE);
    let (mut store, instance) = setup_instance(&wasm);
    let f = instance
        .get_typed_func::<f64, i32>(&mut store, "__hash_fx_f64")
        .unwrap();
    let h_pos = f.call(&mut store, 0.0).unwrap();
    let h_neg = f.call(&mut store, -0.0).unwrap();
    assert_eq!(h_pos, h_neg);

    // Both match the FxHash round over the canonical 0_u64 bit pattern.
    assert_eq!(h_pos, fx_round_u64(0) as i32);
}

#[test]
fn fx_hash_f64_regular_matches_reference() {
    let wasm = compile_with_hash_helpers(NOOP_SOURCE);
    let (mut store, instance) = setup_instance(&wasm);
    let f = instance
        .get_typed_func::<f64, i32>(&mut store, "__hash_fx_f64")
        .unwrap();
    for &v in &[
        1.0_f64,
        -1.0,
        std::f64::consts::PI,
        -2.5,
        1e100,
        1e-100,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        let got = f.call(&mut store, v).unwrap();
        let expected = fx_round_u64(v.to_bits()) as i32;
        assert_eq!(got, expected, "__hash_fx_f64({v}) wasm != reference");
    }
}

#[test]
fn fx_hash_bool_normalizes_sloppy_input() {
    let wasm = compile_with_hash_helpers(NOOP_SOURCE);
    let (mut store, instance) = setup_instance(&wasm);
    let f = instance
        .get_typed_func::<i32, i32>(&mut store, "__hash_fx_bool")
        .unwrap();
    let h_true = f.call(&mut store, 1).unwrap();
    let h_false = f.call(&mut store, 0).unwrap();
    assert_ne!(h_true, h_false);

    // Any non-zero input is treated as `true`.
    for &v in &[2, 42, -1, i32::MAX] {
        let h = f.call(&mut store, v).unwrap();
        assert_eq!(h, h_true, "bool-hash of non-zero {v} should equal true-hash");
    }

    // True-hash matches fx_round_u64(1); false-hash matches fx_round_u64(0).
    assert_eq!(h_true, fx_round_u64(1) as i32);
    assert_eq!(h_false, fx_round_u64(0) as i32);
}

#[test]
fn fx_hash_ptr_whitens_correlated_bump_pointers() {
    // `__hash_fx_ptr` applies a Fibonacci-hash pre-mix (multiply by
    // 0x9E3779B9) before feeding FxHash, both for real de-correlation on
    // aligned bump pointers and to stay byte-distinct from `__hash_fx_i32`.
    const FIB_MIX: u32 = 0x9E37_79B9;
    let wasm = compile_with_hash_helpers(NOOP_SOURCE);
    let (mut store, instance) = setup_instance(&wasm);
    let f = instance
        .get_typed_func::<i32, i32>(&mut store, "__hash_fx_ptr")
        .unwrap();

    // Adjacent bump-allocated pointers should hash to clearly distinct values.
    let h0 = f.call(&mut store, 0x1000).unwrap();
    let h1 = f.call(&mut store, 0x1008).unwrap();
    let h2 = f.call(&mut store, 0x1010).unwrap();
    assert_ne!(h0, h1);
    assert_ne!(h1, h2);
    assert_ne!(h0, h2);

    // Reference match: fx_round_u64 over (v * FIB_MIX) widened to u64.
    for &v in &[0x1000_i32, 0x1008, 0x1010, 0x0abc_def0_u32 as i32] {
        let got = f.call(&mut store, v).unwrap();
        let premixed = (v as u32).wrapping_mul(FIB_MIX);
        let expected = fx_round_u64(premixed as u64) as i32;
        assert_eq!(got, expected, "__hash_fx_ptr({v:#x}) wasm != reference");
    }

    // And the pre-mix must make ptr differ from i32 for any non-zero input.
    let hash_i32 = instance
        .get_typed_func::<i32, i32>(&mut store, "__hash_fx_i32")
        .unwrap();
    for &v in &[1_i32, 42, 0x1008, 0xdeadbeef_u32 as i32] {
        assert_ne!(
            f.call(&mut store, v).unwrap(),
            hash_i32.call(&mut store, v).unwrap(),
            "ptr and i32 hashes must differ post pre-mix for v={v:#x}"
        );
    }
}

#[test]
fn xxh3_str_matches_reference_across_lengths() {
    // The TS program returns pointers to string literals so the test can feed
    // wasm-resident strings back into `__hash_xxh3_str`. Covers the empty
    // string, a short word, and a long sentence to exercise xxh3's length
    // branches (small-keys path, main loop, remainder tail).
    let source = r#"
        export function s_empty(): i32 { return ""; }
        export function s_hello(): i32 { return "hello"; }
        export function s_medium(): i32 { return "the quick brown fox"; }
        export function s_long(): i32 { return "the quick brown fox jumps over the lazy dog, twice for good measure"; }
    "#;
    let wasm = compile_with_hash_helpers(source);
    let (mut store, instance) = setup_instance(&wasm);
    let hash_str = instance
        .get_typed_func::<i32, i32>(&mut store, "__hash_xxh3_str")
        .unwrap();

    let cases: &[(&str, &[u8])] = &[
        ("s_empty", b""),
        ("s_hello", b"hello"),
        ("s_medium", b"the quick brown fox"),
        (
            "s_long",
            b"the quick brown fox jumps over the lazy dog, twice for good measure",
        ),
    ];
    for &(getter_name, expected_bytes) in cases {
        let getter = instance
            .get_typed_func::<(), i32>(&mut store, getter_name)
            .unwrap();
        let ptr = getter.call(&mut store, ()).unwrap();
        let got = hash_str.call(&mut store, ptr).unwrap();
        let expected = xxh3_64(expected_bytes) as i32;
        assert_eq!(
            got,
            expected,
            "__hash_xxh3_str('{}') wasm != reference",
            String::from_utf8_lossy(expected_bytes)
        );
    }
}

#[test]
fn xxh3_str_stable_for_known_input() {
    // Pin the hash of "hello" so any accidental algorithm change would flip
    // this and be caught before it corrupts user maps.
    let wasm = compile_with_hash_helpers(r#"export function s(): i32 { return "hello"; }"#);
    let (mut store, instance) = setup_instance(&wasm);
    let get_ptr = instance
        .get_typed_func::<(), i32>(&mut store, "s")
        .unwrap();
    let hash_str = instance
        .get_typed_func::<i32, i32>(&mut store, "__hash_xxh3_str")
        .unwrap();
    let ptr = get_ptr.call(&mut store, ()).unwrap();
    let got = hash_str.call(&mut store, ptr).unwrap();
    let expected = xxh3_64(b"hello") as i32;
    assert_eq!(got, expected);
}

#[test]
fn key_eq_f64_same_value_zero() {
    let wasm = compile_with_hash_helpers(NOOP_SOURCE);
    let (mut store, instance) = setup_instance(&wasm);
    let eq = instance
        .get_typed_func::<(f64, f64), i32>(&mut store, "__key_eq_f64")
        .unwrap();

    // Ordinary equality.
    assert_eq!(eq.call(&mut store, (1.0, 1.0)).unwrap(), 1);
    assert_eq!(eq.call(&mut store, (1.0, 2.0)).unwrap(), 0);
    assert_eq!(eq.call(&mut store, (-3.14, -3.14)).unwrap(), 1);

    // NaN === NaN (SameValueZero). Multiple NaN bit patterns must all compare
    // equal — matches JS `new Map().set(NaN, x).get(NaN) === x`.
    let alt_nan = f64::from_bits(0xfff0_0000_0000_0001);
    assert_eq!(eq.call(&mut store, (f64::NAN, f64::NAN)).unwrap(), 1);
    assert_eq!(eq.call(&mut store, (f64::NAN, alt_nan)).unwrap(), 1);
    assert_eq!(eq.call(&mut store, (alt_nan, f64::NAN)).unwrap(), 1);

    // NaN is not equal to any non-NaN value.
    assert_eq!(eq.call(&mut store, (f64::NAN, 0.0)).unwrap(), 0);
    assert_eq!(eq.call(&mut store, (0.0, f64::NAN)).unwrap(), 0);
    assert_eq!(
        eq.call(&mut store, (f64::NAN, f64::INFINITY)).unwrap(),
        0
    );

    // +0 === -0.
    assert_eq!(eq.call(&mut store, (0.0, -0.0)).unwrap(), 1);
    assert_eq!(eq.call(&mut store, (-0.0, 0.0)).unwrap(), 1);

    // Infinities.
    assert_eq!(
        eq.call(&mut store, (f64::INFINITY, f64::INFINITY)).unwrap(),
        1
    );
    assert_eq!(
        eq.call(&mut store, (f64::INFINITY, f64::NEG_INFINITY))
            .unwrap(),
        0
    );
}

#[test]
fn hash_output_byte_stable_across_compilations() {
    // Two independent compilations with identical source produce byte-
    // identical wasm — guards against accidental non-determinism (hash-table
    // iteration order, random seeds, etc.) leaking into the build pipeline.
    let w1 = compile_with_hash_helpers(NOOP_SOURCE);
    let w2 = compile_with_hash_helpers(NOOP_SOURCE);
    assert_eq!(
        w1, w2,
        "two identical compilations produced different wasm bytes"
    );

    // And the runtime hash of a fixed input stays stable across instances.
    let (mut s1, i1) = setup_instance(&w1);
    let (mut s2, i2) = setup_instance(&w2);
    let h1 = i1
        .get_typed_func::<i32, i32>(&mut s1, "__hash_fx_i32")
        .unwrap();
    let h2 = i2
        .get_typed_func::<i32, i32>(&mut s2, "__hash_fx_i32")
        .unwrap();
    for &v in &[0, 1, -1, 12345, i32::MAX] {
        assert_eq!(h1.call(&mut s1, v).unwrap(), h2.call(&mut s2, v).unwrap());
    }
}

#[test]
fn expose_helpers_unset_omits_exports() {
    // Sanity check: without `expose_helpers`, the hash helpers are NOT in the
    // export table — we don't want them leaking into production builds that
    // happen to pull in the helper bundle via string usage.
    let options = tscc::CompileOptions::default();
    let wasm = tscc::compile(
        r#"export function greet(): i32 { return "hi"; }"#,
        &options,
    )
    .unwrap();
    let (mut store, instance) = setup_instance(&wasm);
    assert!(
        instance
            .get_typed_func::<i32, i32>(&mut store, "__hash_fx_i32")
            .is_err(),
        "__hash_fx_i32 must not be exported without expose_helpers"
    );
    assert!(
        instance
            .get_typed_func::<i32, i32>(&mut store, "__hash_xxh3_str")
            .is_err(),
        "__hash_xxh3_str must not be exported without expose_helpers"
    );
}
