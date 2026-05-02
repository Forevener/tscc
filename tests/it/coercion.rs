//! Coercion constructors: `String(x)`, `Number(x)`, `Boolean(x)`. Cover the
//! per-WasmType dispatch paths, the literal fast-paths, the empty-call form,
//! and user-shadowing precedence.

use wasmtime::*;

use super::common::{compile, read_wasm_string};

fn run_string_export(source: &str, name: &str) -> String {
    let wasm = compile(source);
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let f = instance.get_typed_func::<(), i32>(&mut store, name).unwrap();
    let ptr = f.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    read_wasm_string(&store, &memory, ptr)
}

fn run_i32_export(source: &str, name: &str) -> i32 {
    let wasm = compile(source);
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let f = instance.get_typed_func::<(), i32>(&mut store, name).unwrap();
    f.call(&mut store, ()).unwrap()
}

fn run_f64_export(source: &str, name: &str) -> f64 {
    let wasm = compile(source);
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let f = instance.get_typed_func::<(), f64>(&mut store, name).unwrap();
    f.call(&mut store, ()).unwrap()
}

// ---------- String(x) ----------

#[test]
fn string_of_no_args_is_empty() {
    assert_eq!(
        run_string_export(
            r#"export function test(): i32 { return String(); }"#,
            "test"
        ),
        ""
    );
}

#[test]
fn string_of_string_is_identity() {
    assert_eq!(
        run_string_export(
            r#"export function test(): i32 {
                let s: string = "hello";
                return String(s);
            }"#,
            "test"
        ),
        "hello"
    );
}

#[test]
fn string_of_i32() {
    assert_eq!(
        run_string_export(
            r#"export function test(): i32 {
                let n: i32 = 42;
                return String(n);
            }"#,
            "test"
        ),
        "42"
    );
}

#[test]
fn string_of_f64() {
    assert_eq!(
        run_string_export(
            r#"export function test(): i32 {
                let x: f64 = 3.5;
                return String(x);
            }"#,
            "test"
        ),
        "3.5"
    );
}

#[test]
fn string_of_boolean_literal_true() {
    assert_eq!(
        run_string_export(
            r#"export function test(): i32 { return String(true); }"#,
            "test"
        ),
        "true"
    );
}

#[test]
fn string_of_boolean_literal_false() {
    assert_eq!(
        run_string_export(
            r#"export function test(): i32 { return String(false); }"#,
            "test"
        ),
        "false"
    );
}

#[test]
fn string_of_comparison_expression() {
    // Detectable bool expression — branch between "true"/"false" statics.
    assert_eq!(
        run_string_export(
            r#"export function test(): i32 {
                let a: i32 = 1;
                let b: i32 = 2;
                return String(a < b);
            }"#,
            "test"
        ),
        "true"
    );
}

#[test]
fn string_of_logical_not() {
    assert_eq!(
        run_string_export(
            r#"export function test(): i32 { return String(!false); }"#,
            "test"
        ),
        "true"
    );
}

#[test]
fn string_of_negative_f64() {
    assert_eq!(
        run_string_export(
            r#"export function test(): i32 {
                let x: f64 = -2.25;
                return String(x);
            }"#,
            "test"
        ),
        "-2.25"
    );
}

// ---------- Number(x) ----------

#[test]
fn number_of_no_args_is_zero() {
    assert_eq!(
        run_f64_export(
            r#"export function test(): f64 { return Number(); }"#,
            "test"
        ),
        0.0
    );
}

#[test]
fn number_of_string() {
    assert_eq!(
        run_f64_export(
            r#"export function test(): f64 { return Number("3.14"); }"#,
            "test"
        ),
        3.14
    );
}

#[test]
fn number_of_string_unparseable_is_nan() {
    assert!(
        run_f64_export(
            r#"export function test(): f64 { return Number("abc"); }"#,
            "test"
        )
        .is_nan()
    );
}

#[test]
fn number_of_i32() {
    assert_eq!(
        run_f64_export(
            r#"export function test(): f64 {
                let n: i32 = -7;
                return Number(n);
            }"#,
            "test"
        ),
        -7.0
    );
}

#[test]
fn number_of_f64_is_identity() {
    assert_eq!(
        run_f64_export(
            r#"export function test(): f64 {
                let x: f64 = 1.5;
                return Number(x);
            }"#,
            "test"
        ),
        1.5
    );
}

