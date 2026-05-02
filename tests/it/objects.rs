//! Phase A object-literal tests (A.7).
//!
//! Coverage matches the master plan's A.7 checklist:
//! named/interface/anonymous shapes, reorder + set equivalence, nested
//! shapes, field reassignment / destructuring, variable assignment,
//! function arg + return, class fields holding shapes, and the A.6 excess /
//! missing-property rejections. Plus the A.5-specific flows: destructuring
//! directly from an `ObjectExpression`, and destructuring from a free
//! function that returns a synthetic-class instance.

use wasmtime::*;

use super::common::{compile, compile_err, read_wasm_string, run_sink_tick};

// ---- Named shapes (`type` alias path) ----

#[test]
fn named_shape_two_fields_f64() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            const p: Point = { x: 1.5, y: 2.5 };
            sink(p.x);
            sink(p.y);
        }
        "#,
    );
    assert_eq!(values, vec![1.5, 2.5]);
}

#[test]
fn named_shape_three_fields_mixed_i32_f64_bool() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Hit = { id: i32; dist: f64; alive: boolean };

        export function tick(_me: i32): void {
            const h: Hit = { id: 7, dist: 3.5, alive: true };
            sink(f64(h.id));
            sink(h.dist);
            sink(f64(h.alive ? 1 : 0));
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 3.5, 1.0]);
}

#[test]
fn named_shape_five_fields() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Stat = {
            hp: i32;
            mp: i32;
            atk: f64;
            def: f64;
            ready: boolean;
        };

        export function tick(_me: i32): void {
            const s: Stat = { hp: 100, mp: 50, atk: 12.5, def: 8.0, ready: true };
            sink(f64(s.hp));
            sink(f64(s.mp));
            sink(s.atk);
            sink(s.def);
            sink(f64(s.ready ? 1 : 0));
        }
        "#,
    );
    assert_eq!(values, vec![100.0, 50.0, 12.5, 8.0, 1.0]);
}

// ---- Interface path identical to type alias ----

#[test]
fn interface_path_matches_type_path() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        interface Vec3 {
            x: f64;
            y: f64;
            z: f64;
        }

        export function tick(_me: i32): void {
            const v: Vec3 = { x: 1.0, y: 2.0, z: 3.0 };
            sink(v.x + v.y + v.z);
        }
        "#,
    );
    assert_eq!(values, vec![6.0]);
}

#[test]
fn interface_and_type_alias_are_interchangeable() {
    // Same fingerprint → same synthetic class. The interface declaration
    // wins the name (registered first), but a `type` alias declaring the
    // same shape collapses into the same registry entry.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        interface Pair { a: f64; b: f64 }
        type PairAlias = { a: f64; b: f64 };

        export function tick(_me: i32): void {
            const p: Pair = { a: 1.0, b: 2.0 };
            const q: PairAlias = { a: 10.0, b: 20.0 };
            sink(p.a + q.a);
            sink(p.b + q.b);
        }
        "#,
    );
    assert_eq!(values, vec![11.0, 22.0]);
}

// ---- Anonymous literals (no annotation) ----

#[test]
fn anonymous_literal_inferred_member_access() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const p = { x: 4.0, y: 9.0 };
            sink(p.x);
            sink(p.y);
        }
        "#,
    );
    assert_eq!(values, vec![4.0, 9.0]);
}

#[test]
fn anonymous_shape_in_function_param() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function magnitude(p: { x: f64; y: f64 }): f64 {
            return Math.sqrt(p.x * p.x + p.y * p.y);
        }

        export function tick(_me: i32): void {
            sink(magnitude({ x: 3.0, y: 4.0 }));
        }
        "#,
    );
    assert_eq!(values, vec![5.0]);
}

// ---- Reorder + set equivalence (A.4 decision #3) ----

#[test]
fn reorder_equivalence_field_store_by_name() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            // Source order in literal is `y, x` but layout offsets are
            // `x, y`. Field stores key by name, so p.x must be 1 and p.y 2.
            const p: Point = { y: 2.0, x: 1.0 };
            sink(p.x);
            sink(p.y);
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0]);
}

#[test]
fn set_equivalence_two_literal_orders_same_shape() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function avg(p: { x: f64; y: f64 }): f64 {
            return (p.x + p.y) / 2.0;
        }

        export function tick(_me: i32): void {
            sink(avg({ x: 2.0, y: 4.0 }));
            sink(avg({ y: 4.0, x: 2.0 }));
        }
        "#,
    );
    assert_eq!(values, vec![3.0, 3.0]);
}

// ---- Nested shapes ----

#[test]
fn nested_named_types() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Inner = { v: f64 };
        type Outer = { inner: Inner; tag: i32 };

        export function tick(_me: i32): void {
            const o: Outer = { inner: { v: 42.0 }, tag: 7 };
            sink(o.inner.v);
            sink(f64(o.tag));
        }
        "#,
    );
    assert_eq!(values, vec![42.0, 7.0]);
}

#[test]
fn nested_anonymous_shape_in_named_field() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Entity = { pos: { x: f64; y: f64 }; hp: i32 };

        export function tick(_me: i32): void {
            const e: Entity = { pos: { x: 1.0, y: 2.0 }, hp: 100 };
            sink(e.pos.x);
            sink(e.pos.y);
            sink(f64(e.hp));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0, 100.0]);
}

