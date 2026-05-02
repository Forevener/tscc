//! Typed-array higher-order method emitters — sub-phase 4 of the plan.
//!
//! Covers `forEach`, `map`, `filter`, `reduce` / `reduceRight`, `find` /
//! `findIndex` / `findLast` / `findLastIndex`, `some` / `every`. Each
//! emitter builds the same shape:
//!
//!   1. Resolve the arrow up front and collect its parameter names so the
//!      arrow scope can be set up.
//!   2. Evaluate the receiver into the standard `Receiver` triple
//!      (`arr_local`, `len_local`, `buf_ptr_local`).
//!   3. Walk i = 0..len (or len-1..0 for reverse) loading via the
//!      descriptor's `load_inst`, run the arrow body inline, then act on
//!      the result.
//!
//! `map` and `filter` return a fresh self-owned typed array of the same
//! kind as the receiver (the spec rule). `map`'s body must produce
//! `desc.elem_wasm_ty` (i32→f64 promotion is allowed, same as construction).
//! `filter` allocates worst-case, fixes the header `len` after the loop.
//! `reduce`'s accumulator type is whatever the user-provided initial value
//! evaluates to — no constraint from the descriptor.

use oxc_ast::ast::*;
use wasm_encoder::{BlockType, Instruction};

use crate::codegen::array_builtins::{
    eval_arrow_body, extract_arrow, restore_arrow_scope, setup_arrow_scope,
};
use crate::codegen::func::FuncContext;
use crate::codegen::typed_arrays::{TYPED_ARRAY_HEADER_SIZE, TypedArrayDescriptor};
use crate::error::CompileError;
use crate::types::WasmType;

use super::typed_array_methods::{Receiver, len_memarg};

/// Pull simple-identifier params from an arrow function and validate the
/// arity against the HOF's expectations. Centralised so each HOF doesn't
/// repeat the same destructuring/error shape.
fn extract_simple_arrow_params<'a>(
    arrow: &ArrowFunctionExpression<'a>,
    method: &str,
    desc_name: &str,
    min_arity: usize,
    max_arity: usize,
) -> Result<Vec<String>, CompileError> {
    let mut names = Vec::with_capacity(arrow.params.items.len());
    for p in &arrow.params.items {
        match &p.pattern {
            BindingPattern::BindingIdentifier(id) => names.push(id.name.as_str().to_string()),
            _ => {
                return Err(CompileError::unsupported(format!(
                    "{desc_name}.{method}: callback parameters must be simple identifiers",
                )));
            }
        }
    }
    if names.len() < min_arity || names.len() > max_arity {
        return Err(CompileError::codegen(format!(
            "{desc_name}.{method} callback must take {min_arity}..={max_arity} parameters, got {}",
            names.len()
        )));
    }
    Ok(names)
}

