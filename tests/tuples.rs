//! Phase D tuple tests (D.7).
//!
//! Coverage matches the master plan's D.7 checklist: heterogeneous shapes
//! `[i32, f64]`, `[string, i32]`, `[bool, bool, bool]`; tuple return
//! (minMax); destructuring; `t[0]` / `t[1]` access; nested
//! `[[i32, i32], string]`; rejection of variable index and shape
//! disagreements; plus the function-arg and `Array<[i32, i32]>` flows.

mod common;

use common::{compile_err, run_sink_tick};

// ---- Literals + indexed access ----

#[test]
fn tuple_i32_f64() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const t: [i32, f64] = [7, 1.5];
            sink(f64(t[0]));
            sink(t[1]);
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 1.5]);
}

#[test]
fn tuple_string_i32_reads_fields_by_index() {
    // String slots live in i32 pointer land; reading `.length` after indexing
    // sanity-checks that `t[0]` returns the right slot type.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const t: [string, i32] = ["hello", 42];
            sink(f64(t[0].length));
            sink(f64(t[1]));
        }
        "#,
    );
    assert_eq!(values, vec![5.0, 42.0]);
}

#[test]
fn tuple_three_bools() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const t: [boolean, boolean, boolean] = [true, false, true];
            sink(f64(t[0] ? 1 : 0));
            sink(f64(t[1] ? 1 : 0));
            sink(f64(t[2] ? 1 : 0));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 0.0, 1.0]);
}

#[test]
fn tuple_literal_order_matters() {
    // `[i32, f64]` and `[f64, i32]` register as distinct shapes because
    // tuples have positional identity. This test exercises both so the
    // two layouts coexist.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const a: [i32, f64] = [1, 2.5];
            const b: [f64, i32] = [3.5, 4];
            sink(f64(a[0]));
            sink(a[1]);
            sink(b[0]);
            sink(f64(b[1]));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.5, 3.5, 4.0]);
}

// ---- Function signatures ----

#[test]
fn tuple_return_from_function() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function minMax(arr: f64[]): [f64, f64] {
            let lo: f64 = arr[0];
            let hi: f64 = arr[0];
            for (let i: i32 = 1; i < arr.length; i++) {
                if (arr[i] < lo) { lo = arr[i]; }
                if (arr[i] > hi) { hi = arr[i]; }
            }
            return [lo, hi];
        }

        export function tick(_me: i32): void {
            const data: f64[] = [5.0, 2.0, 9.0, 1.0, 7.0];
            const mm = minMax(data);
            sink(mm[0]);
            sink(mm[1]);
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 9.0]);
}

#[test]
fn tuple_as_function_parameter() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function magnitude(p: [f64, f64]): f64 {
            return p[0] * p[0] + p[1] * p[1];
        }

        export function tick(_me: i32): void {
            sink(magnitude([3.0, 4.0]));
        }
        "#,
    );
    assert_eq!(values, vec![25.0]);
}

#[test]
fn tuple_literal_pass_through_local() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function useIt(p: [i32, i32]): i32 { return p[0] + p[1]; }

        export function tick(_me: i32): void {
            const t: [i32, i32] = [3, 4];
            sink(f64(useIt(t)));
        }
        "#,
    );
    assert_eq!(values, vec![7.0]);
}

// ---- Destructuring ----

#[test]
fn tuple_destructuring_from_local() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const t: [i32, f64] = [10, 2.5];
            const [a, b] = t;
            sink(f64(a));
            sink(b);
        }
        "#,
    );
    assert_eq!(values, vec![10.0, 2.5]);
}

#[test]
fn tuple_destructuring_from_function_return() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function make(): [i32, f64] { return [3, 4.5]; }

        export function tick(_me: i32): void {
            const [a, b] = make();
            sink(f64(a));
            sink(b);
        }
        "#,
    );
    assert_eq!(values, vec![3.0, 4.5]);
}

#[test]
fn tuple_destructuring_with_hole() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const t: [i32, f64, i32] = [1, 2.5, 3];
            const [, b, c] = t;
            sink(b);
            sink(f64(c));
        }
        "#,
    );
    assert_eq!(values, vec![2.5, 3.0]);
}