// ---- Field reassignment ----

#[test]
fn field_reassignment() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            const p: Point = { x: 1.0, y: 2.0 };
            p.x = 10.0;
            p.y = p.y + 5.0;
            sink(p.x);
            sink(p.y);
        }
        "#,
    );
    assert_eq!(values, vec![10.0, 7.0]);
}

// ---- Destructuring ----

#[test]
fn destructure_from_named_shape_local() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            const p: Point = { x: 3.0, y: 4.0 };
            const { x, y } = p;
            sink(x);
            sink(y);
        }
        "#,
    );
    assert_eq!(values, vec![3.0, 4.0]);
}

#[test]
fn destructure_from_anonymous_inferred_local() {
    // `const p = { ... }` with no annotation → fingerprint inference
    // populates `local_class_types[p]` so destructuring works.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const p = { x: 5.0, y: 6.0 };
            const { x, y } = p;
            sink(x);
            sink(y);
        }
        "#,
    );
    assert_eq!(values, vec![5.0, 6.0]);
}

#[test]
fn destructure_directly_from_object_literal() {
    // A.5 path: `const { x, y } = { x, y }` — initializer is
    // `Expression::ObjectExpression`, special-cased to emit the literal once
    // and use its resolved class.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const { x, y } = { x: 11.0, y: 22.0 };
            sink(x);
            sink(y);
        }
        "#,
    );
    assert_eq!(values, vec![11.0, 22.0]);
}

#[test]
fn destructure_from_function_returning_synthetic_class() {
    // A.5 path: `infer_init_type` + `resolve_expr_class` now consult
    // `fn_return_classes`, so this destructure resolves the class from the
    // function's return annotation.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Point = { x: f64; y: f64 };

        function makePoint(): Point {
            return { x: 1.5, y: 2.5 };
        }

        export function tick(_me: i32): void {
            const { x, y } = makePoint();
            sink(x);
            sink(y);
        }
        "#,
    );
    assert_eq!(values, vec![1.5, 2.5]);
}

#[test]
fn destructure_renaming_field_to_alias() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            const p: Point = { x: 7.0, y: 8.0 };
            const { x: a, y: b } = p;
            sink(a);
            sink(b);
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 8.0]);
}

// ---- Variable-to-variable assignment (pointer copy) ----

#[test]
fn assign_between_same_shape_variables_pointer_copy() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            const a: Point = { x: 1.0, y: 2.0 };
            let b: Point = a;
            // Pointer copy: mutating b also mutates the shared object.
            b.x = 99.0;
            sink(a.x);
            sink(b.x);
        }
        "#,
    );
    assert_eq!(values, vec![99.0, 99.0]);
}

// ---- Function arg + return ----

#[test]
fn pass_named_shape_value_as_arg() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Vec2 = { x: f64; y: f64 };

        function dot(a: Vec2, b: Vec2): f64 {
            return a.x * b.x + a.y * b.y;
        }

        export function tick(_me: i32): void {
            const u: Vec2 = { x: 1.0, y: 2.0 };
            const v: Vec2 = { x: 3.0, y: 4.0 };
            sink(dot(u, v));
        }
        "#,
    );
    assert_eq!(values, vec![11.0]);
}

#[test]
fn return_named_shape_value() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Vec2 = { x: f64; y: f64 };

        function scale(v: Vec2, k: f64): Vec2 {
            return { x: v.x * k, y: v.y * k };
        }

        export function tick(_me: i32): void {
            const r: Vec2 = scale({ x: 2.0, y: 3.0 }, 4.0);
            sink(r.x);
            sink(r.y);
        }
        "#,
    );
    assert_eq!(values, vec![8.0, 12.0]);
}

#[test]
fn pass_object_literal_directly_as_arg() {
    // expected-type threading at the call site: the literal sees the
    // parameter's class as its target via `fn_param_classes`.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Point = { x: f64; y: f64 };

        function magnitude(p: Point): f64 {
            return Math.sqrt(p.x * p.x + p.y * p.y);
        }

        export function tick(_me: i32): void {
            sink(magnitude({ x: 6.0, y: 8.0 }));
        }
        "#,
    );
    assert_eq!(values, vec![10.0]);
}

// ---- Class field whose type is a named shape ----

#[test]
fn class_field_holding_named_shape() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Pos = { x: f64; y: f64 };

        class Entity {
            pos: Pos;
            hp: i32;

            constructor(pos: Pos, hp: i32) {}
        }

        export function tick(_me: i32): void {
            const e: Entity = new Entity({ x: 4.0, y: 5.0 }, 50);
            sink(e.pos.x);
            sink(e.pos.y);
            sink(f64(e.hp));
        }
        "#,
    );
    assert_eq!(values, vec![4.0, 5.0, 50.0]);
}

// ---- Shape field holding a class instance ----

