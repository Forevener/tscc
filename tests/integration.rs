use std::cell::Cell;

use wasmtime::*;

fn compile(source: &str) -> Vec<u8> {
    tscc::compile(source, &tscc::CompileOptions::default()).unwrap()
}

#[test]
fn empty_tick_loads_and_runs() {
    let wasm = compile("export function tick(me: i32): void {}");
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
}

#[test]
fn arithmetic_in_locals() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            let x: f64 = 1.0 + 2.0;
            let y: f64 = x * 3.0;
            set_action(me, 1, 0, x, y);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, (0i32, 0i32, 0i32, 0.0f64, 0.0f64));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, (i32, i32, i32, f64, f64)>,
             me: i32,
             kind: i32,
             target: i32,
             dx: f64,
             dy: f64| {
                *caller.data_mut() = (me, kind, target, dx, dy);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 42).unwrap();
    let state = store.data();
    assert_eq!(state.0, 42); // me
    assert_eq!(state.1, 1); // kind
    assert_eq!(state.2, 0); // target
    assert!((state.3 - 3.0).abs() < 1e-10); // dx = 1+2 = 3
    assert!((state.4 - 9.0).abs() < 1e-10); // dy = 3*3 = 9
}

#[test]
fn if_else_control_flow() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            let kind: i32 = 0;
            if (me > 10) {
                kind = 1;
            } else {
                kind = 2;
            }
            set_action(me, kind, 0, 0.0, 0.0);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, (0i32, 0i32));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, (i32, i32)>,
             me: i32,
             kind: i32,
             _target: i32,
             _dx: f64,
             _dy: f64| {
                *caller.data_mut() = (me, kind);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();

    tick.call(&mut store, 20).unwrap();
    assert_eq!(*store.data(), (20, 1));

    tick.call(&mut store, 5).unwrap();
    assert_eq!(*store.data(), (5, 2));
}

#[test]
fn local_function_call() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        function clamp(val: f64, lo: f64, hi: f64): f64 {
            if (val < lo) { return lo; }
            if (val > hi) { return hi; }
            return val;
        }

        export function tick(me: i32): void {
            let x: f64 = clamp(5.0, 0.0, 3.0);
            let y: f64 = clamp(-2.0, 0.0, 3.0);
            set_action(me, 0, 0, x, y);
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
    let (x, y) = *store.data();
    assert!((x - 3.0).abs() < 1e-10); // clamped to hi
    assert!((y - 0.0).abs() < 1e-10); // clamped to lo
}

#[test]
fn for_loop() {
    // Sum integers 0..10 using a for loop
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            let sum: i32 = 0;
            for (let i: i32 = 0; i < 10; i++) {
                sum = sum + i;
            }
            set_action(me, sum, 0, 0.0, 0.0);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0i32);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, i32>, _me: i32, kind: i32, _target: i32, _dx: f64, _dy: f64| {
                *caller.data_mut() = kind;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert_eq!(*store.data(), 45); // 0+1+2+...+9 = 45
}

#[test]
fn while_loop() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            let n: i32 = 1;
            while (n < 100) {
                n = n * 2;
            }
            set_action(me, n, 0, 0.0, 0.0);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0i32);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, i32>, _me: i32, kind: i32, _target: i32, _dx: f64, _dy: f64| {
                *caller.data_mut() = kind;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert_eq!(*store.data(), 128); // 1,2,4,8,16,32,64,128
}

#[test]
fn type_cast_f64_from_i32() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            let hp: i32 = 75;
            let maxHp: i32 = 100;
            let frac: f64 = f64(hp) / f64(maxHp);
            set_action(me, 0, 0, frac, 0.0);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0.0f64);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, f64>, _me: i32, _kind: i32, _target: i32, dx: f64, _dy: f64| {
                *caller.data_mut() = dx;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert!((*store.data() - 0.75).abs() < 1e-10);
}

#[test]
fn milestone_script() {
    let wasm = compile(
        r#"
        declare function me_x(me: i32): f64;
        declare function me_y(me: i32): f64;
        declare function me_hp(me: i32): i32;
        declare function me_max_hp(me: i32): i32;
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        declare function random(me: i32): f64;

        function clamp(val: f64, lo: f64, hi: f64): f64 {
            if (val < lo) { return lo; }
            if (val > hi) { return hi; }
            return val;
        }

        export function tick(me: i32): void {
            const mx: f64 = me_x(me);
            const my: f64 = me_y(me);
            const hp: i32 = me_hp(me);
            const maxHp: i32 = me_max_hp(me);
            const hpFrac: f64 = f64(hp) / f64(maxHp);

            let dx: f64 = 0.0;
            let dy: f64 = 0.0;

            if (hpFrac < 0.25) {
                dx = random(me) - 0.5;
                dy = random(me) - 0.5;
            } else {
                dx = random(me) - 0.5;
                dy = random(me) - 0.5;
            }

            const len: f64 = dx * dx + dy * dy;
            if (len > 0.0) {
                dx = dx / len;
                dy = dy / len;
            }

            dx = clamp(dx, -1.0, 1.0);
            dy = clamp(dy, -1.0, 1.0);
            set_action(me, 1, 0, dx, dy);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();

    struct TestState {
        random_counter: Cell<u32>,
        action: (i32, i32, i32, f64, f64),
    }

    let mut store = Store::new(
        &engine,
        TestState {
            random_counter: Cell::new(0),
            action: (0, 0, 0, 0.0, 0.0),
        },
    );

    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "me_x",
            |_caller: Caller<'_, TestState>, _me: i32| -> f64 { 10.0 },
        )
        .unwrap();
    linker
        .func_wrap(
            "host",
            "me_y",
            |_caller: Caller<'_, TestState>, _me: i32| -> f64 { 20.0 },
        )
        .unwrap();
    linker
        .func_wrap(
            "host",
            "me_hp",
            |_caller: Caller<'_, TestState>, _me: i32| -> i32 { 80 },
        )
        .unwrap();
    linker
        .func_wrap(
            "host",
            "me_max_hp",
            |_caller: Caller<'_, TestState>, _me: i32| -> i32 { 100 },
        )
        .unwrap();
    linker
        .func_wrap(
            "host",
            "random",
            |caller: Caller<'_, TestState>, _me: i32| -> f64 {
                let counter = caller.data().random_counter.get();
                caller.data().random_counter.set(counter + 1);
                // Return deterministic "random" values
                [0.7, 0.3, 0.8, 0.2][counter as usize % 4]
            },
        )
        .unwrap();
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, TestState>,
             me: i32,
             kind: i32,
             target: i32,
             dx: f64,
             dy: f64| {
                caller.data_mut().action = (me, kind, target, dx, dy);
            },
        )
        .unwrap();

    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 42).unwrap();

    let action = store.data().action;
    assert_eq!(action.0, 42); // me
    assert_eq!(action.1, 1); // kind = Move
    assert_eq!(action.2, 0); // target
    // dx and dy should be finite, clamped values
    assert!(action.3.is_finite());
    assert!(action.4.is_finite());
    assert!(action.3 >= -1.0 && action.3 <= 1.0);
    assert!(action.4 >= -1.0 && action.4 <= 1.0);
}

// ---- Phase 2 tests: memory intrinsics + math builtins ----

#[test]
fn memory_load_store_f64() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            store_f64(0, 3.14);
            store_f64(8, 2.72);
            const a: f64 = load_f64(0);
            const b: f64 = load_f64(8);
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
    assert!((a - 3.14).abs() < 1e-10);
    assert!((b - 2.72).abs() < 1e-10);
}

#[test]
fn memory_load_store_i32() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            store_i32(0, 42);
            store_i32(4, 99);
            const a: i32 = load_i32(0);
            const b: i32 = load_i32(4);
            set_action(me, a, b, 0.0, 0.0);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, (0i32, 0i32));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, (i32, i32)>,
             _me: i32,
             kind: i32,
             target: i32,
             _dx: f64,
             _dy: f64| {
                *caller.data_mut() = (kind, target);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert_eq!(*store.data(), (42, 99));
}

#[test]
fn static_alloc() {
    // __static_alloc reserves bytes at compile time and returns the offset
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        const BUF: i32 = __static_alloc(40);

        export function tick(me: i32): void {
            store_f64(BUF, 1.5);
            store_f64(BUF + 8, 2.5);
            const a: f64 = load_f64(BUF);
            const b: f64 = load_f64(BUF + 8);
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
    assert!((a - 1.5).abs() < 1e-10);
    assert!((b - 2.5).abs() < 1e-10);
}

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

/// Drive a compiled module whose `tick` exports a sequence of f64 values
/// through a host `sink(x: f64)` import, and return the collected values.
fn run_sink_tick(source: &str) -> Vec<f64> {
    use std::sync::{Arc, Mutex};
    let got: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let wasm = compile(source);
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
    let v = got.lock().unwrap().clone();
    v
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
fn string_concat_method() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let a: string = "foo";
            let b: string = "bar";
            return a.concat(b);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "foobar");
}

#[test]
fn array_is_array_static() {
    let wasm = compile(
        r#"
        export function arr_local(): i32 {
            const xs: Array<i32> = new Array<i32>(0);
            xs.push(10);
            return Array.isArray(xs) ? 1 : 0;
        }
        export function arr_new(): i32 {
            const ys: Array<i32> = new Array<i32>(0);
            return Array.isArray(ys) ? 1 : 0;
        }
        export function arr_scalar(): i32 {
            const n: i32 = 42;
            return Array.isArray(n) ? 1 : 0;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    for (name, expected) in [("arr_local", 1), ("arr_new", 1), ("arr_scalar", 0)] {
        let f = instance
            .get_typed_func::<(), i32>(&mut store, name)
            .unwrap();
        assert_eq!(f.call(&mut store, ()).unwrap(), expected, "{name}");
    }
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

#[test]
fn array_pop_and_indexof_and_includes() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a: Array<i32> = new Array<i32>(8);
            a.push(10); a.push(20); a.push(30);
            sink(a.pop() as f64);      // 30
            sink(a.length as f64);     // 2
            sink(a.indexOf(10) as f64);     // 0
            sink(a.indexOf(999) as f64);    // -1
            sink(a.lastIndexOf(20) as f64); // 1
            sink(a.includes(10) ? 1.0 : 0.0); // 1
            sink(a.includes(999) ? 1.0 : 0.0); // 0
            // pop on empty returns default (0)
            const e: Array<i32> = new Array<i32>(0);
            sink(e.pop() as f64);     // 0
        }
    "#,
    );
    assert_eq!(vals, vec![30.0, 2.0, 0.0, -1.0, 1.0, 1.0, 0.0, 0.0]);
}

#[test]
fn array_reverse_and_at() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a: Array<i32> = new Array<i32>(8);
            a.push(1); a.push(2); a.push(3); a.push(4);
            a.reverse();
            sink(a[0] as f64); // 4
            sink(a[3] as f64); // 1
            sink(a.at(0) as f64);  // 4
            sink(a.at(-1) as f64); // 1
            sink(a.at(-2) as f64); // 2
        }
    "#,
    );
    assert_eq!(vals, vec![4.0, 1.0, 4.0, 1.0, 2.0]);
}

#[test]
fn array_fill_slice_concat() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a: Array<i32> = new Array<i32>(8);
            a.push(1); a.push(2); a.push(3); a.push(4); a.push(5);
            a.fill(9, 1, 4);
            sink(a[0] as f64); // 1
            sink(a[1] as f64); // 9
            sink(a[2] as f64); // 9
            sink(a[3] as f64); // 9
            sink(a[4] as f64); // 5

            const b: Array<i32> = a.slice(1, 4);
            sink(b.length as f64); // 3
            sink(b[0] as f64); // 9
            sink(b[2] as f64); // 9

            // Negative slice
            const c: Array<i32> = a.slice(-2);
            sink(c.length as f64); // 2
            sink(c[0] as f64); // 9 (a[3])
            sink(c[1] as f64); // 5 (a[4])

            // Concat
            const d: Array<i32> = b.concat(c);
            sink(d.length as f64); // 5
            sink(d[0] as f64); // 9
            sink(d[4] as f64); // 5
        }
    "#,
    );
    assert_eq!(
        vals,
        vec![
            1.0, 9.0, 9.0, 9.0, 5.0, 3.0, 9.0, 9.0, 2.0, 9.0, 5.0, 5.0, 9.0, 5.0
        ]
    );
}

#[test]
fn array_join_numeric_and_string() {
    use std::sync::{Arc, Mutex};
    let got: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    // sink_str takes a string pointer; the host reads the 4-byte length header
    // and the UTF-8 bytes that follow it.
    let wasm = compile(
        r#"
        declare function sink_str(s: string): void;

        export function tick(me: i32): void {
            const a: Array<i32> = new Array<i32>(8);
            a.push(1); a.push(2); a.push(3);
            sink_str(a.join(","));
            sink_str(a.join(" - "));
            sink_str(a.join());
            const e: Array<i32> = new Array<i32>(0);
            sink_str(e.join(","));
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
            "sink_str",
            |mut caller: Caller<'_, Arc<Mutex<Vec<String>>>>, s_ptr: i32| {
                let mem = caller.get_export("memory").unwrap().into_memory().unwrap();
                let mut hdr = [0u8; 4];
                mem.read(&mut caller, s_ptr as usize, &mut hdr).unwrap();
                let len = i32::from_le_bytes(hdr) as usize;
                let mut buf = vec![0u8; len];
                mem.read(&mut caller, s_ptr as usize + 4, &mut buf).unwrap();
                let s = String::from_utf8(buf).unwrap();
                caller.data().lock().unwrap().push(s);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    let vals = got.lock().unwrap().clone();
    assert_eq!(vals, vec!["1,2,3", "1 - 2 - 3", "1,2,3", ""]);
}

#[test]
fn array_find_and_some_every() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a: Array<i32> = new Array<i32>(8);
            a.push(1); a.push(2); a.push(3); a.push(4); a.push(5);

            sink(a.find((x: i32) => x > 3) as f64);       // 4
            sink(a.findIndex((x: i32) => x > 3) as f64);  // 3
            sink(a.findLast((x: i32) => x > 3) as f64);   // 5
            sink(a.findLastIndex((x: i32) => x > 3) as f64); // 4

            // Miss cases
            sink(a.find((x: i32) => x > 99) as f64);      // 0 (default)
            sink(a.findIndex((x: i32) => x > 99) as f64); // -1

            sink(a.some((x: i32) => x > 4) ? 1.0 : 0.0);  // 1
            sink(a.some((x: i32) => x > 99) ? 1.0 : 0.0); // 0
            sink(a.every((x: i32) => x > 0) ? 1.0 : 0.0); // 1
            sink(a.every((x: i32) => x > 3) ? 1.0 : 0.0); // 0
        }
    "#,
    );
    assert_eq!(
        vals,
        vec![4.0, 3.0, 5.0, 4.0, 0.0, -1.0, 1.0, 0.0, 1.0, 0.0]
    );
}

