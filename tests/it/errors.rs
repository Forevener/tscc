use super::common::compile_err;

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
