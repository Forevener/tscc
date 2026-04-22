use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

use super::super::ARRAY_HEADER_SIZE;

impl<'a> FuncContext<'a> {
    /// Emit `arr.indexOf(x)` or `arr.lastIndexOf(x)` — linear scan returning
    /// the first (or last, if `reverse`) matching index, or -1 when absent.
    /// Uses strict equality: f64 compares via F64Eq (so NaN ≠ NaN, matching JS).
    pub(crate) fn emit_array_index_of(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        needle_expr: &Expression<'a>,
        reverse: bool,
        from_idx: Option<&Expression<'a>>,
    ) -> Result<(), CompileError> {
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };

        // Evaluate needle once (and, if given, fromIndex after — JS arg order).
        let needle_local = self.alloc_local(elem_ty);
        self.emit_expr(needle_expr)?;
        self.push(Instruction::LocalSet(needle_local));

        let from_local = if let Some(from) = from_idx {
            let local = self.alloc_local(WasmType::I32);
            let ty = self.emit_expr(from)?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
            self.push(Instruction::LocalSet(local));
            Some(local)
        } else {
            None
        };

        // Evaluate array
        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(arr_local));

        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(len_local));

        let i_local = self.alloc_local(WasmType::I32);
        let result_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(-1));
        self.push(Instruction::LocalSet(result_local));

        // Starting index:
        //  no fromIndex → 0 (forward) / len-1 (reverse).
        //  with fromIndex → normalize negatives (+len), then clamp. Forward
        //  values >= len are handled by the loop bound (immediate exit);
        //  values < -len clamp to 0. Reverse values > len-1 clamp to len-1;
        //  values < -len stay negative so the i<0 guard exits immediately.
        if let Some(from) = from_local {
            self.push(Instruction::LocalGet(from));
            self.push(Instruction::LocalSet(i_local));
            self.push(Instruction::LocalGet(i_local));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(wasm_encoder::BlockType::Empty));
            self.push(Instruction::LocalGet(i_local));
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(i_local));
            self.push(Instruction::End);
            if reverse {
                // Clamp i > len-1 down to len-1.
                self.push(Instruction::LocalGet(i_local));
                self.push(Instruction::LocalGet(len_local));
                self.push(Instruction::I32GeS);
                self.push(Instruction::If(wasm_encoder::BlockType::Empty));
                self.push(Instruction::LocalGet(len_local));
                self.push(Instruction::I32Const(1));
                self.push(Instruction::I32Sub);
                self.push(Instruction::LocalSet(i_local));
                self.push(Instruction::End);
            } else {
                // Forward: clamp still-negative to 0.
                self.push(Instruction::LocalGet(i_local));
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32LtS);
                self.push(Instruction::If(wasm_encoder::BlockType::Empty));
                self.push(Instruction::I32Const(0));
                self.push(Instruction::LocalSet(i_local));
                self.push(Instruction::End);
            }
        } else if reverse {
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::I32Const(1));
            self.push(Instruction::I32Sub);
            self.push(Instruction::LocalSet(i_local));
        } else {
            self.push(Instruction::I32Const(0));
            self.push(Instruction::LocalSet(i_local));
        }

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        // Loop bound: forward: i >= len; reverse: i < 0
        self.push(Instruction::LocalGet(i_local));
        if reverse {
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
        } else {
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::I32GeS);
        }
        self.push(Instruction::BrIf(1));

        // Load arr[i]
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg {
                offset: 0,
                align: 3,
                memory_index: 0,
            })),
            _ => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            })),
        }
        // Compare
        self.push(Instruction::LocalGet(needle_local));
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Eq),
            _ => self.push(Instruction::I32Eq),
        }
        // if match: result = i, break
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalSet(result_local));
        self.push(Instruction::Br(2));
        self.push(Instruction::End);

        // i += ±1
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(if reverse { -1 } else { 1 }));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End);
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(result_local));
        Ok(())
    }

    /// Emit `arr.at(i)` — negative-index lookup. Traps on out-of-range to
    /// match our bounds-check posture; callers wanting "undefined on OOB" can
    /// guard with length first.
    pub(crate) fn emit_array_at(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        idx_expr: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };

        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(arr_local));

        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(len_local));

        let idx_local = self.alloc_local(WasmType::I32);
        let ty = self.emit_expr(idx_expr)?;
        if ty == WasmType::F64 {
            self.push(Instruction::I32TruncF64S);
        }
        self.push(Instruction::LocalSet(idx_local));

        // If idx < 0, idx += len
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32LtS);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(idx_local));
        self.push(Instruction::End);

        // Bounds check: if idx < 0 || idx >= len, trap
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32LtS);
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::I32Or);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Unreachable);
        self.push(Instruction::End);

        // Load arr[idx]
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg {
                offset: 0,
                align: 3,
                memory_index: 0,
            })),
            _ => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            })),
        }
        Ok(())
    }
}