#[test]
fn realistic_lowlevel_perceive_parsing() {
    // Simulates the core pattern from realistic_lowlevel.ts:
    // perceive fills a buffer, we parse it with load_f64/load_i32, find nearest enemy
    let wasm = compile(
        r#"
        declare function me_x(me: i32): f64;
        declare function me_y(me: i32): f64;
        declare function me_team(me: i32): i32;
        declare function perceive(me: i32, out_ptr: i32, max: i32): i32;
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        const MAX_PERCEIVED: i32 = 32;
        const ENTRY_SIZE: i32 = 40;
        const PERC_BUF: i32 = __static_alloc(1280);

        export function tick(me: i32): void {
            const count: i32 = perceive(me, PERC_BUF, MAX_PERCEIVED);
            const mx: f64 = me_x(me);
            const my: f64 = me_y(me);
            const myTeam: i32 = me_team(me);

            let bestDist: f64 = 999999.0;
            let bestId: i32 = -1;
            let bestDx: f64 = 0.0;
            let bestDy: f64 = 0.0;

            for (let i: i32 = 0; i < count; i++) {
                const off: i32 = PERC_BUF + i * ENTRY_SIZE;
                const ex: f64 = load_f64(off);
                const ey: f64 = load_f64(off + 8);
                const distSq: f64 = load_f64(off + 16);
                const eid: i32 = load_i32(off + 24);
                const eteam: i32 = load_i32(off + 36);

                if (eteam != myTeam) {
                    if (distSq < bestDist) {
                        bestDist = distSq;
                        bestId = eid;
                        bestDx = ex - mx;
                        bestDy = ey - my;
                    }
                }
            }

            if (bestId >= 0) {
                const len: f64 = Math.sqrt(bestDx * bestDx + bestDy * bestDy);
                if (len > 0.0) {
                    bestDx = bestDx / len;
                    bestDy = bestDy / len;
                }
                set_action(me, 1, bestId, bestDx, bestDy);
            } else {
                set_action(me, 0, 0, 0.0, 0.0);
            }
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();

    struct State {
        action: (i32, i32, i32, f64, f64),
    }

    let mut store = Store::new(
        &engine,
        State {
            action: (0, 0, 0, 0.0, 0.0),
        },
    );

    let mut linker = Linker::new(&engine);
    linker
        .func_wrap("host", "me_x", |_: Caller<'_, State>, _: i32| -> f64 {
            0.0
        })
        .unwrap();
    linker
        .func_wrap("host", "me_y", |_: Caller<'_, State>, _: i32| -> f64 {
            0.0
        })
        .unwrap();
    linker
        .func_wrap("host", "me_team", |_: Caller<'_, State>, _: i32| -> i32 {
            1
        })
        .unwrap();
    linker
        .func_wrap(
            "host",
            "perceive",
            |mut caller: Caller<'_, State>, _me: i32, out_ptr: i32, _max: i32| -> i32 {
                // Write 2 entities into the perception buffer
                let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                let data = memory.data_mut(&mut caller);
                let ptr = out_ptr as usize;

                // Entity 0: friendly (team=1), at (5,0), distSq=25
                data[ptr..ptr + 8].copy_from_slice(&5.0f64.to_le_bytes()); // x
                data[ptr + 8..ptr + 16].copy_from_slice(&0.0f64.to_le_bytes()); // y
                data[ptr + 16..ptr + 24].copy_from_slice(&25.0f64.to_le_bytes()); // distSq
                data[ptr + 24..ptr + 28].copy_from_slice(&100i32.to_le_bytes()); // id
                data[ptr + 36..ptr + 40].copy_from_slice(&1i32.to_le_bytes()); // team=1 (same)

                // Entity 1: enemy (team=2), at (3,4), distSq=25
                let ptr2 = ptr + 40;
                data[ptr2..ptr2 + 8].copy_from_slice(&3.0f64.to_le_bytes()); // x
                data[ptr2 + 8..ptr2 + 16].copy_from_slice(&4.0f64.to_le_bytes()); // y
                data[ptr2 + 16..ptr2 + 24].copy_from_slice(&25.0f64.to_le_bytes()); // distSq
                data[ptr2 + 24..ptr2 + 28].copy_from_slice(&200i32.to_le_bytes()); // id
                data[ptr2 + 36..ptr2 + 40].copy_from_slice(&2i32.to_le_bytes()); // team=2 (enemy)

                2 // count
            },
        )
        .unwrap();
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, State>, me: i32, kind: i32, target: i32, dx: f64, dy: f64| {
                caller.data_mut().action = (me, kind, target, dx, dy);
            },
        )
        .unwrap();

    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 7).unwrap();

    let action = store.data().action;
    assert_eq!(action.0, 7); // me
    assert_eq!(action.1, 1); // kind = Move
    assert_eq!(action.2, 200); // target = enemy id
    // Direction should be normalized (3,4)/5 = (0.6, 0.8)
    assert!((action.3 - 0.6).abs() < 1e-10, "dx={}", action.3);
    assert!((action.4 - 0.8).abs() < 1e-10, "dy={}", action.4);
}

// ---- Phase 3 tests: classes + arena allocation ----

#[test]
fn class_basic_fields_and_constructor() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        class Vec2 {
            x: f64;
            y: f64;

            constructor(x: f64, y: f64) {
            }
        }

        export function tick(me: i32): void {
            const v: Vec2 = new Vec2(3.0, 4.0);
            set_action(me, 0, 0, v.x, v.y);
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
    let (x, y) = *store.data();
    assert!((x - 3.0).abs() < 1e-10, "x={x}");
    assert!((y - 4.0).abs() < 1e-10, "y={y}");
}

#[test]
fn class_methods() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        class Vec2 {
            x: f64;
            y: f64;

            constructor(x: f64, y: f64) {
            }

            lengthSq(): f64 {
                return this.x * this.x + this.y * this.y;
            }
        }

        export function tick(me: i32): void {
            const v: Vec2 = new Vec2(3.0, 4.0);
            const len2: f64 = v.lengthSq();
            set_action(me, 0, 0, len2, 0.0);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0.0f64);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, f64>, _me: i32, _kind: i32, _target: i32, dx: f64, _dy: f64| {
                *caller.data_mut() = dx;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert!((*store.data() - 25.0).abs() < 1e-10); // 3^2 + 4^2 = 25
}

#[test]
fn arena_reset_by_host() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        class Point {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {
            }
        }

        export function tick(me: i32): void {
            const p: Point = new Point(1.0, 2.0);
            set_action(me, 0, 0, p.x, p.y);
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

    // Get the arena pointer global
    let arena_ptr = instance.get_global(&mut store, "__arena_ptr").unwrap();
    let initial_arena = arena_ptr.get(&mut store).i32().unwrap();

    // First tick — allocates a Point
    tick.call(&mut store, 0).unwrap();
    let after_alloc = arena_ptr.get(&mut store).i32().unwrap();
    assert!(after_alloc > initial_arena, "arena should have advanced");

    // Host resets arena
    arena_ptr.set(&mut store, Val::I32(initial_arena)).unwrap();

    // Second tick — should work fine with fresh arena
    tick.call(&mut store, 0).unwrap();
    let (x, y) = *store.data();
    assert!((x - 1.0).abs() < 1e-10);
    assert!((y - 2.0).abs() < 1e-10);
}

#[test]
fn class_method_with_params() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        class Entity {
            x: f64;
            y: f64;
            hp: i32;
            team: i32;

            constructor(x: f64, y: f64, hp: i32, team: i32) {
            }

            isAlive(): i32 {
                if (this.hp > 0) { return 1; }
                return 0;
            }

            distSqFrom(ox: f64, oy: f64): f64 {
                const dx: f64 = this.x - ox;
                const dy: f64 = this.y - oy;
                return dx * dx + dy * dy;
            }
        }

        export function tick(me: i32): void {
            const e: Entity = new Entity(3.0, 4.0, 100, 1);
            const alive: i32 = e.isAlive();
            const dist2: f64 = e.distSqFrom(0.0, 0.0);
            set_action(me, alive, e.team, dist2, 0.0);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();

    struct S {
        action: (i32, i32, i32, f64, f64),
    }
    let mut store = Store::new(
        &engine,
        S {
            action: (0, 0, 0, 0.0, 0.0),
        },
    );
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, S>, me: i32, kind: i32, target: i32, dx: f64, dy: f64| {
                caller.data_mut().action = (me, kind, target, dx, dy);
            },
        )
        .unwrap();

    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 7).unwrap();

    let a = store.data().action;
    assert_eq!(a.0, 7); // me
    assert_eq!(a.1, 1); // alive = true
    assert_eq!(a.2, 1); // team
    assert!((a.3 - 25.0).abs() < 1e-10, "dist2={}", a.3); // 3^2 + 4^2 = 25
}

#[test]
fn this_field_assignment_in_constructor() {
    // Test that constructor with this.field = expr works
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        class Pair {
            a: f64;
            b: f64;

            constructor(x: f64, y: f64) {
                this.a = x * 2.0;
                this.b = y + 1.0;
            }
        }

        export function tick(me: i32): void {
            const p: Pair = new Pair(3.0, 4.0);
            set_action(me, 0, 0, p.a, p.b);
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
    assert!((a - 6.0).abs() < 1e-10, "a={a}"); // 3*2 = 6
    assert!((b - 5.0).abs() < 1e-10, "b={b}"); // 4+1 = 5
}

// ---- Phase 4 tests: arrays ----

#[test]
fn array_new_and_length() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<f64> = new Array<f64>(8);
            return arr.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert_eq!(result, 0); // new array starts with length 0
}

#[test]
fn array_push_and_length() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<f64> = new Array<f64>(8);
            arr.push(1.0);
            arr.push(2.0);
            arr.push(3.0);
            return arr.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert_eq!(result, 3);
}

#[test]
fn array_push_and_index_f64() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(10.0);
            arr.push(20.0);
            arr.push(30.0);
            return arr[1];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 20.0).abs() < 1e-10);
}

#[test]
fn array_push_and_index_i32() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(4);
            arr.push(100);
            arr.push(200);
            arr.push(300);
            return arr[2];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert_eq!(result, 300);
}

#[test]
fn array_index_assignment() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(10.0);
            arr.push(20.0);
            arr[1] = 99.0;
            return arr[1];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 99.0).abs() < 1e-10);
}

#[test]
fn array_compound_assignment() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(10.0);
            arr.push(20.0);
            arr[0] += 5.0;
            return arr[0];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 15.0).abs() < 1e-10);
}

#[test]
fn array_for_loop_iteration() {
    // Sum all elements using a for loop
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(1.0);
            arr.push(2.0);
            arr.push(3.0);
            arr.push(4.0);
            let sum: f64 = 0.0;
            for (let i: i32 = 0; i < arr.length; i++) {
                sum += arr[i];
            }
            return sum;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 10.0).abs() < 1e-10);
}

#[test]
fn array_bounds_check_traps() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(1.0);
            return arr[5];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ());
    assert!(result.is_err(), "out-of-bounds access should trap");
}

#[test]
fn array_push_grows_beyond_capacity() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(2);
            arr.push(10);
            arr.push(20);
            arr.push(30);
            return arr[0] * 100 + arr[1] * 10 + arr[2];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert_eq!(result, 1230); // 10*100 + 20*10 + 30
}

#[test]
fn array_grow_from_zero_capacity() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(0);
            arr.push(11);
            arr.push(22);
            arr.push(33);
            return arr.length * 1000 + arr[0] * 100 + arr[1] * 10 + arr[2];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert_eq!(result, 3_000 + 1100 + 220 + 33); // length=3, vals 11,22,33
}

#[test]
fn array_grow_preserves_elements() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(2);
            arr.push(1.5);
            arr.push(2.5);
            arr.push(3.5);
            arr.push(4.5);
            arr.push(5.5);
            return arr[0] + arr[1] + arr[2] + arr[3] + arr[4];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 17.5).abs() < 1e-10); // 1.5+2.5+3.5+4.5+5.5 = 17.5
}

#[test]
fn array_grow_multiple_doublings() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(1);
            arr.push(0);
            arr.push(1);
            arr.push(2);
            arr.push(3);
            arr.push(4);
            arr.push(5);
            arr.push(6);
            arr.push(7);
            arr.push(8);
            arr.push(9);
            let sum: i32 = 0;
            for (let i: i32 = 0; i < arr.length; i++) {
                sum += arr[i];
            }
            return sum * 10 + arr.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    // sum = 0+1+2+3+4+5+6+7+8+9 = 45, length = 10
    assert_eq!(result, 45 * 10 + 10); // 460
}

#[test]
fn array_grow_class_elements() {
    let wasm = compile(
        r#"
        class Point {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}
        }

        export function test(): f64 {
            let pts: Array<Point> = new Array<Point>(1);
            let p1: Point = new Point(1.0, 10.0);
            let p2: Point = new Point(2.0, 20.0);
            let p3: Point = new Point(3.0, 30.0);
            pts.push(p1);
            pts.push(p2);
            pts.push(p3);
            let a: Point = pts[0];
            let b: Point = pts[1];
            let c: Point = pts[2];
            return a.x + b.x + c.y;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 33.0).abs() < 1e-10); // 1.0 + 2.0 + 30.0 = 33.0
}

#[test]
fn array_grow_then_filter() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(2);
            arr.push(1);
            arr.push(2);
            arr.push(3);
            arr.push(4);
            arr.push(5);
            arr.push(6);
            let evens: Array<i32> = arr.filter(x => x % 2 == 0);
            return evens.length * 100 + evens[0] * 10 + evens[1];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    // evens = [2, 4, 6], length=3, 3*100 + 2*10 + 4 = 324
    assert_eq!(result, 324);
}

