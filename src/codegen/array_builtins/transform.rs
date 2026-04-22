use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

use super::{
    build_elem_index_bindings, elem_size, emit_alloc_array, emit_arr_length, emit_elem_addr,
    emit_elem_load, emit_inline_push, eval_arrow_body, extract_arrow, extract_arrow_params,
    restore_arrow_scope, setup_arrow_scope,
};

impl<'a> FuncContext<'a> {
    /// arr.filter(e => predicate) — returns a new array with elements where predicate is truthy.
    pub(super) fn emit_array_filter(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: &Expression<'a>,
    ) -> Result<WasmType, CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_arrow_params(arrow)?;
        if params.is_empty() || params.len() > 2 {
            return Err(CompileError::codegen(
                "filter callback must have 1-2 parameters",
            ));
        }
        let esize = elem_size(elem_ty)?;

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len = self.alloc_local(WasmType::I32);
        emit_arr_length(self, src_local);
        self.push(Instruction::LocalSet(src_len));

        let result_local = emit_alloc_array(self, src_len, elem_ty)?;

        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        let elem_local = self.alloc_local(elem_ty);

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
                "filter predicate must return i32/bool",
            ));
        }

        // Restore scope
        restore_arrow_scope(self, scope);

        // If truthy, push element to result
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::LocalGet(elem_local));
        emit_inline_push(self, result_local, elem_ty)?;
        self.push(Instruction::End);

        // i++
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End); // end loop
        self.push(Instruction::End); // end block

        // Return result array pointer
        self.push(Instruction::LocalGet(result_local));
        Ok(WasmType::I32)
    }

    /// arr.map(e => expr) — returns a new array with transformed elements.
    pub(crate) fn emit_array_map(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: &Expression<'a>,
    ) -> Result<WasmType, CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_arrow_params(arrow)?;
        if params.is_empty() || params.len() > 2 {
            return Err(CompileError::codegen(
                "map callback must have 1-2 parameters",
            ));
        }
        let esize = elem_size(elem_ty)?;

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len = self.alloc_local(WasmType::I32);
        emit_arr_length(self, src_local);
        self.push(Instruction::LocalSet(src_len));

        // We need to figure out the result element type by evaluating the arrow
        // on a dummy element. For now, we'll allocate with the same elem type
        // and determine the actual type during body evaluation.
        // Actually, we allocate the result after knowing the type. Let's do a
        // two-pass or just use a reasonable approach: evaluate the arrow body
        // once and track the type. Since we're inlining, we'll do it in the loop.

        // Temp local for element value
        let elem_local = self.alloc_local(elem_ty);

        // We need to determine the result element type first.
        // For the common case, we can infer it from the arrow's return type annotation
        // or from the first evaluation. Let's use a practical approach: if the arrow
        // param type is a class and we're accessing a field, we know the result type.
        // For now, allocate the result array assuming same elem_size as source.
        // We'll fix up if needed once we know the result type from first eval.

        // Actually, the cleanest approach: always allocate with i32 element type initially,
        // then use f64 if the mapped result is f64. Since we're inlining, we know
        // the result type at the point we push. Let's determine it upfront by
        // checking the arrow body type.

        // Pre-allocate result with max possible element size (f64=8).
        // The actual push will use the correct type.
        let result_elem_ty = self.infer_arrow_result_type(arrow, &params, elem_ty, elem_class)?;
        let result_esize = elem_size(result_elem_ty)?;
        let _ = result_esize; // used indirectly via emit_inline_push

        let result_local = emit_alloc_array(self, src_len, result_elem_ty)?;

        // Loop
        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        // if i >= src_len, break
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(src_len));
        self.push(Instruction::I32GeU);
        self.push(Instruction::BrIf(1));

        // Load element: src[i]
        emit_elem_addr(self, src_local, i_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(elem_local));

        let (param_locals, param_classes) =
            build_elem_index_bindings(&params, elem_local, elem_ty, i_local, elem_class);
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);

        // Evaluate arrow body — result value is on the stack
        let _result_ty = eval_arrow_body(self, arrow)?;

        // Restore scope
        restore_arrow_scope(self, scope);

        // Push result to output array
        emit_inline_push(self, result_local, result_elem_ty)?;

        // i++
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End); // end loop
        self.push(Instruction::End); // end block

        // Return result array pointer
        self.push(Instruction::LocalGet(result_local));
        Ok(WasmType::I32)
    }

    /// arr.forEach(e => { ... }) — execute callback for each element, no result.
    pub(super) fn emit_array_foreach(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_arrow_params(arrow)?;
        if params.is_empty() || params.len() > 2 {
            return Err(CompileError::codegen(
                "forEach callback must have 1-2 parameters",
            ));
        }
        let esize = elem_size(elem_ty)?;

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len = self.alloc_local(WasmType::I32);
        emit_arr_length(self, src_local);
        self.push(Instruction::LocalSet(src_len));

        let elem_local = self.alloc_local(elem_ty);
        let i_local = self.alloc_local(WasmType::I32);
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

        let body_ty = eval_arrow_body(self, arrow)?;
        if body_ty != WasmType::Void {
            self.push(Instruction::Drop);
        }

        // Restore scope
        restore_arrow_scope(self, scope);

        // i++
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End); // end loop
        self.push(Instruction::End); // end block

        Ok(())
    }

    /// arr.reduce((acc, e) => expr, initialValue) — fold array to a single value.
    pub(super) fn emit_array_reduce(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: &Expression<'a>,
        init_expr: &Expression<'a>,
        reverse: bool,
    ) -> Result<WasmType, CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_arrow_params(arrow)?;
        if params.len() != 2 {
            let name = if reverse { "reduceRight" } else { "reduce" };
            return Err(CompileError::codegen(format!(
                "{name} callback must have exactly 2 parameters (acc, elem)"
            )));
        }
        let esize = elem_size(elem_ty)?;

        // Evaluate initial value
        let acc_ty = self.emit_expr(init_expr)?;
        let acc_local = self.alloc_local(acc_ty);
        self.push(Instruction::LocalSet(acc_local));

        // Evaluate source array
        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len = self.alloc_local(WasmType::I32);
        emit_arr_length(self, src_local);
        self.push(Instruction::LocalSet(src_len));

        let elem_local = self.alloc_local(elem_ty);
        let i_local = self.alloc_local(WasmType::I32);
        // i = reverse ? len - 1 : 0
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

        // Loop bound: forward i >= len, reverse i < 0. Use signed compare in
        // reverse so that len==0 (i starts at -1) exits immediately.
        self.push(Instruction::LocalGet(i_local));
        if reverse {
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
        } else {
            self.push(Instruction::LocalGet(src_len));
            self.push(Instruction::I32GeS);
        }
        self.push(Instruction::BrIf(1));

        // Load element
        emit_elem_addr(self, src_local, i_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(elem_local));

        // Set up arrow scope: bind (acc, elem)
        let scope = setup_arrow_scope(
            self,
            &params,
            &[(acc_local, acc_ty), (elem_local, elem_ty)],
            &[None, elem_class.map(|s| s.to_string())],
        );

        // Evaluate arrow body — result is the new accumulator
        let body_ty = eval_arrow_body(self, arrow)?;
        if body_ty != acc_ty {
            let name = if reverse { "reduceRight" } else { "reduce" };
            return Err(CompileError::type_err(format!(
                "{name} callback returns {body_ty:?} but accumulator is {acc_ty:?}"
            )));
        }

        // Restore scope
        restore_arrow_scope(self, scope);

        // Update accumulator
        self.push(Instruction::LocalSet(acc_local));

        // i += ±1
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(if reverse { -1 } else { 1 }));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End); // end loop
        self.push(Instruction::End); // end block

        // Return accumulator
        self.push(Instruction::LocalGet(acc_local));
        Ok(acc_ty)
    }
}
