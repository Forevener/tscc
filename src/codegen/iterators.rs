//! User-defined iterables (Phase 2 of the iterator-protocol roadmap).
//!
//! Recognizes the structural pair
//!
//! ```ts
//! class Foo {
//!     [Symbol.iterator](): FooIterator { … }   // iterable side
//! }
//! class FooIterator {
//!     next(): { value: T; done: boolean } { … } // iterator side
//! }
//! ```
//!
//! and exposes the resolved layout information `emit_for_of_user_iterable`
//! consumes — value field offset/type, done field offset, the iterator
//! class name, and whether the iterator declares `return()` / `throw()`.
//!
//! Detection is on-demand at the `for..of` site rather than a separate
//! pre-codegen pass: every lookup is a handful of `HashMap::get` calls
//! against `ClassRegistry`, which is already final by codegen time.
//!
//! ## Spec divergence
//!
//! The ECMAScript `IteratorResult.done` is a getter that may be missing —
//! the spec says "done is undefined" is treated as `false`. tscc requires
//! the field to be present (it's part of the static `next()` return
//! shape); a missing field is rejected at protocol-detection time.
//!
//! `iterator.return()` is supported: when declared on the iterator class
//! it runs on `break` and on early function-return through the loop,
//! mirroring spec semantics. Its declared return type is ignored — the
//! result is dropped — so users can write `return(): void` or
//! `return(): { value: T; done: boolean }` interchangeably for cleanup.
//! `iterator.throw()` is rejected indefinitely (gated on the exceptions
//! feature, long-term roadmap).
//!
//! ## Inheritance
//!
//! `[Symbol.iterator]()` is looked up via parent-chain walk: a subclass
//! inherits its parent's iterator method without re-declaring it. The
//! same rule applies to the iterator class's `next` method.

use std::collections::{HashMap, HashSet};

use crate::codegen::classes::{ClassLayout, ClassRegistry, ITERATOR_METHOD_NAME, MethodSig};
use crate::error::CompileError;
use crate::types::WasmType;

/// Resolved iterator-protocol metadata for an iterable class. Populated by
/// [`resolve_iterator_protocol`]; consumed by `emit_for_of_user_iterable`.
#[derive(Debug, Clone)]
pub(crate) struct IteratorInfo {
    /// Class name returned by the iterable's `[Symbol.iterator]()`.
    /// May equal the iterable's own name when the class is its own
    /// iterator (`[Symbol.iterator]() { return this; }`).
    pub iter_class: String,
    /// Byte offset of the `value` field within the result class.
    pub value_offset: u32,
    /// WasmType of `value`. Drives the loop binding's local type.
    pub value_wasm_ty: WasmType,
    /// Class name when `value` is a class instance (for threading the
    /// loop binding's static type into method-call resolution). `None`
    /// when `value` is a primitive.
    pub value_class: Option<String>,
    /// `true` when the loop binding type is `string` — `member.rs`-style
    /// string locals require the `local_string_vars` flag, not just I32
    /// type info.
    pub value_is_string: bool,
    /// Byte offset of the `done` field. `done` is i32 (the boolean repr).
    pub done_offset: u32,
    /// `true` when the iterator class declares a `return()` method (directly
    /// or via parent-chain inheritance). Drives the Phase 2b cleanup hook:
    /// the for..of lowering wraps the loop in a cleanup block so user
    /// `break` and early function-return route through `__it.return()`
    /// before exiting. Normal completion (`done=true` from `next()`) skips
    /// cleanup per spec.
    pub has_return_method: bool,
}

/// Look up `[Symbol.iterator]()` on `class_name` walking the parent chain.
/// Returns the declaring class and the method signature when found.
pub(crate) fn find_iterator_method<'r>(
    registry: &'r ClassRegistry,
    class_name: &str,
) -> Option<(&'r ClassLayout, &'r MethodSig)> {
    find_method_inherited(registry, class_name, ITERATOR_METHOD_NAME)
}

/// Generic parent-chain method lookup. `methods` on each `ClassLayout`
/// only carries directly-declared methods, not inherited ones — so a
/// subclass that doesn't override its parent's method produces no entry.
/// Walk explicitly to fix that.
fn find_method_inherited<'r>(
    registry: &'r ClassRegistry,
    class_name: &str,
    method_name: &str,
) -> Option<(&'r ClassLayout, &'r MethodSig)> {
    let mut cur = class_name;
    loop {
        let layout = registry.get(cur)?;
        if let Some(sig) = layout.methods.get(method_name) {
            return Some((layout, sig));
        }
        cur = layout.parent.as_deref()?;
    }
}

