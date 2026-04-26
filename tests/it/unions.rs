//! Phase 1 union-types tests.
//!
//! Sub-phases land here as they ship:
//! 1. literal types (this file's first cluster)
//! 2. union registry + BoundType::Union
//! 3. variant→union assignability
//! 4. narrowing skeleton
//! 5. discriminator predicate
//! 6. literal-union predicate
//! 7. member access on union
//! 8. generic union arguments

use super::common::{compile, compile_err, run_sink_tick};

// ---- Sub-phase 1: literal types (without union machinery yet) ----

#[test]
fn string_literal_field_accepts_matching_initializer() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 2.5 };
            sink(c.r);
        }
        "#,
    );
    assert_eq!(values, vec![2.5]);
}

#[test]
fn string_literal_field_rejects_mismatched_initializer() {
    let err = compile_err(
        r#"
        type Circle = { kind: 'circle'; r: f64 };
        export function tick(_me: i32): void {
            const c: Circle = { kind: 'square', r: 2.5 };
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("'circle'") && msg.contains("'square'"),
        "expected literal-mismatch error mentioning both 'circle' and 'square', got: {msg}"
    );
}

#[test]
fn string_literal_field_rejects_non_literal_initializer() {
    let err = compile_err(
        r#"
        type Circle = { kind: 'circle'; r: f64 };
        export function tick(_me: i32): void {
            const k: string = 'circle';
            const c: Circle = { kind: k, r: 2.5 };
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("literal") && msg.contains("kind"),
        "expected literal-type error mentioning 'kind', got: {msg}"
    );
}

#[test]
fn integer_literal_field_accepts_matching() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Op = { code: 7; payload: f64 };

        export function tick(_me: i32): void {
            const o: Op = { code: 7, payload: 1.5 };
            sink(o.payload);
        }
        "#,
    );
    assert_eq!(values, vec![1.5]);
}

#[test]
fn integer_literal_field_rejects_mismatched() {
    let err = compile_err(
        r#"
        type Op = { code: 7; payload: f64 };
        export function tick(_me: i32): void {
            const o: Op = { code: 8, payload: 1.5 };
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("7") && msg.contains("8"),
        "expected literal-mismatch error mentioning '7' and '8', got: {msg}"
    );
}

#[test]
fn boolean_literal_field_accepts_matching() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Toggle = { on: true; level: f64 };

        export function tick(_me: i32): void {
            const t: Toggle = { on: true, level: 0.75 };
            sink(t.level);
        }
        "#,
    );
    assert_eq!(values, vec![0.75]);
}

// ---- Sub-phase 2: union registry + BoundType::Union ----
//
// At this point unions register but have no consumer yet (no resolver arms,
// no assignability, no narrowing). The test surface is therefore limited to:
//   1. A program containing a `type X = A | B` named alias compiles when no
//      one references X yet (registration runs as a side-effect, must not
//      reject the alias).
//   2. Mixed-WasmType members (e.g. `string | f64-literal`) get caught at
//      registration with a clear error pointing at the wrapper workaround.

#[test]
fn named_union_alias_unreferenced_compiles() {
    // The alias is declared but not used. Registration must not error,
    // and the rest of the module must still compile.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 3.0 };
            sink(c.r);
        }
        "#,
    );
    assert_eq!(values, vec![3.0]);
}

#[test]
fn mixed_wasm_type_literal_union_rejected() {
    // `'a' | 1.5` mixes a string literal (i32 ptr) with an f64 literal.
    // Both kinds are valid Phase 1 members individually; the validator
    // must catch the mixed-WasmType combination and point users at the
    // discriminated-wrapper workaround.
    let err = compile_err(
        r#"
        type Bad = 'a' | 1.5;
        export function tick(_me: i32): void {}
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("WasmType") || msg.contains("discriminated"),
        "expected mixed-WasmType rejection mentioning the wrapper workaround, got: {msg}"
    );
}

#[test]
fn unknown_union_member_rejected_with_clear_error() {
    // Bare primitive types (e.g. `string`, `number`) and unknown class
    // names aren't valid Phase 1 union members. Confirm the rejection
    // message points at the supported subset.
    let err = compile_err(
        r#"
        type Bad = string | number;
        export function tick(_me: i32): void {}
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("shape") || msg.contains("literal"),
        "expected unsupported-member error mentioning supported subset, got: {msg}"
    );
}

#[test]
fn distinct_literal_tags_produce_distinct_shapes() {
    // Two shapes that differ only in their discriminator literal must
    // register as distinct synthetic classes. If the fingerprint collapsed
    // them we'd see the second `const` overwrite the first's layout and
    // the field offsets would shift; instead, both should compile and
    // emit independent loads.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 1.0 };
            const sq: Square = { kind: 'square', s: 2.0 };
            sink(c.r);
            sink(sq.s);
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0]);
}

// ---- Sub-phase 3: variant → union assignability ----
//
// At this point unions are real types you can spell in annotations: variant
// pointers can flow into union slots, narrower unions can flow into wider
// ones, and the inverse direction is rejected (Sub-phase 4's narrowing will
// be the only way to recover a variant from a union).
//
// Member access on union receivers is still Sub-phase 7 work, so these
// tests don't read fields off `sh: Shape` — they exercise the assignability
// surface only. Sinking f64s from the variant before / instead of the union
// confirms codegen still works end-to-end.

#[test]
fn variant_assigns_to_union_local() {
    // `const sh: Shape = c` — variant pointer flows into a union slot
    // without copy. We can still read the original variant's field through
    // its own binding to confirm the program ran.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 4.0 };
            const sh: Shape = c;
            sink(c.r);
        }
        "#,
    );
    assert_eq!(values, vec![4.0]);
}

#[test]
fn variant_passes_to_union_function_param() {
    // Union as a function parameter type — variant arg flows in zero-copy.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        function take(_sh: Shape): void {
            sink(1.5);
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 2.0 };
            take(c);
        }
        "#,
    );
    assert_eq!(values, vec![1.5]);
}

#[test]
fn function_returns_variant_into_union_slot() {
    // `function getC(): Circle` returns a variant pointer; assigning it to
    // a `Shape` slot must compile and run.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        function getC(): Circle {
            return { kind: 'circle', r: 7.0 };
        }

        export function tick(_me: i32): void {
            const _sh: Shape = getC();
            sink(3.25);
        }
        "#,
    );
    assert_eq!(values, vec![3.25]);
}

#[test]
fn narrower_union_widens_to_broader_union() {
    // `const big: Big = sm` where Sm's members are a subset of Big's.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Rect   = { kind: 'rect';   w: f64; h: f64 };

        type Sm  = Circle | Square;
        type Big = Circle | Square | Rect;

        function widen(sm: Sm): Big {
            return sm;
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 5.0 };
            const sm: Sm = c;
            const _big: Big = widen(sm);
            sink(5.0);
        }
        "#,
    );
    assert_eq!(values, vec![5.0]);
}

#[test]
fn inline_union_function_param_compiles() {
    // Inline `A | B` annotation (no `type` alias) — exercises the
    // `TSUnionType` arm of `get_class_type_name_from_ts_type_with_bindings`
    // (the named-alias case is covered by the other tests).
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };

        function take(_sh: Circle | Square): void {
            sink(9.0);
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 1.0 };
            take(c);
        }
        "#,
    );
    assert_eq!(values, vec![9.0]);
}

#[test]
fn union_assignment_to_variant_rejected_without_narrowing() {
    // `const c: Circle = sh` where `sh: Shape` — must error pointing at
    // narrowing as the recovery path.
    let err = compile_err(
        r#"
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        export function tick(_me: i32): void {
            const c0: Circle = { kind: 'circle', r: 1.0 };
            const sh: Shape = c0;
            const c: Circle = sh;
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("narrow") || msg.contains("===") || msg.contains("kind"),
        "expected narrowing-suggestion error, got: {msg}"
    );
}

#[test]
fn variant_not_in_union_rejected() {
    // `Rect` exists as a shape but isn't a member of `Just = Circle | Square` —
    // assignment must be rejected with a member-set error.
    let err = compile_err(
        r#"
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Rect   = { kind: 'rect';   w: f64; h: f64 };
        type Just = Circle | Square;

        export function tick(_me: i32): void {
            const r: Rect = { kind: 'rect', w: 1.0, h: 2.0 };
            const j: Just = r;
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("not a member") || msg.contains("Rect") || msg.contains("Just"),
        "expected member-set error mentioning 'Rect' or 'Just', got: {msg}"
    );
}

#[test]
fn wider_union_to_narrower_union_rejected() {
    // Subset check goes the right way: Big has more members than Sm, so
    // `const sm: Sm = big` must error.
    let err = compile_err(
        r#"
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Rect   = { kind: 'rect';   w: f64; h: f64 };

        type Sm  = Circle | Square;
        type Big = Circle | Square | Rect;

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 1.0 };
            const big: Big = c;
            const sm: Sm = big;
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("members not in target") || msg.contains("Sm") || msg.contains("Big"),
        "expected widening-direction error, got: {msg}"
    );
}

#[test]
fn inline_pure_f64_literal_union_compiles_and_runs() {
    // Inline `0.5 | 1.5 | 2.5` annotation on a local: the union resolves to
    // `WasmType::F64` (the resolver walks union members and unifies their
    // types). Reading the local back into `sink` proves the wasm type
    // pipeline (load / arg / return) handled f64 throughout.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const x: 0.5 | 1.5 | 2.5 = 1.5;
            sink(x);
        }
        "#,
    );
    assert_eq!(values, vec![1.5]);
}