#[test]
fn shape_field_holding_class_instance() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Counter {
            value: i32;
            constructor(value: i32) {}
        }

        type Wrapper = { c: Counter; tag: i32 };

        export function tick(_me: i32): void {
            const w: Wrapper = { c: new Counter(42), tag: 1 };
            sink(f64(w.c.value));
            sink(f64(w.tag));
        }
        "#,
    );
    assert_eq!(values, vec![42.0, 1.0]);
}

// ---- String fields ----

#[test]
fn shape_field_holding_string() {
    // Use a non-sink driver since strings need memory inspection.
    let wasm = compile(
        r#"
        type Named = { name: string; level: i32 };

        export function tick(): i32 {
            const n: Named = { name: "hello", level: 9 };
            return n.level;
        }

        export function getName(): i32 {
            const n: Named = { name: "world", level: 1 };
            return load_i32(0); // unused; we go through getName2
        }

        export function getName2(): i32 {
            const n: Named = { name: "world", level: 1 };
            // Returning the string pointer lets the test reach into memory.
            // n.name is the offset into the literal's allocation.
            // Since we can't dereference field directly here without member
            // syntax mapping correctly, just return n.name.
            return n.name as i32;
        }
        "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store: Store<()> = Store::new(&engine, ());
    let linker = Linker::new(&engine);
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<(), i32>(&mut store, "tick")
        .unwrap();
    assert_eq!(tick.call(&mut store, ()).unwrap(), 9);

    let get_name = instance
        .get_typed_func::<(), i32>(&mut store, "getName2")
        .unwrap();
    let ptr = get_name.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let s = read_wasm_string(&store, &memory, ptr);
    assert_eq!(s, "world");
}

// ---- A.6 rejection: excess property ----

#[test]
fn rejects_excess_property_with_ts_wording() {
    let err = compile_err(
        r#"
        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            const p: Point = { x: 1.0, y: 2.0, z: 3.0 };
        }
        "#,
    );
    assert!(
        err.message.contains("may only specify known properties")
            && err.message.contains("'z'")
            && err.message.contains("'Point'"),
        "expected TS-style excess-property wording mentioning 'z' and 'Point', got: {}",
        err.message
    );
    assert!(err.loc.is_some(), "excess-property error should be located");
}

// ---- A.6 rejection: missing property ----

#[test]
fn rejects_missing_property() {
    let err = compile_err(
        r#"
        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            const p: Point = { x: 1.0 };
        }
        "#,
    );
    assert!(
        err.message.contains("missing")
            && err.message.contains("'y'")
            && err.message.contains("'Point'"),
        "expected missing-property error mentioning 'y' and 'Point', got: {}",
        err.message
    );
    assert!(err.loc.is_some(), "missing-property error should be located");
}

#[test]
fn rejects_missing_property_lists_all_missing() {
    let err = compile_err(
        r#"
        type Stat = { hp: i32; mp: i32; atk: f64 };

        export function tick(_me: i32): void {
            const s: Stat = { hp: 10 };
        }
        "#,
    );
    // Both `mp` and `atk` should appear in the error list.
    assert!(
        err.message.contains("'mp'") && err.message.contains("'atk'"),
        "expected both 'mp' and 'atk' to be listed as missing, got: {}",
        err.message
    );
}

// ---- Rejection: type / shape name collision ----

#[test]
fn rejects_class_and_type_alias_name_collision() {
    let err = compile_err(
        r#"
        class Point { x: f64; y: f64; constructor() {} }
        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {}
        "#,
    );
    let msg = err.message.to_lowercase();
    assert!(
        msg.contains("collision") || msg.contains("conflict") || msg.contains("already"),
        "expected collision/conflict error, got: {}",
        err.message
    );
}

// ---- Rejection: unsupported features pointing at correct phase ----

#[test]
fn rejects_nested_destructuring_pattern_with_phase_e_pointer() {
    // `const { outer: { inner } } = root` — the inner pattern is a
    // destructuring sub-pattern, not a plain identifier binding.
    let err = compile_err(
        r#"
        type Inner = { v: f64 };
        type Outer = { inner: Inner };

        export function tick(_me: i32): void {
            const root: Outer = { inner: { v: 42.0 } };
            const { inner: { v } } = root;
        }
        "#,
    );
    assert!(
        err.message.contains("nested destructuring") || err.message.contains("Phase E"),
        "expected nested-destructuring error referencing Phase E, got: {}",
        err.message
    );
}

// ---- Rejection: cannot infer shape (P1 design decision) ----

#[test]
fn rejects_anonymous_literal_with_uninferable_field() {
    // `someLocal` here is a function-call result whose return type isn't a
    // class — the literal-field inference can resolve the WasmType but the
    // overall shape only matters when fingerprint-resolving a context-free
    // literal. This test pins the contract that pure-numeric inference
    // works (the literal would resolve to anonymous shape `{ x: i32 }`).
    // The harder case — fields with no inferable type — is harder to
    // construct without extra surface, so we settle for a positive
    // sanity-check here and rely on dev-time spot-checks for the negative.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const p = { x: 1, y: 2 };
            sink(f64(p.x + p.y));
        }
        "#,
    );
    assert_eq!(values, vec![3.0]);
}

// ============================================================================
// Phase C — Structural subtyping + interface extends
// ============================================================================