/// Resolve the iterator-protocol layout for `iterable_class`. Returns
/// `Ok(Some(info))` when the class declares (or inherits) a valid
/// `[Symbol.iterator]()` AND the returned iterator class has a valid
/// `next()` shape. Returns `Ok(None)` when the class does not declare
/// the iterator method at all (caller falls back to the generic
/// "not iterable" diagnostic). Returns `Err(_)` when the iterator
/// method exists but the protocol shape is malformed — the error
/// message names which side failed.
pub(crate) fn resolve_iterator_protocol(
    registry: &ClassRegistry,
    iterable_class: &str,
) -> Result<Option<IteratorInfo>, CompileError> {
    let Some((iterable_layout, iter_sig)) = find_iterator_method(registry, iterable_class) else {
        return Ok(None);
    };

    let iter_class = iter_sig.return_class.as_ref().ok_or_else(|| {
        CompileError::type_err(format!(
            "class '{}' has `[Symbol.iterator]()` but its return type is not a class — \
             annotate the return as a class implementing `next(): {{ value: T; done: boolean }}`",
            iterable_layout.name
        ))
    })?;

    let (_iter_layout, next_sig) =
        find_method_inherited(registry, iter_class, "next").ok_or_else(|| {
            CompileError::type_err(format!(
                "class '{}' returned by `[Symbol.iterator]()` on '{}' has no `next()` method — \
                 add `next(): {{ value: T; done: boolean }}` to make it an iterator",
                iter_class, iterable_layout.name
            ))
        })?;

    // `throw()` requires exception support (long-term roadmap); reject so
    // users see a precise hint rather than a silent semantic gap. Walking
    // the parent chain matches the same lookup rule as `next` / `return`.
    if find_method_inherited(registry, iter_class, "throw").is_some() {
        return Err(CompileError::unsupported(format!(
            "iterator class '{}' declares `throw()`, which requires exception support \
             (planned, not yet implemented) — remove the method or split the cleanup \
             into `return()` once the exceptions feature ships",
            iter_class
        )));
    }

    // `return()` is supported via the cleanup hook. Detect — including
    // inherited declarations — so the call site knows to emit the hook.
    let has_return_method = find_method_inherited(registry, iter_class, "return").is_some();

    let result_class = next_sig.return_class.as_ref().ok_or_else(|| {
        CompileError::type_err(format!(
            "`{}.next()` must return `{{ value: T; done: boolean }}` — its return type \
             is not a recognized class or shape",
            iter_class
        ))
    })?;

    let result_layout = registry.get(result_class).ok_or_else(|| {
        CompileError::codegen(format!(
            "internal: result class '{}' from `{}.next()` is not registered",
            result_class, iter_class
        ))
    })?;

    let &(value_offset, value_wasm_ty) = result_layout.field_map.get("value").ok_or_else(|| {
        CompileError::type_err(format!(
            "`{}.next()` returns `{}` which has no `value` field — \
             expected `{{ value: T; done: boolean }}`",
            iter_class, result_class
        ))
    })?;

    let &(done_offset, done_wasm_ty) = result_layout.field_map.get("done").ok_or_else(|| {
        CompileError::type_err(format!(
            "`{}.next()` returns `{}` which has no `done` field — \
             expected `{{ value: T; done: boolean }}`",
            iter_class, result_class
        ))
    })?;

    if done_wasm_ty != WasmType::I32 {
        return Err(CompileError::type_err(format!(
            "`done` field of `{}` must be a boolean (i32), got {:?}",
            result_class, done_wasm_ty
        )));
    }

    let value_class = result_layout.field_class_types.get("value").cloned();
    let value_is_string = result_layout.field_string_types.contains("value");

    // Suppress unused-binding warnings now that the cached `MethodSig`
    // and `result_class` were trimmed from `IteratorInfo` — they remain
    // useful at recognition time for the validation above.
    let _ = next_sig;
    let _ = result_class;

    Ok(Some(IteratorInfo {
        iter_class: iter_class.clone(),
        value_offset,
        value_wasm_ty,
        value_class,
        value_is_string,
        done_offset,
        has_return_method,
    }))
}

