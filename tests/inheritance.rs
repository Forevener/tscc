mod common;

use std::cell::Cell;

use wasmtime::*;
use common::compile;

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

