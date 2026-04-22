use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

use super::{
    build_elem_index_bindings, elem_size, emit_arr_length, emit_elem_addr, emit_elem_load,
    eval_arrow_body, extract_arrow, extract_arrow_params, restore_arrow_scope, setup_arrow_scope,
};

impl<'a> FuncContext<'a> {
    /// `arr.some(pred)` / `arr.every(pred)` — short-circuit scan. `some`
    /// returns 1 on first truthy predicate, 0 otherwise. `every` returns 0
    /// on first falsy predicate, 1 otherwise.
    pub(super) fn emit_array_some_every(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: &Expression<'a>,
        all: bool,
    ) -> Result<WasmType, CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_arrow_params(arrow)?;
        if params.is_empty() || params.len() > 2 {
            return Err(CompileError::codegen(
                "some/every predicate must take 1-2 parameters",
            ));
        }
        let esize = elem_size(elem_ty)?;

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len = self.alloc_local(WasmType::I32);
        emit_arr_length(self, src_local);
        self.push(Instruction::LocalSet(src_len));

        let result_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(if all { 1 } else { 0 }));
        self.push(Instruction::LocalSet(result_local));

        let i_local = self.alloc_local(WasmType::I32);
        let elem_local = self.alloc_local(elem_ty);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(src_len));
        self.push(Instruction::I32GeU);
        self.push(Instruction::BrIf(1));

        emit_elem_addr(self, src_local, i_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(elem_local));

        let (param_locals, param_classes) =
            build_elem_index_bindings(&params, elem_local, elem_ty, i_local, elem_class);
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);
        let pred_ty = eval_arrow_body(self, arrow)?;
        if pred_ty != WasmType::I32 {
            return Err(CompileError::type_err(
                "some/every predicate must return i32/bool",
            ));
        }
        restore_arrow_scope(self, scope);

        if all {
            // If !pred: result = 0; break
            self.push(Instruction::I32Eqz);
            self.push(Instruction::If(wasm_encoder::BlockType::Empty));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::LocalSet(result_local));
            self.push(Instruction::Br(2));
            self.push(Instruction::End);
        } else {
            // If pred: result = 1; break
            self.push(Instruction::If(wasm_encoder::BlockType::Empty));
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
        Ok(WasmType::I32)
    }

    /// `arr.find(pred)` / `arr.findIndex(pred)` / `arr.findLast(pred)` /
    /// `arr.findLastIndex(pred)` — linear search returning the element or
    /// index of the first match (or last match, if `reverse`).
    ///
    /// When nothing matches:
    /// - `find*Index` returns -1 (matches JS).
    /// - `find` / `findLast` returns a default value (0 / 0.0) because our
    ///   typed world has no undefined for numeric or class pointer cells.
    ///   Scripts that need "not found" discrimination should use `findIndex`
    ///   or guard with `.some()`.
    pub(super) fn emit_array_find(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: &Expression<'a>,
        reverse: bool,
        return_index: bool,
    ) -> Result<WasmType, CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_arrow_params(arrow)?;
        if params.is_empty() || params.len() > 2 {
            return Err(CompileError::codegen(
                "find predicate must take 1-2 parameters",
            ));
        }
        let esize = elem_size(elem_ty)?;

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len = self.alloc_local(WasmType::I32);
        emit_arr_length(self, src_local);
        self.push(Instruction::LocalSet(src_len));

        let i_local = self.alloc_local(WasmType::I32);
        let elem_local = self.alloc_local(elem_ty);
        let found_idx = self.alloc_local(WasmType::I32);
        let found_val = self.alloc_local(elem_ty);
        self.push(Instruction::I32Const(-1));
        self.push(Instruction::LocalSet(found_idx));
        match elem_ty {
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
            self.push(Instruction::LocalGet(src_len));
            self.push(Instruction::I32Const(1));
            self.push(Instruction::I32Sub);
        } else {
            self.push(Instruction::I32Const(0));
        }
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        self.push(Instruction::LocalGet(i_local));
        if reverse {
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
        } else {
            self.push(Instruction::LocalGet(src_len));
            self.push(Instruction::I32GeS);
        }
        self.push(Instruction::BrIf(1));

        emit_elem_addr(self, src_local, i_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(elem_local));

        let (param_locals, param_classes) =
            build_elem_index_bindings(&params, elem_local, elem_ty, i_local, elem_class);
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);
        let pred_ty = eval_arrow_body(self, arrow)?;
        if pred_ty != WasmType::I32 {
            return Err(CompileError::type_err(
                "find predicate must return i32/bool",
            ));
        }
        restore_arrow_scope(self, scope);

        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
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
            Ok(elem_ty)
        }
    }
}
