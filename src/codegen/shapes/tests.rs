//! Unit tests for shape discovery. Exercise the private walker internals,
//! which is why these live as a child module rather than in `tests/`.

use super::*;
use crate::codegen::generics;
use oxc_allocator::Allocator;

/// Parse + run Pass 0a through 0a-iii so discover_shapes has the
/// populated class_names/templates it expects.
fn discover(source: &str) -> Result<ShapeRegistry, CompileError> {
    let alloc = Allocator::default();
    let program = crate::parse::parse(&alloc, source)?;
    let (class_templates, fn_templates) = generics::discover_templates(&program);
    let mut class_names: HashSet<String> = HashSet::new();
    for stmt in &program.body {
        if let Statement::ClassDeclaration(class) = stmt
            && let Some(id) = &class.id
        {
            let name = id.name.as_str().to_string();
            if !class_templates.contains_key(&name) {
                class_names.insert(name);
            }
        }
    }
    // Drive generic instantiation collection so mangled names land in
    // class_names — mirrors what compile_module does before shape
    // discovery.
    let empty_overrides = std::collections::HashMap::new();
    let result = generics::collect_instantiations(
        &program,
        &class_templates,
        &fn_templates,
        &class_names,
        &empty_overrides,
    )?;
    for inst in &result.class_insts {
        class_names.insert(inst.mangled_name.clone());
    }
    // Shape discovery is the unit under test — drive it directly.
    // Note: the AST is dropped when the allocator goes out of scope here
    // but the ShapeRegistry's resolved BoundType + String field data is
    // owned, so the registry survives.
    discover_shapes(&program, &class_names, &class_templates, &fn_templates, &empty_overrides)
}

#[test]
fn named_type_alias_is_registered() {
    let reg = discover(
        r#"
            type Point = { x: number; y: number };
            "#,
    )
    .unwrap();
    let s = reg.get_by_name("Point").expect("Point registered");
    assert_eq!(s.kind, ShapeKind::Named);
    assert_eq!(s.fields.len(), 2);
    let names: Vec<&str> = s.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["x", "y"]);
    assert!(matches!(s.fields[0].ty, BoundType::F64));
    assert!(matches!(s.fields[1].ty, BoundType::F64));
}

#[test]
fn named_interface_is_registered() {
    let reg = discover(
        r#"
            interface Point { x: number; y: number }
            "#,
    )
    .unwrap();
    let s = reg.get_by_name("Point").expect("Point registered");
    assert_eq!(s.kind, ShapeKind::Named);
    assert_eq!(s.fields.len(), 2);
}

#[test]
fn reorder_has_same_fingerprint_and_dedupes_to_first_seen_layout() {
    let reg = discover(
        r#"
            type A = { x: number; y: number };
            type B = { y: number; x: number };
            "#,
    )
    .unwrap();
    // Same fingerprint => both by_name entries point at the same shape
    // index, and that shape's layout is A's declaration order.
    let a_idx = reg.by_name["A"];
    let b_idx = reg.by_name["B"];
    assert_eq!(a_idx, b_idx);
    let s = &reg.shapes[a_idx];
    let names: Vec<&str> = s.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["x", "y"], "first-seen (A) layout wins");
}

#[test]
fn different_shapes_stay_distinct() {
    let reg = discover(
        r#"
            type P2 = { x: number; y: number };
            type P3 = { x: number; y: number; z: number };
            "#,
    )
    .unwrap();
    assert_eq!(reg.shapes.len(), 2);
    assert_ne!(reg.shapes[0].fingerprint, reg.shapes[1].fingerprint);
}

#[test]
fn anonymous_inline_type_literal_in_annotation() {
    let reg = discover(
        r#"
            function f(p: { x: number; y: number }): void {}
            "#,
    )
    .unwrap();
    assert_eq!(reg.shapes.len(), 1);
    let s = &reg.shapes[0];
    assert_eq!(s.kind, ShapeKind::Anonymous);
    assert!(s.name.starts_with(ANON_SHAPE_PREFIX));
}

#[test]
fn anonymous_inline_aliases_into_named() {
    let reg = discover(
        r#"
            type Point = { x: number; y: number };
            function f(p: { x: number; y: number }): void {}
            "#,
    )
    .unwrap();
    assert_eq!(reg.shapes.len(), 1, "anonymous dedupes into named");
    let s = &reg.shapes[0];
    assert_eq!(s.kind, ShapeKind::Named);
    assert_eq!(s.name, "Point");
}

#[test]
fn object_literal_with_inferable_fields_registers_shape() {
    let reg = discover(
        r#"
            function f(): void {
                const p = { x: 1.5, y: 2.5 };
            }
            "#,
    )
    .unwrap();
    // Inferred as `{x: f64, y: f64}` — first literal registers an
    // anonymous shape.
    assert_eq!(reg.shapes.len(), 1);
    let s = &reg.shapes[0];
    assert_eq!(s.kind, ShapeKind::Anonymous);
    assert!(matches!(s.fields[0].ty, BoundType::F64));
    assert!(matches!(s.fields[1].ty, BoundType::F64));
}

