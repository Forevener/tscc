//! Tuple-literal emit (Phase D.3 of `plan-object-literals-tuples.md`).
//!
//! A tuple literal is an `ArrayExpression` that appears in a tuple-typed
//! context. `expr/mod.rs::emit_expr_with_expected` detects the context, looks
//! up the tuple shape's registered synthetic class, and routes here instead
//! of falling through to `emit_array_literal`.
//!
//! Layout is a regular synthetic class with fields `_0, _1, _2, …`. Element
//! types and count must match the target tuple exactly — tuple identity is
//! positional, so the arity check is cheap and the type check is per-slot.
//!
//! Unlike arrays, tuple literals don't allow spread or holes — each slot is a
//! fixed position with a fixed type.

use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::classes::ClassLayout;
use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

impl<'a> FuncContext<'a> {
    /// Emit an `ArrayExpression` as a tuple-literal store into a freshly
    /// arena-allocated synthetic class instance. Returns `(WasmType::I32,
    /// class_name)` so callers can update `local_class_types` without
    /// re-resolving.
    ///
    /// Preconditions: `target_class` is the name of a registered tuple
    /// shape (checked by the caller via `ShapeRegistry::get_by_name`'s
    /// `is_tuple` flag). The literal must have the same arity as the tuple
    /// and each element's WASM type must widen into the slot's declared type
    /// (same i32→f64 rule as object literals and array elements).
    pub(crate) fn emit_tuple_literal(
        &mut self,
        arr: &ArrayExpression<'a>,
        target_class: &str,
    ) -> Result<(WasmType, String), CompileError> {
        // Reject holes + spread — a tuple literal has fixed positions and
        // fixed types. Array-style spreads don't make sense here.
        for el in &arr.elements {
            match el {
                ArrayExpressionElement::Elision(_) => {
                    return Err(self.locate(
                        CompileError::type_err("hole in tuple literal"),
                        arr.span.start,
                    ));
                }
                ArrayExpressionElement::SpreadElement(s) => {
                    return Err(self.locate(
                        CompileError::unsupported(
                            "spread in tuple literal — not yet supported",
                        ),
                        s.span.start,
                    ));
                }
                _ => {}
            }
        }

        let layout = self
            .module_ctx
            .class_registry
            .get(target_class)
            .ok_or_else(|| {
                CompileError::codegen(format!(
                    "tuple shape '{target_class}' not registered (tuple literal)"
                ))
            })?
            .clone();

        if arr.elements.len() != layout.fields.len() {
            return Err(self.locate(
                CompileError::type_err(format!(
                    "tuple literal has {} element(s), tuple type '{target_class}' expects {}",
                    arr.elements.len(),
                    layout.fields.len()
                )),
                arr.span.start,
            ));
        }

        if all_elements_pure(arr) {
            emit_tuple_inline(self, arr, &layout)?;
        } else {
            emit_tuple_with_temps(self, arr, &layout)?;
        }

        Ok((WasmType::I32, target_class.to_string()))
    }
}