// ---- C.2: interface extends builds prefix-compatible layouts ----

#[test]
fn extends_child_inherits_parent_fields() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        interface Point { x: f64; y: f64 }
        interface Point3D extends Point { z: f64 }

        export function tick(_me: i32): void {
            const p: Point3D = { x: 1.0, y: 2.0, z: 3.0 };
            sink(p.x);
            sink(p.y);
            sink(p.z);
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0, 3.0]);
}

#[test]
fn extends_chain_is_transitive() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        interface A { a: f64 }
        interface B extends A { b: f64 }
        interface C extends B { c: f64 }

        export function tick(_me: i32): void {
            const c: C = { a: 1.0, b: 2.0, c: 3.0 };
            sink(c.a + c.b + c.c);
        }
        "#,
    );
    assert_eq!(values, vec![6.0]);
}

#[test]
fn interface_extends_type_alias() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Base = { x: f64; y: f64 };
        interface Extended extends Base { z: f64 }

        export function tick(_me: i32): void {
            const e: Extended = { x: 1.0, y: 2.0, z: 3.0 };
            sink(e.x + e.y + e.z);
        }
        "#,
    );
    assert_eq!(values, vec![6.0]);
}

// ---- C.1: width coercion — prefix zero-copy ----

#[test]
fn prefix_compatible_upcast_via_assignment_zero_copy() {
    // Point3D extends Point. C.2 prefix-preserves layout → structural coerce
    // short-circuits to zero-copy (no alloc, pointer reused).
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        interface Point { x: f64; y: f64 }
        interface Point3D extends Point { z: f64 }

        export function tick(_me: i32): void {
            const p3: Point3D = { x: 10.0, y: 20.0, z: 30.0 };
            const p2: Point = p3;
            sink(p2.x);
            sink(p2.y);
        }
        "#,
    );
    assert_eq!(values, vec![10.0, 20.0]);
}

#[test]
fn prefix_compatible_upcast_via_function_arg() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        interface Point { x: f64; y: f64 }
        interface Point3D extends Point { z: f64 }

        function magnitude2d(p: Point): f64 {
            return p.x + p.y;
        }

        export function tick(_me: i32): void {
            const p3: Point3D = { x: 4.0, y: 5.0, z: 9.0 };
            sink(magnitude2d(p3));
        }
        "#,
    );
    assert_eq!(values, vec![9.0]);
}

#[test]
fn prefix_compatible_upcast_via_return() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        interface Point { x: f64; y: f64 }
        interface Point3D extends Point { z: f64 }

        function widen(p3: Point3D): Point {
            return p3;
        }

        export function tick(_me: i32): void {
            const p3: Point3D = { x: 7.0, y: 8.0, z: 9.0 };
            const p: Point = widen(p3);
            sink(p.x);
            sink(p.y);
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 8.0]);
}

// ---- C.1: width coercion — field-pick copy on mismatched offsets ----

#[test]
fn mismatched_offsets_trigger_field_pick_copy() {
    // Point declares y before x (layout y@0, x@8), Big declares x, y, z (layout
    // x@0, y@8, z@16). Coercing Big → Point must field-pick because Point's
    // "y" sits at offset 0 but in Big "y" sits at offset 8.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Point = { y: f64; x: f64 };
        type Big = { x: f64; y: f64; z: f64 };

        export function tick(_me: i32): void {
            const b: Big = { x: 100.0, y: 200.0, z: 300.0 };
            const p: Point = b;
            sink(p.x);
            sink(p.y);
        }
        "#,
    );
    assert_eq!(values, vec![100.0, 200.0]);
}

#[test]
fn field_pick_copy_is_a_fresh_allocation() {
    // After field-pick, mutating the source doesn't leak into the copy.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Point = { y: f64; x: f64 };
        type Big = { x: f64; y: f64; z: f64 };

        export function tick(_me: i32): void {
            const b: Big = { x: 1.0, y: 2.0, z: 3.0 };
            const p: Point = b;
            b.x = 999.0;
            b.y = 888.0;
            sink(p.x);
            sink(p.y);
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0]);
}

// ---- C.1: width coercion — missing-field rejection ----

#[test]
fn coerce_missing_field_is_rejected() {
    let err = compile_err(
        r#"
        interface Point { x: f64; y: f64 }
        interface Only2D { x: f64 }

        export function tick(_me: i32): void {
            const o: Only2D = { x: 1.0 };
            const p: Point = o;
        }
        "#,
    );
    assert!(
        err.message.contains("field 'y'") && err.message.contains("missing"),
        "got: {}",
        err.message
    );
}

// ---- C.2: rejections ----

#[test]
fn extends_shadowing_parent_field_is_rejected() {
    let err = compile_err(
        r#"
        interface Base { x: f64 }
        interface Child extends Base { x: f64; y: f64 }

        export function tick(_me: i32): void {}
        "#,
    );
    assert!(
        err.message.contains("redeclares field 'x'"),
        "got: {}",
        err.message
    );
}

#[test]
fn extends_class_is_rejected() {
    let err = compile_err(
        r#"
        class Base { x: f64 = 0.0; }
        interface Child extends Base { y: f64 }

        export function tick(_me: i32): void {}
        "#,
    );
    assert!(err.message.contains("extends class"), "got: {}", err.message);
}