#[test]
fn named_pure_f64_literal_union_compiles_and_runs() {
    // Named `type Half = 0.5 | 1.5 | 2.5;` — the resolver consults
    // `non_i32_union_wasm_types` so `let x: Half = …` produces an `f64`
    // local rather than the default-`i32` mapping that `class_names`
    // membership would otherwise give the union name.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Half = 0.5 | 1.5 | 2.5;

        export function tick(_me: i32): void {
            const x: Half = 0.5;
            sink(x);
        }
        "#,
    );
    assert_eq!(values, vec![0.5]);
}

#[test]
fn pure_f64_literal_union_narrowing_runs() {
    // `===` against an `f64` literal narrows the union member set just as
    // it does for the i32-literal case (`type Op = 0 | 1 | 2`). The
    // narrowing recognizer compares `TagValue`s, so the f64 path is the
    // same code with a different `TagValue` variant.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Half = 0.5 | 1.5;

        export function tick(_me: i32): void {
            const x: Half = 1.5;
            if (x === 0.5) {
                sink(10.0);
            } else {
                sink(20.0);
            }
        }
        "#,
    );
    assert_eq!(values, vec![20.0]);
}

#[test]
fn mixed_string_and_number_union_still_rejected() {
    // Mixed-WasmType members remain rejected: `'a' | 1.0` would need a
    // tagged runtime representation, which is the deferred large piece.
    // Diagnostic should mention `mixes WasmType` and the discriminated-
    // wrapper workaround.
    let err = compile_err(
        r#"
        type Bad = 'a' | 1.0;
        export function tick(_me: i32): void {}
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("mixes WasmType") || msg.contains("discriminated"),
        "expected mixed-WasmType rejection, got: {msg}"
    );
}

// ---- Sub-phase 5: discriminator predicate ----
//
// The recognizer in `func.rs::recognize_narrowing_facts` pattern-matches the
// test expression and feeds refinement facts into `FuncContext`. To make the
// refinement observable we also threaded `resolve_expr_class`'s Identifier
// arm through `current_class_of` and added shared-field access on unions
// (Sub-phase 7's receiver rule), so `sh.kind` works pre-narrow and `sh.r`
// works inside the narrowed branch.

#[test]
fn narrow_on_discriminator_allows_variant_field_access() {
    // Inside the narrowed branch `sh` refines from Shape to Circle, so the
    // variant-only `r` field becomes accessible.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        function radius(sh: Shape): f64 {
            if (sh.kind === 'circle') return sh.r;
            return 0.0;
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 4.5 };
            sink(radius(c));
        }
        "#,
    );
    assert_eq!(values, vec![4.5]);
}

#[test]
fn narrow_two_variant_union_refines_else_branch() {
    // Two-variant union: matching one variant leaves exactly one in the
    // negative branch, so `else` refines to that remaining variant. We
    // store the result in a local instead of returning from each branch to
    // avoid the wasm validator's implicit-fallthrough requirement on the
    // function body.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        function side(sh: Shape): f64 {
            let result: f64 = 0.0;
            if (sh.kind === 'circle') {
                result = sh.r;
            } else {
                result = sh.s;
            }
            return result;
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 1.5 };
            const sq: Square = { kind: 'square', s: 2.5 };
            sink(side(c));
            sink(side(sq));
        }
        "#,
    );
    assert_eq!(values, vec![1.5, 2.5]);
}

#[test]
fn narrow_on_discriminator_inequality_swaps_branches() {
    // `!==` swaps positive and negative. In the if-true branch `sh` is the
    // negative-case variant (Square); in the else branch it's Circle.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        function side(sh: Shape): f64 {
            let result: f64 = 0.0;
            if (sh.kind !== 'circle') {
                result = sh.s;
            } else {
                result = sh.r;
            }
            return result;
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 1.5 };
            const sq: Square = { kind: 'square', s: 2.5 };
            sink(side(c));
            sink(side(sq));
        }
        "#,
    );
    assert_eq!(values, vec![1.5, 2.5]);
}

#[test]
fn narrow_on_discriminator_operand_order_swapped() {
    // Recognizer must accept the literal on either side.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        function side(sh: Shape): f64 {
            if ('circle' === sh.kind) return sh.r;
            return 0.0;
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 9.0 };
            sink(side(c));
        }
        "#,
    );
    assert_eq!(values, vec![9.0]);
}

#[test]
fn narrow_on_discriminator_ternary_branches_get_refinement() {
    // Ternary branches go through the same refinement lifecycle as `if`.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        function side(sh: Shape): f64 {
            return sh.kind === 'circle' ? sh.r : sh.s;
        }

        export function tick(_me: i32): void {
            const sq: Square = { kind: 'square', s: 7.0 };
            sink(side(sq));
        }
        "#,
    );
    assert_eq!(values, vec![7.0]);
}

#[test]
fn narrow_allows_union_to_variant_assignment() {
    // Sub-phase 3 rejects `const c: Circle = sh` without narrowing. Under an
    // active refinement `sh` resolves to Circle, so the assignment is an
    // ordinary same-name no-op.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        export function tick(_me: i32): void {
            const c0: Circle = { kind: 'circle', r: 6.25 };
            const sh: Shape = c0;
            if (sh.kind === 'circle') {
                const c: Circle = sh;
                sink(c.r);
            }
        }
        "#,
    );
    assert_eq!(values, vec![6.25]);
}

#[test]
fn narrow_integer_discriminator_allows_variant_access() {
    // Same pattern with an integer discriminator (`code: 7` / `code: 8`).
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type OpA = { code: 7; a: f64 };
        type OpB = { code: 8; b: f64 };
        type Op = OpA | OpB;

        function payload(op: Op): f64 {
            let result: f64 = 0.0;
            if (op.code === 7) {
                result = op.a;
            } else {
                result = op.b;
            }
            return result;
        }

        export function tick(_me: i32): void {
            const a: OpA = { code: 7, a: 11.0 };
            const b: OpB = { code: 8, b: 22.0 };
            sink(payload(a));
            sink(payload(b));
        }
        "#,
    );
    assert_eq!(values, vec![11.0, 22.0]);
}

#[test]
fn narrow_three_variant_positive_refines_only_positive_branch() {
    // With three variants, matching one leaves two in the else branch. The
    // recognizer refuses to install a multi-variant negative fact (Phase 1
    // limitation), so else-branch variant-only accesses must still fail.
    let err = compile_err(
        r#"
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Rect   = { kind: 'rect';   w: f64; h: f64 };
        type Shape = Circle | Square | Rect;

        export function tick(_me: i32): void {
            const c0: Circle = { kind: 'circle', r: 1.0 };
            const sh: Shape = c0;
            if (sh.kind === 'circle') {
            } else {
                const _x: f64 = sh.s;
            }
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("shared") || msg.contains("narrow"),
        "expected shared-field / narrowing error in else branch, got: {msg}"
    );
}

#[test]
fn narrow_does_not_leak_into_sibling_branches() {
    // The positive refinement is undone before the else branch runs.
    // Sub-phase 4's `sibling_scopes_do_not_leak_refinements` unit test
    // covers this at the scope level; here we confirm the end-to-end
    // behavior: inside `else` the declared type Shape is back, so
    // variant-only access is rejected with the shared-field error.
    let err = compile_err(
        r#"
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        export function tick(_me: i32): void {
            const c0: Circle = { kind: 'circle', r: 1.0 };
            const sh: Shape = c0;
            if (sh.kind === 'circle') {
            }
            const _r: f64 = sh.r;
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("shared") || msg.contains("narrow"),
        "expected variant-only access error after the if-block, got: {msg}"
    );
}

// ---- Sub-phase 6: literal-union predicate ----

#[test]
fn literal_union_string_narrowing_compiles_and_runs() {
    // Pure literal unions don't produce observable class-name refinement
    // in Phase 1, but the recognizer must not reject the pattern and the
    // program must still run end-to-end.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Color = 'red' | 'green' | 'blue';

        export function tick(_me: i32): void {
            const c: Color = 'red';
            if (c === 'red') {
                sink(1.0);
            } else {
                sink(2.0);
            }
        }
        "#,
    );
    assert_eq!(values, vec![1.0]);
}

#[test]
fn literal_union_integer_narrowing_compiles_and_runs() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Op = 0 | 1 | 2;

        export function tick(_me: i32): void {
            const o: Op = 1;
            if (o === 1) {
                sink(10.0);
            } else {
                sink(20.0);
            }
        }
        "#,
    );
    assert_eq!(values, vec![10.0]);
}

#[test]
fn literal_union_inequality_runs() {
    // `!==` on a literal union.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Color = 'red' | 'green';

        export function tick(_me: i32): void {
            const c: Color = 'green';
            if (c !== 'red') {
                sink(42.0);
            }
        }
        "#,
    );
    assert_eq!(values, vec![42.0]);
}

// ---- Sub-phase 7: member access on union (shared-field rule) ----

#[test]
fn shared_field_access_on_union_without_narrowing() {
    // `kind` is declared at the same offset with the same `string` type on
    // every variant, so `sh.kind` must load correctly with no narrowing
    // guard. The `===` against the literal path exercises the string-binary
    // helper with a union-sourced left side.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        export function tick(_me: i32): void {
            const c0: Circle = { kind: 'circle', r: 1.0 };
            const sh: Shape = c0;
            if (sh.kind === 'circle') {
                sink(2.0);
            } else {
                sink(3.0);
            }
        }
        "#,
    );
    assert_eq!(values, vec![2.0]);
}

