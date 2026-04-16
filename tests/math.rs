mod common;

use wasmtime::*;

use common::{compile, run_sink_tick};

#[test]
fn math_builtins() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            const a: f64 = Math.sqrt(16.0);
            const b: f64 = Math.abs(-5.0);
            set_action(me, 0, 0, a, b);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, (0.0f64, 0.0f64));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, (f64, f64)>,
             _me: i32,
             _kind: i32,
             _target: i32,
             dx: f64,
             dy: f64| {
                *caller.data_mut() = (dx, dy);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    let (a, b) = *store.data();
    assert!((a - 4.0).abs() < 1e-10); // sqrt(16) = 4
    assert!((b - 5.0).abs() < 1e-10); // abs(-5) = 5
}

#[test]
fn math_min_max() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            const a: f64 = Math.min(3.0, 7.0);
            const b: f64 = Math.max(3.0, 7.0);
            set_action(me, 0, 0, a, b);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, (0.0f64, 0.0f64));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, (f64, f64)>,
             _me: i32,
             _kind: i32,
             _target: i32,
             dx: f64,
             dy: f64| {
                *caller.data_mut() = (dx, dy);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    let (a, b) = *store.data();
    assert!((a - 3.0).abs() < 1e-10); // min(3, 7) = 3
    assert!((b - 7.0).abs() < 1e-10); // max(3, 7) = 7
}

#[test]
fn math_floor_ceil() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            const a: f64 = Math.floor(3.7);
            const b: f64 = Math.ceil(3.2);
            set_action(me, 0, 0, a, b);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, (0.0f64, 0.0f64));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, (f64, f64)>,
             _me: i32,
             _kind: i32,
             _target: i32,
             dx: f64,
             dy: f64| {
                *caller.data_mut() = (dx, dy);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    let (a, b) = *store.data();
    assert!((a - 3.0).abs() < 1e-10); // floor(3.7) = 3
    assert!((b - 4.0).abs() < 1e-10); // ceil(3.2) = 4
}

#[test]
fn math_random_is_deterministic_given_seed() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            const a: f64 = Math.random();
            const b: f64 = Math.random();
            set_action(me, 0, 0, a, b);
        }
    "#,
    );

    use std::sync::{Arc, Mutex};
    let collected: Arc<Mutex<Vec<(f64, f64)>>> = Arc::new(Mutex::new(Vec::new()));

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();

    // Run twice with the same seed; sequences must match bit-exactly.
    let run_with_seed = |seed: i64| -> (f64, f64) {
        let mut store = Store::new(&engine, collected.clone());
        let mut linker = Linker::new(&engine);
        linker
            .func_wrap(
                "host",
                "set_action",
                |mut caller: Caller<'_, Arc<Mutex<Vec<(f64, f64)>>>>,
                 _me: i32,
                 _kind: i32,
                 _target: i32,
                 dx: f64,
                 dy: f64| {
                    caller.data_mut().lock().unwrap().push((dx, dy));
                },
            )
            .unwrap();
        let instance = linker.instantiate(&mut store, &module).unwrap();
        // Seed via the exported __rng_state global before any Math.random call.
        let state = instance.get_global(&mut store, "__rng_state").unwrap();
        state.set(&mut store, wasmtime::Val::I64(seed)).unwrap();
        collected.lock().unwrap().clear();
        let tick = instance
            .get_typed_func::<i32, ()>(&mut store, "tick")
            .unwrap();
        tick.call(&mut store, 0).unwrap();
        let v = collected.lock().unwrap();
        (v[0].0, v[0].1)
    };

    let r1 = run_with_seed(0xCAFEBABE);
    let r2 = run_with_seed(0xCAFEBABE);
    assert_eq!(
        r1.0.to_bits(),
        r2.0.to_bits(),
        "same seed must give same first value"
    );
    assert_eq!(
        r1.1.to_bits(),
        r2.1.to_bits(),
        "same seed must give same second value"
    );

    // Outputs must be in [0, 1).
    assert!(
        r1.0 >= 0.0 && r1.0 < 1.0,
        "first value out of range: {}",
        r1.0
    );
    assert!(
        r1.1 >= 0.0 && r1.1 < 1.0,
        "second value out of range: {}",
        r1.1
    );

    // Different seed must give different sequence.
    let r3 = run_with_seed(0xDEADBEEF);
    assert_ne!(
        r1.0.to_bits(),
        r3.0.to_bits(),
        "different seeds must give different output"
    );
}