#[test]
fn tuple_destructuring_partial_prefix_ok() {
    // Destructuring fewer elements than the arity is valid — the trailing
    // slots are simply unused.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const t: [i32, f64, boolean] = [9, 1.5, true];
            const [a, b] = t;
            sink(f64(a));
            sink(b);
        }
        "#,
    );
    assert_eq!(values, vec![9.0, 1.5]);
}

// ---- Nested and array-of-tuple ----

#[test]
fn nested_tuple_literal_and_access() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const t: [[i32, i32], string] = [[5, 6], "hi"];
            sink(f64(t[0][0]));
            sink(f64(t[0][1]));
            sink(f64(t[1].length));
        }
        "#,
    );
    assert_eq!(values, vec![5.0, 6.0, 2.0]);
}

#[test]
fn array_of_tuples() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const pairs: Array<[i32, i32]> = [[1, 2], [3, 4], [5, 6]];
            for (let i: i32 = 0; i < pairs.length; i++) {
                const p = pairs[i];
                sink(f64(p[0]));
                sink(f64(p[1]));
            }
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
}

// ---- Class field holding a tuple ----

#[test]
fn class_field_holding_tuple() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Box {
            pos: [f64, f64] = [0.0, 0.0];
            constructor() {}
            set(x: f64, y: f64): void {
                this.pos = [x, y];
            }
            emit(): void {
                sink(this.pos[0]);
                sink(this.pos[1]);
            }
        }

        export function tick(_me: i32): void {
            const b = new Box();
            b.set(7.5, -2.0);
            b.emit();
        }
        "#,
    );
    assert_eq!(values, vec![7.5, -2.0]);
}

// ---- Rejections ----

#[test]
fn variable_tuple_index_is_rejected() {
    let err = compile_err(
        r#"
        export function tick(_me: i32): void {
            const t: [i32, f64] = [1, 2.5];
            let i: i32 = 0;
            const x = t[i];
        }
        "#,
    );
    let msg = err.to_string();
    assert!(
        msg.contains("literal numeric index") || msg.contains("dynamic"),
        "got: {msg}"
    );
}

#[test]
fn out_of_bounds_tuple_index_is_rejected() {
    let err = compile_err(
        r#"
        export function tick(_me: i32): void {
            const t: [i32, f64] = [1, 2.5];
            const x = t[5];
        }
        "#,
    );
    assert!(
        err.to_string().contains("out of bounds"),
        "got: {}",
        err
    );
}

#[test]
fn tuple_literal_arity_mismatch_is_rejected() {
    let err = compile_err(
        r#"
        export function tick(_me: i32): void {
            const t: [i32, f64, i32] = [1, 2.5];
        }
        "#,
    );
    let msg = err.to_string();
    assert!(msg.contains("element") && msg.contains("expects"), "got: {msg}");
}

#[test]
fn tuple_slot_f64_receives_i32_widens_silently() {
    // Mirror the array-literal behavior: i32→f64 widens without a diagnostic.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const t: [i32, f64] = [1, 2];
            sink(f64(t[0]));
            sink(t[1]);
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0]);
}

#[test]
fn tuple_slot_i32_rejects_f64() {
    // The genuine mismatch: f64 RHS into i32 slot (no narrowing).
    let err = compile_err(
        r#"
        export function tick(_me: i32): void {
            const t: [i32, f64] = [1.5, 2.5];
        }
        "#,
    );
    let msg = err.to_string();
    assert!(
        msg.contains("expects") && msg.contains("I32"),
        "got: {msg}"
    );
}

// ---- Phase E.5: named tuple labels are accepted and discarded ----

#[test]
fn named_tuple_labels_work_like_bare_tuple() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const p: [x: i32, y: f64] = [7, 1.5];
            sink(f64(p[0]));
            sink(p[1]);
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 1.5]);
}

#[test]
fn named_tuple_and_bare_tuple_share_shape() {
    // A value typed as the bare form must assign into the named form — they
    // resolve to the same synthetic class.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function bare(): [i32, f64] {
            return [42, 2.5];
        }

        export function tick(_me: i32): void {
            const t: [a: i32, b: f64] = bare();
            sink(f64(t[0]));
            sink(t[1]);
        }
        "#,
    );
    assert_eq!(values, vec![42.0, 2.5]);
}