#[test]
fn variant_only_field_rejected_on_union_with_narrowing_suggestion() {
    let err = compile_err(
        r#"
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        export function tick(_me: i32): void {
            const c0: Circle = { kind: 'circle', r: 1.0 };
            const sh: Shape = c0;
            const _r: f64 = sh.r;
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("shared") || msg.contains("narrow"),
        "expected variant-only field error with narrowing suggestion, got: {msg}"
    );
}

#[test]
fn area_three_variant_demo_runs() {
    // The doc-top canonical example from plan-unions.md, adapted for
    // Phase 1: uses an explicit `if`-chain instead of the doc's
    // `if`/`if`/`return` pattern (which relies on the multi-variant
    // negative refinement deferred to Phase 1.5 — see
    // `narrow_three_variant_positive_refines_only_positive_branch`).
    // Confirms variant assignment into a union slot, shared-field
    // `kind` access pre-narrow, and variant-specific field access
    // under narrowing all compose in a realistic sum-type program.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Rect   = { kind: 'rect';   w: f64; h: f64 };
        type Shape = Circle | Square | Rect;

        function area(sh: Shape): f64 {
            if (sh.kind === 'circle') return 3.14 * sh.r * sh.r;
            if (sh.kind === 'square') return sh.s * sh.s;
            if (sh.kind === 'rect') return sh.w * sh.h;
            return 0.0;
        }

        export function tick(_me: i32): void {
            const c0: Circle = { kind: 'circle', r: 2.0 };
            const s0: Square = { kind: 'square', s: 3.0 };
            const r0: Rect   = { kind: 'rect',   w: 4.0, h: 5.0 };
            sink(area(c0));
            sink(area(s0));
            sink(area(r0));
        }
        "#,
    );
    assert_eq!(values, vec![3.14 * 2.0 * 2.0, 3.0 * 3.0, 4.0 * 5.0]);
}

#[test]
fn nested_narrowing_refines_independent_unions() {
    // Two union-typed locals narrowed in nested `if` scopes. The outer
    // refinement stays active across the inner scope (field access on
    // `sh.r` works inside the inner body), and the inner refinement
    // stacks on top without disturbing it. Confirms the lifecycle of
    // `RefinementEnv` under composition.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;
        type Red  = { tone: 'red';  v: f64 };
        type Blue = { tone: 'blue'; v: f64 };
        type Color = Red | Blue;

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 2.0 };
            const red: Red = { tone: 'red', v: 0.5 };
            const sh: Shape = c;
            const cl: Color = red;
            if (sh.kind === 'circle') {
                if (cl.tone === 'red') {
                    sink(sh.r * cl.v);
                }
            }
        }
        "#,
    );
    assert_eq!(values, vec![2.0 * 0.5]);
}

#[test]
fn shared_numeric_field_access_on_union() {
    // The shared-field rule isn't string-specific — any field declared
    // at the same offset with the same `WasmType` across every variant
    // loads without narrowing. Here `id: f64` is shared; the variant-
    // specific fields (`r`, `s`) follow it in the layout.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Wheel = { kind: 'wheel'; id: f64; r: f64 };
        type Plate = { kind: 'plate'; id: f64; s: f64 };
        type Part = Wheel | Plate;

        export function tick(_me: i32): void {
            const w: Wheel = { kind: 'wheel', id: 7.0, r: 2.0 };
            const p: Part = w;
            sink(p.id);
        }
        "#,
    );
    assert_eq!(values, vec![7.0]);
}

#[test]
fn mixed_shape_literal_union_rejects_shared_field_access() {
    // `resolve_union_shared_field` bails on unions that contain any
    // literal member — literals have no fields, so no field can be
    // "shared across all variants". Even when every shape member
    // declares the field, the access must fail: the user's recourse
    // is to narrow away the literal member first.
    let err = compile_err(
        r#"
        type Circle = { kind: 'circle'; r: f64 };
        type MaybeCircle = Circle | 'disabled';

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 2.0 };
            const mc: MaybeCircle = c;
            const _k: string = mc.kind;
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("shared") || msg.contains("narrow"),
        "expected shared-field rejection on mixed shape/literal union, got: {msg}"
    );
}

// ---- Sub-phase 8: generic union arguments ----

#[test]
fn array_of_named_union_push_and_load() {
    // `Array<Shape>` — the pickup notes' canonical Sub-phase 8 test.
    // Confirms the `TSTypeReference` to a union name resolves to
    // `BoundType::Union` (not class) so `Array<T>` monomorphization
    // mangles as `Array$Shape`, push accepts any variant, and element
    // loads come back typed as the union (shared-field access works).
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 2.0 };
            const s: Square = { kind: 'square', s: 3.0 };
            const xs: Array<Shape> = [];
            xs.push(c);
            xs.push(s);
            const sh0: Shape = xs[0];
            if (sh0.kind === 'circle') {
                sink(sh0.r);
            }
            const sh1: Shape = xs[1];
            if (sh1.kind === 'square') {
                sink(sh1.s);
            }
        }
        "#,
    );
    assert_eq!(values, vec![2.0, 3.0]);
}

#[test]
fn user_generic_class_with_union_argument() {
    // User-written generic `class Box<T> { value: T }` instantiated with a
    // named union. Monomorphization mangles as `Box$Shape`, the field
    // binding uses the union type, and narrowing on the extracted value
    // still works inside `tick`.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        class Box<T> {
            value: T;
            constructor(v: T) { this.value = v; }
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 4.0 };
            const b: Box<Shape> = new Box<Shape>(c);
            const sh: Shape = b.value;
            if (sh.kind === 'circle') {
                sink(sh.r);
            }
        }
        "#,
    );
    assert_eq!(values, vec![4.0]);
}

#[test]
fn user_generic_function_with_union_parameter() {
    // User-written generic function `identity<T>(x: T): T` invoked with a
    // union type. Tests that `resolve_bound_type`'s TSTypeReference arm
    // produces the correct BoundType for the union name so the
    // monomorphized `identity$Shape` signature threads through without
    // type errors.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        function identity<T>(x: T): T { return x; }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 7.0 };
            const sh: Shape = identity<Shape>(c);
            if (sh.kind === 'circle') {
                sink(sh.r);
            }
        }
        "#,
    );
    assert_eq!(values, vec![7.0]);
}

#[test]
fn inline_union_as_generic_argument_compiles() {
    // Inline `Box<Circle | Square>` — exercises the `TSUnionType` arm
    // of `resolve_bound_type`. Without the arm, generic-instantiation
    // collection bails on the union with "unsupported TS type as
    // generic argument". With it, the inline union resolves to the
    // registered layout and `Box$__Union$Circle$Square` mangles
    // correctly.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };

        class Box<T> {
            value: T;
            constructor(v: T) { this.value = v; }
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 5.0 };
            const b: Box<Circle | Square> = new Box<Circle | Square>(c);
            const sh: Circle | Square = b.value;
            if (sh.kind === 'circle') {
                sink(sh.r);
            }
        }
        "#,
    );
    assert_eq!(values, vec![5.0]);
}

// ---- Phase 1.5 — Sub-phase 1: multi-variant negative refinement ----

#[test]
fn else_branch_subunion_allows_shared_field_access() {
    // Three-variant union, narrow on `kind === 'cat'` so the else
    // branch sees `Dog | Bird` as a `Refinement::Subunion`. Both
    // surviving variants declare `weight: f64` at the same offset, so
    // the shared-field rule (refined-set edition) succeeds and the
    // else branch can read `pet.weight` without a second narrowing.
    // Pre-Phase-1.5 this required an explicit `if (pet.kind === 'dog'
    // || pet.kind === 'bird')` chain or an `as` cast.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Cat  = { kind: 'cat'  };
        type Dog  = { kind: 'dog';  weight: f64 };
        type Bird = { kind: 'bird'; weight: f64 };
        type Pet = Cat | Dog | Bird;

        function pet_weight(pet: Pet): f64 {
            let result: f64 = 0.0;
            if (pet.kind === 'cat') {
                result = 0.0;
            } else {
                // pet refined to Dog | Bird here — both declare `weight`
                result = pet.weight;
            }
            return result;
        }

        export function tick(_me: i32): void {
            const d: Dog = { kind: 'dog', weight: 12.5 };
            const b: Bird = { kind: 'bird', weight: 0.3 };
            const c: Cat = { kind: 'cat' };
            sink(pet_weight(d));
            sink(pet_weight(b));
            sink(pet_weight(c));
        }
        "#,
    );
    assert_eq!(values, vec![12.5, 0.3, 0.0]);
}

#[test]
fn else_branch_subunion_assigns_to_narrower_union() {
    // The refined sub-union flows into a slot whose declared type is
    // exactly the surviving variants. Exercises `emit_shape_coerce`'s
    // new `Refinement::Subunion` arm: source is declared `Pet` (the
    // wider union) but the refinement reduces it to `Dog | Bird`,
    // which is a subset of the target `Mammal = Dog | Bird` slot.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Cat  = { kind: 'cat';  weight: f64 };
        type Dog  = { kind: 'dog';  weight: f64 };
        type Bird = { kind: 'bird'; weight: f64 };
        type Pet = Cat | Dog | Bird;
        type Mammal = Cat | Dog;

        function as_mammal(pet: Pet): f64 {
            let result: f64 = 0.0;
            if (pet.kind === 'bird') {
                result = 0.0;
            } else {
                // pet refined to Cat | Dog here — assignable to Mammal
                const m: Mammal = pet;
                result = m.weight;
            }
            return result;
        }

        export function tick(_me: i32): void {
            const c: Cat = { kind: 'cat', weight: 4.0 };
            const d: Dog = { kind: 'dog', weight: 12.0 };
            const b: Bird = { kind: 'bird', weight: 0.3 };
            sink(as_mammal(c));
            sink(as_mammal(d));
            sink(as_mammal(b));
        }
        "#,
    );
    assert_eq!(values, vec![4.0, 12.0, 0.0]);
}

