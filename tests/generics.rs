mod common;

use wasmtime::*;

use common::{compile, run_sink_tick};

/// `Box<T>` with T = i32. One monomorphization, concrete constructor call.
#[test]
fn generic_class_box_i32() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Box<T> {
            value: T;
            constructor(value: T) {}
            get(): T { return this.value; }
        }

        export function tick(_me: i32): void {
            const b: Box<i32> = new Box<i32>(42);
            sink(f64(b.get()));
        }
        "#,
    );
    assert_eq!(values, vec![42.0]);
}

/// `Box<T>` with T = f64. Verifies field type substitution flows through to
/// storage width (8 bytes instead of 4).
#[test]
fn generic_class_box_f64() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Box<T> {
            value: T;
            constructor(value: T) {}
            get(): T { return this.value; }
        }

        export function tick(_me: i32): void {
            const b: Box<f64> = new Box<f64>(3.25);
            sink(b.get());
        }
        "#,
    );
    assert_eq!(values, vec![3.25]);
}

/// Two instantiations of the same template in one module — each gets its own
/// class layout and methods. No collision at registration time.
#[test]
fn generic_class_two_instantiations() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Box<T> {
            value: T;
            constructor(value: T) {}
            get(): T { return this.value; }
        }

        export function tick(_me: i32): void {
            const a: Box<i32> = new Box<i32>(7);
            const b: Box<f64> = new Box<f64>(1.5);
            sink(f64(a.get()));
            sink(b.get());
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 1.5]);
}

/// Two-parameter generic class — Pair<K, V> with different K/V across uses.
#[test]
fn generic_class_pair() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Pair<K, V> {
            key: K;
            value: V;
            constructor(key: K, value: V) {}
            sum(): f64 { return f64(this.key) + this.value; }
        }

        export function tick(_me: i32): void {
            const p: Pair<i32, f64> = new Pair<i32, f64>(3, 4.25);
            sink(p.sum());
        }
        "#,
    );
    assert_eq!(values, vec![7.25]);
}

/// Generic free function — `identity<T>(x)`. Explicit type arg form.
#[test]
fn generic_fn_identity() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function identity<T>(x: T): T { return x; }

        export function tick(_me: i32): void {
            sink(f64(identity<i32>(10)));
            sink(identity<f64>(2.5));
        }
        "#,
    );
    assert_eq!(values, vec![10.0, 2.5]);
}

/// Method body references `this.field` whose type is a type parameter — the
/// field-type substitution must flow through field access codegen.
#[test]
fn generic_class_field_access_through_this() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Holder<T> {
            a: T;
            b: T;
            constructor(a: T, b: T) {}
            first(): T { return this.a; }
            second(): T { return this.b; }
        }

        export function tick(_me: i32): void {
            const h: Holder<f64> = new Holder<f64>(1.5, 2.5);
            sink(h.first());
            sink(h.second());
        }
        "#,
    );
    assert_eq!(values, vec![1.5, 2.5]);
}

/// A.6 inference: numeric-literal arguments drive T for integer and float
/// forms independently — `identity(10)` is i32, `identity(2.5)` is f64.
#[test]
fn generic_fn_identity_inference_numeric_literals() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function identity<T>(x: T): T { return x; }

        export function tick(_me: i32): void {
            sink(f64(identity(10)));
            sink(identity(2.5));
        }
        "#,
    );
    assert_eq!(values, vec![10.0, 2.5]);
}

/// A.6 inference: boolean-literal argument binds T to `bool`.
#[test]
fn generic_fn_identity_inference_bool() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function identity<T>(x: T): T { return x; }

        export function tick(_me: i32): void {
            const b: bool = identity(true);
            if (b) { sink(1.0); } else { sink(0.0); }
        }
        "#,
    );
    assert_eq!(values, vec![1.0]);
}

/// A.6 inference: string-literal argument binds T to `string`. Exercises the
/// string-return path through the monomorphized body (no-op for identity, but
/// the callee must declare `__str_len` etc. correctly).
#[test]
fn generic_fn_identity_inference_string_literal() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function identity<T>(x: T): T { return x; }

        export function tick(_me: i32): void {
            const s: string = identity("hello");
            sink(f64(s.length));
        }
        "#,
    );
    assert_eq!(values, vec![5.0]);
}

/// A.6 inference: arguments bound to outer function parameters flow through
/// the locals env. `foo(x)` inside `function outer(x: i32)` infers T=i32.
#[test]
fn generic_fn_inference_from_outer_param() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function identity<T>(x: T): T { return x; }

        function outer(x: i32): i32 { return identity(x); }

        export function tick(_me: i32): void {
            sink(f64(outer(7)));
        }
        "#,
    );
    assert_eq!(values, vec![7.0]);
}