// ============================================================================
// Trivial-iterator inlining (roadmap: near-term).
//
// When a user iterable's `[Symbol.iterator]()` returns a fresh iterator that
// does nothing more than walk a backing `Array<T>` with a single cursor, the
// for..of lowering can rewrite against the underlying array directly — no
// `next()` call survives in the wasm. This is a strict AST-level peephole:
// any heuristic mismatch falls back to the standard protocol path, so user
// code keeps working unchanged.
//
// The recognized shape (canonical):
//
// ```ts
// class BufIter {
//     cursor: i32;          // or `cursor: i32 = 0;`
//     buf: Array<T>;
//     constructor(buf: Array<T>) {
//         this.cursor = 0;  // omit if PropertyDefinition initializer set it
//         this.buf = buf;
//     }
//     next(): { value: T; done: boolean } {
//         if (this.cursor >= this.buf.length) {
//             return { value: <sentinel>, done: true };
//         }
//         const v: T = this.buf[this.cursor];
//         this.cursor = this.cursor + 1;   // or this.cursor++ / this.cursor += 1
//         return { value: v, done: false };
//     }
// }
// class Buf {
//     items: Array<T>;
//     constructor(items: Array<T>) { this.items = items; }
//     [Symbol.iterator](): BufIter { return new BufIter(this.items); }
// }
// ```
//
// Invariants enforced (any miss => fall back, no diagnostic):
// - Iterable's `[Symbol.iterator]()` is directly declared, body is exactly
//   `return new <IterClass>(this.<bufField>);`.
// - Iterable's `<bufField>` is an `Array<T>` field, written ONLY in the
//   iterable's constructor.
// - Iterator class declares no `return()` / `throw()` (direct or inherited).
// - Iterator class has a single-param constructor that initializes the buffer
//   field from the param. Cursor is set to `0` either via constructor body or
//   PropertyDefinition initializer.
// - Iterator's buffer field is written ONLY in its constructor.
// - `next()` body matches the canonical shape above (statements in order).
//
// Detection runs once per program, after class registration. The cached info
// drives a fast path inside `emit_for_of` that loads `iterable.<bufField>`
// (one i32 load) and reuses the standard array `for..of` lowering. The
// result is identical wasm to writing `for (const x of iterable.items)`
// directly — but the user's program keeps the iterator abstraction.

use oxc_ast::ast::{
    AssignmentOperator, AssignmentTarget, BindingPattern, Class, ClassElement, Expression,
    MethodDefinitionKind, ObjectPropertyKind, PropertyKey, PropertyKind, SimpleAssignmentTarget,
    Statement, UpdateOperator, VariableDeclarationKind,
};

use crate::types;

/// Resolved metadata for an iterable class whose iterator is a trivial
/// single-cursor walk over a backing `Array<T>`. Populated by
/// [`detect_trivial_iterables`]; consumed by `emit_for_of`'s fast path.
#[derive(Debug, Clone)]
pub struct TrivialIterableInfo {
    /// Field on the iterable that holds the underlying `Array<T>`. Carried
    /// for diagnostic / debugging reads (e.g. `Debug` printing during
    /// detector authoring); codegen drives off `buffer_offset`.
    #[allow(dead_code, reason = "kept for diagnostics; codegen uses buffer_offset")]
    pub buffer_field: String,
    /// Byte offset of `buffer_field` within the iterable layout.
    pub buffer_offset: u32,
    /// Element `WasmType` of the backing array — drives the loop binding's
    /// local type and the per-element load opcode.
    pub elem_wasm_ty: WasmType,
    /// Element class name when the backing array's element is a class
    /// instance. Threaded into `local_class_types` for the loop binding so
    /// member access on the binding resolves through the class layout.
    pub elem_class: Option<String>,
    /// `true` when the backing array's element type is `string`. Drives the
    /// `local_string_vars` flag for the loop binding (string locals need
    /// the explicit marker, not just I32 type info).
    pub elem_is_string: bool,
}