#[test]
fn else_branch_subunion_rejects_variant_only_field() {
    // After narrowing `Shape = Circle | Square | Rect` with `kind ===
    // 'circle'`, the else branch's refinement is Square | Rect — but
    // Square's `s` and Rect's `w` are not shared. The shared-field
    // rule on the refined member set must still reject access, with
    // the same narrowing-suggestion error as the un-refined case
    // (just walking the refined subset, not the full union).
    let err = compile_err(
        r#"
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Rect   = { kind: 'rect';   w: f64; h: f64 };
        type Shape = Circle | Square | Rect;

        export function tick(_me: i32): void {
            const sq: Square = { kind: 'square', s: 3.0 };
            const sh: Shape = sq;
            if (sh.kind === 'circle') {
                // unreachable
            } else {
                // sh refined to Square | Rect — `s` is on Square only
                const _v: f64 = sh.s;
            }
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("'s'") && (msg.contains("shared") || msg.contains("narrow")),
        "expected shared-field rejection on refined sub-union, got: {msg}"
    );
}

#[test]
fn nested_narrowing_composes_subunion_then_class() {
    // Outer narrowing: `Shape = A | B | C | D`, `kind !== 'a'` puts
    // sh into `Refinement::Subunion(B | C | D)`. Inner narrowing:
    // `kind === 'c'` against the refined sub-union must walk only
    // those three members, not the original four — yielding
    // `Refinement::Class("C")`. This exercises the recognizer's
    // composability fix (`active_member_set` honoring the active
    // refinement) end-to-end.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type A = { kind: 'a'; av: f64 };
        type B = { kind: 'b'; bv: f64 };
        type C = { kind: 'c'; cv: f64 };
        type D = { kind: 'd'; dv: f64 };
        type Shape = A | B | C | D;

        function pick_c(sh: Shape): f64 {
            let result: f64 = -2.0;
            if (sh.kind !== 'a') {
                // sh refined to B | C | D
                if (sh.kind === 'c') {
                    // sh refined to C
                    result = sh.cv;
                } else {
                    result = -1.0;
                }
            }
            return result;
        }

        export function tick(_me: i32): void {
            const a: A = { kind: 'a', av: 99.0 };
            const c: C = { kind: 'c', cv: 7.5 };
            const d: D = { kind: 'd', dv: 1.0 };
            sink(pick_c(a));
            sink(pick_c(c));
            sink(pick_c(d));
        }
        "#,
    );
    assert_eq!(values, vec![-2.0, 7.5, -1.0]);
}

#[test]
fn nested_narrowing_inner_else_negative_subunion_remains() {
    // Inside an outer `Subunion(B | C | D)`, narrowing `kind === 'c'`
    // gives the inner else branch a further refined `Subunion(B | D)`.
    // Both surviving variants must declare a shared field for access
    // to compile — confirms the negative path's `active_member_set`
    // call also picks up the refined-rather-than-original member set.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type A = { kind: 'a' };
        type B = { kind: 'b'; common: f64 };
        type C = { kind: 'c'; cv: f64 };
        type D = { kind: 'd'; common: f64 };
        type Shape = A | B | C | D;

        function pick_common(sh: Shape): f64 {
            let result: f64 = -1.0;
            if (sh.kind === 'a') {
                result = -1.0;
            } else {
                // outer: B | C | D
                if (sh.kind === 'c') {
                    result = sh.cv;
                } else {
                    // inner else: B | D — both declare `common`
                    result = sh.common;
                }
            }
            return result;
        }

        export function tick(_me: i32): void {
            const b: B = { kind: 'b', common: 11.0 };
            const c: C = { kind: 'c', cv: 22.0 };
            const d: D = { kind: 'd', common: 33.0 };
            sink(pick_common(b));
            sink(pick_common(c));
            sink(pick_common(d));
        }
        "#,
    );
    assert_eq!(values, vec![11.0, 22.0, 33.0]);
}

#[test]
fn two_variant_else_still_refines_to_class() {
    // Regression: when the surviving set has exactly one shape,
    // `build_negative_refinement` returns `Class`, not `Subunion`.
    // The else branch reads a variant-specific field — only possible
    // because the refinement narrowed to a single class, the same as
    // Phase 1's behavior.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        function area(sh: Shape): f64 {
            let result: f64 = 0.0;
            if (sh.kind === 'circle') {
                result = 3.14 * sh.r * sh.r;
            } else {
                result = sh.s * sh.s; // sh refined to Square (Class, not Subunion)
            }
            return result;
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 2.0 };
            const sq: Square = { kind: 'square', s: 3.0 };
            sink(area(c));
            sink(area(sq));
        }
        "#,
    );
    assert_eq!(values, vec![3.14 * 2.0 * 2.0, 3.0 * 3.0]);
}

#[test]
fn ternary_else_branch_carries_subunion_refinement() {
    // The refinement lifecycle in `expr::emit_conditional` mirrors
    // `stmt::emit_if`, so multi-variant negatives apply to ternaries
    // too. Same shared-field shape as the if-test, just on the
    // expression side.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Cat  = { kind: 'cat'  };
        type Dog  = { kind: 'dog';  weight: f64 };
        type Bird = { kind: 'bird'; weight: f64 };
        type Pet = Cat | Dog | Bird;

        function pet_weight(pet: Pet): f64 {
            return pet.kind === 'cat' ? 0.0 : pet.weight;
        }

        export function tick(_me: i32): void {
            const d: Dog = { kind: 'dog', weight: 9.5 };
            const b: Bird = { kind: 'bird', weight: 0.2 };
            sink(pet_weight(d));
            sink(pet_weight(b));
        }
        "#,
    );
    assert_eq!(values, vec![9.5, 0.2]);
}

#[test]
fn literal_union_else_branch_subunion_assigns_to_narrower() {
    // Literal-union `===` narrowing: `Color = 'red' | 'green' | 'blue'`,
    // `if (c === 'red')` else branch refines to `Subunion('green',
    // 'blue')`. The refined sub-union should be assignable to a
    // narrower `'green' | 'blue'` slot via the coerce arm.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Color = 'red' | 'green' | 'blue';
        type Cool = 'green' | 'blue';

        function not_red_score(c: Color): f64 {
            let result: f64 = 0.0;
            if (c === 'red') {
                result = 0.0;
            } else {
                const cool: Cool = c; // c refined to Subunion('green', 'blue')
                if (cool === 'green') {
                    result = 1.0;
                } else {
                    result = 2.0;
                }
            }
            return result;
        }

        export function tick(_me: i32): void {
            sink(not_red_score('red'));
            sink(not_red_score('green'));
            sink(not_red_score('blue'));
        }
        "#,
    );
    assert_eq!(values, vec![0.0, 1.0, 2.0]);
}

// ---- Sub-phase 1.5.2: tag-value inference on unannotated object literals ----
//
// Phase 1 required `as Circle` casts on object literals assigned directly
// into a union slot (`const sh: Shape = { kind: 'circle', r: 1.0 } as Circle`)
// because the literal's inferred fingerprint omitted the discriminator's
// tag value and didn't match any variant's. Sub-phase 2 of plan-unions
// teaches both `infer_expr_bound_type` (Pass 0) and `literal_field_bound_type`
// (emit-time fingerprint) to capture string/integer literal tags, with a
// tagged-then-untagged fallback so plain shape literals (`{x: 1, y: 2}`)
// keep sharing one anonymous class instead of splitting per value.

#[test]
fn direct_literal_into_named_union_variable() {
    // `const sh: Shape = { kind: 'circle', r: 1.0 }` — no `as Circle` —
    // must compile and the resolved layout must be Circle's.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        export function tick(_me: i32): void {
            const sh: Shape = { kind: 'circle', r: 4.5 };
            if (sh.kind === 'circle') {
                sink(sh.r);
            } else {
                sink(0.0);
            }
        }
        "#,
    );
    assert_eq!(values, vec![4.5]);
}

#[test]
fn direct_literal_into_union_function_arg() {
    // Direct unannotated literal as a function argument typed as a union.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        function area(sh: Shape): f64 {
            return sh.kind === 'circle' ? sh.r * sh.r : sh.s * sh.s;
        }

        export function tick(_me: i32): void {
            sink(area({ kind: 'circle', r: 2.0 }));
            sink(area({ kind: 'square', s: 3.0 }));
        }
        "#,
    );
    assert_eq!(values, vec![4.0, 9.0]);
}

#[test]
fn direct_literal_into_union_array_element() {
    // `const arr: Shape[] = [{kind:'circle', r: 1.0}, ...]` — each element
    // resolves to its variant's layout via the tagged fingerprint.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        export function tick(_me: i32): void {
            const arr: Shape[] = [
                { kind: 'circle', r: 1.5 },
                { kind: 'square', s: 2.5 },
            ];
            for (let i = 0; i < arr.length; i = i + 1) {
                const sh = arr[i];
                if (sh.kind === 'circle') {
                    sink(sh.r);
                } else {
                    sink(sh.s);
                }
            }
        }
        "#,
    );
    assert_eq!(values, vec![1.5, 2.5]);
}