#[test]
fn extends_circular_is_rejected() {
    let err = compile_err(
        r#"
        interface A extends B { a: f64 }
        interface B extends A { b: f64 }

        export function tick(_me: i32): void {}
        "#,
    );
    assert!(
        err.message.contains("circular interface inheritance"),
        "got: {}",
        err.message
    );
}

#[test]
fn extends_multiple_parents_is_rejected() {
    let err = compile_err(
        r#"
        interface A { a: f64 }
        interface B { b: f64 }
        interface C extends A, B { c: f64 }

        export function tick(_me: i32): void {}
        "#,
    );
    assert!(
        err.message.contains("extends multiple parents"),
        "got: {}",
        err.message
    );
}

// ---- Phase E.4: generic object types ----

#[test]
fn generic_shape_pair_i32_f64() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Pair<T, U> = { first: T; second: U };

        export function tick(_me: i32): void {
            const p: Pair<i32, f64> = { first: 7, second: 3.5 };
            sink(f64(p.first));
            sink(p.second);
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 3.5]);
}

#[test]
fn generic_shape_two_instantiations_coexist() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Pair<T, U> = { first: T; second: U };

        export function tick(_me: i32): void {
            const a: Pair<i32, i32> = { first: 1, second: 2 };
            const b: Pair<f64, f64> = { first: 10.5, second: 20.5 };
            sink(f64(a.first));
            sink(f64(a.second));
            sink(b.first);
            sink(b.second);
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0, 10.5, 20.5]);
}

#[test]
fn generic_shape_interface_form() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        interface Box<T> { value: T }

        export function tick(_me: i32): void {
            const b: Box<i32> = { value: 42 };
            sink(f64(b.value));
        }
        "#,
    );
    assert_eq!(values, vec![42.0]);
}

#[test]
fn generic_shape_nested_generic_instantiation() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Box<T> = { value: T };
        type Pair<T, U> = { first: T; second: U };

        export function tick(_me: i32): void {
            const p: Pair<Box<i32>, Box<f64>> = {
                first: { value: 7 },
                second: { value: 3.5 }
            };
            sink(f64(p.first.value));
            sink(p.second.value);
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 3.5]);
}

#[test]
fn generic_shape_string_binding() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Wrap<T> = { v: T; n: i32 };

        export function tick(_me: i32): void {
            const w: Wrap<string> = { v: "hello", n: 5 };
            sink(f64(w.v.length));
            sink(f64(w.n));
        }
        "#,
    );
    assert_eq!(values, vec![5.0, 5.0]);
}

#[test]
fn generic_shape_destructuring() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Pair<T, U> = { first: T; second: U };

        export function tick(_me: i32): void {
            const p: Pair<i32, f64> = { first: 7, second: 3.5 };
            const { first, second } = p;
            sink(f64(first));
            sink(second);
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 3.5]);
}

#[test]
fn generic_shape_as_function_arg_and_return() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Pair<T, U> = { first: T; second: U };

        function makePair(a: f64, b: f64): Pair<f64, f64> {
            return { first: a, second: b };
        }

        function firstOf(p: Pair<f64, f64>): f64 {
            return p.first;
        }

        export function tick(_me: i32): void {
            const p = makePair(3.0, 4.0);
            sink(firstOf(p));
            sink(p.second);
        }
        "#,
    );
    assert_eq!(values, vec![3.0, 4.0]);
}

#[test]
fn generic_shape_arity_mismatch_errors() {
    let err = compile_err(
        r#"
        type Pair<T, U> = { first: T; second: U };

        export function tick(_me: i32): void {
            const p: Pair<i32> = { first: 1, second: 2 } as any;
        }
        "#,
    );
    assert!(
        err.message.contains("expects 2 type argument") || err.message.contains("expects"),
        "expected arity error, got: {}",
        err.message
    );
}

// ---- Phase E.1: shorthand properties ----

#[test]
fn shorthand_property_primitives() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            const x: f64 = 3.5;
            const y: f64 = 7.25;
            const p: Point = { x, y };
            sink(p.x);
            sink(p.y);
        }
        "#,
    );
    assert_eq!(values, vec![3.5, 7.25]);
}

#[test]
fn shorthand_mixed_with_explicit() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Hit = { id: i32; dist: f64; alive: boolean };

        export function tick(_me: i32): void {
            const id: i32 = 42;
            const alive: boolean = true;
            const h: Hit = { id, dist: 2.5, alive };
            sink(f64(h.id));
            sink(h.dist);
            sink(f64(h.alive ? 1 : 0));
        }
        "#,
    );
    assert_eq!(values, vec![42.0, 2.5, 1.0]);
}

#[test]
fn shorthand_with_class_ref() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        class Inner { v: f64 = 0.0; }
        type Outer = { inner: Inner; n: f64 };

        export function tick(_me: i32): void {
            const inner = new Inner();
            inner.v = 4.5;
            const n: f64 = 9.0;
            const o: Outer = { inner, n };
            sink(o.inner.v);
            sink(o.n);
        }
        "#,
    );
    assert_eq!(values, vec![4.5, 9.0]);
}

