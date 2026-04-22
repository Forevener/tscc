use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::classes::ClassLayout;
use crate::codegen::func::FuncContext;
use crate::codegen::shapes::ShapeField;
use crate::error::CompileError;
use crate::types::{BoundType, WasmType};

impl<'a> FuncContext<'a> {
    /// Emit an `ObjectExpression` literal as a synthetic-class instance store.
    ///
    /// `expected` is the caller-supplied class name hint (from a declarator
    /// annotation, a function parameter, a return type, or an assignment
    /// target). When present, it overrides any fingerprint-based inference.
    /// When absent, the literal's own field types fingerprint into the
    /// `ShapeRegistry` — a fingerprint miss is a hard error instructing the
    /// user to add an annotation (see Phase A.4 design doc, decision P1).
    ///
    /// Returns the resolved `(WasmType::I32, class_name)`. The class name
    /// lets callers populate `local_class_types` / track downstream usage
    /// without re-running shape resolution.
    pub(crate) fn emit_object_literal(
        &mut self,
        obj: &ObjectExpression<'a>,
        expected: Option<&str>,
    ) -> Result<(WasmType, String), CompileError> {
        reject_unsupported_properties(self, obj)?;

        let class_name = resolve_target_class(self, obj, expected)?;

        let layout = self
            .module_ctx
            .class_registry
            .get(&class_name)
            .ok_or_else(|| {
                CompileError::codegen(format!(
                    "synthetic class '{class_name}' not registered (object-literal target)"
                ))
            })?
            .clone();

        if has_spread(obj) {
            emit_with_spreads(self, obj, &layout)?;
            return Ok((WasmType::I32, class_name));
        }

        check_property_set(self, obj, &layout)?;

        if all_properties_pure(obj) {
            emit_inline(self, obj, &layout)?;
        } else {
            emit_with_temps(self, obj, &layout)?;
        }

        Ok((WasmType::I32, class_name))
    }
}

/// Phase A.6 excess- and missing-property checks. When `resolve_target_class`
/// picked the layout via fingerprint inference, the literal's key set matches
/// the layout's field set by construction and both checks are no-ops; the
/// non-trivial cases all involve an explicit `expected` hint where the literal
/// was bound to a narrower / wider named type than its keys describe.
fn check_property_set<'a>(
    ctx: &FuncContext<'a>,
    obj: &ObjectExpression<'a>,
    layout: &ClassLayout,
) -> Result<(), CompileError> {
    use std::collections::HashSet;

    let mut literal_keys: HashSet<String> = HashSet::with_capacity(obj.properties.len());
    for prop in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(p) = prop else {
            unreachable!("reject_unsupported_properties filtered spreads");
        };
        let key = extract_property_key(p)?;
        if !layout.field_map.contains_key(&key) {
            return Err(ctx.locate(
                CompileError::type_err(format!(
                    "object literal may only specify known properties, and '{key}' does not \
                     exist in type '{}'",
                    layout.name
                )),
                p.span.start,
            ));
        }
        literal_keys.insert(key);
    }

    let mut missing: Vec<&str> = layout
        .field_map
        .keys()
        .filter(|k| !literal_keys.contains(k.as_str()))
        .map(|k| k.as_str())
        .collect();
    if !missing.is_empty() {
        missing.sort_unstable();
        let list = missing
            .iter()
            .map(|k| format!("'{k}'"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ctx.locate(
            CompileError::type_err(format!(
                "object literal is missing the following properties from type '{}': {list}",
                layout.name
            )),
            obj.span.start,
        ));
    }

    Ok(())
}