#[test]
fn number_of_string_literal() {
    assert_eq!(
        run_f64_export(
            r#"export function test(): f64 { return Number("42"); }"#,
            "test"
        ),
        42.0
    );
}

// ---------- Boolean(x) ----------

#[test]
fn boolean_of_no_args_is_false() {
    assert_eq!(
        run_i32_export(
            r#"export function test(): i32 { return Boolean(); }"#,
            "test"
        ),
        0
    );
}

#[test]
fn boolean_of_nonempty_string_is_true() {
    assert_eq!(
        run_i32_export(
            r#"export function test(): i32 { return Boolean("hi"); }"#,
            "test"
        ),
        1
    );
}

#[test]
fn boolean_of_empty_string_is_false() {
    assert_eq!(
        run_i32_export(
            r#"export function test(): i32 { return Boolean(""); }"#,
            "test"
        ),
        0
    );
}

#[test]
fn boolean_of_nonzero_i32_is_true() {
    assert_eq!(
        run_i32_export(
            r#"export function test(): i32 {
                let n: i32 = 7;
                return Boolean(n);
            }"#,
            "test"
        ),
        1
    );
}

#[test]
fn boolean_of_zero_i32_is_false() {
    assert_eq!(
        run_i32_export(
            r#"export function test(): i32 {
                let n: i32 = 0;
                return Boolean(n);
            }"#,
            "test"
        ),
        0
    );
}

#[test]
fn boolean_of_nonzero_f64_is_true() {
    assert_eq!(
        run_i32_export(
            r#"export function test(): i32 { return Boolean(2.5); }"#,
            "test"
        ),
        1
    );
}

#[test]
fn boolean_of_zero_f64_is_false() {
    assert_eq!(
        run_i32_export(
            r#"export function test(): i32 { return Boolean(0.0); }"#,
            "test"
        ),
        0
    );
}

#[test]
fn boolean_of_negative_zero_f64_is_false() {
    assert_eq!(
        run_i32_export(
            r#"export function test(): i32 {
                let x: f64 = -0.0;
                return Boolean(x);
            }"#,
            "test"
        ),
        0
    );
}

#[test]
fn boolean_of_nan_is_false() {
    assert_eq!(
        run_i32_export(
            r#"export function test(): i32 { return Boolean(NaN); }"#,
            "test"
        ),
        0
    );
}

#[test]
fn boolean_of_negative_f64_is_true() {
    assert_eq!(
        run_i32_export(
            r#"export function test(): i32 {
                let x: f64 = -1.5;
                return Boolean(x);
            }"#,
            "test"
        ),
        1
    );
}

// ---------- User shadowing ----------

#[test]
fn user_function_shadows_string_builtin() {
    // A user-declared `function String(...)` takes precedence over the
    // coercion constructor — same shadow rule the rest of the dispatcher
    // observes.
    assert_eq!(
        run_i32_export(
            r#"
            function String(x: i32): i32 { return x + 1; }
            export function test(): i32 { return String(41); }
            "#,
            "test"
        ),
        42
    );
}

#[test]
fn user_function_shadows_number_builtin() {
    assert_eq!(
        run_f64_export(
            r#"
            function Number(x: f64): f64 { return x * 2.0; }
            export function test(): f64 { return Number(3.0); }
            "#,
            "test"
        ),
        6.0
    );
}

#[test]
fn user_function_shadows_boolean_builtin() {
    assert_eq!(
        run_i32_export(
            r#"
            function Boolean(x: i32): i32 { return x - 1; }
            export function test(): i32 { return Boolean(11); }
            "#,
            "test"
        ),
        10
    );
}

// ---------- Composition ----------

#[test]
fn coercion_round_trip() {
    // Number(String(n)) == n (within parseFloat's idempotence on integer
    // representations).
    assert_eq!(
        run_f64_export(
            r#"export function test(): f64 {
                let n: i32 = 123;
                return Number(String(n));
            }"#,
            "test"
        ),
        123.0
    );
}

#[test]
fn boolean_used_as_condition() {
    assert_eq!(
        run_i32_export(
            r#"export function test(): i32 {
                if (Boolean("hello")) { return 100; }
                return 0;
            }"#,
            "test"
        ),
        100
    );
}