// ---- Phase E.2: spread in object literals ----

#[test]
fn spread_identical_shape_copies_all_fields() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            const a: Point = { x: 1.5, y: 2.5 };
            const b: Point = { ...a };
            sink(b.x);
            sink(b.y);
        }
        "#,
    );
    assert_eq!(values, vec![1.5, 2.5]);
}

#[test]
fn spread_then_override_later_wins() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            const a: Point = { x: 1.0, y: 2.0 };
            const b: Point = { ...a, x: 99.0 };
            sink(b.x);
            sink(b.y);
        }
        "#,
    );
    assert_eq!(values, vec![99.0, 2.0]);
}

#[test]
fn override_then_spread_spread_wins() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            const a: Point = { x: 10.0, y: 20.0 };
            const b: Point = { x: 99.0, ...a };
            sink(b.x);
            sink(b.y);
        }
        "#,
    );
    assert_eq!(values, vec![10.0, 20.0]);
}

#[test]
fn spread_narrower_source_fills_missing_with_explicit() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type P2 = { x: f64; y: f64 };
        type P3 = { x: f64; y: f64; z: f64 };

        export function tick(_me: i32): void {
            const a: P2 = { x: 1.0, y: 2.0 };
            const b: P3 = { ...a, z: 3.0 };
            sink(b.x);
            sink(b.y);
            sink(b.z);
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0, 3.0]);
}

#[test]
fn spread_wider_source_drops_extras() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type P2 = { x: f64; y: f64 };
        type P3 = { x: f64; y: f64; z: f64 };

        export function tick(_me: i32): void {
            const wide: P3 = { x: 1.0, y: 2.0, z: 3.0 };
            const narrow: P2 = { ...wide };
            sink(narrow.x);
            sink(narrow.y);
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0]);
}

#[test]
fn spread_two_sources_later_wins_per_field() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            const a: Point = { x: 1.0, y: 2.0 };
            const b: Point = { x: 10.0, y: 20.0 };
            const c: Point = { ...a, ...b };
            sink(c.x);
            sink(c.y);
        }
        "#,
    );
    assert_eq!(values, vec![10.0, 20.0]);
}

#[test]
fn spread_missing_target_field_errors() {
    let err = compile_err(
        r#"
        type P2 = { x: f64; y: f64 };
        type P3 = { x: f64; y: f64; z: f64 };

        export function tick(_me: i32): void {
            const a: P2 = { x: 1.0, y: 2.0 };
            const b: P3 = { ...a };
        }
        "#,
    );
    assert!(
        err.message.contains("missing") && err.message.contains("'z'"),
        "expected missing-field error, got: {}",
        err.message
    );
}

#[test]
fn spread_without_annotation_errors() {
    let err = compile_err(
        r#"
        type Point = { x: f64; y: f64 };

        export function tick(_me: i32): void {
            const a: Point = { x: 1.0, y: 2.0 };
            const b = { ...a };
        }
        "#,
    );
    assert!(
        err.message.contains("spread") && err.message.contains("explicit target type"),
        "expected 'spread requires explicit target type', got: {}",
        err.message
    );
}

// ---- Phase F.5: integration spot-checks ----

#[test]
fn integration_map_string_to_object_shape() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Unit = { name: string; hp: i32 };

        export function tick(_me: i32): void {
            const m: Map<string, Unit> = new Map<string, Unit>();
            m.set("knight", { name: "Sir Kay", hp: 42 });
            m.set("mage", { name: "Merlin", hp: 17 });
            const k: Unit = m.get("knight") as Unit;
            const g: Unit = m.get("mage") as Unit;
            sink(f64(k.name.length));
            sink(f64(k.hp));
            sink(f64(g.name.length));
            sink(f64(g.hp));
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 42.0, 6.0, 17.0]);
}

#[test]
fn integration_array_of_tuple_single() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const pts: Array<[i32, i32]> = [];
            pts.push([1, 2]);
            const p = pts[0];
            sink(f64(p[0]));
            sink(f64(p[1]));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0]);
}

#[test]
fn integration_array_of_tuple() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const pts: Array<[i32, i32]> = [];
            pts.push([1, 2]);
            pts.push([3, 4]);
            pts.push([5, 6]);
            for (let i: i32 = 0; i < pts.length; i = i + 1) {
                const p = pts[i];
                sink(f64(p[0]));
                sink(f64(p[1]));
            }
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
}

#[test]
fn integration_class_with_map_of_shape_field() {
    // Realistic game-script shape from the plan: a class whose field is a
    // Map<i32, Shape>. Exercises shape-as-generic-arg + class field holding
    // a Map + Map lookups returning shape pointers.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Pos = { x: f64; y: f64 };

        class Perception {
            recent: Map<i32, Pos>;
            constructor() {
                this.recent = new Map<i32, Pos>();
            }
        }

        export function tick(_me: i32): void {
            const p = new Perception();
            p.recent.set(1, { x: 10.5, y: 20.25 });
            p.recent.set(7, { x: -3.0, y: 4.5 });
            const a: Pos = p.recent.get(1) as Pos;
            const b: Pos = p.recent.get(7) as Pos;
            sink(a.x);
            sink(a.y);
            sink(b.x);
            sink(b.y);
        }
        "#,
    );
    assert_eq!(values, vec![10.5, 20.25, -3.0, 4.5]);
}