fn reject_unsupported_properties<'a>(
    ctx: &FuncContext<'a>,
    obj: &ObjectExpression<'a>,
) -> Result<(), CompileError> {
    for prop in &obj.properties {
        match prop {
            ObjectPropertyKind::SpreadProperty(_) => {
                // Spreads are handled by `emit_with_spreads`.
            }
            ObjectPropertyKind::ObjectProperty(p) => {
                if p.method {
                    return Err(ctx.locate(
                        CompileError::unsupported(
                            "method shorthand in object literal — not yet supported (Phase E)",
                        ),
                        p.span.start,
                    ));
                }
                if p.computed {
                    return Err(ctx.locate(
                        CompileError::unsupported("computed property key in object literal"),
                        p.span.start,
                    ));
                }
                match &p.key {
                    PropertyKey::StaticIdentifier(_) | PropertyKey::StringLiteral(_) => {}
                    _ => {
                        return Err(ctx.locate(
                            CompileError::unsupported("computed property key in object literal"),
                            p.span.start,
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn resolve_target_class<'a>(
    ctx: &FuncContext<'a>,
    obj: &ObjectExpression<'a>,
    expected: Option<&str>,
) -> Result<String, CompileError> {
    if let Some(name) = expected
        && ctx.module_ctx.class_registry.get(name).is_some()
    {
        return Ok(name.to_string());
    }
    if has_spread(obj) {
        return Err(ctx.locate(
            CompileError::type_err(
                "object literal with spread `...x` requires an explicit target type — \
                 add a type annotation on the receiving variable or cast",
            ),
            obj.span.start,
        ));
    }
    let fp = fingerprint_object_expression(ctx, obj)?;
    ctx.module_ctx
        .shape_registry
        .get_by_fingerprint(&fp)
        .map(|s| s.name.clone())
        .ok_or_else(|| {
            ctx.locate(
                CompileError::type_err(
                    "cannot infer shape of object literal — add a type annotation on the \
                     receiving variable or cast: `let p: { x: number } = { x: 1 }`",
                ),
                obj.span.start,
            )
        })
}

fn has_spread(obj: &ObjectExpression<'_>) -> bool {
    obj.properties
        .iter()
        .any(|p| matches!(p, ObjectPropertyKind::SpreadProperty(_)))
}

/// Fingerprint this literal by inferring each RHS's `BoundType` standalone.
/// Errors if any field resists standalone inference — this is the P1 case.
fn fingerprint_object_expression<'a>(
    ctx: &FuncContext<'a>,
    obj: &ObjectExpression<'a>,
) -> Result<String, CompileError> {
    let mut fields: Vec<ShapeField> = Vec::with_capacity(obj.properties.len());
    for prop in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(p) = prop else {
            return Err(ctx.locate(
                CompileError::type_err("cannot infer shape of object literal with spread"),
                obj.span.start,
            ));
        };
        let key = extract_property_key(p)?;
        let ty = literal_field_bound_type(ctx, &p.value).ok_or_else(|| {
            ctx.locate(
                CompileError::type_err(format!(
                    "cannot infer shape of object literal — field '{key}' needs an explicit \
                     type; add an annotation on the receiving variable: \
                     `let v: {{ ... }} = {{ ... }}`"
                )),
                p.span.start,
            )
        })?;
        fields.push(ShapeField { name: key, ty });
    }
    Ok(fingerprint_of(&fields))
}

/// Canonical fingerprint identical to `shapes.rs::fingerprint_of`. Duplicated
/// here (not extracted) because A.4's caller surface is narrow; extraction
/// waits for D.3 tuples when the same helper is needed again.
fn fingerprint_of(fields: &[ShapeField]) -> String {
    let mut pairs: Vec<(&str, String)> = fields
        .iter()
        .map(|f| (f.name.as_str(), f.ty.mangle_token()))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(b.0));
    pairs
        .into_iter()
        .map(|(n, t)| format!("{n}_{t}"))
        .collect::<Vec<_>>()
        .join("$")
}

/// Narrow, standalone expression typer for fingerprinting. Mirrors
/// `shapes.rs::ShapeWalker::infer_expr_bound_type` but runs at emit time so
/// it can consult `local_class_types` / `local_string_vars`. Returns `None`
/// when the RHS cannot be typed without more context — the caller surfaces
/// that as the "add an annotation" error.
fn literal_field_bound_type<'a>(
    ctx: &FuncContext<'a>,
    expr: &Expression<'a>,
) -> Option<BoundType> {
    match expr {
        Expression::StringLiteral(_) | Expression::TemplateLiteral(_) => Some(BoundType::Str),
        Expression::BooleanLiteral(_) => Some(BoundType::Bool),
        Expression::NumericLiteral(lit) => {
            let is_float = lit.raw.as_ref().is_some_and(|r| r.contains('.'))
                || lit.value.fract() != 0.0
                || !((i32::MIN as f64)..=(i32::MAX as f64)).contains(&lit.value);
            Some(if is_float {
                BoundType::F64
            } else {
                BoundType::I32
            })
        }
        Expression::NullLiteral(_) => None,
        Expression::ParenthesizedExpression(p) => literal_field_bound_type(ctx, &p.expression),
        Expression::UnaryExpression(u) if matches!(u.operator, UnaryOperator::UnaryNegation) => {
            literal_field_bound_type(ctx, &u.argument)
        }
        Expression::TSAsExpression(a) => {
            // Trust the cast when we can resolve the target.
            if let Some(class_name) = crate::types::get_class_type_name_from_ts_type(
                &a.type_annotation,
                Some(&ctx.module_ctx.shape_registry),
            ) && ctx.module_ctx.class_names.contains(&class_name)
            {
                return Some(BoundType::Class(class_name));
            }
            let ty = crate::types::resolve_ts_type(&a.type_annotation, &ctx.module_ctx.class_names)
                .ok()?;
            Some(match ty {
                WasmType::F64 => BoundType::F64,
                WasmType::I32 => BoundType::I32,
                WasmType::Void => return None,
            })
        }
        Expression::Identifier(ident) => {
            let name = ident.name.as_str();
            if let Some(cn) = ctx.local_class_types.get(name) {
                return Some(BoundType::Class(cn.clone()));
            }
            if ctx.local_string_vars.contains(name) {
                return Some(BoundType::Str);
            }
            if let Some(&(_, ty)) = ctx.locals.get(name) {
                return Some(match ty {
                    WasmType::F64 => BoundType::F64,
                    WasmType::I32 => BoundType::I32,
                    WasmType::Void => return None,
                });
            }
            if let Some(cn) = ctx.module_ctx.var_class_types.get(name) {
                return Some(BoundType::Class(cn.clone()));
            }
            if let Some(&(_, ty)) = ctx.module_ctx.globals.get(name) {
                return Some(match ty {
                    WasmType::F64 => BoundType::F64,
                    WasmType::I32 => BoundType::I32,
                    WasmType::Void => return None,
                });
            }
            None
        }
        Expression::NewExpression(n) => {
            let Expression::Identifier(id) = &n.callee else {
                return None;
            };
            let base = id.name.as_str();
            if let Some(type_args) = n.type_arguments.as_ref() {
                let mut tokens = Vec::with_capacity(type_args.params.len());
                for p in &type_args.params {
                    let bt = crate::codegen::generics::resolve_bound_type(
                        p,
                        &ctx.module_ctx.class_names,
                        ctx.type_bindings.as_ref(),
                    )
                    .ok()?;
                    tokens.push(bt.mangle_token());
                }
                let mangled = format!("{base}${}", tokens.join("$"));
                if ctx.module_ctx.class_names.contains(&mangled) {
                    return Some(BoundType::Class(mangled));
                }
            }
            if ctx.module_ctx.class_names.contains(base) {
                return Some(BoundType::Class(base.to_string()));
            }
            None
        }
        Expression::ObjectExpression(inner) => {
            // Nested literal: recurse through the same inference. A.1 will
            // have pre-registered the shape whenever its fields are
            // standalone-inferable — which is exactly the case this function
            // returns `Some` for.
            let mut inner_fields: Vec<ShapeField> = Vec::with_capacity(inner.properties.len());
            for prop in &inner.properties {
                let ObjectPropertyKind::ObjectProperty(p) = prop else {
                    return None;
                };
                if p.method || p.computed {
                    return None;
                }
                let key = match &p.key {
                    PropertyKey::StaticIdentifier(id) => id.name.as_str().to_string(),
                    PropertyKey::StringLiteral(s) => s.value.as_str().to_string(),
                    _ => return None,
                };
                let ty = literal_field_bound_type(ctx, &p.value)?;
                inner_fields.push(ShapeField { name: key, ty });
            }
            let fp = fingerprint_of(&inner_fields);
            ctx.module_ctx
                .shape_registry
                .get_by_fingerprint(&fp)
                .map(|s| BoundType::Class(s.name.clone()))
        }
        _ => None,
    }
}

fn all_properties_pure(obj: &ObjectExpression) -> bool {
    obj.properties.iter().all(|p| match p {
        ObjectPropertyKind::ObjectProperty(prop) => is_pure_rhs(&prop.value),
        _ => false,
    })
}

fn is_pure_rhs(expr: &Expression) -> bool {
    match expr {
        Expression::NumericLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::Identifier(_)
        | Expression::ThisExpression(_) => true,
        Expression::ParenthesizedExpression(p) => is_pure_rhs(&p.expression),
        Expression::UnaryExpression(u) => is_pure_rhs(&u.argument),
        Expression::TSAsExpression(a) => is_pure_rhs(&a.expression),
        Expression::StaticMemberExpression(m) => is_pure_rhs(&m.object),
        _ => false,
    }
}

fn extract_property_key(p: &ObjectProperty) -> Result<String, CompileError> {
    match &p.key {
        PropertyKey::StaticIdentifier(id) => Ok(id.name.as_str().to_string()),
        PropertyKey::StringLiteral(s) => Ok(s.value.as_str().to_string()),
        _ => Err(CompileError::unsupported(
            "computed property key in object literal",
        )),
    }
}

fn emit_field_store(ctx: &mut FuncContext<'_>, offset: u32, ty: WasmType) {
    match ty {
        WasmType::F64 => ctx.push(Instruction::F64Store(wasm_encoder::MemArg {
            offset: offset as u64,
            align: 3,
            memory_index: 0,
        })),
        WasmType::I32 => ctx.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: offset as u64,
            align: 2,
            memory_index: 0,
        })),
        _ => {}
    }
}