#[test]
fn integer_tag_literal_into_union_variable() {
    // Integer-discriminator union: `{code: 1, payload: ...}` directly
    // assigned to the union slot — exercises the `TagValue::I32` path
    // alongside the string-literal path.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Add = { code: 1; payload: f64 };
        type Mul = { code: 2; payload: f64 };
        type Op = Add | Mul;

        function eval_op(o: Op): f64 {
            return o.code === 1 ? o.payload + 1.0 : o.payload * 2.0;
        }

        export function tick(_me: i32): void {
            const a: Op = { code: 1, payload: 4.0 };
            const m: Op = { code: 2, payload: 4.0 };
            sink(eval_op(a));
            sink(eval_op(m));
        }
        "#,
    );
    assert_eq!(values, vec![5.0, 8.0]);
}

#[test]
fn variant_then_assign_pattern_still_works() {
    // Regression for the existing route — bind a literal to a variant-typed
    // local first, then widen to the union. This must keep compiling after
    // the inference change.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 7.0 };
            const sh: Shape = c;
            if (sh.kind === 'circle') {
                sink(sh.r);
            }
        }
        "#,
    );
    assert_eq!(values, vec![7.0]);
}

#[test]
fn plain_shape_literals_share_one_class() {
    // The tagged-then-untagged fallback must keep `{x: 1, y: 2}` and
    // `{x: 3, y: 4}` aliased to the same anonymous shape: each literal's
    // tagged fingerprint misses the registry, the untagged fingerprint
    // hits the shared class, both locals are typed identically, and a
    // function returning that shape can accept either initializer.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        function pick(flag: bool): { x: i32, y: i32 } {
            if (flag) {
                return { x: 1, y: 2 };
            }
            return { x: 3, y: 4 };
        }

        export function tick(_me: i32): void {
            const p = pick(true);
            const q = pick(false);
            sink((p.x + q.x) as f64);
            sink((p.y + q.y) as f64);
        }
        "#,
    );
    assert_eq!(values, vec![4.0, 6.0]);
}

// ---- Sub-phase 1.5.3: switch-statement narrowing ----
//
// `switch (sh.kind) { case 'circle': ... }` narrows `sh` inside each case
// body the same way `if (sh.kind === 'circle')` does, and `default:` sees
// the cumulative-negative refinement built from the remaining members.

#[test]
fn switch_on_discriminator_narrows_each_case_body() {
    // Standard discriminated-union switch — every case accesses its
    // variant's specific field. Without per-case narrowing, member
    // access on `sh.r` / `sh.s` / `sh.w` would fail the shared-field
    // check.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Rect   = { kind: 'rect';   w: f64; h: f64 };
        type Shape = Circle | Square | Rect;

        function area(sh: Shape): f64 {
            let result: f64 = 0.0;
            switch (sh.kind) {
                case 'circle':
                    result = sh.r * sh.r;
                    break;
                case 'square':
                    result = sh.s * sh.s;
                    break;
                case 'rect':
                    result = sh.w * sh.h;
                    break;
            }
            return result;
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 2.0 };
            const sq: Square = { kind: 'square', s: 3.0 };
            const r: Rect = { kind: 'rect', w: 4.0, h: 5.0 };
            sink(area(c));
            sink(area(sq));
            sink(area(r));
        }
        "#,
    );
    assert_eq!(values, vec![4.0, 9.0, 20.0]);
}

#[test]
fn switch_default_refines_to_remaining_single_variant() {
    // Cases for circle / square; default reaches when the variant is
    // the third one. Default's refinement is `Class(Rect)` — accessing
    // Rect-specific fields must compile.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Rect   = { kind: 'rect';   w: f64; h: f64 };
        type Shape = Circle | Square | Rect;

        function describe(sh: Shape): f64 {
            let result: f64 = 0.0;
            switch (sh.kind) {
                case 'circle':
                    result = sh.r;
                    break;
                case 'square':
                    result = sh.s;
                    break;
                default:
                    // sh narrowed to Rect — can access w / h directly.
                    result = sh.w + sh.h;
            }
            return result;
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 1.5 };
            const sq: Square = { kind: 'square', s: 2.5 };
            const r: Rect = { kind: 'rect', w: 3.0, h: 4.0 };
            sink(describe(c));
            sink(describe(sq));
            sink(describe(r));
        }
        "#,
    );
    assert_eq!(values, vec![1.5, 2.5, 7.0]);
}

#[test]
fn switch_default_subunion_reads_shared_field() {
    // 4-variant union with a single case + default. Default's
    // cumulative-negative is a 3-member sub-union sharing a field —
    // the shared-field rule must walk the refined member set.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type A = { kind: 'a'; tag: f64; va: f64 };
        type B = { kind: 'b'; tag: f64; vb: f64 };
        type C = { kind: 'c'; tag: f64; vc: f64 };
        type D = { kind: 'd'; tag: f64; vd: f64 };
        type U = A | B | C | D;

        function take(u: U): f64 {
            let result: f64 = 0.0;
            switch (u.kind) {
                case 'a':
                    result = u.va;
                    break;
                default:
                    // u refined to Subunion(B, C, D) — `tag` is shared.
                    result = u.tag;
            }
            return result;
        }

        export function tick(_me: i32): void {
            const a: A = { kind: 'a', tag: 100.0, va: 1.0 };
            const b: B = { kind: 'b', tag: 200.0, vb: 2.0 };
            const c: C = { kind: 'c', tag: 300.0, vc: 3.0 };
            const d: D = { kind: 'd', tag: 400.0, vd: 4.0 };
            sink(take(a));
            sink(take(b));
            sink(take(c));
            sink(take(d));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 200.0, 300.0, 400.0]);
}

#[test]
fn switch_on_literal_union_default_subunion_assigns_to_narrower() {
    // Literal-union switch: cases handle some literals; default's
    // cumulative-negative is a `Subunion` of the rest. Assign the
    // refined value into a narrower union slot — exercises the
    // coerce-side `Subunion ⊆ target` path on the switch path,
    // mirroring the if-statement test in sub-phase 1.5.1.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Color = 'red' | 'green' | 'blue' | 'yellow';
        type Cool = 'green' | 'blue' | 'yellow';

        function not_red_score(c: Color): f64 {
            let result: f64 = -1.0;
            switch (c) {
                case 'red':
                    result = 0.0;
                    break;
                default:
                    const cool: Cool = c; // refined to Subunion(green, blue, yellow)
                    if (cool === 'green') {
                        result = 1.0;
                    } else if (cool === 'blue') {
                        result = 2.0;
                    } else {
                        result = 3.0;
                    }
            }
            return result;
        }

        export function tick(_me: i32): void {
            sink(not_red_score('red'));
            sink(not_red_score('green'));
            sink(not_red_score('blue'));
            sink(not_red_score('yellow'));
        }
        "#,
    );
    assert_eq!(values, vec![0.0, 1.0, 2.0, 3.0]);
}

#[test]
fn switch_no_default_does_not_narrow_after_switch() {
    // Without a `default`, the switch only narrows inside its cases.
    // Code after the switch must see the original union type — so a
    // bare `sh.r` member access (not shared) is still rejected.
    let err = compile_err(
        r#"
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        export function tick(_me: i32): void {
            const sh: Shape = { kind: 'circle', r: 1.0 };
            switch (sh.kind) {
                case 'circle':
                    break;
                case 'square':
                    break;
            }
            // Outside the switch — `sh` is back to its declared union type.
            const r: f64 = sh.r;
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("not shared") || msg.contains("narrow") || msg.contains("'r'"),
        "expected shared-field-access error after un-narrowed switch, got: {msg}"
    );
}

#[test]
fn switch_per_case_scope_does_not_leak_across_cases() {
    // Each case body owns its own refinement — narrowing in case
    // 'circle' must NOT carry into case 'square'. Reading `sh.s`
    // inside the 'circle' case body would be a member-access error
    // if case scopes leaked. (Positive: every case accesses only
    // its own field, and `break` keeps fall-through impossible in
    // our codegen anyway, so this is a static-narrowing check.)
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        function pick(sh: Shape): f64 {
            switch (sh.kind) {
                case 'circle':
                    return sh.r;
                case 'square':
                    return sh.s;
            }
            return -1.0;
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 11.0 };
            const sq: Square = { kind: 'square', s: 22.0 };
            sink(pick(c));
            sink(pick(sq));
        }
        "#,
    );
    assert_eq!(values, vec![11.0, 22.0]);
}

// ---- Sub-phase 1.5.4: exhaustiveness checking via `never` ----

#[test]
fn exhaustive_switch_with_assert_never_compiles_and_runs() {
    // Standard exhaustiveness pattern: a `default` clause that calls a
    // `(x: never) => ...` helper. The default body is reached only if
    // some variant escapes every prior `case` — i.e. never, in a sound
    // exhaustive switch. The compiler must accept the `assertNever(sh)`
    // call because `sh` is `Refinement::Never` inside default after
    // `circle` / `square` have been handled.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        function assertNever(x: never): i32 {
            return 0;
        }

        function area(sh: Shape): f64 {
            switch (sh.kind) {
                case 'circle': return sh.r * sh.r;
                case 'square': return sh.s * sh.s;
                default:
                    assertNever(sh);
                    return -1.0;
            }
            return -1.0;
        }

        export function tick(_me: i32): void {
            const c: Circle = { kind: 'circle', r: 3.0 };
            const sq: Square = { kind: 'square', s: 4.0 };
            sink(area(c));
            sink(area(sq));
        }
        "#,
    );
    assert_eq!(values, vec![9.0, 16.0]);
}

