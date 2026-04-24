use wasmtime::*;

use super::common::compile;

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