#[test]
fn math_random_matches_pcg32_reference() {
    // Regression lock: tscc's wasm PCG32 step must produce bit-exact
    // matches against a pure-Rust PCG32 XSH-RR 64/32 reference for a fixed
    // seed. If this test fails, the algorithm in compile_rng_next has drifted
    // — that's a determinism break for every embedder, so investigate before
    // updating the expected values.
    fn pcg32_rust(state: &mut u64) -> f64 {
        let oldstate = *state;
        *state = oldstate
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let xorshifted: u32 = (((oldstate >> 18) ^ oldstate) >> 27) as u32;
        let rot: u32 = (oldstate >> 59) as u32;
        let out = xorshifted.rotate_right(rot);
        (out as f64) * (1.0 / 4294967296.0)
    }

    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            const a: f64 = Math.random();
            const b: f64 = Math.random();
            set_action(me, 0, 0, a, b);
        }
    "#,
    );

    use std::sync::{Arc, Mutex};
    let collected: Arc<Mutex<Vec<(f64, f64)>>> = Arc::new(Mutex::new(Vec::new()));
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, collected.clone());
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, Arc<Mutex<Vec<(f64, f64)>>>>,
             _me: i32,
             _kind: i32,
             _target: i32,
             dx: f64,
             dy: f64| {
                caller.data_mut().lock().unwrap().push((dx, dy));
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();

    let seed: u64 = 0x123456789ABCDEF0;
    let state = instance.get_global(&mut store, "__rng_state").unwrap();
    state
        .set(&mut store, wasmtime::Val::I64(seed as i64))
        .unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();

    let (wasm_a, wasm_b) = collected.lock().unwrap()[0];

    let mut ref_state = seed;
    let ref_a = pcg32_rust(&mut ref_state);
    let ref_b = pcg32_rust(&mut ref_state);

    assert_eq!(
        wasm_a.to_bits(),
        ref_a.to_bits(),
        "PCG32 wasm output diverged from Rust reference at step 1"
    );
    assert_eq!(
        wasm_b.to_bits(),
        ref_b.to_bits(),
        "PCG32 wasm output diverged from Rust reference at step 2"
    );
}

#[test]
fn math_random_not_emitted_when_unused() {
    // A module that doesn't call Math.random must not export __rng_state.
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            set_action(me, 0, 0, 1.0, 2.0);
        }
    "#,
    );

    let parser = wasmparser::Parser::new(0);
    let mut export_names: Vec<String> = Vec::new();
    for payload in parser.parse_all(&wasm) {
        if let Ok(wasmparser::Payload::ExportSection(reader)) = payload {
            for exp in reader {
                let exp = exp.unwrap();
                export_names.push(exp.name.to_string());
            }
        }
    }
    assert!(
        !export_names.contains(&"__rng_state".to_string()),
        "expected no __rng_state export when Math.random is unused, got: {export_names:?}"
    );
}