fn widen_or_check(
    rhs_ty: WasmType,
    field_ty: WasmType,
    key: &str,
    ctx: &mut FuncContext<'_>,
) -> Result<(), CompileError> {
    if rhs_ty == field_ty {
        return Ok(());
    }
    if field_ty == WasmType::F64 && rhs_ty == WasmType::I32 {
        ctx.push(Instruction::F64ConvertI32S);
        return Ok(());
    }
    Err(CompileError::type_err(format!(
        "object literal field '{key}' has type {rhs_ty:?}, expected {field_ty:?}"
    )))
}

/// Lookup field offset+type by name; error clearly if the name is not on the
/// layout (the A.4 "unknown field" case; A.6 will later rephrase as a TS-style
/// excess-property diagnostic).
fn field_slot(
    layout: &ClassLayout,
    key: &str,
) -> Result<(u32, WasmType), CompileError> {
    layout.field_map.get(key).copied().ok_or_else(|| {
        CompileError::codegen(format!(
            "object literal has field '{key}' which does not exist on type '{}'",
            layout.name
        ))
    })
}

fn emit_inline<'a>(
    ctx: &mut FuncContext<'a>,
    obj: &ObjectExpression<'a>,
    layout: &ClassLayout,
) -> Result<(), CompileError> {
    ctx.push(Instruction::I32Const(layout.size as i32));
    let ptr_local = ctx.emit_arena_alloc_to_local(true)?;

    for prop in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(p) = prop else {
            unreachable!("reject_unsupported_properties filtered spreads");
        };
        let key = extract_property_key(p)?;
        let (offset, field_ty) = field_slot(layout, &key)?;
        let expected_class = layout.field_class_types.get(&key).cloned();
        ctx.push(Instruction::LocalGet(ptr_local));
        let rhs_ty = emit_field_rhs(ctx, &p.value, expected_class.as_deref())?;
        widen_or_check(rhs_ty, field_ty, &key, ctx)?;
        emit_field_store(ctx, offset, field_ty);
    }

    ctx.push(Instruction::LocalGet(ptr_local));
    Ok(())
}