/// Walk every concrete (non-template) class and detect trivial iterables.
/// Returns a map keyed by iterable class name. Generic templates and
/// monomorphizations are skipped — the AST contains type parameters that
/// don't resolve cleanly without per-instantiation bindings, and the
/// existing protocol path serves them correctly.
pub fn detect_trivial_iterables<'a>(
    class_asts: &HashMap<String, &'a Class<'a>>,
    registry: &ClassRegistry,
    class_names: &HashSet<String>,
    union_overrides: &HashMap<String, WasmType>,
) -> HashMap<String, TrivialIterableInfo> {
    let mut out = HashMap::new();
    for (iterable_name, iterable_ast) in class_asts {
        // Skip monomorphized classes (their AST belongs to a generic template
        // and field/method types depend on per-instantiation bindings).
        if iterable_name.contains('$') {
            continue;
        }
        if let Some(info) = try_detect_trivial_iterable(
            iterable_name,
            iterable_ast,
            class_asts,
            registry,
            class_names,
            union_overrides,
        ) {
            out.insert(iterable_name.clone(), info);
        }
    }
    out
}

fn try_detect_trivial_iterable<'a>(
    iterable_name: &str,
    iterable_ast: &'a Class<'a>,
    class_asts: &HashMap<String, &'a Class<'a>>,
    registry: &ClassRegistry,
    class_names: &HashSet<String>,
    union_overrides: &HashMap<String, WasmType>,
) -> Option<TrivialIterableInfo> {
    // Step 1: directly-declared `[Symbol.iterator]()` whose body is exactly
    // `return new IterClass(this.<bufField>);`.
    let iter_method = find_direct_method(iterable_ast, ITERATOR_METHOD_NAME)?;
    let iter_body = iter_method.value.body.as_ref()?;
    if iter_body.statements.len() != 1 {
        return None;
    }
    let Statement::ReturnStatement(ret) = &iter_body.statements[0] else {
        return None;
    };
    let Expression::NewExpression(new_expr) = ret.argument.as_ref()? else {
        return None;
    };
    let iter_class_name = match &new_expr.callee {
        Expression::Identifier(id) => id.name.as_str().to_string(),
        _ => return None,
    };
    if new_expr.arguments.len() != 1 {
        return None;
    }
    let arg0 = new_expr.arguments[0].as_expression()?;
    let buffer_field = match_this_member(arg0)?;

    // Step 2: iterable's <bufField> is `Array<T>` and constructor-only-write.
    let iterable_layout = registry.get(iterable_name)?;
    let &(buffer_offset, _) = iterable_layout.field_map.get(&buffer_field)?;
    let elem_ty = field_array_element_type(
        iterable_ast,
        &buffer_field,
        class_names,
        union_overrides,
    )?;
    if !field_is_constructor_only(iterable_ast, &buffer_field) {
        return None;
    }

    // Step 3: iterator class — no return()/throw() (direct or inherited),
    // single-arg constructor that sets cursor=0 and buf=param, next() shape.
    let iter_ast = class_asts.get(&iter_class_name)?;
    if find_method_inherited(registry, &iter_class_name, "return").is_some()
        || find_method_inherited(registry, &iter_class_name, "throw").is_some()
    {
        return None;
    }
    let IterCtor {
        cursor_field,
        buf_field,
    } = analyze_iter_constructor(iter_ast)?;
    let iter_buf_elem_ty =
        field_array_element_type(iter_ast, &buf_field, class_names, union_overrides)?;
    if iter_buf_elem_ty != elem_ty {
        return None;
    }
    if !field_is_constructor_only(iter_ast, &buf_field) {
        return None;
    }
    if !next_matches_canonical_shape(iter_ast, &cursor_field, &buf_field) {
        return None;
    }

    // Step 4: thread element-class / string flags from the iterable's layout
    // so the loop binding gets full static-type info (member access on the
    // binding, string-local marker). Field metadata for `Array<T>` fields
    // lives in `field_array_elem_classes` / no string carrier today, since
    // the detector only matches when `<F>` is `Array<T>` (not a string and
    // not a direct class instance).
    let elem_class = iterable_layout
        .field_array_elem_classes
        .get(&buffer_field)
        .cloned();
    let elem_is_string = false;

    Some(TrivialIterableInfo {
        buffer_field,
        buffer_offset,
        elem_wasm_ty: elem_ty,
        elem_class,
        elem_is_string,
    })
}