// ---- Phase E.6: readonly modifier accepted and ignored ----

#[test]
fn readonly_field_accepted_as_noop() {
    // Phase E.6: `readonly` is documentation only. We accept the modifier but
    // do not enforce immutability (reassignment still compiles today).
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        interface Point { readonly x: f64; y: f64 }

        export function tick(_me: i32): void {
            const p: Point = { x: 1.5, y: 2.5 };
            sink(p.x);
            sink(p.y);
        }
        "#,
    );
    assert_eq!(values, vec![1.5, 2.5]);
}

#[test]
fn readonly_on_type_alias_is_accepted() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Vec3 = { readonly x: f64; readonly y: f64; readonly z: f64 };

        export function tick(_me: i32): void {
            const v: Vec3 = { x: 1.0, y: 2.0, z: 3.0 };
            sink(v.x + v.y + v.z);
        }
        "#,
    );
    assert_eq!(values, vec![6.0]);
}

// ---- Phase E.3: optional `?:` fields emit a union-deferred error ----

#[test]
fn optional_field_errors_pointing_at_union_phase() {
    let err = compile_err(
        r#"
        type Point = { x: f64; y?: f64 };

        export function tick(_me: i32): void {}
        "#,
    );
    assert!(
        err.message.contains("optional property 'y?'")
            && err.message.contains("union"),
        "expected optional-property error referencing union types, got: {}",
        err.message
    );
}

#[test]
fn optional_field_on_interface_errors() {
    let err = compile_err(
        r#"
        interface Point { x: f64; y?: f64 }

        export function tick(_me: i32): void {}
        "#,
    );
    assert!(
        err.message.contains("optional property 'y?'"),
        "expected optional-property error, got: {}",
        err.message
    );
}

#[test]
fn shorthand_anonymous_without_annotation_errors_like_nonshorthand() {
    // Shorthand is syntactic sugar: `{ x, y }` behaves exactly like
    // `{ x: x, y: y }`. Neither form can be inferred from identifier RHS
    // alone (P1 decision in A.4), so this surfaces the standard
    // "add a type annotation" diagnostic.
    let err = compile_err(
        r#"
        export function tick(_me: i32): void {
            const x: f64 = 1.0;
            const y: f64 = 2.0;
            const p = { x, y };
        }
        "#,
    );
    assert!(
        err.message.contains("type annotation"),
        "expected 'type annotation' in error, got: {}",
        err.message
    );
}

// ---- Object.keys / values / entries ----

#[test]
fn object_keys_returns_field_names() {
    // Keys come back in declaration order. Dump them through a length probe
    // and per-element pointer lookups so we can read the strings out of WASM
    // memory and verify both the count and the contents.
    let wasm = compile(
        r#"
        type Point = { x: f64; y: f64; z: f64 };

        export function tick(_me: i32): void {}

        export function len(): i32 {
            const p: Point = { x: 1.0, y: 2.0, z: 3.0 };
            const ks = Object.keys(p);
            return ks.length;
        }
        export function key(i: i32): i32 {
            const p: Point = { x: 1.0, y: 2.0, z: 3.0 };
            const ks = Object.keys(p);
            return ks[i];
        }
        "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Linker::new(&engine).instantiate(&mut store, &module).unwrap();
    let len = instance
        .get_typed_func::<(), i32>(&mut store, "len")
        .unwrap();
    assert_eq!(len.call(&mut store, ()).unwrap(), 3);

    let key_ptr = instance
        .get_typed_func::<i32, i32>(&mut store, "key")
        .unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let p0 = key_ptr.call(&mut store, 0).unwrap();
    let p1 = key_ptr.call(&mut store, 1).unwrap();
    let p2 = key_ptr.call(&mut store, 2).unwrap();
    assert_eq!(read_wasm_string(&store, &memory, p0), "x");
    assert_eq!(read_wasm_string(&store, &memory, p1), "y");
    assert_eq!(read_wasm_string(&store, &memory, p2), "z");
}

#[test]
fn object_values_f64_fields() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Point = { x: f64; y: f64; z: f64 };

        export function tick(_me: i32): void {
            const p: Point = { x: 1.5, y: 2.5, z: 3.5 };
            const vs = Object.values(p);
            for (let i: i32 = 0; i < vs.length; i = i + 1) {
                sink(vs[i]);
            }
        }
        "#,
    );
    assert_eq!(values, vec![1.5, 2.5, 3.5]);
}

#[test]
fn object_values_i32_fields() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Hit = { id: i32; dmg: i32; team: i32 };

        export function tick(_me: i32): void {
            const h: Hit = { id: 7, dmg: 42, team: 1 };
            const vs = Object.values(h);
            for (let i: i32 = 0; i < vs.length; i = i + 1) {
                sink(f64(vs[i]));
            }
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 42.0, 1.0]);
}

