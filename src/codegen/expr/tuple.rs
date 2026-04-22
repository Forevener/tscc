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
use crate::codegen::coerce::emit_field_store;
use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

use super::{SlotRef, is_pure_rhs, widen_or_check};

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
        let rhs_ty = ctx.emit_expr_with_expected(expr, expected_class.as_deref())?;
        widen_or_check(rhs_ty, slot_ty, SlotRef::Tuple { index: i, target: target_name(layout) }, ctx)?;
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
        let rhs_ty = ctx.emit_expr_with_expected(expr, expected_class.as_deref())?;
        widen_or_check(rhs_ty, slot_ty, SlotRef::Tuple { index: i, target: target_name(layout) }, ctx)?;
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

fn slot_info(layout: &ClassLayout, i: usize) -> (&str, u32, WasmType) {
    let (name, offset, ty) = &layout.fields[i];
    (name.as_str(), *offset, *ty)
}

fn target_name(layout: &ClassLayout) -> &str {
    layout.name.as_str()
}