/// Find a directly-declared method by name on `class_ast`. Mirrors the
/// canonical-name handling in `classes::property_key_name`.
fn find_direct_method<'a>(
    class_ast: &'a Class<'a>,
    method_name: &str,
) -> Option<&'a oxc_ast::ast::MethodDefinition<'a>> {
    for element in &class_ast.body.body {
        if let ClassElement::MethodDefinition(method) = element
            && method.kind != MethodDefinitionKind::Constructor
            && let Ok(key_name) = super::classes::property_key_name(&method.key)
            && key_name == method_name
        {
            return Some(method);
        }
    }
    None
}

/// Find this class's constructor, if any.
fn find_constructor<'a>(
    class_ast: &'a Class<'a>,
) -> Option<&'a oxc_ast::ast::MethodDefinition<'a>> {
    for element in &class_ast.body.body {
        if let ClassElement::MethodDefinition(method) = element
            && method.kind == MethodDefinitionKind::Constructor
        {
            return Some(method);
        }
    }
    None
}

/// Match `this.<field>` member access. Returns the field name on hit.
fn match_this_member(expr: &Expression<'_>) -> Option<String> {
    if let Expression::StaticMemberExpression(member) = expr
        && matches!(&member.object, Expression::ThisExpression(_))
    {
        return Some(member.property.name.as_str().to_string());
    }
    None
}

/// Match `this.<field>.length`. Returns the field name on hit.
fn match_this_member_length(expr: &Expression<'_>) -> Option<String> {
    if let Expression::StaticMemberExpression(outer) = expr
        && outer.property.name.as_str() == "length"
        && let Expression::StaticMemberExpression(inner) = &outer.object
        && matches!(&inner.object, Expression::ThisExpression(_))
    {
        return Some(inner.property.name.as_str().to_string());
    }
    None
}

/// Resolve the element `WasmType` of a class field declared as `Array<T>`.
/// Returns `None` when the field is missing, has no annotation, or is not an
/// array shape. Names are looked up directly on the class AST so the result
/// is independent of inheritance — the caller tests each side separately.
fn field_array_element_type<'a>(
    class_ast: &'a Class<'a>,
    field_name: &str,
    class_names: &HashSet<String>,
    union_overrides: &HashMap<String, WasmType>,
) -> Option<WasmType> {
    for element in &class_ast.body.body {
        if let ClassElement::PropertyDefinition(prop) = element
            && let Ok(name) = super::classes::property_key_name(&prop.key)
            && name == field_name
            && let Some(ann) = &prop.type_annotation
        {
            return types::get_array_element_type(ann, class_names, union_overrides);
        }
    }
    None
}

/// Walk every method body (constructor excluded) for any
/// `this.<field> = …` / `this.<field> += …` / `this.<field>++` write.
/// Returns `true` when no such write exists — i.e. the field is only
/// initialized in the constructor (or via PropertyDefinition initializer).
fn field_is_constructor_only<'a>(class_ast: &'a Class<'a>, field_name: &str) -> bool {
    for element in &class_ast.body.body {
        let ClassElement::MethodDefinition(method) = element else {
            continue;
        };
        if method.kind == MethodDefinitionKind::Constructor {
            continue;
        }
        let Some(body) = &method.value.body else {
            continue;
        };
        for stmt in &body.statements {
            if statement_writes_this_field(stmt, field_name) {
                return false;
            }
        }
    }
    true
}

/// Recursive walk: any `this.<field>` write inside a statement subtree.
fn statement_writes_this_field(stmt: &Statement<'_>, field_name: &str) -> bool {
    match stmt {
        Statement::BlockStatement(block) => block
            .body
            .iter()
            .any(|s| statement_writes_this_field(s, field_name)),
        Statement::IfStatement(if_stmt) => {
            statement_writes_this_field(&if_stmt.consequent, field_name)
                || if_stmt
                    .alternate
                    .as_ref()
                    .is_some_and(|a| statement_writes_this_field(a, field_name))
        }
        Statement::WhileStatement(w) => statement_writes_this_field(&w.body, field_name),
        Statement::DoWhileStatement(w) => statement_writes_this_field(&w.body, field_name),
        Statement::ForStatement(f) => statement_writes_this_field(&f.body, field_name),
        Statement::ForOfStatement(f) => statement_writes_this_field(&f.body, field_name),
        Statement::ForInStatement(f) => statement_writes_this_field(&f.body, field_name),
        Statement::ExpressionStatement(es) => expr_writes_this_field(&es.expression, field_name),
        Statement::ReturnStatement(rs) => rs
            .argument
            .as_ref()
            .is_some_and(|e| expr_writes_this_field(e, field_name)),
        Statement::SwitchStatement(s) => s.cases.iter().any(|c| {
            c.consequent
                .iter()
                .any(|st| statement_writes_this_field(st, field_name))
        }),
        Statement::TryStatement(t) => {
            t.block
                .body
                .iter()
                .any(|s| statement_writes_this_field(s, field_name))
                || t.handler.as_ref().is_some_and(|h| {
                    h.body
                        .body
                        .iter()
                        .any(|s| statement_writes_this_field(s, field_name))
                })
                || t.finalizer.as_ref().is_some_and(|f| {
                    f.body
                        .iter()
                        .any(|s| statement_writes_this_field(s, field_name))
                })
        }
        _ => false,
    }
}