fn all_elements_pure(arr: &ArrayExpression) -> bool {
    arr.elements.iter().all(|e| match e {
        ArrayExpressionElement::SpreadElement(_) | ArrayExpressionElement::Elision(_) => false,
        other => other.as_expression().is_some_and(is_pure_rhs),
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

fn emit_tuple_inline<'a>(
    ctx: &mut FuncContext<'a>,
    arr: &ArrayExpression<'a>,
    layout: &ClassLayout,
) -> Result<(), CompileError> {
    ctx.push(Instruction::I32Const(layout.size as i32));
    let ptr_local = ctx.emit_arena_alloc_to_local(true)?;

    for (i, el) in arr.elements.iter().enumerate() {
        let expr = el.as_expression().ok_or_else(|| {
            CompileError::codegen("unsupported tuple element kind")
        })?;
        let (slot_name, offset, slot_ty) = slot_info(layout, i);
        let expected_class = layout.field_class_types.get(slot_name).cloned();
        ctx.push(Instruction::LocalGet(ptr_local));
        let rhs_ty = emit_slot_rhs(ctx, expr, expected_class.as_deref())?;
        widen_or_check(rhs_ty, slot_ty, i, target_name(layout), ctx)?;
        emit_field_store(ctx, offset, slot_ty);
    }

    ctx.push(Instruction::LocalGet(ptr_local));
    Ok(())
}

fn emit_tuple_with_temps<'a>(
    ctx: &mut FuncContext<'a>,
    arr: &ArrayExpression<'a>,
    layout: &ClassLayout,
) -> Result<(), CompileError> {
    let mut evaluated: Vec<(u32, u32, WasmType)> = Vec::with_capacity(arr.elements.len());
    for (i, el) in arr.elements.iter().enumerate() {
        let expr = el.as_expression().ok_or_else(|| {
            CompileError::codegen("unsupported tuple element kind")
        })?;
        let (slot_name, offset, slot_ty) = slot_info(layout, i);
        let expected_class = layout.field_class_types.get(slot_name).cloned();
        let rhs_ty = emit_slot_rhs(ctx, expr, expected_class.as_deref())?;
        widen_or_check(rhs_ty, slot_ty, i, target_name(layout), ctx)?;
        let tmp = ctx.alloc_local(slot_ty);
        ctx.push(Instruction::LocalSet(tmp));
        evaluated.push((offset, tmp, slot_ty));
    }

    ctx.push(Instruction::I32Const(layout.size as i32));
    let ptr_local = ctx.emit_arena_alloc_to_local(true)?;

    for (offset, tmp, slot_ty) in evaluated {
        ctx.push(Instruction::LocalGet(ptr_local));
        ctx.push(Instruction::LocalGet(tmp));
        emit_field_store(ctx, offset, slot_ty);
    }

    ctx.push(Instruction::LocalGet(ptr_local));
    Ok(())
}

/// Emit a tuple slot's RHS, threading the declared class into nested literal
/// forms (object + tuple) so shape-typed positions compose naturally.
fn emit_slot_rhs<'a>(
    ctx: &mut FuncContext<'a>,
    value: &Expression<'a>,
    expected_class: Option<&str>,
) -> Result<WasmType, CompileError> {
    match value {
        Expression::ObjectExpression(inner) => {
            let (ty, _) = ctx.emit_object_literal(inner, expected_class)?;
            Ok(ty)
        }
        Expression::ArrayExpression(inner) => {
            // Nested tuple literal — only if the slot's declared class is
            // itself a tuple shape.
            if let Some(target) = expected_class
                && let Some(shape) =
                    ctx.module_ctx.shape_registry.by_name.get(target).copied()
                && ctx.module_ctx.shape_registry.shapes[shape].is_tuple
            {
                let (ty, _) = ctx.emit_tuple_literal(inner, target)?;
                return Ok(ty);
            }
            // No tuple hint — fall through to array literal.
            ctx.emit_array_literal(inner, None)
        }
        Expression::ParenthesizedExpression(p) => {
            emit_slot_rhs(ctx, &p.expression, expected_class)
        }
        _ => ctx.emit_expr(value),
    }
}

fn slot_info(layout: &ClassLayout, i: usize) -> (&str, u32, WasmType) {
    let (name, offset, ty) = &layout.fields[i];
    (name.as_str(), *offset, *ty)
}

fn target_name(layout: &ClassLayout) -> &str {
    layout.name.as_str()
}

fn widen_or_check(
    rhs_ty: WasmType,
    slot_ty: WasmType,
    index: usize,
    target: &str,
    ctx: &mut FuncContext<'_>,
) -> Result<(), CompileError> {
    if rhs_ty == slot_ty {
        return Ok(());
    }
    if slot_ty == WasmType::F64 && rhs_ty == WasmType::I32 {
        ctx.push(Instruction::F64ConvertI32S);
        return Ok(());
    }
    Err(CompileError::type_err(format!(
        "tuple literal element {index} has type {rhs_ty:?}, tuple type '{target}' expects {slot_ty:?}"
    )))
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
        WasmType::Void => {}
    }
}