#[test]
fn object_literal_aliases_to_named_type_when_fingerprint_matches() {
    let reg = discover(
        r#"
            type Point = { x: number; y: number };
            function f(): void {
                const p: Point = { x: 1.0, y: 2.0 };
            }
            "#,
    )
    .unwrap();
    assert_eq!(reg.shapes.len(), 1);
    let s = &reg.shapes[0];
    assert_eq!(s.kind, ShapeKind::Named);
    assert_eq!(s.name, "Point");
}

#[test]
fn class_collision_is_an_error() {
    let err = discover(
        r#"
            class Point { x: f64 = 0; y: f64 = 0; }
            type Point = { x: number; y: number };
            "#,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Point"), "got: {msg}");
    assert!(msg.contains("already declared as a class"), "got: {msg}");
}

#[test]
fn generic_template_collision_is_an_error() {
    let err = discover(
        r#"
            class Box<T> { value: T = 0 as any; }
            type Box = { value: number };
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("generic class"));
}

#[test]
fn duplicate_named_shape_is_an_error() {
    let err = discover(
        r#"
            type Point = { x: number; y: number };
            type Point = { a: string };
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("duplicate shape type"));
}

#[test]
fn interface_extends_prefixes_parent_fields() {
    let reg = discover(
        r#"
            interface Base { x: number }
            interface Child extends Base { y: number }
            "#,
    )
    .unwrap();
    let child = reg.get_by_name("Child").expect("Child registered");
    let names: Vec<&str> = child.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["x", "y"], "parent's fields form the prefix");
}

#[test]
fn interface_extends_works_regardless_of_declaration_order() {
    let reg = discover(
        r#"
            interface Child extends Base { y: number }
            interface Base { x: number }
            "#,
    )
    .unwrap();
    let child = reg.get_by_name("Child").expect("Child registered");
    let names: Vec<&str> = child.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["x", "y"]);
}

#[test]
fn interface_extends_chain_is_transitive() {
    let reg = discover(
        r#"
            interface A { a: number }
            interface B extends A { b: number }
            interface C extends B { c: number }
            "#,
    )
    .unwrap();
    let c = reg.get_by_name("C").unwrap();
    let names: Vec<&str> = c.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);
}

#[test]
fn interface_extends_type_alias_is_allowed() {
    // TS allows `interface X extends TypeAlias` when the alias resolves to
    // an object shape. Our topo sort pulls the alias before the interface.
    let reg = discover(
        r#"
            type Base = { x: number };
            interface Child extends Base { y: number }
            "#,
    )
    .unwrap();
    let child = reg.get_by_name("Child").unwrap();
    let names: Vec<&str> = child.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["x", "y"]);
}

#[test]
fn interface_extends_unknown_parent_errors() {
    let err = discover(
        r#"
            interface Child extends Missing { y: number }
            "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("not a known shape type"),
        "got: {}",
        err
    );
}

#[test]
fn interface_extends_class_is_rejected() {
    let err = discover(
        r#"
            class Base { x: f64 = 0; }
            interface Child extends Base { y: number }
            "#,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("extends class"), "got: {msg}");
}

#[test]
fn interface_extends_field_shadow_is_rejected() {
    let err = discover(
        r#"
            interface Base { x: number }
            interface Child extends Base { x: number; y: number }
            "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("redeclares field 'x'"),
        "got: {}",
        err
    );
}

#[test]
fn interface_circular_extends_is_rejected() {
    let err = discover(
        r#"
            interface A extends B { a: number }
            interface B extends A { b: number }
            "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("circular interface inheritance"),
        "got: {}",
        err
    );
}

#[test]
fn interface_self_extends_is_rejected() {
    let err = discover(
        r#"
            interface A extends A { a: number }
            "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("circular interface inheritance"),
        "got: {}",
        err
    );
}

#[test]
fn interface_multiple_extends_is_rejected() {
    let err = discover(
        r#"
            interface A { a: number }
            interface B { b: number }
            interface C extends A, B { c: number }
            "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("extends multiple parents"),
        "got: {}",
        err
    );
}

#[test]
fn method_signature_in_interface_is_rejected() {
    let err = discover(
        r#"
            interface Logger { log(): void }
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("method signatures"));
}

#[test]
fn optional_property_is_rejected() {
    let err = discover(
        r#"
            type Partial = { x?: number };
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("optional"));
}

#[test]
fn nested_anonymous_shape_in_field_is_registered() {
    let reg = discover(
        r#"
            type Outer = { inner: { v: number } };
            "#,
    )
    .unwrap();
    // Two shapes: the inner anonymous and the outer named.
    assert_eq!(reg.shapes.len(), 2);
    let inner = &reg.shapes[0];
    let outer = &reg.shapes[1];
    assert_eq!(inner.kind, ShapeKind::Anonymous);
    assert_eq!(outer.kind, ShapeKind::Named);
    assert_eq!(outer.name, "Outer");
    // Outer's only field references the inner shape by class name.
    match &outer.fields[0].ty {
        BoundType::Class(n) => {
            assert_eq!(n, &inner.name);
        }
        other => panic!("expected Class(...), got {other:?}"),
    }
}

#[test]
fn empty_shape_is_rejected() {
    let err = discover(
        r#"
            type Empty = {};
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("empty object shape"));
}

#[test]
fn shape_referencing_known_class_resolves_to_class_binding() {
    let reg = discover(
        r#"
            class Entity { id: i32 = 0; }
            type Ref = { target: Entity };
            "#,
    )
    .unwrap();
    let s = reg.get_by_name("Ref").unwrap();
    match &s.fields[0].ty {
        BoundType::Class(n) => assert_eq!(n, "Entity"),
        other => panic!("expected Class(Entity), got {other:?}"),
    }
}

#[test]
fn fingerprint_is_order_independent() {
    let reg = discover(
        r#"
            type A = { y: number; x: number };
            "#,
    )
    .unwrap();
    let fp = reg.get_by_name("A").unwrap().fingerprint.clone();
    assert_eq!(fp, "x_f64$y_f64", "got: {fp}");
}

#[test]
fn tuple_annotation_is_registered() {
    let reg = discover(
        r#"
            function f(t: [i32, f64]): void {}
            "#,
    )
    .unwrap();
    assert_eq!(reg.shapes.len(), 1);
    let s = &reg.shapes[0];
    assert!(s.is_tuple);
    assert_eq!(s.name, "__Tuple$i32$f64");
    assert_eq!(s.fingerprint, "i32$f64");
    let names: Vec<&str> = s.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["_0", "_1"]);
}