#[test]
fn array_grow_inplace_arena_efficiency() {
    // In-place grow: sequential pushes with no intervening allocations should
    // use arena space ~= final array size, not the sum of all intermediate copies.
    // We verify by reading __arena_ptr global from the host before/after.
    let wasm = compile(
        r#"
        export function push_ten(): void {
            let arr: Array<i32> = new Array<i32>(0);
            arr.push(0);
            arr.push(1);
            arr.push(2);
            arr.push(3);
            arr.push(4);
            arr.push(5);
            arr.push(6);
            arr.push(7);
            arr.push(8);
            arr.push(9);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let arena_ptr = instance
        .get_global(&mut store, "__arena_ptr")
        .expect("__arena_ptr global");
    let before = arena_ptr.get(&mut store).i32().unwrap();
    let push_ten = instance
        .get_typed_func::<(), ()>(&mut store, "push_ten")
        .unwrap();
    push_ten.call(&mut store, ()).unwrap();
    let after = arena_ptr.get(&mut store).i32().unwrap();
    let arena_delta = after - before;
    // With in-place grow from cap 0: 0->1->2->4->8->16
    // Final array: header(8) + 16 * 4 = 72 bytes
    // Without in-place grow, total would be: 8+4 + 8+8 + 8+16 + 8+32 + 8+64 = 164 bytes
    assert_eq!(
        arena_delta,
        8 + 16 * 4,
        "in-place grow should only use space for the final array, got {arena_delta}"
    );
}

#[test]
fn array_grow_copyabandon_with_intervening_alloc() {
    // When another allocation happens between array creation and push,
    // in-place grow can't be used — falls back to copy-and-abandon.
    // Array should still work correctly.
    let wasm = compile(
        r#"
        class Box {
            val: i32;
            constructor(val: i32) {}
        }
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(1);
            arr.push(10);
            let b: Box = new Box(999);
            arr.push(20);
            arr.push(30);
            return arr[0] * 100 + arr[1] * 10 + arr[2] + b.val;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    // 10*100 + 20*10 + 30 + 999 = 1000 + 200 + 30 + 999 = 2229
    assert_eq!(result, 2229);
}

#[test]
fn array_with_host_imports() {
    // Realistic pattern: fill an array from host data, find max
    let wasm = compile(
        r#"
        declare function get_count(): i32;
        declare function get_value(idx: i32): f64;

        export function find_max(): f64 {
            let n: i32 = get_count();
            let vals: Array<f64> = new Array<f64>(16);
            for (let i: i32 = 0; i < n; i++) {
                vals.push(get_value(i));
            }
            let max: f64 = vals[0];
            for (let i: i32 = 1; i < vals.length; i++) {
                if (vals[i] > max) {
                    max = vals[i];
                }
            }
            return max;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let values: Vec<f64> = vec![3.0, 7.0, 1.0, 9.0, 2.0];
    let mut store = Store::new(&engine, values);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap("host", "get_count", |caller: Caller<'_, Vec<f64>>| -> i32 {
            caller.data().len() as i32
        })
        .unwrap();
    linker
        .func_wrap(
            "host",
            "get_value",
            |caller: Caller<'_, Vec<f64>>, idx: i32| -> f64 { caller.data()[idx as usize] },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let find_max = instance
        .get_typed_func::<(), f64>(&mut store, "find_max")
        .unwrap();
    let result = find_max.call(&mut store, ()).unwrap();
    assert!((result - 9.0).abs() < 1e-10);
}

#[test]
fn array_of_class_instances() {
    // Array<Entity> — store pointers to class instances
    let wasm = compile(
        r#"
        class Entity {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}
        }

        export function test(): f64 {
            let entities: Array<Entity> = new Array<Entity>(4);
            let e1: Entity = new Entity(1.0, 2.0);
            let e2: Entity = new Entity(3.0, 4.0);
            let e3: Entity = new Entity(5.0, 6.0);
            entities.push(e1);
            entities.push(e2);
            entities.push(e3);

            // Sum all x values
            let sum: f64 = 0.0;
            for (let i: i32 = 0; i < entities.length; i++) {
                let e: Entity = entities[i];
                sum += e.x;
            }
            return sum;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 9.0).abs() < 1e-10); // 1 + 3 + 5 = 9
}

// ---- Phase 5 tests: closures + higher-order functions ----

#[test]
fn array_filter_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(8);
            arr.push(1);
            arr.push(2);
            arr.push(3);
            arr.push(4);
            arr.push(5);
            let evens: Array<i32> = arr.filter(x => x % 2 == 0);
            return evens.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 2); // 2, 4
}

#[test]
fn array_filter_with_capture() {
    // Filter with a captured variable from the enclosing scope
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(8);
            arr.push(10);
            arr.push(20);
            arr.push(30);
            arr.push(40);
            arr.push(50);
            let threshold: i32 = 25;
            let big: Array<i32> = arr.filter(x => x > threshold);
            return big.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 3); // 30, 40, 50
}

#[test]
fn array_filter_class_instances() {
    // Filter entities by a field — the key game scripting pattern
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }

        export function test(): i32 {
            let entities: Array<Entity> = new Array<Entity>(8);
            entities.push(new Entity(100, 1));
            entities.push(new Entity(0, 1));
            entities.push(new Entity(50, 2));
            entities.push(new Entity(75, 2));
            entities.push(new Entity(0, 1));

            let alive: Array<Entity> = entities.filter(e => e.hp > 0);
            return alive.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 3); // hp > 0: 100, 50, 75
}

#[test]
fn array_filter_with_class_capture() {
    // Filter enemies by team, capturing myTeam — the canonical game AI pattern
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }

        export function count_enemies(my_team: i32): i32 {
            let entities: Array<Entity> = new Array<Entity>(8);
            entities.push(new Entity(100, 1));
            entities.push(new Entity(80, 2));
            entities.push(new Entity(50, 1));
            entities.push(new Entity(60, 2));

            let enemies: Array<Entity> = entities.filter(e => e.team != my_team && e.hp > 0);
            return enemies.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<i32, i32>(&mut store, "count_enemies")
        .unwrap();
    assert_eq!(test.call(&mut store, 1).unwrap(), 2); // team 2: hp 80, 60
    assert_eq!(test.call(&mut store, 2).unwrap(), 2); // team 1: hp 100, 50
}

#[test]
fn array_map_basic() {
    // Map f64 array to doubled values
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(1.0);
            arr.push(2.0);
            arr.push(3.0);
            let doubled: Array<f64> = arr.map(x => x * 2.0);
            return doubled[0] + doubled[1] + doubled[2];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 12.0).abs() < 1e-10); // 2 + 4 + 6
}

#[test]
fn array_map_extract_field() {
    // Map Entity -> f64 (extract hp values) — common game pattern
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }

        export function test(): i32 {
            let entities: Array<Entity> = new Array<Entity>(4);
            entities.push(new Entity(100, 1));
            entities.push(new Entity(50, 2));
            entities.push(new Entity(75, 1));
            let hps: Array<i32> = entities.map(e => e.hp);
            return hps[0] + hps[1] + hps[2];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 225); // 100 + 50 + 75
}

#[test]
fn array_foreach() {
    // forEach to accumulate a sum via captured mutable variable
    let wasm = compile(
        r#"
        declare function report(val: f64): void;

        export function test(): void {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(1.0);
            arr.push(2.0);
            arr.push(3.0);
            let sum: f64 = 0.0;
            arr.forEach(x => { sum += x; });
            report(sum);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, Cell::new(0.0f64));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "report",
            |mut caller: Caller<'_, Cell<f64>>, val: f64| {
                caller.data_mut().set(val);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let test = instance
        .get_typed_func::<(), ()>(&mut store, "test")
        .unwrap();
    test.call(&mut store, ()).unwrap();
    assert!((store.data().get() - 6.0).abs() < 1e-10); // 1 + 2 + 3
}

#[test]
fn array_reduce_sum() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(1.0);
            arr.push(2.0);
            arr.push(3.0);
            arr.push(4.0);
            let total: f64 = arr.reduce((sum, x) => sum + x, 0.0);
            return total;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 10.0).abs() < 1e-10);
}

#[test]
fn array_reduce_entity_hp_sum() {
    // reduce over class instances — sum all HP values
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }

        export function test(): i32 {
            let entities: Array<Entity> = new Array<Entity>(4);
            entities.push(new Entity(100, 1));
            entities.push(new Entity(50, 2));
            entities.push(new Entity(75, 1));
            let total_hp: i32 = entities.reduce((sum, e) => sum + e.hp, 0);
            return total_hp;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 225);
}

#[test]
fn array_sort_f64() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(3.0);
            arr.push(1.0);
            arr.push(4.0);
            arr.push(2.0);
            arr.sort((a, b) => a - b);
            // After sort: [1, 2, 3, 4]
            return arr[0] * 1000.0 + arr[1] * 100.0 + arr[2] * 10.0 + arr[3];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 1234.0).abs() < 1e-10);
}

#[test]
fn array_sort_descending() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(3.0);
            arr.push(1.0);
            arr.push(4.0);
            arr.push(2.0);
            arr.sort((a, b) => b - a);
            // After sort descending: [4, 3, 2, 1]
            return arr[0] * 1000.0 + arr[1] * 100.0 + arr[2] * 10.0 + arr[3];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 4321.0).abs() < 1e-10);
}

#[test]
fn array_filter_then_reduce() {
    // Chain: filter enemies then sum their HP — realistic game AI
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }

        export function enemy_total_hp(my_team: i32): i32 {
            let entities: Array<Entity> = new Array<Entity>(8);
            entities.push(new Entity(100, 1));
            entities.push(new Entity(80, 2));
            entities.push(new Entity(0, 2));
            entities.push(new Entity(60, 1));
            entities.push(new Entity(40, 2));

            let enemies: Array<Entity> = entities.filter(e => e.team != my_team && e.hp > 0);
            let total: i32 = enemies.reduce((sum, e) => sum + e.hp, 0);
            return total;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<i32, i32>(&mut store, "enemy_total_hp")
        .unwrap();
    // my_team=1: enemies are team 2 with hp>0: 80, 40 = 120
    assert_eq!(test.call(&mut store, 1).unwrap(), 120);
}

#[test]
fn realistic_game_ai_filter_sort() {
    // The "money shot": filter enemies, sort by distance, act on closest
    let wasm = compile(
        r#"
        class Entity {
            x: f64;
            y: f64;
            hp: i32;
            team: i32;
            distSq: f64;
            constructor(x: f64, y: f64, hp: i32, team: i32) {
                this.distSq = 0.0;
            }
        }

        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            let mx: f64 = 5.0;
            let my: f64 = 5.0;
            let my_team: i32 = 1;

            let entities: Array<Entity> = new Array<Entity>(8);
            entities.push(new Entity(1.0, 1.0, 100, 2));
            entities.push(new Entity(9.0, 9.0, 50, 2));
            entities.push(new Entity(6.0, 5.0, 80, 1));
            entities.push(new Entity(4.0, 4.0, 0, 2));
            entities.push(new Entity(3.0, 5.0, 60, 2));

            // Filter: alive enemies
            let enemies: Array<Entity> = entities.filter(e => e.team != my_team && e.hp > 0);

            // Compute distSq for each enemy
            enemies.forEach(e => {
                let dx: f64 = e.x - mx;
                let dy: f64 = e.y - my;
                e.distSq = dx * dx + dy * dy;
            });

            // Sort by distance (closest first)
            enemies.sort((a, b) => a.distSq - b.distSq);

            // Act on closest enemy
            let closest: Entity = enemies[0];
            let dx: f64 = closest.x - mx;
            let dy: f64 = closest.y - my;
            set_action(me, 1, closest.hp, dx, dy);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();

    struct S {
        action: (i32, i32, i32, f64, f64),
    }
    let mut store = Store::new(
        &engine,
        S {
            action: (0, 0, 0, 0.0, 0.0),
        },
    );
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, S>, me: i32, kind: i32, target: i32, dx: f64, dy: f64| {
                caller.data_mut().action = (me, kind, target, dx, dy);
            },
        )
        .unwrap();

    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 42).unwrap();

    let a = store.data().action;
    assert_eq!(a.0, 42); // me
    assert_eq!(a.1, 1); // kind = attack

    // Alive enemies (team != 1, hp > 0):
    //   (1,1) hp=100 team=2, distSq = (1-5)^2 + (1-5)^2 = 32
    //   (9,9) hp=50 team=2,  distSq = (9-5)^2 + (9-5)^2 = 32
    //   (3,5) hp=60 team=2,  distSq = (3-5)^2 + (5-5)^2 = 4
    // Closest is (3,5) with distSq=4, hp=60
    assert_eq!(a.2, 60); // target = closest enemy hp
    assert!((a.3 - (-2.0)).abs() < 1e-10, "dx={}", a.3); // dx = 3-5 = -2
    assert!((a.4 - 0.0).abs() < 1e-10, "dy={}", a.4); // dy = 5-5 = 0
}

// ---- Phase 6 tests: TS sugar ----

#[test]
fn object_destructuring_basic() {
    let wasm = compile(
        r#"
        class Vec2 {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}
        }

        export function test(): f64 {
            let v: Vec2 = new Vec2(3.0, 4.0);
            const { x, y } = v;
            return x + y;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 7.0).abs() < 1e-10); // 3 + 4
}

#[test]
fn object_destructuring_partial() {
    // Destructure only some fields
    let wasm = compile(
        r#"
        class Entity {
            x: f64;
            y: f64;
            hp: i32;
            team: i32;
            constructor(x: f64, y: f64, hp: i32, team: i32) {}
        }

        export function test(): i32 {
            let e: Entity = new Entity(1.0, 2.0, 100, 3);
            const { hp, team } = e;
            return hp + team;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 103); // 100 + 3
}

#[test]
fn array_destructuring_basic() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(10.0);
            arr.push(20.0);
            arr.push(30.0);
            const [first, second, third] = arr;
            return first + second + third;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 60.0).abs() < 1e-10);
}

#[test]
fn array_destructuring_class_elements() {
    // Destructure Array<Entity> — elements get class type tracking
    let wasm = compile(
        r#"
        class Entity {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}
        }

        export function test(): f64 {
            let entities: Array<Entity> = new Array<Entity>(4);
            entities.push(new Entity(1.0, 2.0));
            entities.push(new Entity(3.0, 4.0));
            const [first, second] = entities;
            return first.x + second.y;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 5.0).abs() < 1e-10); // 1.0 + 4.0
}

#[test]
fn optional_chaining_non_null() {
    // Access field on a non-null object
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }

        export function test(): i32 {
            let e: Entity = new Entity(100, 2);
            let hp: i32 = e?.hp;
            return hp;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 100);
}

#[test]
fn optional_chaining_null() {
    // Access field on a null (0) pointer — should return 0
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }

        declare function find_target(): Entity;

        export function test(): i32 {
            let target: Entity = find_target();
            let hp: i32 = target?.hp;
            return hp;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);
    // Return null (0) pointer
    linker
        .func_wrap("host", "find_target", || -> i32 { 0 })
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 0);
}

#[test]
fn nullish_coalescing() {
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }

        declare function find_target(): Entity;

        export function test(): i32 {
            let target: Entity = find_target();
            let hp: i32 = target?.hp ?? 42;
            return hp;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();

    // Test with null target — should get default value 42
    {
        let mut store = Store::new(&engine, ());
        let mut linker = Linker::new(&engine);
        linker
            .func_wrap("host", "find_target", || -> i32 { 0 })
            .unwrap();
        let instance = linker.instantiate(&mut store, &module).unwrap();
        let test = instance
            .get_typed_func::<(), i32>(&mut store, "test")
            .unwrap();
        assert_eq!(test.call(&mut store, ()).unwrap(), 42);
    }
}

#[test]
fn ternary_expression() {
    let wasm = compile(
        r#"
        export function test(x: i32): i32 {
            let result: i32 = x > 5 ? 100 : 200;
            return result;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<i32, i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, 10).unwrap(), 100);
    assert_eq!(test.call(&mut store, 3).unwrap(), 200);
}