#[test]
fn non_exhaustive_switch_never_assignment_lists_missing_variants() {
    // 3-variant union with cases for `circle` / `square` only. `default`
    // sees `sh` refined to `Class(Rect)` — a `: never` slot rejects it
    // and the diagnostic must name the missing variant so users can fix
    // the switch directly.
    let err = compile_err(
        r#"
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Rect   = { kind: 'rect';   w: f64; h: f64 };
        type Shape = Circle | Square | Rect;

        export function tick(_me: i32): void {
            const sh: Shape = { kind: 'circle', r: 1.0 };
            switch (sh.kind) {
                case 'circle': break;
                case 'square': break;
                default:
                    const _exhaustive: never = sh;
                    break;
            }
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("never") && msg.contains("Rect"),
        "expected diagnostic to mention `never` and the un-handled variant Rect, got: {msg}"
    );
}

#[test]
fn never_param_accepts_refinement_never_argument() {
    // Mirror of the assertNever pattern but exercising direct
    // `: never` parameter passing on the literal-union path: every
    // case handles a literal, the default body sees `c` as
    // `Refinement::Never`, and the call into `(x: never) => ...`
    // type-checks via the early `Refinement::Never` short-circuit
    // in `emit_shape_coerce`.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Color = 'red' | 'green' | 'blue';

        function assertNever(x: never): i32 {
            return 0;
        }

        function score(c: Color): f64 {
            switch (c) {
                case 'red':   return 1.0;
                case 'green': return 2.0;
                case 'blue':  return 3.0;
                default:
                    assertNever(c);
                    return -1.0;
            }
            return -1.0;
        }

        export function tick(_me: i32): void {
            sink(score('red'));
            sink(score('green'));
            sink(score('blue'));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0, 3.0]);
}

#[test]
fn never_param_rejects_un_narrowed_union_with_member_list() {
    // Calling a `(x: never) => ...` helper on a still-inhabited union
    // must fail with a diagnostic that enumerates the source's full
    // member set — this is the load-bearing user-facing message of the
    // exhaustiveness feature.
    let err = compile_err(
        r#"
        type Circle = { kind: 'circle'; r: f64 };
        type Square = { kind: 'square'; s: f64 };
        type Shape = Circle | Square;

        function assertNever(x: never): i32 {
            return 0;
        }

        export function tick(_me: i32): void {
            const sh: Shape = { kind: 'circle', r: 1.0 };
            assertNever(sh);
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("never") && msg.contains("Circle") && msg.contains("Square"),
        "expected diagnostic to mention `never` and both un-handled variants, got: {msg}"
    );
}

// ---- Phase 2 — Sub-phase 1: class-union polymorphism gate ----

#[test]
fn class_union_with_common_base_compiles() {
    // Two children of the same base — both polymorphic — pass the gate.
    // Variant→union widening (Phase 1 sub-phase 3) already accepts class
    // names because `UnionMember::Shape` covers the unified namespace.
    let _ = compile(
        r#"
        class Animal {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Cat extends Animal {
            purrs: i32;
            constructor(hp: i32, purrs: i32) { super(hp); }
        }
        class Dog extends Animal {
            barks: i32;
            constructor(hp: i32, barks: i32) { super(hp); }
        }
        type Pet = Cat | Dog;

        export function run(): i32 {
            const p: Pet = new Cat(10, 3);
            return 0;
        }
        "#,
    );
}

#[test]
fn class_union_with_non_polymorphic_leaf_rejected() {
    // Two leaf classes with no inheritance — neither carries a vtable
    // pointer. The gate fires at the union declaration with a clear
    // suggestion to add a common base.
    let err = compile_err(
        r#"
        class Cat {
            purrs: i32;
            constructor(purrs: i32) {}
        }
        class Dog {
            barks: i32;
            constructor(barks: i32) {}
        }
        type Pet = Cat | Dog;

        export function run(): i32 {
            return 0;
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("non-polymorphic")
            && msg.contains("Pet")
            && (msg.contains("'Cat'") || msg.contains("'Dog'"))
            && msg.contains("base"),
        "expected diagnostic to name the union, the offending class, and suggest a base class, got: {msg}"
    );
}

#[test]
fn mixed_shape_and_polymorphic_class_union_compiles() {
    // The class member is polymorphic (has children), the shape member is
    // unaffected by the gate. This is the Phase 2 sub-phase 4 surface
    // delivered early through the gate's design.
    let _ = compile(
        r#"
        class Animal {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Cat extends Animal {
            purrs: i32;
            constructor(hp: i32, purrs: i32) { super(hp); }
        }
        type Rect = { kind: 'rect'; w: f64; h: f64 };
        type Mixed = Rect | Cat;

        export function run(): i32 {
            const m: Mixed = new Cat(7, 2);
            return 0;
        }
        "#,
    );
}

#[test]
fn three_class_union_parent_and_two_children_compiles() {
    // Animal participates as a parent → polymorphic. Cat and Dog have a
    // parent → polymorphic. All three pass the gate. (Sub-phases 2/3 will
    // exercise narrowing and shared-method dispatch on this surface.)
    let _ = compile(
        r#"
        class Animal {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Cat extends Animal {
            purrs: i32;
            constructor(hp: i32, purrs: i32) { super(hp); }
        }
        class Dog extends Animal {
            barks: i32;
            constructor(hp: i32, barks: i32) { super(hp); }
        }
        type Pet = Animal | Cat | Dog;

        export function run(): i32 {
            const p: Pet = new Animal(99);
            return 0;
        }
        "#,
    );
}

#[test]
fn inline_class_union_polymorphism_gate_fires() {
    // Same gate, inline-union path (no `type` alias). Diagnostic uses the
    // synthetic `(inline union)` label so users still see the offending
    // class name and the base-class suggestion.
    let err = compile_err(
        r#"
        class Cat {
            purrs: i32;
            constructor(purrs: i32) {}
        }
        class Dog {
            barks: i32;
            constructor(barks: i32) {}
        }

        function feed(p: Cat | Dog): void {}

        export function run(): i32 {
            return 0;
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("non-polymorphic")
            && (msg.contains("'Cat'") || msg.contains("'Dog'"))
            && msg.contains("base"),
        "expected inline-union diagnostic to name the offending class and suggest a base, got: {msg}"
    );
}

// ---- Phase 2 — Sub-phase 2: `instanceof` recognizer + runtime check ----

#[test]
fn instanceof_narrows_two_class_union_and_runs() {
    // Positive branch sees `Class(Cat)` — the variant-only field `purrs` is
    // accessible. Negative branch sees `Class(Dog)` — the variant-only
    // field `barks` is accessible. Runtime check loads vtable at offset 0
    // and compares against Cat's `vtable_offset`.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Animal {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Cat extends Animal {
            purrs: i32;
            constructor(hp: i32, purrs: i32) { super(hp); }
        }
        class Dog extends Animal {
            barks: i32;
            constructor(hp: i32, barks: i32) { super(hp); }
        }
        type Pet = Cat | Dog;

        function describe(p: Pet): void {
            if (p instanceof Cat) {
                sink(p.purrs as f64);
            } else {
                sink(p.barks as f64);
            }
        }

        export function tick(_me: i32): void {
            const a: Pet = new Cat(10, 5);
            const b: Pet = new Dog(20, 3);
            describe(a);
            describe(b);
        }
        "#,
    );
    assert_eq!(values, vec![5.0, 3.0]);
}

#[test]
fn instanceof_parent_match_else_is_never() {
    // `instanceof Animal` on `Cat | Dog` matches both members (both
    // descend from Animal). Positive branch keeps `Subunion(Cat, Dog)` —
    // shared field `hp` is accessible. Negative branch is `Never`, so an
    // `assertNever` call there compiles.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Animal {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Cat extends Animal {
            purrs: i32;
            constructor(hp: i32, purrs: i32) { super(hp); }
        }
        class Dog extends Animal {
            barks: i32;
            constructor(hp: i32, barks: i32) { super(hp); }
        }
        type Pet = Cat | Dog;

        function assertNever(x: never): i32 { return 0; }

        function show_hp(p: Pet): void {
            if (p instanceof Animal) {
                sink(p.hp as f64);
            } else {
                assertNever(p);
            }
        }

        export function tick(_me: i32): void {
            const a: Pet = new Cat(7, 1);
            show_hp(a);
        }
        "#,
    );
    assert_eq!(values, vec![7.0]);
}

#[test]
fn instanceof_child_match_else_is_subunion_with_shared_field() {
    // 3-class union (parent + 2 children). `instanceof Cat` matches one
    // variant; the negative branch is `Subunion(Animal, Dog)` and shared
    // field `hp` resolves on it via the Phase 1 sub-phase 7 path.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Animal {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Cat extends Animal {
            purrs: i32;
            constructor(hp: i32, purrs: i32) { super(hp); }
        }
        class Dog extends Animal {
            barks: i32;
            constructor(hp: i32, barks: i32) { super(hp); }
        }
        type Pet = Animal | Cat | Dog;

        function summarize(p: Pet): void {
            if (p instanceof Cat) {
                sink(p.purrs as f64);
            } else {
                sink(p.hp as f64);
            }
        }

        export function tick(_me: i32): void {
            const c: Pet = new Cat(11, 4);
            const d: Pet = new Dog(22, 9);
            const a: Pet = new Animal(33);
            summarize(c);
            summarize(d);
            summarize(a);
        }
        "#,
    );
    assert_eq!(values, vec![4.0, 22.0, 33.0]);
}