fn expr_writes_this_field(expr: &Expression<'_>, field_name: &str) -> bool {
    match expr {
        Expression::AssignmentExpression(assign) => {
            if let AssignmentTarget::StaticMemberExpression(member) = &assign.left
                && matches!(&member.object, Expression::ThisExpression(_))
                && member.property.name.as_str() == field_name
            {
                return true;
            }
            expr_writes_this_field(&assign.right, field_name)
        }
        Expression::UpdateExpression(upd) => {
            if let SimpleAssignmentTarget::StaticMemberExpression(member) = &upd.argument
                && matches!(&member.object, Expression::ThisExpression(_))
                && member.property.name.as_str() == field_name
            {
                return true;
            }
            false
        }
        Expression::SequenceExpression(seq) => seq
            .expressions
            .iter()
            .any(|e| expr_writes_this_field(e, field_name)),
        Expression::ConditionalExpression(c) => {
            expr_writes_this_field(&c.test, field_name)
                || expr_writes_this_field(&c.consequent, field_name)
                || expr_writes_this_field(&c.alternate, field_name)
        }
        Expression::LogicalExpression(l) => {
            expr_writes_this_field(&l.left, field_name)
                || expr_writes_this_field(&l.right, field_name)
        }
        Expression::BinaryExpression(b) => {
            expr_writes_this_field(&b.left, field_name)
                || expr_writes_this_field(&b.right, field_name)
        }
        _ => false,
    }
}

/// Resolved cursor + buffer field names from the iterator class's
/// constructor. Both fields must be initialized exactly once.
struct IterCtor {
    cursor_field: String,
    buf_field: String,
}

/// Validate the iterator class's constructor and identify which field is
/// the cursor (numeric, init to 0) and which is the buffer (init from the
/// single param). Cursor may be initialized via PropertyDefinition value
/// instead of a constructor statement.
fn analyze_iter_constructor<'a>(iter_ast: &'a Class<'a>) -> Option<IterCtor> {
    let ctor = find_constructor(iter_ast)?;
    if ctor.value.params.items.len() != 1 {
        return None;
    }
    let param_name = match &ctor.value.params.items[0].pattern {
        BindingPattern::BindingIdentifier(id) => id.name.as_str().to_string(),
        _ => return None,
    };

    // Pick up any PropertyDefinition initializers up front: a field with
    // `cursor: i32 = 0` counts as cursor-set even with no constructor stmt.
    let mut cursor_init_via_prop: Option<String> = None;
    for element in &iter_ast.body.body {
        if let ClassElement::PropertyDefinition(prop) = element
            && let Some(value) = &prop.value
            && expr_is_zero_literal(value)
            && let Ok(name) = super::classes::property_key_name(&prop.key)
        {
            cursor_init_via_prop = Some(name);
            break;
        }
    }

    let body = ctor.value.body.as_ref()?;
    let mut cursor_field: Option<String> = cursor_init_via_prop;
    let mut buf_field: Option<String> = None;

    // Strict per-statement match. The trivial path skips constructing the
    // iterator entirely — any side effect the constructor would otherwise
    // perform (host calls, writes to other fields) would be silently
    // dropped on inline. Reject any constructor whose body is more than
    // the canonical cursor-init / buf-from-param pair so the optimization
    // remains observably equivalent to the protocol path.
    for stmt in &body.statements {
        let Statement::ExpressionStatement(es) = stmt else {
            return None;
        };
        let Expression::AssignmentExpression(assign) = &es.expression else {
            return None;
        };
        if assign.operator != AssignmentOperator::Assign {
            return None;
        }
        let AssignmentTarget::StaticMemberExpression(member) = &assign.left else {
            return None;
        };
        if !matches!(&member.object, Expression::ThisExpression(_)) {
            return None;
        }
        let target_field = member.property.name.as_str().to_string();

        if expr_is_zero_literal(&assign.right) {
            if cursor_field.is_some() && cursor_field.as_deref() != Some(target_field.as_str()) {
                return None;
            }
            cursor_field = Some(target_field);
            continue;
        }

        if let Expression::Identifier(id) = &assign.right
            && id.name.as_str() == param_name
        {
            if buf_field.is_some() {
                return None;
            }
            buf_field = Some(target_field);
            continue;
        }

        return None;
    }

    let cursor_field = cursor_field?;
    let buf_field = buf_field?;
    if cursor_field == buf_field {
        return None;
    }
    Some(IterCtor {
        cursor_field,
        buf_field,
    })
}