#[test]
fn tuple_is_positional_not_set() {
    // `[i32, f64]` and `[f64, i32]` are distinct tuples — unlike object
    // shapes whose identity is unordered.
    let reg = discover(
        r#"
            function f(a: [i32, f64], b: [f64, i32]): void {}
            "#,
    )
    .unwrap();
    assert_eq!(
        reg.shapes.len(),
        2,
        "different orderings register distinctly"
    );
}

#[test]
fn identical_tuples_dedupe() {
    let reg = discover(
        r#"
            function f(a: [string, i32]): void {}
            function g(b: [string, i32]): void {}
            "#,
    )
    .unwrap();
    assert_eq!(reg.shapes.len(), 1);
}

#[test]
fn tuple_field_in_shape_registers_both() {
    let reg = discover(
        r#"
            type Row = { key: string; pos: [f64, f64] };
            "#,
    )
    .unwrap();
    assert_eq!(reg.shapes.len(), 2);
    let tuple = &reg.shapes[0];
    let row = reg.get_by_name("Row").unwrap();
    assert!(tuple.is_tuple);
    assert_eq!(tuple.name, "__Tuple$f64$f64");
    match &row.fields[1].ty {
        BoundType::Class(n) => assert_eq!(n, &tuple.name),
        other => panic!("expected Class(tuple), got {other:?}"),
    }
}

#[test]
fn nested_tuple_registers() {
    let reg = discover(
        r#"
            function f(n: [[i32, i32], string]): void {}
            "#,
    )
    .unwrap();
    // Inner [i32, i32] and outer [[i32, i32], string].
    assert_eq!(reg.shapes.len(), 2);
    let inner = reg.get_by_name("__Tuple$i32$i32").unwrap();
    let outer = reg.get_by_name("__Tuple$__Tuple$i32$i32$string").unwrap();
    assert!(inner.is_tuple);
    assert!(outer.is_tuple);
}

#[test]
fn empty_tuple_is_rejected() {
    let err = discover(
        r#"
            function f(t: []): void {}
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("empty tuple"));
}

#[test]
fn optional_tuple_element_is_rejected() {
    let err = discover(
        r#"
            function f(t: [i32, f64?]): void {}
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("optional tuple"));
}

#[test]
fn rest_tuple_element_is_rejected() {
    let err = discover(
        r#"
            function f(t: [i32, ...i32[]]): void {}
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("rest tuple"));
}

#[test]
fn named_tuple_member_labels_are_accepted_and_discarded() {
    // Phase E.5: `[x: i32, y: f64]` parses as two TSNamedTupleMember nodes.
    // The labels are purely documentation — identity stays positional, so
    // the fingerprint must match a plain `[i32, f64]` tuple.
    let reg = discover(
        r#"
            function f(t: [x: i32, y: f64]): void {}
            function g(u: [i32, f64]): void {}
            "#,
    )
    .unwrap();
    // Both signatures must dedupe to a single tuple shape.
    let tuples: Vec<_> = reg.shapes.iter().filter(|s| s.is_tuple).collect();
    assert_eq!(tuples.len(), 1, "named-tuple should dedupe with bare tuple");
    assert_eq!(tuples[0].fingerprint, "i32$f64");
}

#[test]
fn named_tuple_optional_is_rejected() {
    let err = discover(
        r#"
            function f(t: [x: i32, y?: f64]): void {}
            "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("optional named tuple element"),
        "got: {err}"
    );
}