#[test]
fn math_transcendentals_via_host_imports() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            const s: f64 = Math.sin(0.0);
            const c: f64 = Math.cos(0.0);
            const l: f64 = Math.log(Math.E);
            const p: f64 = Math.pow(2.0, 10.0);
            // Combine via set_action — we'll smuggle four values through two
            // calls to verify each.
            set_action(me, 0, 0, s, c);
            set_action(me, 1, 0, l, p);
        }
    "#,
    );

    use std::sync::{Arc, Mutex};
    let collected: Arc<Mutex<Vec<(i32, f64, f64)>>> = Arc::new(Mutex::new(Vec::new()));

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, collected.clone());
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, Arc<Mutex<Vec<(i32, f64, f64)>>>>,
             _me: i32,
             kind: i32,
             _target: i32,
             dx: f64,
             dy: f64| {
                caller.data_mut().lock().unwrap().push((kind, dx, dy));
            },
        )
        .unwrap();
    // Wire transcendentals — this is what the README JS shim / tscc-host-libm
    // companion crate would do for users.
    linker
        .func_wrap("host", "__tscc_sin", |x: f64| x.sin())
        .unwrap();
    linker
        .func_wrap("host", "__tscc_cos", |x: f64| x.cos())
        .unwrap();
    linker
        .func_wrap("host", "__tscc_log", |x: f64| x.ln())
        .unwrap();
    linker
        .func_wrap("host", "__tscc_pow", |x: f64, y: f64| x.powf(y))
        .unwrap();

    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();

    let v = collected.lock().unwrap();
    assert_eq!(v.len(), 2);
    assert!((v[0].1 - 0.0).abs() < 1e-12); // sin(0) = 0
    assert!((v[0].2 - 1.0).abs() < 1e-12); // cos(0) = 1
    assert!((v[1].1 - 1.0).abs() < 1e-12); // ln(e) = 1
    assert!((v[1].2 - 1024.0).abs() < 1e-9); // 2^10 = 1024
}

#[test]
fn math_transcendentals_tree_shaken() {
    // Module that uses ONLY Math.sin should not declare imports for cos/log/etc.
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            const s: f64 = Math.sin(0.5);
            set_action(me, 0, 0, s, 0.0);
        }
    "#,
    );

    // Decode imports from the wasm module and assert tree-shaking behavior.
    let parser = wasmparser::Parser::new(0);
    let mut import_names: Vec<String> = Vec::new();
    for payload in parser.parse_all(&wasm) {
        if let Ok(wasmparser::Payload::ImportSection(reader)) = payload {
            for imp in reader {
                let imp = imp.unwrap();
                import_names.push(imp.name.to_string());
            }
        }
    }
    let math_imports: Vec<&String> = import_names
        .iter()
        .filter(|n| n.starts_with("__tscc_"))
        .collect();
    assert_eq!(
        math_imports.len(),
        1,
        "expected only __tscc_sin, got: {math_imports:?}"
    );
    assert_eq!(math_imports[0], "__tscc_sin");
}

#[test]
fn math_sign_and_hypot() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            const s: f64 = Math.sign(-42.0);
            const h: f64 = Math.hypot(3.0, 4.0);
            set_action(me, 0, 0, s, h);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, (0.0f64, 0.0f64));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, (f64, f64)>,
             _me: i32,
             _kind: i32,
             _target: i32,
             dx: f64,
             dy: f64| {
                *caller.data_mut() = (dx, dy);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    let (s, h) = *store.data();
    assert_eq!(s, -1.0);
    assert_eq!(h, 5.0);
}

#[test]
fn math_sign_preserves_zero_and_nan() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick_zero(me: i32): void {
            const pz: f64 = Math.sign(0.0);
            const nz: f64 = Math.sign(-0.0);
            set_action(me, 0, 0, pz, nz);
        }

        export function tick_nan(me: i32): void {
            const nan_in: f64 = 0.0 / 0.0;
            const r: f64 = Math.sign(nan_in);
            set_action(me, 0, 0, r, r);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();

    // Zero preservation
    let mut store = Store::new(&engine, (1.0f64, 1.0f64));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, (f64, f64)>,
             _me: i32,
             _kind: i32,
             _target: i32,
             dx: f64,
             dy: f64| {
                *caller.data_mut() = (dx, dy);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick_zero = instance
        .get_typed_func::<i32, ()>(&mut store, "tick_zero")
        .unwrap();
    tick_zero.call(&mut store, 0).unwrap();
    let (pz, nz) = *store.data();
    assert_eq!(pz.to_bits(), 0.0f64.to_bits());
    assert_eq!(nz.to_bits(), (-0.0f64).to_bits());

    // NaN propagation
    let tick_nan = instance
        .get_typed_func::<i32, ()>(&mut store, "tick_nan")
        .unwrap();
    tick_nan.call(&mut store, 0).unwrap();
    let (r, _) = *store.data();
    assert!(r.is_nan());
}

#[test]
fn math_constants() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            const a: f64 = Math.PI;
            const b: f64 = Math.E;
            set_action(me, 0, 0, a, b);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, (0.0f64, 0.0f64));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, (f64, f64)>,
             _me: i32,
             _kind: i32,
             _target: i32,
             dx: f64,
             dy: f64| {
                *caller.data_mut() = (dx, dy);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    let (a, b) = *store.data();
    // Bit-exact: constants are emitted as f64 literals, not computed.
    assert_eq!(a.to_bits(), std::f64::consts::PI.to_bits());
    assert_eq!(b.to_bits(), std::f64::consts::E.to_bits());
}