/// A.6 inference on a two-parameter generic: K bound by first arg, V by
/// second. Both inferences must succeed and agree with the explicit form.
#[test]
fn generic_fn_pair_inference_two_params() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function first<K, V>(k: K, _v: V): K { return k; }
        function second<K, V>(_k: K, v: V): V { return v; }

        export function tick(_me: i32): void {
            sink(f64(first(3, 4.25)));
            sink(second(3, 4.25));
        }
        "#,
    );
    assert_eq!(values, vec![3.0, 4.25]);
}

/// A.6 inference with the type parameter appearing in multiple positions.
/// First slot drives the binding; the later slots re-use it.
#[test]
fn generic_fn_inference_same_t_two_slots() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function addT<T>(a: T, b: T): T { return a + b; }

        export function tick(_me: i32): void {
            sink(f64(addT(2, 3)));
            sink(addT(1.5, 0.25));
        }
        "#,
    );
    assert_eq!(values, vec![5.0, 1.75]);
}

/// A.6 inference via an explicit `as` cast on the argument — the cast's
/// target type drives T even when the underlying expression would otherwise
/// infer to a different BoundType.
#[test]
fn generic_fn_inference_via_as_cast() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function identity<T>(x: T): T { return x; }

        export function tick(_me: i32): void {
            sink(identity(42 as f64));
        }
        "#,
    );
    assert_eq!(values, vec![42.0]);
}

/// A.9: `class Child<T> extends Parent<T>` — both sides generic, the child
/// passes its T through to the parent. Instantiating `Child<i32>` must also
/// pull in `Parent<i32>` and wire field layouts + method vtables correctly.
#[test]
fn generic_class_inheritance_t_passthrough() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Parent<T> {
            value: T;
            constructor(value: T) {}
            get(): T { return this.value; }
        }

        class Child<T> extends Parent<T> {
            tag: i32;
            constructor(value: T, tag: i32) {
                super(value);
            }
        }

        export function tick(_me: i32): void {
            const c: Child<i32> = new Child<i32>(42, 7);
            sink(f64(c.get()));
            sink(f64(c.tag));
        }
        "#,
    );
    assert_eq!(values, vec![42.0, 7.0]);
}

/// A.9: mixed inheritance — generic child extends a concrete parent. The
/// parent's layout is fixed independent of T, so field offsets for the
/// inherited members resolve against the static parent registration.
#[test]
fn generic_class_inheritance_concrete_parent() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Base {
            kind: i32;
            constructor(kind: i32) {}
        }

        class Wrapper<T> extends Base {
            value: T;
            constructor(kind: i32, value: T) {
                super(kind);
            }
            get(): T { return this.value; }
        }

        export function tick(_me: i32): void {
            const w: Wrapper<f64> = new Wrapper<f64>(3, 1.75);
            sink(f64(w.kind));
            sink(w.get());
        }
        "#,
    );
    assert_eq!(values, vec![3.0, 1.75]);
}

/// A.9: child overrides a method declared on the generic parent. Dispatch via
/// the child instance must reach the override, not the parent's body.
#[test]
fn generic_class_inheritance_method_override() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Parent<T> {
            value: T;
            constructor(value: T) {}
            get(): T { return this.value; }
        }

        class Child<T> extends Parent<T> {
            constructor(value: T) {
                super(value);
            }
            get(): T {
                return this.value;
            }
            getDoubled(): f64 {
                return f64(this.get()) * 2.0;
            }
        }

        export function tick(_me: i32): void {
            const c: Child<i32> = new Child<i32>(21);
            sink(c.getDoubled());
        }
        "#,
    );
    assert_eq!(values, vec![42.0]);
}

/// A.9: a generic class field whose type is ANOTHER generic instantiation.
/// `collect_instantiations`' fixed-point worklist needs to cascade — seeing
/// `Pair<Box<i32>, f64>` must also pull in `Box<i32>`.
#[test]
fn generic_class_field_is_another_generic_instantiation() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Box<T> {
            value: T;
            constructor(value: T) {}
            get(): T { return this.value; }
        }

        class Pair<K, V> {
            key: K;
            value: V;
            constructor(key: K, value: V) {}
        }

        export function tick(_me: i32): void {
            const inner: Box<i32> = new Box<i32>(5);
            const p: Pair<Box<i32>, f64> = new Pair<Box<i32>, f64>(inner, 3.25);
            sink(f64(p.key.get()));
            sink(p.value);
        }
        "#,
    );
    assert_eq!(values, vec![5.0, 3.25]);
}

/// Unused generic templates produce no code (no monomorphizations collected).
/// The program still compiles and runs successfully.
#[test]
fn generic_class_unused_template_is_elided() {
    let wasm = compile(
        r#"
        class Dead<T> {
            value: T;
            constructor(value: T) {}
        }

        export function tick(_me: i32): i32 { return 42; }
        "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Linker::new(&engine).instantiate(&mut store, &module).unwrap();
    let tick = instance.get_typed_func::<i32, i32>(&mut store, "tick").unwrap();
    assert_eq!(tick.call(&mut store, 0).unwrap(), 42);
}
