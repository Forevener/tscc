use std::cell::Cell;

use wasmtime::*;

use super::common::compile;

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