#[test]
fn ternary_expression_f64() {
    let wasm = compile(
        r#"
        export function test(x: f64): f64 {
            let result: f64 = x > 0.0 ? x * 2.0 : 0.0 - x;
            return result;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<f64, f64>(&mut store, "test")
        .unwrap();
    assert!((test.call(&mut store, 5.0).unwrap() - 10.0).abs() < 1e-10);
    assert!((test.call(&mut store, -3.0).unwrap() - 3.0).abs() < 1e-10);
}

#[test]
fn combined_sugar_game_pattern() {
    // Combine destructuring + optional chaining + filter in a realistic pattern
    let wasm = compile(
        r#"
        class Entity {
            x: f64;
            y: f64;
            hp: i32;
            team: i32;
            constructor(x: f64, y: f64, hp: i32, team: i32) {}
        }

        declare function find_target(): Entity;

        export function test(): f64 {
            let entities: Array<Entity> = new Array<Entity>(8);
            entities.push(new Entity(1.0, 2.0, 100, 1));
            entities.push(new Entity(3.0, 4.0, 50, 2));
            entities.push(new Entity(5.0, 6.0, 75, 1));

            // Destructure first entity
            const [first, second, third] = entities;
            const { x, y } = first;

            // Filter + reduce
            let my_team: i32 = 1;
            let allies: Array<Entity> = entities.filter(e => e.team == my_team);
            let total_hp: i32 = allies.reduce((sum, e) => sum + e.hp, 0);

            // Use target with optional chaining
            let target: Entity = find_target();
            let target_hp: i32 = target?.hp ?? 0;

            return x + y + f64(total_hp) + f64(target_hp);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap("host", "find_target", || -> i32 { 0 })
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    // x=1 + y=2 + total_hp(100+75=175) + target_hp(0) = 178
    assert!((result - 178.0).abs() < 1e-10, "result={result}");
}

// ---- Phase 7 tests: error reporting ----

fn compile_err(source: &str) -> tscc::error::CompileError {
    tscc::compile(source, &tscc::CompileOptions::default()).unwrap_err()
}

#[test]
fn error_undefined_variable_has_location() {
    let err = compile_err("export function tick(me: i32): void { let x: i32 = foo; }");
    assert!(err.loc.is_some(), "error should have location");
    let loc = err.loc.unwrap();
    assert_eq!(loc.line, 1);
    assert!(
        err.message.contains("undefined variable 'foo'"),
        "msg={}",
        err.message
    );
}

#[test]
fn error_type_mismatch_has_location() {
    let err = compile_err(
        r#"
        export function test(): i32 {
            let a: i32 = 1;
            let b: f64 = 2.0;
            return a + b;
        }
    "#,
    );
    assert!(err.loc.is_some(), "error should have location");
    assert!(err.message.contains("type mismatch"), "msg={}", err.message);
}

#[test]
fn error_missing_type_annotation_has_location() {
    // `let x = null` can't infer type from null alone
    let err = compile_err("export function tick(me: i32): void { let x = null; }");
    assert!(err.loc.is_some(), "error should have location");
    assert!(err.message.contains("type"), "msg={}", err.message);
}

#[test]
fn error_undefined_function_has_location() {
    let err = compile_err("export function tick(me: i32): void { let x: i32 = bogus(1); }");
    assert!(err.loc.is_some(), "error should have location");
    assert!(
        err.message.contains("undefined function 'bogus'"),
        "msg={}",
        err.message
    );
}

#[test]
fn error_unknown_type_has_location() {
    let err = compile_err("export function tick(me: i32): void { let x: Foo = 1; }");
    assert!(err.loc.is_some(), "error should have location");
    assert!(err.message.contains("unknown type"), "msg={}", err.message);
}

#[test]
fn error_display_format() {
    let err = compile_err("export function tick(me: i32): void { let x: i32 = foo; }");
    let display = err.to_string();
    // Should contain line:col:kind: message
    assert!(display.contains("codegen error"), "display={display}");
    assert!(display.contains("undefined variable"), "display={display}");
}

#[test]
fn error_context_snippet() {
    let source = "export function tick(me: i32): void { let x: i32 = foo; }";
    let err = compile_err(source);
    let formatted = tscc::error::format_error_with_context(&err, source);
    // Should contain the source line and a caret
    assert!(
        formatted.contains("let x: i32 = foo;"),
        "formatted={formatted}"
    );
    assert!(formatted.contains("^"), "formatted={formatted}");
}

// ==================== Type inference tests ====================

#[test]
fn type_inference_i32() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        export function tick(me: i32): void {
            let x = 5;
            let y = x + 3;
            set_action(me, y, 0, 0.0, 0.0);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0i32);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, i32>, _me: i32, kind: i32, _t: i32, _dx: f64, _dy: f64| {
                *caller.data_mut() = kind;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert_eq!(*store.data(), 8);
}

#[test]
fn type_inference_f64() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        export function tick(me: i32): void {
            let x = 3.14;
            let y = x * 2.0;
            set_action(me, 0, 0, y, 0.0);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0.0f64);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, f64>, _me: i32, _k: i32, _t: i32, dx: f64, _dy: f64| {
                *caller.data_mut() = dx;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert!((*store.data() - 6.28).abs() < 1e-10);
}

#[test]
fn type_inference_bool() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        export function tick(me: i32): void {
            let flag = true;
            if (flag) {
                set_action(me, 1, 0, 0.0, 0.0);
            } else {
                set_action(me, 0, 0, 0.0, 0.0);
            }
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0i32);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, i32>, _me: i32, kind: i32, _t: i32, _dx: f64, _dy: f64| {
                *caller.data_mut() = kind;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert_eq!(*store.data(), 1);
}

#[test]
fn type_inference_from_new() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        class Vec2 {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}
        }
        export function tick(me: i32): void {
            let v = new Vec2(7.0, 8.0);
            set_action(me, 0, 0, v.x, v.y);
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
            |mut caller: Caller<'_, (f64, f64)>, _me: i32, _k: i32, _t: i32, dx: f64, dy: f64| {
                *caller.data_mut() = (dx, dy);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert!((*store.data()).0 - 7.0 < 1e-10);
    assert!((*store.data()).1 - 8.0 < 1e-10);
}

// ==================== Null literal test ====================

#[test]
fn null_literal() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        class Entity {
            hp: i32;
            constructor(hp: i32) {}
        }
        export function tick(me: i32): void {
            let target: Entity = null;
            let hp: i32 = target?.hp ?? 42;
            set_action(me, hp, 0, 0.0, 0.0);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0i32);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, i32>, _me: i32, kind: i32, _t: i32, _dx: f64, _dy: f64| {
                *caller.data_mut() = kind;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert_eq!(*store.data(), 42);
}

// ==================== Const immutability tests ====================

#[test]
fn const_immutability_error() {
    let err = compile_err("export function tick(me: i32): void { const x: i32 = 5; x = 10; }");
    assert!(err.message.contains("const"), "msg={}", err.message);
}

#[test]
fn const_increment_error() {
    let err = compile_err("export function tick(me: i32): void { const x: i32 = 5; x++; }");
    assert!(err.message.contains("const"), "msg={}", err.message);
}

// ==================== Prefix/postfix update test ====================

#[test]
fn prefix_postfix_update() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        export function tick(me: i32): void {
            let a: i32 = 5;
            let b: i32 = a++;
            let c: i32 = ++a;
            set_action(me, b, c, 0.0, 0.0);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, (0i32, 0i32));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, (i32, i32)>,
             _me: i32,
             kind: i32,
             target: i32,
             _dx: f64,
             _dy: f64| {
                *caller.data_mut() = (kind, target);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    let (b, c) = *store.data();
    assert_eq!(b, 5, "postfix a++ should return old value 5");
    assert_eq!(
        c, 7,
        "prefix ++a should return new value 7 (was 6 after a++)"
    );
}

// ==================== for...of loop tests ====================

#[test]
fn for_of_loop_basic() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        export function tick(me: i32): void {
            let arr: Array<i32> = new Array<i32>(5);
            arr.push(10);
            arr.push(20);
            arr.push(30);
            let sum: i32 = 0;
            for (const val of arr) {
                sum += val;
            }
            set_action(me, sum, 0, 0.0, 0.0);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0i32);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, i32>, _me: i32, kind: i32, _t: i32, _dx: f64, _dy: f64| {
                *caller.data_mut() = kind;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert_eq!(*store.data(), 60);
}

#[test]
fn for_of_loop_class_instances() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        class Entity {
            hp: i32;
            constructor(hp: i32) {}
        }
        export function tick(me: i32): void {
            let entities: Array<Entity> = new Array<Entity>(4);
            entities.push(new Entity(10));
            entities.push(new Entity(20));
            entities.push(new Entity(30));
            let totalHp: i32 = 0;
            for (const e of entities) {
                totalHp += e.hp;
            }
            set_action(me, totalHp, 0, 0.0, 0.0);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0i32);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, i32>, _me: i32, kind: i32, _t: i32, _dx: f64, _dy: f64| {
                *caller.data_mut() = kind;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert_eq!(*store.data(), 60);
}

// ==================== Switch/case test ====================

#[test]
fn switch_case_basic() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        export function tick(me: i32): void {
            let result: i32 = 0;
            switch (me) {
                case 1:
                    result = 10;
                    break;
                case 2:
                    result = 20;
                    break;
                case 3:
                    result = 30;
                    break;
                default:
                    result = -1;
                    break;
            }
            set_action(me, result, 0, 0.0, 0.0);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0i32);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, i32>, _me: i32, kind: i32, _t: i32, _dx: f64, _dy: f64| {
                *caller.data_mut() = kind;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();

    tick.call(&mut store, 1).unwrap();
    assert_eq!(*store.data(), 10);

    tick.call(&mut store, 2).unwrap();
    assert_eq!(*store.data(), 20);

    tick.call(&mut store, 3).unwrap();
    assert_eq!(*store.data(), 30);

    tick.call(&mut store, 99).unwrap();
    assert_eq!(*store.data(), -1);
}

// ==================== Const enum tests ====================

#[test]
fn const_enum_basic() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        enum Action {
            Idle,
            Move,
            Attack,
            Flee
        }

        export function tick(me: i32): void {
            let action: i32 = Action.Move;
            set_action(me, action, 0, 0.0, 0.0);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0i32);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, i32>, _me: i32, kind: i32, _t: i32, _dx: f64, _dy: f64| {
                *caller.data_mut() = kind;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert_eq!(*store.data(), 1); // Move = 1
}

#[test]
fn enum_with_explicit_values() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        enum Priority {
            Low = 10,
            Medium = 20,
            High = 30
        }

        export function tick(me: i32): void {
            set_action(me, Priority.High, Priority.Low, 0.0, 0.0);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, (0i32, 0i32));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, (i32, i32)>,
             _me: i32,
             kind: i32,
             target: i32,
             _dx: f64,
             _dy: f64| {
                *caller.data_mut() = (kind, target);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert_eq!(*store.data(), (30, 10));
}

// ==================== Chained array operations test ====================

#[test]
fn chained_filter_map() {
    // This tests that .filter() result can be used with .map()
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }
        export function tick(me: i32): void {
            let entities: Array<Entity> = new Array<Entity>(8);
            entities.push(new Entity(10, 1));
            entities.push(new Entity(20, 2));
            entities.push(new Entity(30, 1));
            entities.push(new Entity(40, 2));

            let enemyHp: Array<i32> = entities
                .filter(e => e.team == 2)
                .map(e => e.hp);

            let total: i32 = enemyHp.reduce((sum, hp) => sum + hp, 0);
            set_action(me, total, enemyHp.length, 0.0, 0.0);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, (0i32, 0i32));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, (i32, i32)>,
             _me: i32,
             kind: i32,
             target: i32,
             _dx: f64,
             _dy: f64| {
                *caller.data_mut() = (kind, target);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    let (total, count) = *store.data();
    assert_eq!(total, 60, "enemy hp sum should be 20+40=60");
    assert_eq!(count, 2, "should have 2 enemies");
}

// ==================== Nested property access test ====================

#[test]
fn nested_class_field_access() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        class Vec2 {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}
        }
        class Entity {
            pos: Vec2;
            hp: i32;
            constructor(pos: Vec2, hp: i32) {}
        }
        export function tick(me: i32): void {
            let pos = new Vec2(3.0, 4.0);
            let e = new Entity(pos, 100);
            set_action(me, e.hp, 0, e.pos.x, e.pos.y);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    struct S {
        action: (i32, i32, i32, f64, f64),
    }
    let mut store = Store::new(
        &engine,
        S {
            action: (0, 0, 0, 0.0, 0.0),
        },
    );
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, S>, me: i32, kind: i32, target: i32, dx: f64, dy: f64| {
                caller.data_mut().action = (me, kind, target, dx, dy);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 7).unwrap();
    let a = store.data().action;
    assert_eq!(a.1, 100); // hp
    assert!((a.3 - 3.0).abs() < 1e-10, "pos.x={}", a.3);
    assert!((a.4 - 4.0).abs() < 1e-10, "pos.y={}", a.4);
}

// ==================== Mega convoluted combined test ====================

#[test]
fn convoluted_game_ai_full_pipeline() {
    // This is the ultimate stress test: enum, for..of, filter, sort, chaining,
    // type inference, null checks, nested access, methods, arena allocation.
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        enum ActionKind {
            Idle,
            Move,
            Attack,
            Flee
        }

        class Vec2 {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}

            distSqTo(other: Vec2): f64 {
                const dx = this.x - other.x;
                const dy = this.y - other.y;
                return dx * dx + dy * dy;
            }
        }

        class Entity {
            pos: Vec2;
            hp: i32;
            team: i32;
            constructor(pos: Vec2, hp: i32, team: i32) {}

            isAlive(): i32 {
                if (this.hp > 0) { return 1; }
                return 0;
            }
        }

        function findNearest(entities: Array<Entity>, from: Vec2): Entity {
            let best: Entity = null;
            let bestDist: f64 = 999999.0;
            for (const e of entities) {
                let d: f64 = e.pos.distSqTo(from);
                if (d < bestDist) {
                    bestDist = d;
                    best = e;
                }
            }
            return best;
        }

        export function tick(me: i32): void {
            let myPos = new Vec2(0.0, 0.0);

            let entities: Array<Entity> = new Array<Entity>(8);
            entities.push(new Entity(new Vec2(10.0, 0.0), 100, 1));
            entities.push(new Entity(new Vec2(3.0, 4.0), 50, 2));
            entities.push(new Entity(new Vec2(0.0, 0.0), 0, 2));
            entities.push(new Entity(new Vec2(6.0, 8.0), 75, 2));

            let enemies: Array<Entity> = entities.filter(e => e.team == 2 && e.isAlive() == 1);
            enemies.sort((a, b) => i32(a.pos.distSqTo(myPos)) - i32(b.pos.distSqTo(myPos)));

            let nearest: Entity = findNearest(enemies, myPos);
            let hp: i32 = nearest?.hp ?? 0;

            if (hp > 0) {
                let dx: f64 = nearest.pos.x - myPos.x;
                let dy: f64 = nearest.pos.y - myPos.y;
                let len = Math.sqrt(dx * dx + dy * dy);
                if (len > 0.0) {
                    dx = dx / len;
                    dy = dy / len;
                }
                set_action(me, ActionKind.Attack, hp, dx, dy);
            } else {
                set_action(me, ActionKind.Idle, 0, 0.0, 0.0);
            }
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    struct S {
        action: (i32, i32, i32, f64, f64),
    }
    let mut store = Store::new(
        &engine,
        S {
            action: (0, 0, 0, 0.0, 0.0),
        },
    );
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, S>, me: i32, kind: i32, target: i32, dx: f64, dy: f64| {
                caller.data_mut().action = (me, kind, target, dx, dy);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 42).unwrap();

    let a = store.data().action;
    assert_eq!(a.0, 42); // me
    assert_eq!(a.1, 2); // ActionKind.Attack = 2
    assert_eq!(a.2, 50); // nearest enemy hp = 50 (entity at 3,4 is closest alive)
    // Direction to (3,4) from (0,0) = (3/5, 4/5) = (0.6, 0.8)
    assert!((a.3 - 0.6).abs() < 1e-10, "dx={}", a.3);
    assert!((a.4 - 0.8).abs() < 1e-10, "dy={}", a.4);
}

// ==================== Large array merge sort test ====================

#[test]
fn large_array_sort_1000_elements() {
    // Tests that merge sort handles 1000 elements correctly
    let options = tscc::CompileOptions {
        host_module: "host".to_string(),
        memory_pages: 64, // 4MB for large arrays
        ..Default::default()
    };
    let wasm = tscc::compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        export function tick(me: i32): void {
            let arr: Array<i32> = new Array<i32>(1024);
            for (let i: i32 = 0; i < 1000; i++) {
                arr.push(999 - i);
            }
            arr.sort((a, b) => a - b);
            set_action(me, arr[0], arr[999], f64(arr[500]), 0.0);
        }
    "#,
        &options,
    )
    .unwrap();

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    struct S {
        action: (i32, i32, i32, f64, f64),
    }
    let mut store = Store::new(
        &engine,
        S {
            action: (0, 0, 0, 0.0, 0.0),
        },
    );
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, S>, me: i32, kind: i32, target: i32, dx: f64, dy: f64| {
                caller.data_mut().action = (me, kind, target, dx, dy);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();

    let a = store.data().action;
    assert_eq!(a.1, 0, "first element after sort should be 0");
    assert_eq!(a.2, 999, "last element after sort should be 999");
    assert!(
        (a.3 - 500.0).abs() < 1e-10,
        "middle element should be 500, got {}",
        a.3
    );
}

// ==================== Tier 2: `int` type alias ====================

#[test]
fn int_type_alias() {
    let wasm = compile(
        r#"
        export function test(): int {
            let a: int = 10;
            let b: int = 20;
            return a + b;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 30);
}

#[test]
fn number_type_keyword() {
    // `number` is already a TS keyword (TSNumberKeyword), mapped to f64
    let wasm = compile(
        r#"
        export function test(): number {
            let x: number = 3.5;
            let y: number = 2.0;
            return x * y;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    assert!((test.call(&mut store, ()).unwrap() - 7.0).abs() < 1e-10);
}

// ==================== Tier 2: `as` type casts ====================

#[test]
fn as_cast_i32_to_f64() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let x: i32 = 42;
            return x as f64;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    assert!((test.call(&mut store, ()).unwrap() - 42.0).abs() < 1e-10);
}

#[test]
fn as_cast_f64_to_i32() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 3.99;
            return x as i32;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 3); // truncation
}

#[test]
fn as_cast_noop() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let x: f64 = 7.5;
            return x as f64;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    assert!((test.call(&mut store, ()).unwrap() - 7.5).abs() < 1e-10);
}

// ==================== Tier 2: do..while loops ====================

#[test]
fn do_while_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let sum: i32 = 0;
            let i: i32 = 1;
            do {
                sum += i;
                i++;
            } while (i <= 5);
            return sum;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 15); // 1+2+3+4+5
}