fn expr_is_zero_literal(expr: &Expression<'_>) -> bool {
    if let Expression::NumericLiteral(num) = expr {
        return num.value == 0.0;
    }
    false
}

fn expr_is_false_literal(expr: &Expression<'_>) -> bool {
    matches!(expr, Expression::BooleanLiteral(b) if !b.value)
}

fn expr_is_true_literal(expr: &Expression<'_>) -> bool {
    matches!(expr, Expression::BooleanLiteral(b) if b.value)
}

/// Match `next()` body against the canonical shape:
///
/// ```text
/// if (this.<C> >= this.<B>.length) {
///     return { value: <sentinel>, done: true };
/// }
/// const <v>: T = this.<B>[this.<C>];
/// this.<C> = this.<C> + 1;   // or this.<C>++ / this.<C> += 1
/// return { value: <v>, done: false };
/// ```
fn next_matches_canonical_shape<'a>(
    iter_ast: &'a Class<'a>,
    cursor_field: &str,
    buf_field: &str,
) -> bool {
    let Some(method) = find_direct_method(iter_ast, "next") else {
        return false;
    };
    if !method.value.params.items.is_empty() {
        return false;
    }
    let Some(body) = &method.value.body else {
        return false;
    };
    if body.statements.len() != 4 {
        return false;
    }

    // Statement 1: `if (this.<C> >= this.<B>.length) { return { ..., done: true }; }`,
    // with no else branch.
    let Statement::IfStatement(if_stmt) = &body.statements[0] else {
        return false;
    };
    if if_stmt.alternate.is_some() {
        return false;
    }
    let Expression::BinaryExpression(test) = &if_stmt.test else {
        return false;
    };
    use oxc_ast::ast::BinaryOperator;
    if test.operator != BinaryOperator::GreaterEqualThan {
        return false;
    }
    if match_this_member(&test.left).as_deref() != Some(cursor_field) {
        return false;
    }
    if match_this_member_length(&test.right).as_deref() != Some(buf_field) {
        return false;
    }
    let consequent = match &if_stmt.consequent {
        Statement::BlockStatement(b) if b.body.len() == 1 => &b.body[0],
        s => s,
    };
    let Statement::ReturnStatement(ret) = consequent else {
        return false;
    };
    let Some(ret_arg) = &ret.argument else {
        return false;
    };
    if !object_has_value_done(ret_arg, /*done=*/ true) {
        return false;
    }

    // Statement 2: `const <v>: T = this.<B>[this.<C>];`
    let Statement::VariableDeclaration(var_decl) = &body.statements[1] else {
        return false;
    };
    if var_decl.kind != VariableDeclarationKind::Const || var_decl.declarations.len() != 1 {
        return false;
    }
    let decl = &var_decl.declarations[0];
    let value_local_name = match &decl.id {
        BindingPattern::BindingIdentifier(id) => id.name.as_str().to_string(),
        _ => return false,
    };
    let Some(init) = &decl.init else {
        return false;
    };
    let Expression::ComputedMemberExpression(idx) = init else {
        return false;
    };
    if match_this_member(&idx.object).as_deref() != Some(buf_field) {
        return false;
    }
    if match_this_member(&idx.expression).as_deref() != Some(cursor_field) {
        return false;
    }

    // Statement 3: cursor advance — three accepted forms.
    if !is_cursor_advance(&body.statements[2], cursor_field) {
        return false;
    }

    // Statement 4: `return { value: <v>, done: false };`
    let Statement::ReturnStatement(ret2) = &body.statements[3] else {
        return false;
    };
    let Some(ret2_arg) = &ret2.argument else {
        return false;
    };
    if !object_has_value_done_with_value_ident(ret2_arg, &value_local_name, /*done=*/ false) {
        return false;
    }

    true
}