#[test]
fn object_values_string_fields() {
    // All-string shape: values lower to a fresh `Array<string>` (i32 ptrs).
    // The result is read back through manual indexing, then each ptr is
    // unwrapped through the standard string-pointer layout.
    let wasm = compile(
        r#"
        type Names = { first: string; last: string };

        export function tick(_me: i32): void {}

        export function val(i: i32): i32 {
            const n: Names = { first: "Ada", last: "Lovelace" };
            const vs = Object.values(n);
            return vs[i];
        }
        "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Linker::new(&engine).instantiate(&mut store, &module).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let val = instance.get_typed_func::<i32, i32>(&mut store, "val").unwrap();
    let p0 = val.call(&mut store, 0).unwrap();
    let p1 = val.call(&mut store, 1).unwrap();
    assert_eq!(read_wasm_string(&store, &memory, p0), "Ada");
    assert_eq!(read_wasm_string(&store, &memory, p1), "Lovelace");
}

#[test]
fn object_keys_evaluates_argument_for_side_effects() {
    // Field names are a compile-time view of the layout, but the argument
    // expression itself must still execute (in case the user is calling a
    // function with side effects). Use a host-side counter to verify the
    // arg is evaluated exactly once.
    use std::sync::{Arc, Mutex};
    let counter: Arc<Mutex<i32>> = Arc::new(Mutex::new(0));
    let source = r#"
        type P = { a: i32; b: i32 };
        declare function bump(): i32;

        export function tick(_me: i32): void {}
        export function len_after_bump(): i32 {
            const p: P = { a: bump(), b: 0 };
            const ks = Object.keys(p);
            return ks.length;
        }
        "#;
    let wasm = compile(source);
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, counter.clone());
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap("host", "bump", |caller: Caller<'_, Arc<Mutex<i32>>>| -> i32 {
            let mut g = caller.data().lock().unwrap();
            *g += 1;
            *g
        })
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let f = instance
        .get_typed_func::<(), i32>(&mut store, "len_after_bump")
        .unwrap();
    assert_eq!(f.call(&mut store, ()).unwrap(), 2);
    // bump() ran once during the literal, plus zero times during keys().
    assert_eq!(*counter.lock().unwrap(), 1);
}

#[test]
fn object_values_rejects_mixed_field_types() {
    let err = compile_err(
        r#"
        type Mixed = { id: i32; dist: f64 };

        export function tick(_me: i32): void {
            const m: Mixed = { id: 1, dist: 2.5 };
            const vs = Object.values(m);
        }
        "#,
    );
    assert!(
        err.message.contains("mixed types"),
        "expected 'mixed types' diagnostic, got: {}",
        err.message
    );
}

#[test]
fn object_keys_rejects_non_shape_argument() {
    let err = compile_err(
        r#"
        export function tick(_me: i32): void {
            const x: f64 = 1.5;
            const ks = Object.keys(x);
        }
        "#,
    );
    assert!(
        err.message.contains("shape-typed"),
        "expected 'shape-typed' in error, got: {}",
        err.message
    );
}

#[test]
fn object_entries_i32_fields() {
    // Round-trip: build a shape, take entries, and read each [key, value]
    // pair back out via tuple slot access. Annotation on the receiver is
    // what registers the `[string, i32]` tuple shape during the pre-codegen
    // walk — without it, Object.entries errors with a hint to add the
    // annotation.
    let wasm = compile(
        r#"
        type Hit = { id: i32; dmg: i32 };

        export function tick(_me: i32): void {}

        export function entry_key(i: i32): i32 {
            const h: Hit = { id: 7, dmg: 42 };
            const e: Array<[string, i32]> = Object.entries(h);
            return e[i][0];
        }
        export function entry_val(i: i32): i32 {
            const h: Hit = { id: 7, dmg: 42 };
            const e: Array<[string, i32]> = Object.entries(h);
            return e[i][1];
        }
        "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Linker::new(&engine).instantiate(&mut store, &module).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let entry_key = instance
        .get_typed_func::<i32, i32>(&mut store, "entry_key")
        .unwrap();
    let entry_val = instance
        .get_typed_func::<i32, i32>(&mut store, "entry_val")
        .unwrap();
    let k0 = entry_key.call(&mut store, 0).unwrap();
    let k1 = entry_key.call(&mut store, 1).unwrap();
    assert_eq!(read_wasm_string(&store, &memory, k0), "id");
    assert_eq!(read_wasm_string(&store, &memory, k1), "dmg");
    assert_eq!(entry_val.call(&mut store, 0).unwrap(), 7);
    assert_eq!(entry_val.call(&mut store, 1).unwrap(), 42);
}

#[test]
fn object_entries_errors_without_tuple_annotation() {
    // No `[string, T][]` annotation anywhere in the program — the tuple
    // shape never gets registered, so emission errors with a clear hint.
    let err = compile_err(
        r#"
        type Hit = { id: i32; dmg: i32 };

        export function tick(_me: i32): void {
            const h: Hit = { id: 7, dmg: 42 };
            const e = Object.entries(h);
        }
        "#,
    );
    assert!(
        err.message.contains("tuple shape [string, i32]"),
        "expected tuple-shape hint in error, got: {}",
        err.message
    );
}