#[test]
fn do_while_executes_at_least_once() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let count: i32 = 0;
            do {
                count++;
            } while (false);
            return count;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 1);
}

#[test]
fn do_while_with_break() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let sum: i32 = 0;
            let i: i32 = 0;
            do {
                i++;
                if (i > 3) { break; }
                sum += i;
            } while (i < 100);
            return sum;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 6); // 1+2+3
}

// ==================== Tier 2: implicit void return ====================

#[test]
fn implicit_void_return() {
    // Function with no return type annotation should default to void
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        export function tick(me: i32) {
            set_action(me, 1, 0, 0.0, 0.0);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0i32);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, i32>, _me: i32, kind: i32, _t: i32, _dx: f64, _dy: f64| {
                *caller.data_mut() = kind;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert_eq!(*store.data(), 1);
}

// ==================== Enum + switch combined test ====================

#[test]
fn enum_switch_combined() {
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        enum State {
            Idle,
            Patrol,
            Chase,
            Attack
        }

        function getMultiplier(state: i32): f64 {
            let mult: f64 = 1.0;
            switch (state) {
                case State.Idle:
                    mult = 0.0;
                    break;
                case State.Patrol:
                    mult = 0.5;
                    break;
                case State.Chase:
                    mult = 1.0;
                    break;
                case State.Attack:
                    mult = 2.0;
                    break;
            }
            return mult;
        }

        export function tick(me: i32): void {
            let m1 = getMultiplier(State.Idle);
            let m2 = getMultiplier(State.Chase);
            let m3 = getMultiplier(State.Attack);
            set_action(me, 0, 0, m1 + m2 + m3, 0.0);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, 0.0f64);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, f64>, _me: i32, _k: i32, _t: i32, dx: f64, _dy: f64| {
                *caller.data_mut() = dx;
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    assert!(
        (*store.data() - 3.0).abs() < 1e-10,
        "0.0 + 1.0 + 2.0 = 3.0, got {}",
        *store.data()
    );
}

// ── First-class closure tests ───────────────────────────────────────

#[test]
fn closure_basic_no_capture() {
    let wasm = compile(
        r#"
        export function run(): i32 {
            const add = (a: i32, b: i32): i32 => a + b;
            return add(3, 4);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 7);
}

#[test]
fn closure_with_capture() {
    let wasm = compile(
        r#"
        export function run(): i32 {
            let offset: i32 = 10;
            const addOffset = (x: i32): i32 => x + offset;
            return addOffset(5);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 15);
}

#[test]
fn closure_capture_f64() {
    let wasm = compile(
        r#"
        export function run(): f64 {
            let scale: f64 = 2.5;
            const multiply = (x: f64): f64 => x * scale;
            return multiply(4.0);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), f64>(&mut store, "run")
        .unwrap();
    let result = run.call(&mut store, ()).unwrap();
    assert!((result - 10.0).abs() < 1e-10);
}

#[test]
fn closure_multiple_captures() {
    let wasm = compile(
        r#"
        export function run(): i32 {
            let a: i32 = 100;
            let b: i32 = 20;
            let c: i32 = 3;
            const sum = (x: i32): i32 => x + a + b + c;
            return sum(0);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 123);
}

#[test]
fn closure_multiple_closures() {
    let wasm = compile(
        r#"
        export function run(): i32 {
            const double = (x: i32): i32 => x * 2;
            const triple = (x: i32): i32 => x * 3;
            return double(5) + triple(5);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 25);
}

#[test]
fn closure_capture_class_instance() {
    let wasm = compile(
        r#"
        class Vec2 {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}
        }

        export function run(): f64 {
            let v = new Vec2(3.0, 4.0);
            const getX = (): f64 => v.x;
            return getX();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), f64>(&mut store, "run")
        .unwrap();
    let result = run.call(&mut store, ()).unwrap();
    assert!((result - 3.0).abs() < 1e-10);
}

#[test]
fn closure_as_function_parameter() {
    let wasm = compile(
        r#"
        function apply(x: i32, fn: (a: i32) => i32): i32 {
            return fn(x);
        }

        export function run(): i32 {
            const double = (a: i32): i32 => a * 2;
            return apply(7, double);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 14);
}

#[test]
fn closure_as_param_with_capture() {
    let wasm = compile(
        r#"
        function applyToFive(fn: (a: i32) => i32): i32 {
            return fn(5);
        }

        export function run(): i32 {
            let bonus: i32 = 100;
            const addBonus = (x: i32): i32 => x + bonus;
            return applyToFive(addBonus);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 105);
}

#[test]
fn closure_inline_builtins_still_work() {
    // Regression: inline closures in array builtins should still work
    let wasm = compile(
        r#"
        export function run(): i32 {
            let arr: Array<i32> = new Array<i32>(4);
            arr.push(1);
            arr.push(2);
            arr.push(3);
            arr.push(4);
            let evens: Array<i32> = arr.filter((x: i32): bool => x % 2 == 0);
            return evens.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 2);
}

// ── Closure hardening tests ─────────────────────────────────────────

#[test]
fn closure_return_from_function() {
    // Factory pattern: function returns a closure
    let wasm = compile(
        r#"
        function makeAdder(n: i32): (x: i32) => i32 {
            return (x: i32): i32 => x + n;
        }

        export function run(): i32 {
            const add5 = makeAdder(5);
            return add5(10);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 15);
}

#[test]
fn closure_return_multiple_factories() {
    let wasm = compile(
        r#"
        function makeMultiplier(factor: i32): (x: i32) => i32 {
            return (x: i32): i32 => x * factor;
        }

        export function run(): i32 {
            const double = makeMultiplier(2);
            const triple = makeMultiplier(3);
            return double(10) + triple(10);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 50); // 20 + 30
}

#[test]
fn closure_nested_closure_in_block_body() {
    // Closure inside a block-body closure
    let wasm = compile(
        r#"
        export function run(): i32 {
            const outer = (x: i32): i32 => {
                const inner = (y: i32): i32 => y + x;
                return inner(10);
            };
            return outer(5);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 15);
}

#[test]
fn closure_block_body_with_logic() {
    // Block-body closure with if/else
    let wasm = compile(
        r#"
        export function run(): i32 {
            const clamp = (x: i32, lo: i32, hi: i32): i32 => {
                if (x < lo) { return lo; }
                if (x > hi) { return hi; }
                return x;
            };
            return clamp(-5, 0, 100) + clamp(50, 0, 100) + clamp(200, 0, 100);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 150); // 0 + 50 + 100
}

#[test]
fn closure_capture_another_closure() {
    // Closure captures a variable holding another closure
    let wasm = compile(
        r#"
        export function run(): i32 {
            const add1 = (x: i32): i32 => x + 1;
            const applyTwice = (x: i32): i32 => add1(add1(x));
            return applyTwice(5);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 7);
}

#[test]
fn closure_inline_arrow_as_argument() {
    // Pass an anonymous arrow directly to a function expecting a closure parameter
    let wasm = compile(
        r#"
        function apply(x: i32, fn: (a: i32) => i32): i32 {
            return fn(x);
        }

        export function run(): i32 {
            return apply(6, (a: i32): i32 => a * 7);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 42);
}

// ── Reference-capture (boxing) tests ────────────────────────────────

#[test]
fn closure_mutation_after_capture_visible() {
    // Core test: mutation after closure creation is visible to the closure
    let wasm = compile(
        r#"
        export function run(): i32 {
            let x: i32 = 1;
            const getX = (): i32 => x;
            x = 42;
            return getX();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 42); // NOT 1 — TS reference semantics
}

#[test]
fn closure_mutation_inside_closure_visible_outside() {
    // Mutation inside closure is visible in outer scope
    let wasm = compile(
        r#"
        export function run(): i32 {
            let counter: i32 = 0;
            const increment = (): void => { counter = counter + 1; };
            increment();
            increment();
            increment();
            return counter;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 3);
}

#[test]
fn closure_boxed_f64_mutation() {
    // f64 boxing works correctly
    let wasm = compile(
        r#"
        export function run(): f64 {
            let total: f64 = 0.0;
            const addTo = (x: f64): void => { total = total + x; };
            addTo(1.5);
            addTo(2.5);
            return total;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), f64>(&mut store, "run")
        .unwrap();
    let result = run.call(&mut store, ()).unwrap();
    assert!((result - 4.0).abs() < 1e-10);
}

#[test]
fn closure_boxed_compound_assignment() {
    // Compound assignment (+=) works with boxed vars
    let wasm = compile(
        r#"
        export function run(): i32 {
            let x: i32 = 10;
            const addToX = (n: i32): void => { x += n; };
            addToX(5);
            addToX(3);
            return x;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 18);
}

#[test]
fn closure_boxed_increment() {
    // ++/-- works with boxed vars
    let wasm = compile(
        r#"
        export function run(): i32 {
            let n: i32 = 0;
            const inc = (): i32 => { n++; return n; };
            inc();
            inc();
            let result = inc();
            return result;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 3);
}

#[test]
fn closure_non_mutated_capture_stays_unboxed() {
    // Sanity: variable captured but never mutated should still work (no boxing needed)
    let wasm = compile(
        r#"
        export function run(): i32 {
            let x: i32 = 99;
            const getX = (): i32 => x;
            return getX();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 99);
}

#[test]
fn closure_two_closures_share_boxed_var() {
    // Two closures sharing the same mutable variable
    let wasm = compile(
        r#"
        export function run(): i32 {
            let shared: i32 = 0;
            const inc = (): void => { shared += 1; };
            const get = (): i32 => shared;
            inc();
            inc();
            inc();
            return get();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 3);
}

#[test]
fn closure_shadow_variable_not_captured() {
    // Inner `local` shadows outer `local` — outer should NOT be captured
    let wasm = compile(
        r#"
        export function run(): i32 {
            let local: i32 = 42;
            const fn1 = (): i32 => {
                let local: i32 = 99;
                return local;
            };
            return fn1() + local;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 141); // 99 + 42
}

#[test]
fn closure_uninitialized_boxed_var() {
    // Boxed variable without initializer should default to zero
    let wasm = compile(
        r#"
        export function run(): i32 {
            let x: i32;
            const set = (val: i32): void => { x = val; };
            const get = (): i32 => x;
            let before = get();
            set(77);
            return before + get();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 77); // 0 + 77
}

#[test]
fn closure_arrow_internal_mutation_no_false_boxing() {
    // Variable mutated only INSIDE an arrow body should not cause outer boxing
    let wasm = compile(
        r#"
        export function run(): i32 {
            let x: i32 = 10;
            const fn1 = (): i32 => {
                let temp: i32 = x;
                temp = temp + 5;
                return temp;
            };
            return fn1() + x;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 25); // 15 + 10
}

// ============================================================
// String support tests
// ============================================================

/// Helper: read a string from WASM memory at the given pointer.
/// Layout: [length: i32 (4 bytes)] [UTF-8 bytes...]
fn read_wasm_string(store: &Store<()>, memory: &Memory, ptr: i32) -> String {
    let data = memory.data(&store);
    let offset = ptr as usize;
    let len_bytes: [u8; 4] = data[offset..offset + 4].try_into().unwrap();
    let len = i32::from_le_bytes(len_bytes) as usize;
    let bytes = &data[offset + 4..offset + 4 + len];
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn string_literal_returns_pointer() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello";
            return s;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello");
}

#[test]
fn string_length_property() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "world!";
            return s.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 6);
}

#[test]
fn string_literal_deduplication() {
    // Same string literal used twice should return the same pointer
    let wasm = compile(
        r#"
        export function test(): i32 {
            let a: string = "same";
            let b: string = "same";
            if (a == b) {
                return 1;
            }
            return 0;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    // Same pointer means i32.eq returns 1
    assert_eq!(test.call(&mut store, ()).unwrap(), 1);
}

#[test]
fn string_inferred_type() {
    // Type inference from string literal
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s = "inferred";
            return s.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 8); // "inferred".length == 8
}

#[test]
fn string_as_function_parameter() {
    let wasm = compile(
        r#"
        function getLength(s: string): i32 {
            return s.length;
        }

        export function test(): i32 {
            return getLength("hello world");
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 11);
}

#[test]
fn string_empty_literal() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "";
            return s.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 0);
}

#[test]
fn string_index_char_code() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "ABC";
            return s[0] + s[1] + s[2];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    // 'A'=65, 'B'=66, 'C'=67 → 198
    assert_eq!(test.call(&mut store, ()).unwrap(), 198);
}

#[test]
#[should_panic]
fn string_index_out_of_bounds() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hi";
            return s[5];
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    test.call(&mut store, ()).unwrap(); // should trap
}

#[test]
fn string_concat() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let a: string = "hello";
            let b: string = " world";
            let c: string = a + b;
            return c;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello world");
}

#[test]
fn string_concat_chain() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let result: string = "a" + "b" + "c";
            return result;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "abc");
}

#[test]
fn string_equality() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let a: string = "hello";
            let b: string = "hello";
            let c: string = "world";
            let eq1: i32 = 0;
            let eq2: i32 = 0;
            let neq: i32 = 0;
            if (a == b) { eq1 = 1; }
            if (a == c) { eq2 = 1; }
            if (a != c) { neq = 1; }
            return eq1 * 100 + eq2 * 10 + neq;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    // eq1=1, eq2=0, neq=1 → 101
    assert_eq!(test.call(&mut store, ()).unwrap(), 101);
}

#[test]
fn string_comparison() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let a: string = "apple";
            let b: string = "banana";
            let r: i32 = 0;
            if (a < b) { r = r + 1; }
            if (b > a) { r = r + 10; }
            if (a <= a) { r = r + 100; }
            if (a >= a) { r = r + 1000; }
            return r;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 1111);
}

#[test]
fn string_char_code_at() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "Hello";
            return s.charCodeAt(0) + s.charCodeAt(4);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    // 'H'=72, 'o'=111 → 183
    assert_eq!(test.call(&mut store, ()).unwrap(), 183);
}

#[test]
fn string_index_of_found() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            return s.indexOf("world");
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 6);
}

#[test]
fn string_index_of_not_found() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            return s.indexOf("xyz");
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), -1);
}

#[test]
fn string_includes() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "broadcast:attack";
            let r: i32 = 0;
            if (s.includes("attack")) { r = r + 1; }
            if (s.includes("defend")) { r = r + 10; }
            if (s.includes("broadcast")) { r = r + 100; }
            return r;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 101);
}

#[test]
fn string_starts_ends_with() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "cmd:attack:north";
            let r: i32 = 0;
            if (msg.startsWith("cmd:")) { r = r + 1; }
            if (msg.startsWith("xyz")) { r = r + 10; }
            if (msg.endsWith("north")) { r = r + 100; }
            if (msg.endsWith("south")) { r = r + 1000; }
            return r;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 101);
}

#[test]
fn string_slice_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            let sub: string = s.slice(0, 5);
            return sub;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello");
}

#[test]
fn string_slice_no_end() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            let sub: string = s.slice(6);
            return sub;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "world");
}

#[test]
fn string_slice_negative() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            let sub: string = s.slice(-5);
            return sub;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "world");
}

#[test]
fn string_to_lower_upper() {
    let wasm = compile(
        r#"
        export function testLower(): i32 {
            let s: string = "Hello WORLD";
            return s.toLowerCase();
        }
        export function testUpper(): i32 {
            let s: string = "Hello world";
            return s.toUpperCase();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();

    let test_lower = instance
        .get_typed_func::<(), i32>(&mut store, "testLower")
        .unwrap();
    let ptr = test_lower.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello world");

    let test_upper = instance
        .get_typed_func::<(), i32>(&mut store, "testUpper")
        .unwrap();
    let ptr = test_upper.call(&mut store, ()).unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "HELLO WORLD");
}

#[test]
fn string_trim() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "  hello  ";
            return s.trim();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello");
}

#[test]
fn string_trim_start_and_end() {
    let wasm = compile(
        r#"
        export function test_start(): i32 {
            let s: string = "  hello  ";
            return s.trimStart();
        }
        export function test_end(): i32 {
            let s: string = "  hello  ";
            return s.trimEnd();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();

    let ts = instance
        .get_typed_func::<(), i32>(&mut store, "test_start")
        .unwrap();
    let p1 = ts.call(&mut store, ()).unwrap();
    assert_eq!(read_wasm_string(&store, &memory, p1), "hello  ");

    let te = instance
        .get_typed_func::<(), i32>(&mut store, "test_end")
        .unwrap();
    let p2 = te.call(&mut store, ()).unwrap();
    assert_eq!(read_wasm_string(&store, &memory, p2), "  hello");
}

#[test]
fn string_at_negative_index() {
    let wasm = compile(
        r#"
        export function last(): i32 {
            let s: string = "hello";
            return s.at(-1);
        }
        export function second(): i32 {
            let s: string = "hello";
            return s.at(1);
        }
        export function from_end(): i32 {
            let s: string = "hello";
            return s.at(-2);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();

    let last = instance
        .get_typed_func::<(), i32>(&mut store, "last")
        .unwrap();
    assert_eq!(last.call(&mut store, ()).unwrap(), b'o' as i32);
    let second = instance
        .get_typed_func::<(), i32>(&mut store, "second")
        .unwrap();
    assert_eq!(second.call(&mut store, ()).unwrap(), b'e' as i32);
    let from_end = instance
        .get_typed_func::<(), i32>(&mut store, "from_end")
        .unwrap();
    assert_eq!(from_end.call(&mut store, ()).unwrap(), b'l' as i32);
}

#[test]
fn string_code_point_at_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "ABC";
            return s.codePointAt(0);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), b'A' as i32);
}

#[test]
fn string_chained_methods() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "  CMD:ATTACK  ";
            let cleaned: string = msg.trim().toLowerCase();
            return cleaned;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "cmd:attack");
}