#[test]
fn instanceof_exhaustive_chain_with_assert_never_compiles_and_runs() {
    // After `if (p instanceof Cat)` on a 2-class union, the else branch
    // sees `Class(Dog)`. The redundant-looking `else if (p instanceof Dog)`
    // narrows `Class(Dog)` further: positive stays `Class(Dog)`, negative
    // becomes `Never` so the trailing `assertNever(p)` type-checks. The
    // recognizer's class-singleton fallback drives this.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Animal {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Cat extends Animal {
            purrs: i32;
            constructor(hp: i32, purrs: i32) { super(hp); }
        }
        class Dog extends Animal {
            barks: i32;
            constructor(hp: i32, barks: i32) { super(hp); }
        }
        type Pet = Cat | Dog;

        function assertNever(x: never): i32 { return 0; }

        function describe(p: Pet): void {
            if (p instanceof Cat) {
                sink(p.purrs as f64);
            } else if (p instanceof Dog) {
                sink(p.barks as f64);
            } else {
                assertNever(p);
            }
        }

        export function tick(_me: i32): void {
            const a: Pet = new Cat(1, 7);
            const b: Pet = new Dog(2, 9);
            describe(a);
            describe(b);
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 9.0]);
}

#[test]
fn instanceof_non_exhaustive_chain_assert_never_rejected() {
    // 3-class union, one variant left un-handled — the trailing
    // `assertNever(p)` sees `Class(Dog)` (or `Subunion`) and rejects.
    // Diagnostic must name `Dog` so the user knows what's missing.
    let err = compile_err(
        r#"
        class Animal {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Cat extends Animal {
            purrs: i32;
            constructor(hp: i32, purrs: i32) { super(hp); }
        }
        class Dog extends Animal {
            barks: i32;
            constructor(hp: i32, barks: i32) { super(hp); }
        }
        type Pet = Cat | Dog;

        function assertNever(x: never): i32 { return 0; }

        export function tick(_me: i32): void {
            const p: Pet = new Cat(1, 2);
            if (p instanceof Cat) {
            } else {
                assertNever(p);
            }
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("never") && msg.contains("Dog"),
        "expected non-exhaustive diagnostic to mention `never` and the un-handled `Dog`, got: {msg}"
    );
}

#[test]
fn instanceof_unrelated_polymorphic_class_rejected() {
    // `Frog` is polymorphic (extends `Reptile`) but unrelated to the union
    // members. Codegen detects the empty matched set and rejects the test
    // as statically false.
    let err = compile_err(
        r#"
        class Reptile {
            scales: i32;
            constructor(s: i32) {}
        }
        class Frog extends Reptile {
            constructor(s: i32) { super(s); }
        }
        class Animal {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Cat extends Animal {
            purrs: i32;
            constructor(hp: i32, purrs: i32) { super(hp); }
        }
        class Dog extends Animal {
            barks: i32;
            constructor(hp: i32, barks: i32) { super(hp); }
        }
        type Pet = Cat | Dog;

        export function tick(_me: i32): void {
            const p: Pet = new Cat(1, 2);
            if (p instanceof Frog) {}
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("Frog") && msg.contains("Pet") && msg.contains("statically false"),
        "expected diagnostic naming the unrelated class and the union, got: {msg}"
    );
}

#[test]
fn instanceof_against_non_polymorphic_class_rejected() {
    // Right operand resolves to a registered class, but the class is a
    // leaf with no inheritance — has no vtable pointer to inspect.
    let err = compile_err(
        r#"
        class Animal {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Cat extends Animal {
            purrs: i32;
            constructor(hp: i32, purrs: i32) { super(hp); }
        }
        class Dog extends Animal {
            barks: i32;
            constructor(hp: i32, barks: i32) { super(hp); }
        }
        class Loner {
            tag: i32;
            constructor(tag: i32) {}
        }
        type Pet = Cat | Dog;

        export function tick(_me: i32): void {
            const p: Pet = new Cat(1, 2);
            if (p instanceof Loner) {}
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("Loner") && msg.contains("polymorphic"),
        "expected diagnostic naming the leaf class and `polymorphic`, got: {msg}"
    );
}

#[test]
fn instanceof_with_non_identifier_right_operand_rejected() {
    // The right operand of `instanceof` must name a class. Anything else
    // (numeric literal, member expression, etc.) fails fast with a clear
    // message instead of a confusing operand-type error.
    let err = compile_err(
        r#"
        class Animal {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Cat extends Animal {
            purrs: i32;
            constructor(hp: i32, purrs: i32) { super(hp); }
        }
        class Dog extends Animal {
            barks: i32;
            constructor(hp: i32, barks: i32) { super(hp); }
        }
        type Pet = Cat | Dog;

        export function tick(_me: i32): void {
            const p: Pet = new Cat(1, 2);
            if (p instanceof 42) {}
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("instanceof") && msg.contains("class"),
        "expected diagnostic naming `instanceof` and the right-operand requirement, got: {msg}"
    );
}

#[test]
fn instanceof_with_unknown_identifier_right_rejected() {
    // Right operand is an identifier but doesn't resolve to a registered
    // class (it's a local variable here). Same surface as the
    // non-identifier case but a separate error path.
    let err = compile_err(
        r#"
        class Animal {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Cat extends Animal {
            purrs: i32;
            constructor(hp: i32, purrs: i32) { super(hp); }
        }
        class Dog extends Animal {
            barks: i32;
            constructor(hp: i32, barks: i32) { super(hp); }
        }
        type Pet = Cat | Dog;

        export function tick(_me: i32): void {
            const p: Pet = new Cat(1, 2);
            const x: i32 = 7;
            if (p instanceof x) {}
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("'x'") && msg.contains("class"),
        "expected diagnostic mentioning the non-class identifier `x`, got: {msg}"
    );
}

// ---- Phase 2 sub-phase 3: shared-method dispatch on union ----

#[test]
fn shared_method_from_common_base_dispatches_polymorphically() {
    // The shared-method rule mirrors Phase 1's shared-field rule: every
    // variant declares the method at the same vtable slot with matching
    // params/return. When the method is owned by a common ancestor, slot
    // alignment is automatic — Cat inherits Animal's `greet` at slot 0,
    // Dog overrides at the same slot. `p.greet()` on the un-narrowed
    // union compiles and dispatches via vtable. Runtime returns the
    // overriding child's value.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Animal {
            constructor() {}
            greet(): i32 { return 7; }
        }
        class Cat extends Animal {
            constructor() { super(); }
        }
        class Dog extends Animal {
            constructor() { super(); }
            greet(): i32 { return 99; }
        }
        type Pet = Cat | Dog;

        function speak(p: Pet): i32 {
            return p.greet();
        }

        export function tick(_me: i32): void {
            const c: Pet = new Cat();
            const d: Pet = new Dog();
            sink(speak(c) as f64);
            sink(speak(d) as f64);
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 99.0]);
}

#[test]
fn shared_method_independently_declared_without_common_base_rejected() {
    // Both children inherit from Animal (passes the polymorphism gate)
    // but neither base owns `greet`. Cat and Dog independently declare
    // it. With Cat's vtable having `greet:0` and Dog's vtable having
    // `bark:0, greet:1`, the slots disagree — no shared call_indirect
    // site is possible. Diagnostic must steer the user to either a
    // common base or `instanceof` narrowing.
    let err = compile_err(
        r#"
        class Animal {
            constructor() {}
        }
        class Cat extends Animal {
            constructor() { super(); }
            greet(): i32 { return 1; }
        }
        class Dog extends Animal {
            constructor() { super(); }
            bark(): i32 { return 2; }
            greet(): i32 { return 3; }
        }
        type Pet = Cat | Dog;

        export function tick(_me: i32): void {
            const p: Pet = new Cat();
            const _ignore: i32 = p.greet();
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("'greet'")
            && msg.contains("'Pet'")
            && (msg.contains("slots differ") || msg.contains("common base"))
            && msg.contains("instanceof"),
        "expected diagnostic naming method/union and suggesting common-base or instanceof, got: {msg}"
    );
}

#[test]
fn shared_method_with_mismatched_signatures_rejected() {
    // Both Cat and Dog declare `greet` at slot 0 (parent has empty
    // vtable, so each child's first declaration lands at slot 0). But
    // signatures disagree — Cat returns i32, Dog returns f64. The
    // call_indirect site can't pick one type, so the helper rejects with
    // a signature-mismatch diagnostic.
    let err = compile_err(
        r#"
        class Animal {
            constructor() {}
        }
        class Cat extends Animal {
            constructor() { super(); }
            greet(): i32 { return 1; }
        }
        class Dog extends Animal {
            constructor() { super(); }
            greet(): f64 { return 1.0; }
        }
        type Pet = Cat | Dog;

        export function tick(_me: i32): void {
            const p: Pet = new Cat();
            p.greet();
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("'greet'")
            && msg.contains("'Pet'")
            && (msg.contains("parameter") || msg.contains("return"))
            && msg.contains("instanceof"),
        "expected diagnostic mentioning param/return mismatch and suggesting instanceof, got: {msg}"
    );
}

#[test]
fn shared_method_missing_on_one_variant_rejected() {
    // Cat declares `greet`, Dog does not. The un-narrowed union can't
    // dispatch — the helper returns `MissingOnVariant("Dog")` and the
    // caller surfaces both the method name and the offending variant.
    let err = compile_err(
        r#"
        class Animal {
            constructor() {}
        }
        class Cat extends Animal {
            constructor() { super(); }
            greet(): i32 { return 1; }
        }
        class Dog extends Animal {
            constructor() { super(); }
        }
        type Pet = Cat | Dog;

        export function tick(_me: i32): void {
            const p: Pet = new Cat();
            const _ignore: i32 = p.greet();
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("'greet'")
            && msg.contains("'Pet'")
            && msg.contains("'Dog'")
            && msg.contains("instanceof"),
        "expected diagnostic naming method/union and the variant lacking it, got: {msg}"
    );
}

#[test]
fn shared_method_dispatch_on_subunion_after_instanceof_narrowing() {
    // 3-class union where the shared method exists on Cat and Dog but
    // not on Fish. The un-narrowed dispatch would fail; after
    // `if (p instanceof Fish)` else, the local refines to
    // `Subunion(Cat, Dog)` and both declare `speak` at slot 0 of their
    // own vtables (parent Animal has empty vtable). The helper accepts
    // the refined member set and dispatches polymorphically. Cat and Dog
    // have unrelated declarations, but their slots happen to align —
    // this is the common case after narrowing.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Animal {
            constructor() {}
        }
        class Cat extends Animal {
            constructor() { super(); }
            speak(): i32 { return 11; }
        }
        class Dog extends Animal {
            constructor() { super(); }
            speak(): i32 { return 22; }
        }
        class Fish extends Animal {
            constructor() { super(); }
        }
        type Pet = Cat | Dog | Fish;

        function maybe_speak(p: Pet): void {
            if (p instanceof Fish) {
                sink(0.0);
            } else {
                sink(p.speak() as f64);
            }
        }

        export function tick(_me: i32): void {
            const c: Pet = new Cat();
            const d: Pet = new Dog();
            const f: Pet = new Fish();
            maybe_speak(c);
            maybe_speak(d);
            maybe_speak(f);
        }
        "#,
    );
    assert_eq!(values, vec![11.0, 22.0, 0.0]);
}

// ---- Phase 2 sub-phase 4: mixed shape + class unions ----

#[test]
fn mixed_union_shared_field_on_unnarrowed_compiles_and_runs() {
    // Phase 1's shared-field rule already walks the unified
    // shape/class registry namespace via `UnionMember::Shape`. This
    // confirms it works across a mixed union when offsets happen to
    // align — Rect lays its fields at [kind:0, hp:4] (no vtable
    // pointer for shapes) while Cat (polymorphic) lays them at
    // [vtable:0, hp:4] inherited from Animal. Both place `hp` at
    // offset 4, so `m.hp` reads correctly on either branch without
    // narrowing.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Rect = { kind: 'rect'; hp: i32 };
        class Animal {
            hp: i32;
            constructor(hp: i32) {}
        }
        class Cat extends Animal {
            constructor(hp: i32) { super(hp); }
        }
        type Mixed = Rect | Cat;

        function read_hp(m: Mixed): i32 {
            return m.hp;
        }

        export function tick(_me: i32): void {
            const r: Mixed = { kind: 'rect', hp: 9 };
            const c: Mixed = new Cat(17);
            sink(read_hp(r) as f64);
            sink(read_hp(c) as f64);
        }
        "#,
    );
    assert_eq!(values, vec![9.0, 17.0]);
}

#[test]
fn mixed_union_narrow_by_instanceof_class_or_shape() {
    // Mirror of the above but using `instanceof Cat` as the predicate.
    // Positive branch refines to `Class(Cat)`; negative branch refines
    // to the surviving shape variant `Rect`. Both paths read variant-
    // specific data — proving the symmetric narrowing works.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Rect = { kind: 'rect'; w: f64; h: f64 };
        class Animal {
            constructor() {}
        }
        class Cat extends Animal {
            constructor() { super(); }
            purr(): i32 { return 7; }
        }
        type Mixed = Rect | Cat;

        function summarize(m: Mixed): void {
            if (m instanceof Cat) {
                sink(m.purr() as f64);
            } else {
                sink(m.w + m.h);
            }
        }

        export function tick(_me: i32): void {
            const r: Mixed = { kind: 'rect', w: 5.0, h: 2.0 };
            const c: Mixed = new Cat();
            summarize(c);
            summarize(r);
        }
        "#,
    );
    assert_eq!(values, vec![7.0, 7.0]);
}

#[test]
fn mixed_union_shared_method_on_unnarrowed_rejected_with_shape_hint() {
    // `m.purr()` on un-narrowed `Mixed = Rect | Cat` cannot satisfy the
    // shared-method rule: shapes have no methods at all. The diagnostic
    // should call this out specifically — a generic "method 'purr' not
    // declared on every variant" is correct but misleading because a
    // shape will *never* satisfy the rule no matter what the user adds.
    let err = compile_err(
        r#"
        type Rect = { kind: 'rect'; w: f64; h: f64 };
        class Animal {
            constructor() {}
        }
        class Cat extends Animal {
            constructor() { super(); }
            purr(): i32 { return 7; }
        }
        type Mixed = Rect | Cat;

        export function tick(_me: i32): void {
            const m: Mixed = new Cat();
            const _ignore: i32 = m.purr();
        }
        "#,
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("'purr'")
            && msg.contains("'Mixed'")
            && msg.contains("shape")
            && msg.contains("no methods"),
        "expected diagnostic mentioning method/union and shapes-have-no-methods, got: {msg}"
    );
}

#[test]
fn mixed_union_three_variants_instanceof_chain_exhaustive() {
    // 3-variant mixed union (one shape + two classes). An `instanceof`
    // chain consumes the class variants; the trailing `else` refines
    // via N-arithmetic to `Class(Rect)` (single survivor) so the shape
    // can be read directly. This is the recommended exhaustive shape
    // for mixed unions — `assertNever` doesn't apply naturally because
    // tscc has no instanceof-equivalent for shapes, so the shape side
    // is consumed by exhausting the chain rather than by an explicit
    // narrowing predicate.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        type Rect = { kind: 'rect'; w: f64; h: f64 };
        class Animal {
            constructor() {}
        }
        class Cat extends Animal {
            constructor() { super(); }
            purr(): i32 { return 11; }
        }
        class Dog extends Animal {
            constructor() { super(); }
            bark(): i32 { return 22; }
        }
        type Mixed = Rect | Cat | Dog;

        function describe(m: Mixed): void {
            if (m instanceof Cat) {
                sink(m.purr() as f64);
            } else if (m instanceof Dog) {
                sink(m.bark() as f64);
            } else {
                sink(m.w * m.h);
            }
        }

        export function tick(_me: i32): void {
            const r: Mixed = { kind: 'rect', w: 4.0, h: 5.0 };
            const c: Mixed = new Cat();
            const d: Mixed = new Dog();
            describe(r);
            describe(c);
            describe(d);
        }
        "#,
    );
    assert_eq!(values, vec![20.0, 11.0, 22.0]);
}

// ---- f64 union path coverage: arrow / array / as / generics ----

#[test]
fn arrow_function_with_f64_union_param_runs() {
    // Arrow function whose parameter is a named pure-`f64`-literal union.
    // Closure inference (`infer_arrow_sig`, `expr/closure.rs`) consults the
    // override map so the closure's signature has `F64` not `I32`.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Half = 0.5 | 1.5;

        export function tick(_me: i32): void {
            const f: (x: Half) => f64 = (x: Half): f64 => {
                if (x === 0.5) return 10.0;
                return 20.0;
            };
            sink(f(0.5));
            sink(f(1.5));
        }
        "#,
    );
    assert_eq!(values, vec![10.0, 20.0]);
}

#[test]
fn array_of_f64_union_loads_correctly() {
    // `Half[]` — element-type extraction (`get_array_element_type`) consults
    // the override map so element loads use `f64.load`, not `i32.load`.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Half = 0.5 | 1.5 | 2.5;

        export function tick(_me: i32): void {
            const arr: Half[] = [0.5, 1.5, 2.5];
            for (let i = 0; i < arr.length; i = i + 1) {
                sink(arr[i]);
            }
        }
        "#,
    );
    assert_eq!(values, vec![0.5, 1.5, 2.5]);
}

#[test]
fn as_cast_to_f64_union_runs() {
    // `0.5 as Half` — the `as`-expression target type resolves through the
    // override map (`expr/mod.rs`'s TSAsExpression arm).
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Half = 0.5 | 1.5;

        export function tick(_me: i32): void {
            const x: Half = 0.5 as Half;
            sink(x);
        }
        "#,
    );
    assert_eq!(values, vec![0.5]);
}

#[test]
fn inline_f64_union_as_generic_argument_runs() {
    // `Box<0.5 | 1.5>` — inline pure-`f64` union as a generic argument.
    // `resolve_bound_type` produces `BoundType::Union { name, wasm_ty: F64 }`
    // by walking the AST members (`inline_union_wasm_ty`), which threads
    // through monomorphization so `Box$__Union$n_0.5$n_1.5` has an `f64`
    // value field.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Box<T> {
            value: T;
            constructor(v: T) { this.value = v; }
        }

        export function tick(_me: i32): void {
            const b: Box<0.5 | 1.5> = new Box<0.5 | 1.5>(1.5);
            sink(b.value);
        }
        "#,
    );
    assert_eq!(values, vec![1.5]);
}

#[test]
fn named_f64_union_as_generic_argument_runs() {
    // `Box<Half>` where `Half = 0.5 | 1.5` — named pure-`f64` union as a
    // generic argument. `resolve_bound_type` runs in Pass 0a-ii, before
    // `discover_unions` populates the registry, so the TSTypeReference arm
    // can't tell `Half` is a union (let alone an `f64` one) and falls
    // through to `BoundType::Class("Half")` which has `wasm_ty == I32`.
    // This test pins the limitation; the inline form `Box<0.5 | 1.5>` is
    // the supported workaround.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        type Half = 0.5 | 1.5;

        class Box<T> {
            value: T;
            constructor(v: T) { this.value = v; }
        }

        export function tick(_me: i32): void {
            const b: Box<Half> = new Box<Half>(1.5);
            sink(b.value);
        }
        "#,
    );
    assert_eq!(values, vec![1.5]);
}