fn emit_with_temps<'a>(
    ctx: &mut FuncContext<'a>,
    obj: &ObjectExpression<'a>,
    layout: &ClassLayout,
) -> Result<(), CompileError> {
    let mut evaluated: Vec<(String, u32, WasmType)> = Vec::with_capacity(obj.properties.len());
    for prop in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(p) = prop else {
            unreachable!("reject_unsupported_properties filtered spreads");
        };
        let key = extract_property_key(p)?;
        let (_, field_ty) = field_slot(layout, &key)?;
        let expected_class = layout.field_class_types.get(&key).cloned();
        let rhs_ty = emit_field_rhs(ctx, &p.value, expected_class.as_deref())?;
        widen_or_check(rhs_ty, field_ty, &key, ctx)?;
        let tmp = ctx.alloc_local(field_ty);
        ctx.push(Instruction::LocalSet(tmp));
        evaluated.push((key, tmp, field_ty));
    }

    ctx.push(Instruction::I32Const(layout.size as i32));
    let ptr_local = ctx.emit_arena_alloc_to_local(true)?;

    for (key, tmp, field_ty) in &evaluated {
        let (offset, _) = field_slot(layout, key)?;
        ctx.push(Instruction::LocalGet(ptr_local));
        ctx.push(Instruction::LocalGet(*tmp));
        emit_field_store(ctx, offset, *field_ty);
    }

    ctx.push(Instruction::LocalGet(ptr_local));
    Ok(())
}