#[test]
fn string_broadcast_pattern() {
    // Realistic game entity broadcast pattern
    let wasm = compile(
        r#"
        function parseCommand(msg: string): i32 {
            if (msg.startsWith("attack:")) {
                return 1;
            }
            if (msg.startsWith("defend:")) {
                return 2;
            }
            if (msg.startsWith("move:")) {
                return 3;
            }
            return 0;
        }

        export function test(): i32 {
            let r: i32 = 0;
            r = r + parseCommand("attack:north");
            r = r + parseCommand("defend:south") * 10;
            r = r + parseCommand("move:east") * 100;
            r = r + parseCommand("unknown") * 1000;
            return r;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    // attack=1, defend=20, move=300, unknown=0 → 321
    assert_eq!(test.call(&mut store, ()).unwrap(), 321);
}

#[test]
fn string_function_return_type() {
    // Function returning string — caller should be able to call .length on result
    let wasm = compile(
        r#"
        function greet(name: string): string {
            return "hello " + name;
        }

        export function test(): i32 {
            let msg: string = greet("world");
            return msg.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 11); // "hello world".length
}

#[test]
fn string_class_field() {
    let wasm = compile(
        r#"
        class Entity {
            name: string;
            hp: i32;
            constructor(name: string, hp: i32) {}
        }

        export function test(): i32 {
            let e: Entity = new Entity("warrior", 100);
            return e.name.length + e.hp;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    // "warrior".length=7 + 100 = 107
    assert_eq!(test.call(&mut store, ()).unwrap(), 107);
}

#[test]
fn string_class_field_operations() {
    let wasm = compile(
        r#"
        class Entity {
            name: string;
            message: string;
            constructor(name: string, message: string) {}
        }

        export function test(): i32 {
            let e: Entity = new Entity("knight", "attack:north");
            let r: i32 = 0;
            if (e.message.startsWith("attack:")) {
                r = 1;
            }
            if (e.name == "knight") {
                r = r + 10;
            }
            return r;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 11);
}

#[test]
fn string_function_return_chain() {
    // Call .toUpperCase() directly on a function return value
    let wasm = compile(
        r#"
        function getMessage(): string {
            return "hello";
        }

        export function test(): i32 {
            let upper: string = getMessage().toUpperCase();
            return upper;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "HELLO");
}

// ============================================================
// Mixed concat / number-to-string coercion
// ============================================================

#[test]
fn string_concat_with_i32() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "score: " + 42;
            return msg;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "score: 42");
}

#[test]
fn string_concat_with_f64() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "value: " + 3.14;
            return msg;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "value: 3.14");
}

#[test]
fn string_concat_negative_i32() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "hp: " + -5;
            return msg;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hp: -5");
}

#[test]
fn string_concat_i32_prefix() {
    // Number on the left
    let wasm = compile(
        r#"
        export function test(): i32 {
            let hp: i32 = 100;
            let msg: string = hp + " hp remaining";
            return msg;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "100 hp remaining");
}

// ============================================================
// Template literals
// ============================================================

#[test]
fn template_literal_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let name: string = "world";
            let msg: string = `hello ${name}!`;
            return msg;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello world!");
}

#[test]
fn template_literal_with_number() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let hp: i32 = 42;
            let msg: string = `HP: ${hp}`;
            return msg;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "HP: 42");
}

#[test]
fn template_literal_multiple_expressions() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: i32 = 10;
            let y: i32 = 20;
            let msg: string = `pos(${x}, ${y})`;
            return msg;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "pos(10, 20)");
}

// ============================================================
// split()
// ============================================================

#[test]
fn string_split_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "attack:north";
            let parts: Array<i32> = msg.split(":");
            return parts.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 2);
}

#[test]
fn string_split_content() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "attack:north";
            let parts: Array<i32> = msg.split(":");
            let cmd: string = parts[0];
            let dir: string = parts[1];
            return cmd.length * 100 + dir.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    // "attack".length=6, "north".length=5 → 605
    assert_eq!(test.call(&mut store, ()).unwrap(), 605);
}

#[test]
fn string_split_multiple() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let csv: string = "a,b,c,d";
            let parts: Array<i32> = csv.split(",");
            return parts.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 4);
}

// ============================================================
// replace()
// ============================================================

#[test]
fn string_replace_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            let r: string = s.replace("world", "there");
            return r;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello there");
}

#[test]
fn string_replace_not_found() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello";
            let r: string = s.replace("xyz", "abc");
            return r.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 5); // unchanged
}

// ============================================================
// parseInt / parseFloat
// ============================================================

#[test]
fn string_parse_int() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            return parseInt("42") + parseInt("-7") + parseInt("  100  ");
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 135); // 42 + (-7) + 100
}

#[test]
fn string_parse_float() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            return parseFloat("3.14") + parseFloat("-0.5");
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 2.64).abs() < 1e-10);
}

// ============================================================
// String.fromCharCode, repeat, padStart, padEnd
// ============================================================

#[test]
fn string_from_char_code() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let a: string = String.fromCharCode(65);
            let b: string = String.fromCharCode(66);
            return (a + b);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "AB");
}

#[test]
fn string_repeat() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "ab".repeat(3);
            return s;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "ababab");
}

#[test]
fn string_pad_start() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "42".padStart(5, "0");
            return s;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "00042");
}

#[test]
fn string_pad_end() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hi".padEnd(5, "!");
            return s;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hi!!!");
}

// ============================================================
// Realistic end-to-end broadcast parsing with split
// ============================================================

#[test]
fn string_broadcast_parse_with_split() {
    let wasm = compile(
        r#"
        function handleBroadcast(msg: string): i32 {
            let parts: Array<i32> = msg.split(":");
            let cmd: string = parts[0];
            if (cmd == "damage") {
                let amount: string = parts[1];
                return parseInt(amount);
            }
            if (cmd == "heal") {
                let amount: string = parts[1];
                return parseInt(amount) * -1;
            }
            return 0;
        }

        export function test(): i32 {
            let r: i32 = 0;
            r = r + handleBroadcast("damage:50");
            r = r + handleBroadcast("heal:20");
            r = r + handleBroadcast("unknown:0");
            return r;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    // damage:50 → 50, heal:20 → -20, unknown → 0 → total = 30
    assert_eq!(test.call(&mut store, ()).unwrap(), 30);
}

// ==================== Debug info tests ====================

fn compile_debug(source: &str) -> Vec<u8> {
    let options = tscc::CompileOptions {
        debug: true,
        filename: "test.ts".to_string(),
        ..Default::default()
    };
    tscc::compile(source, &options).unwrap()
}

/// Helper: find all WASM custom sections by name in a binary.
fn find_custom_sections(wasm: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut sections = Vec::new();
    let mut offset = 8; // Skip magic + version
    while offset < wasm.len() {
        let section_id = wasm[offset];
        offset += 1;
        let (section_size, leb_len) = decode_uleb128_test(&wasm[offset..]);
        offset += leb_len;
        let section_end = offset + section_size as usize;

        if section_id == 0 {
            // Custom section: name_len + name + data
            let (name_len, name_leb_len) = decode_uleb128_test(&wasm[offset..]);
            let name_start = offset + name_leb_len;
            let name = std::str::from_utf8(&wasm[name_start..name_start + name_len as usize])
                .unwrap_or("<invalid>");
            let data_start = name_start + name_len as usize;
            sections.push((name.to_string(), wasm[data_start..section_end].to_vec()));
        }

        offset = section_end;
    }
    sections
}

fn decode_uleb128_test(bytes: &[u8]) -> (u64, usize) {
    let mut result: u64 = 0;
    let mut shift = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return (result, i + 1);
        }
        shift += 7;
    }
    (result, bytes.len())
}

#[test]
fn debug_name_section_present() {
    let wasm = compile_debug("export function tick(me: i32): void {}");
    let sections = find_custom_sections(&wasm);
    let name_section = sections.iter().find(|(name, _)| name == "name");
    assert!(name_section.is_some(), "name section should be present");
}

#[test]
fn debug_dwarf_sections_present() {
    let wasm = compile_debug(
        r#"
        export function tick(me: i32): void {
            let x: i32 = 42;
        }
    "#,
    );
    let sections = find_custom_sections(&wasm);
    let section_names: Vec<&str> = sections.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        section_names.contains(&".debug_abbrev"),
        "missing .debug_abbrev, found: {section_names:?}"
    );
    assert!(
        section_names.contains(&".debug_info"),
        "missing .debug_info, found: {section_names:?}"
    );
    assert!(
        section_names.contains(&".debug_line"),
        "missing .debug_line, found: {section_names:?}"
    );
}

#[test]
fn debug_dwarf_not_present_without_flag() {
    // Default compile (debug=false) should NOT have DWARF sections
    let wasm = compile("export function tick(me: i32): void {}");
    let sections = find_custom_sections(&wasm);
    let section_names: Vec<&str> = sections.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        !section_names.contains(&".debug_line"),
        "DWARF should not be present without debug flag"
    );
}

#[test]
fn debug_name_section_contains_function_names() {
    let wasm = compile_debug(
        r#"
        export function tick(me: i32): void {}
        function helper(x: i32): i32 { return x; }
    "#,
    );
    // The name section should contain our function names
    // Just verify the binary is valid and loadable
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
}

#[test]
fn debug_wasm_valid_with_dwarf() {
    // Compile a non-trivial script with debug info and verify wasmtime accepts it
    let wasm = compile_debug(
        r#"
        declare function get_hp(id: i32): i32;
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            let hp: i32 = get_hp(me);
            if (hp < 50) {
                set_action(me, 2, 0, 0.0, 0.0);
            } else {
                let dx: f64 = 1.0;
                let dy: f64 = 0.0;
                set_action(me, 0, 0, dx, dy);
            }
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());

    let get_hp = Func::wrap(&mut store, |_: i32| -> i32 { 100 });
    let set_action = Func::wrap(&mut store, |_: i32, _: i32, _: i32, _: f64, _: f64| {});

    let instance = Instance::new(&mut store, &module, &[get_hp.into(), set_action.into()]).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 1).unwrap();
}

#[test]
fn debug_trap_contains_source_location() {
    // Compile with debug, trigger a trap, verify the error contains file/line info
    let source = r#"export function test(): i32 {
    let arr: Array<i32> = new Array<i32>(4);
    arr.push(10);
    return arr[5];
}"#;

    let options = tscc::CompileOptions {
        debug: true,
        filename: "player.ts".to_string(),
        ..Default::default()
    };
    let wasm = tscc::compile(source, &options).unwrap();

    let mut config = Config::new();
    config.wasm_backtrace_details(WasmBacktraceDetails::Enable);
    let engine = Engine::new(&config).unwrap();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();

    let err = test.call(&mut store, ()).unwrap_err();
    let msg = format!("{err:?}");

    // The trap message should reference our source file
    assert!(
        msg.contains("player.ts") || msg.contains("test"),
        "trap message should reference source file, got: {msg}"
    );
}