impl<'a> FuncContext<'a> {
    /// `ta.forEach(fn)` — visitor; result of `fn(elem, idx)` is dropped.
    pub(super) fn emit_typed_array_for_each(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        callback: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_simple_arrow_params(arrow, "forEach", desc.name, 1, 2)?;

        let recv = self.emit_typed_array_receiver(ta_expr)?;
        let i_local = self.alloc_local(WasmType::I32);
        let elem_local = self.alloc_local(desc.elem_wasm_ty);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        self.emit_typed_array_elem_addr(recv.buf_ptr_local, i_local, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(Instruction::LocalSet(elem_local));

        let (param_locals, param_classes) =
            elem_index_bindings(&params, elem_local, desc.elem_wasm_ty, i_local);
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);
        let body_ty = eval_arrow_body(self, arrow)?;
        restore_arrow_scope(self, scope);
        if body_ty != WasmType::Void {
            self.push(Instruction::Drop);
        }

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);
        Ok(())
    }

    /// `ta.some(pred)` / `ta.every(pred)` — short-circuit linear scan.
    /// `some` returns 1 on first truthy predicate, 0 if none match.
    /// `every` returns 0 on first falsy predicate, 1 if all match.
    pub(super) fn emit_typed_array_some_every(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        callback: &Expression<'a>,
        all: bool,
    ) -> Result<(), CompileError> {
        let arrow = extract_arrow(callback)?;
        let method = if all { "every" } else { "some" };
        let params = extract_simple_arrow_params(arrow, method, desc.name, 1, 2)?;

        let recv = self.emit_typed_array_receiver(ta_expr)?;
        let result_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(if all { 1 } else { 0 }));
        self.push(Instruction::LocalSet(result_local));

        let i_local = self.alloc_local(WasmType::I32);
        let elem_local = self.alloc_local(desc.elem_wasm_ty);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        self.emit_typed_array_elem_addr(recv.buf_ptr_local, i_local, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(Instruction::LocalSet(elem_local));

        let (param_locals, param_classes) =
            elem_index_bindings(&params, elem_local, desc.elem_wasm_ty, i_local);
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);
        let pred_ty = eval_arrow_body(self, arrow)?;
        restore_arrow_scope(self, scope);
        if pred_ty != WasmType::I32 {
            return Err(CompileError::type_err(format!(
                "{}.{method} predicate must return i32/bool",
                desc.name
            )));
        }

        if all {
            // !pred: result = 0; break out of the outer block.
            self.push(Instruction::I32Eqz);
            self.push(Instruction::If(BlockType::Empty));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::LocalSet(result_local));
            self.push(Instruction::Br(2));
            self.push(Instruction::End);
        } else {
            self.push(Instruction::If(BlockType::Empty));
            self.push(Instruction::I32Const(1));
            self.push(Instruction::LocalSet(result_local));
            self.push(Instruction::Br(2));
            self.push(Instruction::End);
        }

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(result_local));
        Ok(())
    }

    /// `ta.find(pred)` / `ta.findIndex(pred)` / `ta.findLast(pred)` /
    /// `ta.findLastIndex(pred)` — short-circuit linear scan, optionally
    /// reversed. Returns either the matched element or its index. When no
    /// match is found, `find*Index` returns -1 (matches JS) and `find` /
    /// `findLast` return the descriptor's zero element (no `undefined` in
    /// our typed subset; scripts that need not-found discrimination should
    /// use the index-returning variants).
    pub(super) fn emit_typed_array_find(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        callback: &Expression<'a>,
        reverse: bool,
        return_index: bool,
    ) -> Result<WasmType, CompileError> {
        let arrow = extract_arrow(callback)?;
        let method = match (reverse, return_index) {
            (false, false) => "find",
            (false, true) => "findIndex",
            (true, false) => "findLast",
            (true, true) => "findLastIndex",
        };
        let params = extract_simple_arrow_params(arrow, method, desc.name, 1, 2)?;

        let recv = self.emit_typed_array_receiver(ta_expr)?;
        let i_local = self.alloc_local(WasmType::I32);
        let elem_local = self.alloc_local(desc.elem_wasm_ty);
        let found_idx = self.alloc_local(WasmType::I32);
        let found_val = self.alloc_local(desc.elem_wasm_ty);
        self.push(Instruction::I32Const(-1));
        self.push(Instruction::LocalSet(found_idx));
        match desc.elem_wasm_ty {
            WasmType::F64 => {
                self.push(Instruction::F64Const(0.0));
                self.push(Instruction::LocalSet(found_val));
            }
            _ => {
                self.push(Instruction::I32Const(0));
                self.push(Instruction::LocalSet(found_val));
            }
        }

        if reverse {
            self.push(Instruction::LocalGet(recv.len_local));
            self.push(Instruction::I32Const(1));
            self.push(Instruction::I32Sub);
        } else {
            self.push(Instruction::I32Const(0));
        }
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        if reverse {
            // Signed compare so a length-0 input (i = -1) exits cleanly.
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
        } else {
            self.push(Instruction::LocalGet(recv.len_local));
            self.push(Instruction::I32GeS);
        }
        self.push(Instruction::BrIf(1));

        self.emit_typed_array_elem_addr(recv.buf_ptr_local, i_local, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(Instruction::LocalSet(elem_local));

        let (param_locals, param_classes) =
            elem_index_bindings(&params, elem_local, desc.elem_wasm_ty, i_local);
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);
        let pred_ty = eval_arrow_body(self, arrow)?;
        restore_arrow_scope(self, scope);
        if pred_ty != WasmType::I32 {
            return Err(CompileError::type_err(format!(
                "{}.{method} predicate must return i32/bool",
                desc.name
            )));
        }

        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalSet(found_idx));
        self.push(Instruction::LocalGet(elem_local));
        self.push(Instruction::LocalSet(found_val));
        self.push(Instruction::Br(2));
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(if reverse { -1 } else { 1 }));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);

        if return_index {
            self.push(Instruction::LocalGet(found_idx));
            Ok(WasmType::I32)
        } else {
            self.push(Instruction::LocalGet(found_val));
            Ok(desc.elem_wasm_ty)
        }
    }

    /// `ta.reduce(fn, init)` / `ta.reduceRight(fn, init)` — fold to a single
    /// value. Accumulator type is whatever the initial-value expression
    /// evaluates to; the body must return that same type each iteration
    /// (the existing arrow-scope sandwich enforces it through the type
    /// check below).
    pub(super) fn emit_typed_array_reduce(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        callback: &Expression<'a>,
        init_expr: &Expression<'a>,
        reverse: bool,
    ) -> Result<WasmType, CompileError> {
        let arrow = extract_arrow(callback)?;
        let method = if reverse { "reduceRight" } else { "reduce" };
        let params = extract_simple_arrow_params(arrow, method, desc.name, 2, 3)?;
        if params.len() < 2 {
            return Err(CompileError::codegen(format!(
                "{}.{method} callback must take at least (acc, elem)",
                desc.name
            )));
        }

        // Initial accumulator. Evaluate before the receiver so any
        // side-effects in `init_expr` happen exactly once and in source
        // order — matches JS's argument evaluation order.
        let acc_ty = self.emit_expr(init_expr)?;
        let acc_local = self.alloc_local(acc_ty);
        self.push(Instruction::LocalSet(acc_local));

        let recv = self.emit_typed_array_receiver(ta_expr)?;

        let i_local = self.alloc_local(WasmType::I32);
        let elem_local = self.alloc_local(desc.elem_wasm_ty);
        if reverse {
            self.push(Instruction::LocalGet(recv.len_local));
            self.push(Instruction::I32Const(1));
            self.push(Instruction::I32Sub);
        } else {
            self.push(Instruction::I32Const(0));
        }
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        if reverse {
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
        } else {
            self.push(Instruction::LocalGet(recv.len_local));
            self.push(Instruction::I32GeS);
        }
        self.push(Instruction::BrIf(1));

        self.emit_typed_array_elem_addr(recv.buf_ptr_local, i_local, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(Instruction::LocalSet(elem_local));

        // Bind (acc, elem) and optional index. acc carries no class (it's
        // user-typed) and elem follows the descriptor's wasm type.
        let mut param_locals: Vec<(u32, WasmType)> =
            vec![(acc_local, acc_ty), (elem_local, desc.elem_wasm_ty)];
        let mut param_classes: Vec<Option<String>> = vec![None, None];
        if params.len() == 3 {
            param_locals.push((i_local, WasmType::I32));
            param_classes.push(None);
        }
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);
        let body_ty = eval_arrow_body(self, arrow)?;
        restore_arrow_scope(self, scope);
        if body_ty != acc_ty {
            return Err(CompileError::type_err(format!(
                "{}.{method} callback returns {body_ty:?} but accumulator is {acc_ty:?}",
                desc.name
            )));
        }
        self.push(Instruction::LocalSet(acc_local));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(if reverse { -1 } else { 1 }));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(acc_local));
        Ok(acc_ty)
    }

    /// `ta.map(fn)` — fresh self-owned typed array of the same kind. The
    /// body's wasm type must match `desc.elem_wasm_ty` (with i32→f64
    /// promotion allowed) — same constraint as the construction path's
    /// element coercion. Length is fixed equal to the receiver's, so we
    /// allocate up front and store each result into the fixed offset.
    pub(super) fn emit_typed_array_map(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        callback: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_simple_arrow_params(arrow, "map", desc.name, 1, 2)?;

        let recv = self.emit_typed_array_receiver(ta_expr)?;
        let dst_ptr = self.emit_alloc_self_owned_typed(desc, recv.len_local)?;

        let i_local = self.alloc_local(WasmType::I32);
        let elem_local = self.alloc_local(desc.elem_wasm_ty);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        self.emit_typed_array_elem_addr(recv.buf_ptr_local, i_local, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(Instruction::LocalSet(elem_local));

        // Pre-compute the destination element address. dst is self-owned so
        // body lives at dst + HEADER; descriptor's store baked the stride.
        self.push(Instruction::LocalGet(dst_ptr));
        self.push(Instruction::I32Const(TYPED_ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(i_local));
        if desc.byte_stride != 1 {
            self.push(Instruction::I32Const(desc.byte_stride as i32));
            self.push(Instruction::I32Mul);
        }
        self.push(Instruction::I32Add);

        let (param_locals, param_classes) =
            elem_index_bindings(&params, elem_local, desc.elem_wasm_ty, i_local);
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);
        let body_ty = eval_arrow_body(self, arrow)?;
        restore_arrow_scope(self, scope);
        self.coerce_to_typed_array_elem(desc, body_ty, "map")?;
        self.push(desc.store_inst(0));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(dst_ptr));
        Ok(())
    }

    /// `ta.filter(pred)` — fresh self-owned typed array of the same kind.
    /// Worst-case allocation = receiver length; we keep a running write
    /// counter and rewrite the header `len` after the loop. The body
    /// allocation stays sized for the worst case (no realloc) — wasted
    /// bytes are bounded by the receiver length and freed at the next arena
    /// reset, which is the same trade `Array.filter` makes.
    pub(super) fn emit_typed_array_filter(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        callback: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_simple_arrow_params(arrow, "filter", desc.name, 1, 2)?;

        let recv = self.emit_typed_array_receiver(ta_expr)?;
        let dst_ptr = self.emit_alloc_self_owned_typed(desc, recv.len_local)?;
        // Re-read dst body base into a local so the per-element address calc
        // doesn't re-load buf_ptr each iteration. dst is self-owned so the
        // body lives at dst + HEADER — compute it once.
        let dst_body_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(dst_ptr));
        self.push(Instruction::I32Const(TYPED_ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(dst_body_local));

        let write_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(write_local));

        let i_local = self.alloc_local(WasmType::I32);
        let elem_local = self.alloc_local(desc.elem_wasm_ty);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        self.emit_typed_array_elem_addr(recv.buf_ptr_local, i_local, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(Instruction::LocalSet(elem_local));

        let (param_locals, param_classes) =
            elem_index_bindings(&params, elem_local, desc.elem_wasm_ty, i_local);
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);
        let pred_ty = eval_arrow_body(self, arrow)?;
        restore_arrow_scope(self, scope);
        if pred_ty != WasmType::I32 {
            return Err(CompileError::type_err(format!(
                "{}.filter predicate must return i32/bool",
                desc.name
            )));
        }

        self.push(Instruction::If(BlockType::Empty));
        // dst[write] = elem.
        self.emit_typed_array_elem_addr(dst_body_local, write_local, desc.byte_stride);
        self.push(Instruction::LocalGet(elem_local));
        self.push(desc.store_inst(0));
        // write++.
        self.push(Instruction::LocalGet(write_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(write_local));
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);

        // Patch up the header `len` to the actual write count. buf_ptr stays
        // pointing at the same body — surplus tail bytes are unused but live
        // until the next arena reset.
        self.push(Instruction::LocalGet(dst_ptr));
        self.push(Instruction::LocalGet(write_local));
        self.push(Instruction::I32Store(len_memarg()));

        self.push(Instruction::LocalGet(dst_ptr));
        Ok(())
    }
}

/// Build the `(param_locals, param_class_types)` shape used by
/// `setup_arrow_scope` for callbacks taking `(elem)` or `(elem, idx)`.
/// Typed-array elements never carry a class type (no `Int32Array<MyClass>`
/// in the grammar), so the class slot is always `None`.
fn elem_index_bindings(
    params: &[String],
    elem_local: u32,
    elem_ty: WasmType,
    i_local: u32,
) -> (Vec<(u32, WasmType)>, Vec<Option<String>>) {
    let mut locals = vec![(elem_local, elem_ty)];
    let mut classes: Vec<Option<String>> = vec![None];
    if params.len() >= 2 {
        locals.push((i_local, WasmType::I32));
        classes.push(None);
    }
    (locals, classes)
}

// Ensure the `Receiver` field accesses we use compile against the visible
// shape exposed from `typed_array_methods`. This is a compile-time check
// that fails fast if the cross-module surface changes.
const _: fn(&Receiver) -> (u32, u32, u32) = |r| (r.arr_local, r.len_local, r.buf_ptr_local);