/// Emit the RHS of a single field, threading the field's declared class into
/// any nested `ObjectExpression` so shape-typed subfields compose naturally.
fn emit_field_rhs<'a>(
    ctx: &mut FuncContext<'a>,
    value: &Expression<'a>,
    expected_class: Option<&str>,
) -> Result<WasmType, CompileError> {
    match value {
        Expression::ObjectExpression(inner) => {
            let (ty, _) = ctx.emit_object_literal(inner, expected_class)?;
            Ok(ty)
        }
        Expression::ParenthesizedExpression(p) => {
            emit_field_rhs(ctx, &p.expression, expected_class)
        }
        _ => ctx.emit_expr(value),
    }
}

/// Field source recorded during the source-order pre-pass. `Explicit` means a
/// literal property `key: value`; `Spread` means the field was taken from a
/// spread's evaluated source. Later entries in source order overwrite earlier
/// ones for the same field name, matching TS object-spread semantics.
enum FieldSource {
    Explicit {
        tmp_local: u32,
        field_ty: WasmType,
    },
    Spread {
        /// Local holding the evaluated pointer to the spread source.
        src_ptr_local: u32,
        src_offset: u32,
        src_field_ty: WasmType,
        target_field_ty: WasmType,
    },
}

/// Spread-aware emission path. Evaluates every side-effect-bearing expression
/// in source order (spread source pointers first, then RHS values), resolves
/// the final owner of each target field with later-wins semantics, and finally
/// allocates the target buffer and stores each field. Source fields outside
/// the target layout are silently dropped (TS spreads don't emit excess-property
/// errors); any target field left without a source triggers a missing-property
/// diagnostic.
fn emit_with_spreads<'a>(
    ctx: &mut FuncContext<'a>,
    obj: &ObjectExpression<'a>,
    layout: &ClassLayout,
) -> Result<(), CompileError> {
    let mut sources: std::collections::HashMap<String, FieldSource> =
        std::collections::HashMap::with_capacity(layout.fields.len());

    for prop in &obj.properties {
        match prop {
            ObjectPropertyKind::SpreadProperty(s) => {
                let src_class = ctx.resolve_expr_class(&s.argument).map_err(|_| {
                    ctx.locate(
                        CompileError::type_err(
                            "cannot resolve source type of object spread `...x` — \
                             only shape-typed expressions can be spread",
                        ),
                        s.span.start,
                    )
                })?;
                let src_layout = ctx
                    .module_ctx
                    .class_registry
                    .get(&src_class)
                    .ok_or_else(|| {
                        ctx.locate(
                            CompileError::codegen(format!(
                                "spread source class '{src_class}' not registered"
                            )),
                            s.span.start,
                        )
                    })?
                    .clone();

                ctx.emit_expr(&s.argument)?;
                let src_ptr_local = ctx.alloc_local(WasmType::I32);
                ctx.push(Instruction::LocalSet(src_ptr_local));

                for (fname, src_offset, src_field_ty) in &src_layout.fields {
                    if let Some(&(_, target_field_ty)) = layout.field_map.get(fname) {
                        if *src_field_ty != target_field_ty
                            && !(target_field_ty == WasmType::F64
                                && *src_field_ty == WasmType::I32)
                        {
                            return Err(ctx.locate(
                                CompileError::type_err(format!(
                                    "spread source field '{fname}' has type {:?}, but \
                                     target type '{}' expects {:?}",
                                    src_field_ty, layout.name, target_field_ty
                                )),
                                s.span.start,
                            ));
                        }
                        if let Some(expected_class) = layout.field_class_types.get(fname)
                            && let Some(src_field_class) = src_layout.field_class_types.get(fname)
                            && expected_class != src_field_class
                        {
                            return Err(ctx.locate(
                                CompileError::type_err(format!(
                                    "spread source field '{fname}' has class '{src_field_class}', \
                                     but target field expects class '{expected_class}'"
                                )),
                                s.span.start,
                            ));
                        }
                        sources.insert(
                            fname.clone(),
                            FieldSource::Spread {
                                src_ptr_local,
                                src_offset: *src_offset,
                                src_field_ty: *src_field_ty,
                                target_field_ty,
                            },
                        );
                    }
                }
            }
            ObjectPropertyKind::ObjectProperty(p) => {
                let key = extract_property_key(p)?;
                if !layout.field_map.contains_key(&key) {
                    return Err(ctx.locate(
                        CompileError::type_err(format!(
                            "object literal may only specify known properties, and '{key}' does not \
                             exist in type '{}'",
                            layout.name
                        )),
                        p.span.start,
                    ));
                }
                let (_, field_ty) = field_slot(layout, &key)?;
                let expected_class = layout.field_class_types.get(&key).cloned();
                let rhs_ty = emit_field_rhs(ctx, &p.value, expected_class.as_deref())?;
                widen_or_check(rhs_ty, field_ty, &key, ctx)?;
                let tmp_local = ctx.alloc_local(field_ty);
                ctx.push(Instruction::LocalSet(tmp_local));
                sources.insert(
                    key,
                    FieldSource::Explicit {
                        tmp_local,
                        field_ty,
                    },
                );
            }
        }
    }

    let mut missing: Vec<&str> = layout
        .field_map
        .keys()
        .filter(|k| !sources.contains_key(k.as_str()))
        .map(|k| k.as_str())
        .collect();
    if !missing.is_empty() {
        missing.sort_unstable();
        let list = missing
            .iter()
            .map(|k| format!("'{k}'"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ctx.locate(
            CompileError::type_err(format!(
                "object literal (with spread) is missing the following properties from type '{}': {list}",
                layout.name
            )),
            obj.span.start,
        ));
    }

    ctx.push(Instruction::I32Const(layout.size as i32));
    let ptr_local = ctx.emit_arena_alloc_to_local(true)?;

    for (fname, target_offset, _) in &layout.fields {
        let src = sources.get(fname).expect("coverage check verified above");
        match src {
            FieldSource::Explicit { tmp_local, field_ty } => {
                ctx.push(Instruction::LocalGet(ptr_local));
                ctx.push(Instruction::LocalGet(*tmp_local));
                emit_field_store(ctx, *target_offset, *field_ty);
            }
            FieldSource::Spread {
                src_ptr_local,
                src_offset,
                src_field_ty,
                target_field_ty,
            } => {
                ctx.push(Instruction::LocalGet(ptr_local));
                ctx.push(Instruction::LocalGet(*src_ptr_local));
                emit_field_load(ctx, *src_offset, *src_field_ty);
                if src_field_ty != target_field_ty
                    && *target_field_ty == WasmType::F64
                    && *src_field_ty == WasmType::I32
                {
                    ctx.push(Instruction::F64ConvertI32S);
                }
                emit_field_store(ctx, *target_offset, *target_field_ty);
            }
        }
    }

    ctx.push(Instruction::LocalGet(ptr_local));
    Ok(())
}

fn emit_field_load(ctx: &mut FuncContext<'_>, offset: u32, ty: WasmType) {
    match ty {
        WasmType::F64 => ctx.push(Instruction::F64Load(wasm_encoder::MemArg {
            offset: offset as u64,
            align: 3,
            memory_index: 0,
        })),
        WasmType::I32 => ctx.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: offset as u64,
            align: 2,
            memory_index: 0,
        })),
        _ => {}
    }
}