/// Check `this.<C> = this.<C> + 1` / `this.<C> += 1` / `this.<C>++` —
/// the only accepted cursor-advance forms.
fn is_cursor_advance(stmt: &Statement<'_>, cursor_field: &str) -> bool {
    let Statement::ExpressionStatement(es) = stmt else {
        return false;
    };
    match &es.expression {
        Expression::UpdateExpression(upd) => {
            upd.operator == UpdateOperator::Increment
                && match &upd.argument {
                    SimpleAssignmentTarget::StaticMemberExpression(member) => {
                        matches!(&member.object, Expression::ThisExpression(_))
                            && member.property.name.as_str() == cursor_field
                    }
                    _ => false,
                }
        }
        Expression::AssignmentExpression(assign) => {
            let AssignmentTarget::StaticMemberExpression(target) = &assign.left else {
                return false;
            };
            if !matches!(&target.object, Expression::ThisExpression(_))
                || target.property.name.as_str() != cursor_field
            {
                return false;
            }
            match assign.operator {
                AssignmentOperator::Addition => is_one_literal(&assign.right),
                AssignmentOperator::Assign => match &assign.right {
                    Expression::BinaryExpression(b)
                        if b.operator == oxc_ast::ast::BinaryOperator::Addition =>
                    {
                        match_this_member(&b.left).as_deref() == Some(cursor_field)
                            && is_one_literal(&b.right)
                    }
                    _ => false,
                },
                _ => false,
            }
        }
        _ => false,
    }
}

fn is_one_literal(expr: &Expression<'_>) -> bool {
    if let Expression::NumericLiteral(num) = expr {
        return num.value == 1.0;
    }
    false
}

/// `{ value: …, done: <bool> }` object literal — sentinel value side is
/// unrestricted (the for..of consumer never reads `value` when `done=true`,
/// so the iterator may return any well-typed sentinel).
fn object_has_value_done(expr: &Expression<'_>, done: bool) -> bool {
    let Expression::ObjectExpression(obj) = expr else {
        return false;
    };
    if obj.properties.len() != 2 {
        return false;
    }
    let mut has_value = false;
    let mut has_done = false;
    for prop in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(p) = prop else {
            return false;
        };
        if p.kind != PropertyKind::Init {
            return false;
        }
        let PropertyKey::StaticIdentifier(key) = &p.key else {
            return false;
        };
        match key.name.as_str() {
            "value" => has_value = true,
            "done" => {
                if done && !expr_is_true_literal(&p.value) {
                    return false;
                }
                if !done && !expr_is_false_literal(&p.value) {
                    return false;
                }
                has_done = true;
            }
            _ => return false,
        }
    }
    has_value && has_done
}

/// Like `object_has_value_done` but additionally requires `value` to be
/// the named local (the value bound from `this.<B>[this.<C>]`).
fn object_has_value_done_with_value_ident(
    expr: &Expression<'_>,
    value_ident: &str,
    done: bool,
) -> bool {
    let Expression::ObjectExpression(obj) = expr else {
        return false;
    };
    if obj.properties.len() != 2 {
        return false;
    }
    let mut value_ok = false;
    let mut done_ok = false;
    for prop in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(p) = prop else {
            return false;
        };
        if p.kind != PropertyKind::Init {
            return false;
        }
        let PropertyKey::StaticIdentifier(key) = &p.key else {
            return false;
        };
        match key.name.as_str() {
            "value" => {
                if let Expression::Identifier(id) = &p.value
                    && id.name.as_str() == value_ident
                {
                    value_ok = true;
                }
            }
            "done" => {
                if done && expr_is_true_literal(&p.value) {
                    done_ok = true;
                }
                if !done && expr_is_false_literal(&p.value) {
                    done_ok = true;
                }
            }
            _ => return false,
        }
    }
    value_ok && done_ok
}