#[test]
fn math_constants_all_eight() {
    // Exercises every ECMAScript Math constant; reads them back via a pair of
    // exported f64 globals through tick so we can bit-compare each.
    use std::sync::{Arc, Mutex};
    let got: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));

    let wasm = compile(
        r#"
        declare function sink(x: f64): void;

        export function tick(me: i32): void {
            sink(Math.PI);
            sink(Math.E);
            sink(Math.LN2);
            sink(Math.LN10);
            sink(Math.LOG2E);
            sink(Math.LOG10E);
            sink(Math.SQRT2);
            sink(Math.SQRT1_2);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, got.clone());
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "sink",
            |caller: Caller<'_, Arc<Mutex<Vec<f64>>>>, x: f64| {
                caller.data().lock().unwrap().push(x);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();

    let vals = got.lock().unwrap().clone();
    let expected = [
        std::f64::consts::PI,
        std::f64::consts::E,
        std::f64::consts::LN_2,
        std::f64::consts::LN_10,
        std::f64::consts::LOG2_E,
        std::f64::consts::LOG10_E,
        std::f64::consts::SQRT_2,
        std::f64::consts::FRAC_1_SQRT_2,
    ];
    assert_eq!(vals.len(), expected.len());
    for (i, (got, exp)) in vals.iter().zip(expected.iter()).enumerate() {
        assert_eq!(got.to_bits(), exp.to_bits(), "mismatch at constant #{i}");
    }
}
#[test]
fn global_is_nan_and_is_finite() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            sink(isNaN(NaN) ? 1.0 : 0.0);
            sink(isNaN(1.5) ? 1.0 : 0.0);
            sink(isNaN(Infinity) ? 1.0 : 0.0);
            sink(isFinite(1.5) ? 1.0 : 0.0);
            sink(isFinite(Infinity) ? 1.0 : 0.0);
            sink(isFinite(NaN) ? 1.0 : 0.0);
            sink(isFinite(-Infinity) ? 1.0 : 0.0);
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);
}

#[test]
fn number_statics_and_constants() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            sink(Number.MAX_SAFE_INTEGER);
            sink(Number.MIN_SAFE_INTEGER);
            sink(Number.EPSILON);
            sink(Number.POSITIVE_INFINITY);
            sink(Number.NEGATIVE_INFINITY);
            sink(Number.isNaN(NaN) ? 1.0 : 0.0);
            sink(Number.isNaN(Infinity) ? 1.0 : 0.0);
            sink(Number.isFinite(1.5) ? 1.0 : 0.0);
            sink(Number.isFinite(NaN) ? 1.0 : 0.0);
            sink(Number.isInteger(3.0) ? 1.0 : 0.0);
            sink(Number.isInteger(3.5) ? 1.0 : 0.0);
            sink(Number.isInteger(Infinity) ? 1.0 : 0.0);
            sink(Number.isSafeInteger(3.0) ? 1.0 : 0.0);
            sink(Number.isSafeInteger(9007199254740992.0) ? 1.0 : 0.0);
        }
    "#,
    );
    assert_eq!(vals[0], 9_007_199_254_740_991.0);
    assert_eq!(vals[1], -9_007_199_254_740_991.0);
    assert_eq!(vals[2], f64::EPSILON);
    assert!(vals[3].is_infinite() && vals[3] > 0.0);
    assert!(vals[4].is_infinite() && vals[4] < 0.0);
    assert_eq!(&vals[5..], &[1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0]);
}