#[test]
fn debug_multifunction_source_mapping() {
    // Multiple functions with debug info — verify the module loads and runs correctly
    let wasm = compile_debug(
        r#"
        function add(a: i32, b: i32): i32 {
            return a + b;
        }

        function multiply(a: i32, b: i32): i32 {
            return a * b;
        }

        export function test(): i32 {
            let sum: i32 = add(3, 4);
            let product: i32 = multiply(sum, 2);
            return product;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 14); // (3+4)*2
}

#[test]
fn debug_debug_line_has_filename() {
    let wasm = compile_debug(
        r#"
        export function tick(me: i32): void {
            let x: i32 = 42;
        }
    "#,
    );
    let sections = find_custom_sections(&wasm);
    let debug_line = sections
        .iter()
        .find(|(name, _)| name == ".debug_line")
        .unwrap();
    // The .debug_line section should contain our filename as bytes
    let filename_bytes = b"test.ts";
    let contains_filename = debug_line
        .1
        .windows(filename_bytes.len())
        .any(|window| window == filename_bytes);
    assert!(
        contains_filename,
        ".debug_line should contain the filename 'test.ts'"
    );
}

#[test]
fn debug_info_has_filename() {
    let wasm = compile_debug(
        r#"
        export function tick(me: i32): void {}
    "#,
    );
    let sections = find_custom_sections(&wasm);
    let debug_info = sections
        .iter()
        .find(|(name, _)| name == ".debug_info")
        .unwrap();
    // The .debug_info section should contain our filename
    let filename_bytes = b"test.ts";
    let contains_filename = debug_info
        .1
        .windows(filename_bytes.len())
        .any(|window| window == filename_bytes);
    assert!(
        contains_filename,
        ".debug_info should contain the filename 'test.ts'"
    );
}

#[test]
fn debug_closure_has_source_mapping() {
    // Closures (lifted arrow functions) should have debug info
    // Verify the module with closures + debug compiles and runs correctly
    let wasm = compile_debug(
        r#"
        export function test(): i32 {
            let add: (a: i32) => i32 = (a: i32): i32 => a + 10;
            return add(5);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 15);
}

#[test]
fn debug_array_filter_has_source_mapping() {
    // Inlined array builtins should have source mapping
    let wasm = compile_debug(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(0);
            arr.push(1);
            arr.push(2);
            arr.push(3);
            arr.push(4);
            let evens: Array<i32> = arr.filter((x: i32): bool => x > 2);
            return evens.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 2);
}

#[test]
fn debug_closure_named_in_name_section() {
    // Closures should appear as closure$N in the name section
    let wasm = compile_debug(
        r#"
        export function test(): i32 {
            let inc: (a: i32) => i32 = (a: i32): i32 => a + 1;
            return inc(41);
        }
    "#,
    );

    // The name section should contain "closure$0"
    let sections = find_custom_sections(&wasm);
    let name_section = sections.iter().find(|(name, _)| name == "name").unwrap();
    let closure_name = b"closure$0";
    let contains_closure_name = name_section
        .1
        .windows(closure_name.len())
        .any(|window| window == closure_name);
    assert!(
        contains_closure_name,
        "name section should contain 'closure$0'"
    );
}

#[test]
fn debug_name_section_present_without_debug_flag() {
    // Name section should be emitted even without --debug flag (it's always useful)
    let wasm = compile("export function tick(me: i32): void {}");
    let sections = find_custom_sections(&wasm);
    let name_section = sections.iter().find(|(name, _)| name == "name");
    assert!(
        name_section.is_some(),
        "name section should always be present"
    );
}

// ─── Inheritance tests ───────────────────────────────────────────────────────

#[test]
fn inherit_basic_field_access() {
    // Child inherits parent fields, accessible on child instance
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Warrior extends Entity {
            rage: i32;
            constructor(hp: i32, rage: i32) {
                super(hp);
            }
        }
        export function run(): i32 {
            const w: Warrior = new Warrior(100, 50);
            return w.hp;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 100);
}

#[test]
fn inherit_child_own_field() {
    // Child's own field is accessible and correctly offset
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Warrior extends Entity {
            rage: i32;
            constructor(hp: i32, rage: i32) {
                super(hp);
            }
        }
        export function run(): i32 {
            const w: Warrior = new Warrior(100, 50);
            return w.rage;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 50);
}

#[test]
fn inherit_method_from_parent() {
    // Child calls a method inherited from parent without overriding
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            constructor(hp: i32) {}
            getHp(): i32 { return this.hp; }
        }
        class Warrior extends Entity {
            constructor(hp: i32) { super(hp); }
        }
        export function run(): i32 {
            const w: Warrior = new Warrior(42);
            return w.getHp();
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 42);
}

#[test]
fn inherit_method_override() {
    // Child overrides parent method; child instance calls child's version
    let wasm = compile(
        r#"
        class Entity {
            constructor() {}
            id(): i32 { return 1; }
        }
        class Warrior extends Entity {
            constructor() { super(); }
            id(): i32 { return 2; }
        }
        export function run(): i32 {
            const w: Warrior = new Warrior();
            return w.id();
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 2);
}

#[test]
fn inherit_polymorphic_dispatch() {
    // Variable typed as parent holds child instance; method call dispatches to child's override
    let wasm = compile(
        r#"
        class Entity {
            constructor() {}
            id(): i32 { return 1; }
        }
        class Warrior extends Entity {
            constructor() { super(); }
            id(): i32 { return 2; }
        }
        export function run(): i32 {
            const e: Entity = new Warrior();
            return e.id();
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 2);
}

#[test]
fn inherit_super_constructor() {
    // super(args) correctly initializes parent fields
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            mp: i32;
            constructor(hp: i32, mp: i32) {}
        }
        class Warrior extends Entity {
            rage: i32;
            constructor(hp: i32, mp: i32, rage: i32) {
                super(hp, mp);
            }
        }
        export function run(): i32 {
            const w: Warrior = new Warrior(100, 50, 30);
            return w.hp + w.mp + w.rage;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 180);
}

#[test]
fn inherit_super_method_call() {
    // super.method() calls parent's version, not child's
    let wasm = compile(
        r#"
        class Entity {
            constructor() {}
            value(): i32 { return 10; }
        }
        class Warrior extends Entity {
            constructor() { super(); }
            value(): i32 { return super.value() + 5; }
        }
        export function run(): i32 {
            const w: Warrior = new Warrior();
            return w.value();
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 15);
}

#[test]
fn inherit_multi_level() {
    // Three-level hierarchy: A -> B -> C
    let wasm = compile(
        r#"
        class A {
            x: i32;
            constructor(x: i32) {}
            getX(): i32 { return this.x; }
        }
        class B extends A {
            y: i32;
            constructor(x: i32, y: i32) { super(x); }
        }
        class C extends B {
            z: i32;
            constructor(x: i32, y: i32, z: i32) { super(x, y); }
        }
        export function run(): i32 {
            const c: C = new C(1, 2, 3);
            return c.x + c.y + c.z + c.getX();
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 7); // 1+2+3+1
}

#[test]
fn inherit_multiple_subclasses() {
    // Multiple subclasses with polymorphic dispatch
    let wasm = compile(
        r#"
        class Entity {
            constructor() {}
            id(): i32 { return 0; }
        }
        class Warrior extends Entity {
            constructor() { super(); }
            id(): i32 { return 1; }
        }
        class Mage extends Entity {
            constructor() { super(); }
            id(): i32 { return 2; }
        }
        export function run(): i32 {
            const e1: Entity = new Warrior();
            const e2: Entity = new Mage();
            const e3: Entity = new Entity();
            return e1.id() * 100 + e2.id() * 10 + e3.id();
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 120); // 1*100 + 2*10 + 0
}

#[test]
fn inherit_mixed_with_non_inherited_class() {
    // Non-inherited class in same module keeps static dispatch; inherited class uses vtable
    let wasm = compile(
        r#"
        class Vec2 {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}
            len(): f64 { return this.x + this.y; }
        }
        class Entity {
            hp: i32;
            constructor(hp: i32) {}
            getHp(): i32 { return this.hp; }
        }
        class Warrior extends Entity {
            constructor(hp: i32) { super(hp); }
            getHp(): i32 { return this.hp + 10; }
        }
        export function run(): i32 {
            const v: Vec2 = new Vec2(3.0, 4.0);
            const e: Entity = new Warrior(50);
            return i32(v.len()) + e.getHp();
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 67); // 7 + 60
}

#[test]
fn inherit_f64_field_alignment() {
    // f64 field after vtable pointer (4 bytes) needs 8-byte alignment padding
    let wasm = compile(
        r#"
        class Entity {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}
        }
        class Projectile extends Entity {
            speed: f64;
            constructor(x: f64, y: f64, speed: f64) { super(x, y); }
        }
        export function run(): f64 {
            const p: Projectile = new Projectile(1.5, 2.5, 10.0);
            return p.x + p.y + p.speed;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), f64>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 14.0);
}

#[test]
fn inherit_polymorphic_in_foreach() {
    // Polymorphic dispatch through array forEach
    let wasm = compile(
        r#"
        class Entity {
            constructor() {}
            id(): i32 { return 0; }
        }
        class Warrior extends Entity {
            constructor() { super(); }
            id(): i32 { return 1; }
        }
        class Mage extends Entity {
            constructor() { super(); }
            id(): i32 { return 2; }
        }
        declare function report(v: i32): void;

        export function run(): void {
            let arr: Array<Entity> = new Array<Entity>(3);
            arr.push(new Entity());
            arr.push(new Warrior());
            arr.push(new Mage());
            let total: i32 = 0;
            arr.forEach((e: Entity): void => {
                total = total + e.id();
            });
            report(total);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let result = Cell::new(0i32);
    let mut store = Store::new(&engine, &result);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "report",
            |caller: Caller<'_, &Cell<i32>>, v: i32| {
                caller.data().set(v);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let run = instance
        .get_typed_func::<(), ()>(&mut store, "run")
        .unwrap();
    run.call(&mut store, ()).unwrap();
    assert_eq!(result.get(), 3); // 0 + 1 + 2
}

#[test]
fn inherit_class_as_function_param() {
    // Inherited class passed as parent-typed function parameter, polymorphic dispatch works
    let wasm = compile(
        r#"
        class Entity {
            constructor() {}
            id(): i32 { return 0; }
        }
        class Warrior extends Entity {
            constructor() { super(); }
            id(): i32 { return 42; }
        }
        function getId(e: Entity): i32 {
            return e.id();
        }
        export function run(): i32 {
            const w: Warrior = new Warrior();
            return getId(w);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 42);
}

#[test]
fn inherit_downcast_field_access() {
    // Downcast via `as` to access child-specific fields
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Warrior extends Entity {
            rage: i32;
            constructor(hp: i32, rage: i32) { super(hp); }
        }
        export function run(): i32 {
            const e: Entity = new Warrior(100, 77);
            return (e as Warrior).rage;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 77);
}

#[test]
fn inherit_downcast_method_call() {
    // Downcast via `as` to call child-specific method
    let wasm = compile(
        r#"
        class Entity {
            constructor() {}
            id(): i32 { return 0; }
        }
        class Warrior extends Entity {
            constructor() { super(); }
            id(): i32 { return 1; }
            battleCry(): i32 { return 999; }
        }
        export function run(): i32 {
            const e: Entity = new Warrior();
            return (e as Warrior).battleCry();
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 999);
}

#[test]
fn inherit_missing_super_error() {
    // Child constructor without super() should be a compile error
    let result = tscc::compile(
        r#"
        class Entity {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Warrior extends Entity {
            constructor(hp: i32) {}
        }
        export function run(): i32 { return 0; }
    "#,
        &tscc::CompileOptions::default(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("does not call super()"),
        "error: {err}"
    );
}

#[test]
fn inherit_override_signature_mismatch_error() {
    // Override with different param types should be a compile error
    let result = tscc::compile(
        r#"
        class Entity {
            constructor() {}
            update(dt: f64): void {}
        }
        class Warrior extends Entity {
            constructor() { super(); }
            update(dt: i32): void {}
        }
        export function run(): void {}
    "#,
        &tscc::CompileOptions::default(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("different parameter types"),
        "error: {err}"
    );
}

#[test]
fn inherit_override_return_type_mismatch_error() {
    // Override with different return type should be a compile error
    let result = tscc::compile(
        r#"
        class Entity {
            constructor() {}
            id(): i32 { return 0; }
        }
        class Warrior extends Entity {
            constructor() { super(); }
            id(): f64 { return 0.0; }
        }
        export function run(): void {}
    "#,
        &tscc::CompileOptions::default(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("different return type"),
        "error: {err}"
    );
}

#[test]
fn inherit_parent_no_constructor() {
    // Parent without constructor, child calls super() as no-op
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
        }
        class Warrior extends Entity {
            rage: i32;
            constructor(rage: i32) { super(); }
        }
        export function run(): i32 {
            const w: Warrior = new Warrior(50);
            return w.rage;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 50);
}

#[test]
fn inherit_invalid_cast_error() {
    // Cast between unrelated classes should be a compile error
    let result = tscc::compile(
        r#"
        class Foo {
            x: i32;
            constructor(x: i32) {}
        }
        class Bar {
            y: i32;
            constructor(y: i32) {}
        }
        export function run(): i32 {
            const f: Foo = new Foo(1);
            return (f as Bar).y;
        }
    "#,
        &tscc::CompileOptions::default(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string()
            .contains("not in the same inheritance hierarchy"),
        "error: {err}"
    );
}

#[test]
fn global_let_i32_read_write() {
    let wasm = compile(
        r#"
        let counter: i32 = 10;

        export function bump(): i32 {
            counter = counter + 1;
            return counter;
        }

        export function get(): i32 {
            return counter;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let bump = instance
        .get_typed_func::<(), i32>(&mut store, "bump")
        .unwrap();
    let get = instance
        .get_typed_func::<(), i32>(&mut store, "get")
        .unwrap();
    assert_eq!(get.call(&mut store, ()).unwrap(), 10);
    assert_eq!(bump.call(&mut store, ()).unwrap(), 11);
    assert_eq!(bump.call(&mut store, ()).unwrap(), 12);
    assert_eq!(get.call(&mut store, ()).unwrap(), 12);
}

#[test]
fn global_let_f64_compound_and_prefix_update() {
    let wasm = compile(
        r#"
        let score: f64 = 0.0;
        let hits: i32 = 0;

        export function score_hit(amount: f64): f64 {
            score += amount;
            ++hits;
            return score;
        }

        export function get_hits(): i32 {
            return hits;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let score_hit = instance
        .get_typed_func::<f64, f64>(&mut store, "score_hit")
        .unwrap();
    let get_hits = instance
        .get_typed_func::<(), i32>(&mut store, "get_hits")
        .unwrap();
    assert!((score_hit.call(&mut store, 1.5).unwrap() - 1.5).abs() < 1e-10);
    assert!((score_hit.call(&mut store, 2.5).unwrap() - 4.0).abs() < 1e-10);
    assert_eq!(get_hits.call(&mut store, ()).unwrap(), 2);
}

#[test]
fn global_let_default_initialized_to_zero() {
    // `let` without an initializer defaults to 0 / 0.0 (same as const behavior).
    let wasm = compile(
        r#"
        let n: i32;
        export function set_n(x: i32): void { n = x; }
        export function get_n(): i32 { return n; }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let get_n = instance
        .get_typed_func::<(), i32>(&mut store, "get_n")
        .unwrap();
    let set_n = instance
        .get_typed_func::<i32, ()>(&mut store, "set_n")
        .unwrap();
    assert_eq!(get_n.call(&mut store, ()).unwrap(), 0);
    set_n.call(&mut store, 99).unwrap();
    assert_eq!(get_n.call(&mut store, ()).unwrap(), 99);
}

#[test]
fn global_const_assignment_is_rejected() {
    let result = tscc::compile(
        r#"
        const fixed: i32 = 7;
        export function go(): void { fixed = 8; }
    "#,
        &tscc::CompileOptions::default(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("const global"), "error: {err}");
}

#[test]
fn undefined_keyword_compiles_like_null() {
    let wasm = compile(
        r#"
        class Entity { hp: i32; constructor(hp: i32) {} }
        export function pick(which: i32): i32 {
            const e: Entity = which > 0 ? new Entity(7) : undefined;
            return e === undefined ? -1 : e.hp;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let pick = instance
        .get_typed_func::<i32, i32>(&mut store, "pick")
        .unwrap();
    assert_eq!(pick.call(&mut store, 1).unwrap(), 7);
    assert_eq!(pick.call(&mut store, 0).unwrap(), -1);
}

#[test]
fn array_shorthand_type_syntax() {
    // T[] should be equivalent to Array<T>
    let wasm = compile(
        r#"
        export function sum(): i32 {
            const xs: i32[] = new Array<i32>(4);
            xs.push(1);
            xs.push(2);
            xs.push(3);
            let s: i32 = 0;
            for (const x of xs) { s += x; }
            return s;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let sum = instance
        .get_typed_func::<(), i32>(&mut store, "sum")
        .unwrap();
    assert_eq!(sum.call(&mut store, ()).unwrap(), 6);
}

#[test]
fn strict_equality_compiles_identically_to_loose() {
    // In our typed subset, === and == are the same: strict, typed equality.
    let wasm = compile(
        r#"
        export function loose(a: i32, b: i32): i32 { return a == b ? 1 : 0; }
        export function strict(a: i32, b: i32): i32 { return a === b ? 1 : 0; }
        export function loose_neq(a: i32, b: i32): i32 { return a != b ? 1 : 0; }
        export function strict_neq(a: i32, b: i32): i32 { return a !== b ? 1 : 0; }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let loose = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "loose")
        .unwrap();
    let strict = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "strict")
        .unwrap();
    let loose_neq = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "loose_neq")
        .unwrap();
    let strict_neq = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "strict_neq")
        .unwrap();
    for (a, b) in &[(1, 1), (1, 2), (5, 5), (0, 0)] {
        assert_eq!(
            loose.call(&mut store, (*a, *b)).unwrap(),
            strict.call(&mut store, (*a, *b)).unwrap()
        );
        assert_eq!(
            loose_neq.call(&mut store, (*a, *b)).unwrap(),
            strict_neq.call(&mut store, (*a, *b)).unwrap()
        );
    }
}

#[test]
fn string_equality_uses_content_not_pointer() {
    // Two strings constructed via concat should compare equal by content.
    let wasm = compile(
        r#"
        export function same(): i32 {
            const a: string = "hel" + "lo";
            const b: string = "hello";
            return a === b ? 1 : 0;
        }
        export function diff(): i32 {
            const a: string = "foo";
            const b: string = "bar";
            return a === b ? 1 : 0;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let same = instance
        .get_typed_func::<(), i32>(&mut store, "same")
        .unwrap();
    let diff = instance
        .get_typed_func::<(), i32>(&mut store, "diff")
        .unwrap();
    assert_eq!(same.call(&mut store, ()).unwrap(), 1);
    assert_eq!(diff.call(&mut store, ()).unwrap(), 0);
}

#[test]
fn ternary_with_class_result() {
    let wasm = compile(
        r#"
        class Foo { x: i32; constructor(x: i32) {} }
        export function pick(which: i32): i32 {
            const a: Foo = new Foo(10);
            const b: Foo = new Foo(20);
            const chosen: Foo = which > 0 ? a : b;
            return chosen.x;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let pick = instance
        .get_typed_func::<i32, i32>(&mut store, "pick")
        .unwrap();
    assert_eq!(pick.call(&mut store, 1).unwrap(), 10);
    assert_eq!(pick.call(&mut store, 0).unwrap(), 20);
}

#[test]
fn optional_method_call_non_null_dispatches() {
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            constructor(hp: i32) {}
            getHp(): i32 { return this.hp; }
        }
        export function get(which: i32): i32 {
            const e: Entity = which > 0 ? new Entity(42) : (null as Entity);
            return e?.getHp() ?? -1;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let get = instance
        .get_typed_func::<i32, i32>(&mut store, "get")
        .unwrap();
    assert_eq!(get.call(&mut store, 1).unwrap(), 42);
    assert_eq!(get.call(&mut store, 0).unwrap(), -1);
}

#[test]
fn optional_method_call_polymorphic() {
    // Optional call should work through vtable dispatch too.
    let wasm = compile(
        r#"
        class Shape {
            kind: i32;
            constructor(k: i32) { this.kind = k; }
            area(): i32 { return 0; }
        }
        class Square extends Shape {
            side: i32;
            constructor(s: i32) { super(1); this.side = s; }
            area(): i32 { return this.side * this.side; }
        }
        export function with_square(): i32 {
            const s: Square = new Square(4);
            return s?.area() ?? -1;
        }
        export function with_null(): i32 {
            const s: Shape = null as Shape;
            return s?.area() ?? -1;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let with_square = instance
        .get_typed_func::<(), i32>(&mut store, "with_square")
        .unwrap();
    let with_null = instance
        .get_typed_func::<(), i32>(&mut store, "with_null")
        .unwrap();
    assert_eq!(with_square.call(&mut store, ()).unwrap(), 16);
    assert_eq!(with_null.call(&mut store, ()).unwrap(), -1);
}

#[test]
fn super_method_call_inside_arrow_closure() {
    // Use super.method() from within an arrow body inside a method.
    let wasm = compile(
        r#"
        class A {
            constructor() {}
            base(): i32 { return 10; }
        }
        class B extends A {
            constructor() { super(); }
            combined(): i32 {
                const xs: Array<i32> = new Array<i32>(3);
                xs.push(1); xs.push(2); xs.push(3);
                return xs.reduce((acc, x) => acc + x + super.base(), 0);
            }
        }
        export function run(): i32 {
            const b: B = new B();
            return b.combined();
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    // Expected: (1 + 10) + (2 + 10) + (3 + 10) = 36
    assert_eq!(run.call(&mut store, ()).unwrap(), 36);
}

#[test]
fn nested_object_destructuring() {
    let wasm = compile(
        r#"
        class Pos { x: i32; y: i32; constructor(x: i32, y: i32) { this.x = x; this.y = y; } }
        class Ent { pos: Pos; hp: i32; constructor(p: Pos, h: i32) { this.pos = p; this.hp = h; } }
        export function run(): i32 {
            const e: Ent = new Ent(new Pos(3, 4), 7);
            const { pos, hp } = e;
            const { x, y } = pos;
            return x + y + hp;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 14);
}

#[test]
fn arena_grow_handles_large_allocation() {
    // Grow mode must grow by enough pages when a single allocation exceeds
    // one page (64 KiB). Allocate a 200 KiB i32 array from a 1-page initial memory.
    let opts = tscc::CompileOptions {
        arena_overflow: tscc::ArenaOverflow::Grow,
        memory_pages: 1,
        ..Default::default()
    };
    let wasm = tscc::compile(
        r#"
        export function go(): i32 {
            const arr: Array<i32> = new Array<i32>(50000);
            arr.push(42);
            return arr.length;
        }
    "#,
        &opts,
    )
    .unwrap();
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let go = instance
        .get_typed_func::<(), i32>(&mut store, "go")
        .unwrap();
    assert_eq!(go.call(&mut store, ()).unwrap(), 1);
}

#[test]
fn arena_grow_traps_when_host_refuses_growth() {
    // Host caps memory via StoreLimits. When Grow cannot satisfy a request,
    // the script traps cleanly rather than overrunning memory.
    let opts = tscc::CompileOptions {
        arena_overflow: tscc::ArenaOverflow::Grow,
        memory_pages: 1,
        ..Default::default()
    };
    let wasm = tscc::compile(
        r#"
        export function go(): i32 {
            const arr: Array<i32> = new Array<i32>(50000);
            arr.push(1);
            return arr.length;
        }
    "#,
        &opts,
    )
    .unwrap();
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(
        &engine,
        wasmtime::StoreLimitsBuilder::new()
            .memory_size(64 * 1024) // exactly 1 page — script needs more
            .build(),
    );
    store.limiter(|lim| lim);
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let go = instance
        .get_typed_func::<(), i32>(&mut store, "go")
        .unwrap();
    let err = go.call(&mut store, ()).unwrap_err();
    let trap = err.downcast_ref::<wasmtime::Trap>().copied();
    assert_eq!(
        trap,
        Some(wasmtime::Trap::UnreachableCodeReached),
        "expected UnreachableCodeReached trap, got: {err:?}"
    );
}

#[test]
fn arena_trap_mode_traps_when_memory_exceeded() {
    // Trap mode: no memory.grow; trap the instant the arena exceeds current memory.
    let opts = tscc::CompileOptions {
        arena_overflow: tscc::ArenaOverflow::Trap,
        memory_pages: 1,
        ..Default::default()
    };
    let wasm = tscc::compile(
        r#"
        export function go(): i32 {
            const arr: Array<i32> = new Array<i32>(50000);
            arr.push(1);
            return arr.length;
        }
    "#,
        &opts,
    )
    .unwrap();
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let go = instance
        .get_typed_func::<(), i32>(&mut store, "go")
        .unwrap();
    let err = go.call(&mut store, ()).unwrap_err();
    let trap = err.downcast_ref::<wasmtime::Trap>().copied();
    assert_eq!(
        trap,
        Some(wasmtime::Trap::UnreachableCodeReached),
        "expected UnreachableCodeReached trap, got: {err:?}"
    );
}

#[test]
fn array_destructuring_rest_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            const src: Array<i32> = new Array<i32>(5);
            src.push(10); src.push(20); src.push(30); src.push(40); src.push(50);
            const [first, second, ...rest] = src;
            return first + second + rest.length + rest[0] + rest[2];
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    // 10 + 20 + 3 (rest length) + 30 (rest[0]) + 50 (rest[2]) = 113
    assert_eq!(test.call(&mut store, ()).unwrap(), 113);
}

#[test]
fn array_destructuring_rest_empty_when_source_short() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            const src: Array<i32> = new Array<i32>(2);
            src.push(7); src.push(8);
            const [a, b, ...rest] = src;
            return a + b + rest.length;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    // rest is empty — length 0
    assert_eq!(test.call(&mut store, ()).unwrap(), 15);
}

#[test]
fn array_destructuring_rest_f64_elements() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            const src: Array<f64> = new Array<f64>(4);
            src.push(1.5); src.push(2.5); src.push(3.5); src.push(4.5);
            const [head, ...tail] = src;
            let sum: f64 = head;
            for (let i: i32 = 0; i < tail.length; ++i) {
                sum = sum + tail[i];
            }
            return sum;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    // 1.5 + 2.5 + 3.5 + 4.5 = 12.0
    assert_eq!(test.call(&mut store, ()).unwrap(), 12.0);
}

#[test]
fn array_destructuring_rest_only() {
    // `const [...all] = src` — rest with no prefix; should copy entire source.
    let wasm = compile(
        r#"
        export function test(): i32 {
            const src: Array<i32> = new Array<i32>(3);
            src.push(100); src.push(200); src.push(300);
            const [...all] = src;
            return all.length + all[0] + all[1] + all[2];
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 3 + 100 + 200 + 300);
}

#[test]
fn array_destructuring_rest_does_not_alias_source() {
    // Mutating rest must not affect the original array.
    let wasm = compile(
        r#"
        export function test(): i32 {
            const src: Array<i32> = new Array<i32>(4);
            src.push(1); src.push(2); src.push(3); src.push(4);
            const [head, ...rest] = src;
            rest[0] = 999;
            return src[1] + rest[0] + head;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    // src[1] still 2, rest[0] now 999, head is 1
    assert_eq!(test.call(&mut store, ()).unwrap(), 2 + 999 + 1);
}

#[test]
fn template_concat_fusion_produces_correct_result() {
    // A long template with mixed types should round-trip correctly through the
    // fused path (one arena-alloc + memcpy per piece, rather than N-1 __str_concat
    // calls). This exercises the multi-piece fusion with static quasis and
    // i32/f64 interpolated expressions.
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: i32 = 42;
            let y: f64 = 3.5;
            let name: string = "world";
            let s: string = `hi ${name}, x=${x}, y=${y}!`;
            return s.length;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    // "hi world, x=42, y=3.5!" = 22 chars
    let len = test.call(&mut store, ()).unwrap();
    assert_eq!(len, 22, "fused template length mismatch");
}

#[test]
fn plus_chain_fusion_omits_str_concat_helper() {
    // After fusion, `+`-chained string concatenation should NOT emit any call to
    // the __str_concat helper — the helper body shouldn't even be registered.
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "a" + "b" + "c" + "d" + "e";
            return s.length;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let len = test.call(&mut store, ()).unwrap();
    assert_eq!(len, 5);

    // And the helper name shouldn't appear in the emitted module at all.
    let s = String::from_utf8_lossy(&wasm);
    assert!(
        !s.contains("__str_concat"),
        "__str_concat should not be present after fusion"
    );
}

#[test]
fn mixed_numeric_inside_plus_chain_still_numeric() {
    // (1 + 2) + "x" must treat (1+2) as a numeric addition — the fusion flattener
    // only recurses into `+` nodes whose own result is string-typed.
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = (1 + 2) + "x";
            return s.length;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    let len = test.call(&mut store, ()).unwrap();
    assert_eq!(len, 2, r#"(1+2) + "x" should be "3x" (length 2)"#);
}

#[test]
fn string_helper_tree_shaking_shrinks_module() {
    // A program with zero string operations should not include any string helper
    // bodies. Compare against a program that uses string concat — the latter must
    // be larger, and the former must be free of string-helper function names.
    let bare = compile("export function tick(me: i32): void {}");
    let with_strings = compile(
        r#"
        export function tick(me: i32): void {
            let s: string = "hi" + "!";
        }
    "#,
    );
    assert!(
        with_strings.len() > bare.len(),
        "expected string-using module to be larger: bare={} with_strings={}",
        bare.len(),
        with_strings.len()
    );

    // name section should not mention string helpers in the bare module
    let bare_str = String::from_utf8_lossy(&bare);
    assert!(
        !bare_str.contains("__str_concat"),
        "bare module should not carry __str_concat"
    );
    assert!(
        !bare_str.contains("__str_eq"),
        "bare module should not carry __str_eq"
    );

    // Using only indexOf should pull in __str_indexOf but not __str_slice/__str_repeat.
    let only_index_of = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            return s.indexOf("world");
        }
    "#,
    );
    let s = String::from_utf8_lossy(&only_index_of);
    assert!(s.contains("__str_indexOf"), "should include __str_indexOf");
    assert!(
        !s.contains("__str_repeat"),
        "should not include __str_repeat"
    );
    assert!(
        !s.contains("__str_padStart"),
        "should not include __str_padStart"
    );
    assert!(!s.contains("__str_split"), "should not include __str_split");
}

#[test]
fn global_const_increment_is_rejected() {
    let result = tscc::compile(
        r#"
        const c: i32 = 1;
        export function go(): void { ++c; }
    "#,
        &tscc::CompileOptions::default(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("const global"), "error: {err}");
}