#[test]
fn math_round_half_toward_positive_infinity() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            sink(Math.round(0.5));   //  1 (not 0 per half-to-even)
            sink(Math.round(1.5));   //  2
            sink(Math.round(2.5));   //  3 (not 2)
            sink(Math.round(-0.5));  //  0 (toward +inf, not -1)
            sink(Math.round(-1.5));  // -1 (toward +inf)
            sink(Math.round(3.4));   //  3
            sink(Math.round(3.6));   //  4
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 2.0, 3.0, 0.0, -1.0, 3.0, 4.0]);
}

#[test]
fn number_parse_int_and_parse_float_aliases() {
    let wasm = compile(
        r#"
        export function via_number_i(): i32 {
            return Number.parseInt("42");
        }
        export function via_number_f(): f64 {
            return Number.parseFloat("3.14");
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();

    let f1 = instance
        .get_typed_func::<(), i32>(&mut store, "via_number_i")
        .unwrap();
    assert_eq!(f1.call(&mut store, ()).unwrap(), 42);
    let f2 = instance
        .get_typed_func::<(), f64>(&mut store, "via_number_f")
        .unwrap();
    assert!((f2.call(&mut store, ()).unwrap() - 3.14).abs() < 1e-9);
}

#[test]
fn math_fround_clz32_imul() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            // fround bit-exact: 1.1 rounded to f32 then back to f64
            sink(Math.fround(1.1));
            // clz32: leading zeros of 1 is 31; of 0 is 32; of 0xffffffff is 0
            sink(Math.clz32(1) as f64);
            sink(Math.clz32(0) as f64);
            // imul: -1 * 5 = -5 (wraps through 32-bit multiply)
            sink(Math.imul(-1, 5) as f64);
            sink(Math.imul(7, 3) as f64);
        }
    "#,
    );
    assert_eq!(vals[0], 1.1_f32 as f64);
    assert_eq!(vals[1], 31.0);
    assert_eq!(vals[2], 32.0);
    assert_eq!(vals[3], -5.0);
    assert_eq!(vals[4], 21.0);
}

#[test]
fn math_hyperbolic_inverse_and_log1p_expm1() {
    // asinh, acosh, atanh, log1p, expm1 — all declared as host imports.
    // Compile with default options (host_module="host"), wire to libm.
    let wasm = compile(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            sink(Math.asinh(1.0));
            sink(Math.acosh(2.0));
            sink(Math.atanh(0.5));
            sink(Math.log1p(1.0));
            sink(Math.expm1(1.0));
        }
    "#,
    );
    use std::sync::{Arc, Mutex};
    let got: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, got.clone());
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "sink",
            |caller: Caller<'_, Arc<Mutex<Vec<f64>>>>, x: f64| {
                caller.data().lock().unwrap().push(x);
            },
        )
        .unwrap();
    // Wire transcendentals to f64 intrinsics
    linker
        .func_wrap("host", "__tscc_asinh", |x: f64| x.asinh())
        .unwrap();
    linker
        .func_wrap("host", "__tscc_acosh", |x: f64| x.acosh())
        .unwrap();
    linker
        .func_wrap("host", "__tscc_atanh", |x: f64| x.atanh())
        .unwrap();
    linker
        .func_wrap("host", "__tscc_log1p", |x: f64| x.ln_1p())
        .unwrap();
    linker
        .func_wrap("host", "__tscc_expm1", |x: f64| x.exp_m1())
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    let vals = got.lock().unwrap().clone();
    assert!((vals[0] - 1.0_f64.asinh()).abs() < 1e-12);
    assert!((vals[1] - 2.0_f64.acosh()).abs() < 1e-12);
    assert!((vals[2] - 0.5_f64.atanh()).abs() < 1e-12);
    assert!((vals[3] - 1.0_f64.ln_1p()).abs() < 1e-12);
    assert!((vals[4] - 1.0_f64.exp_m1()).abs() < 1e-12);
}

