use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

use super::super::ARRAY_HEADER_SIZE;

impl<'a> FuncContext<'a> {
    /// Emit `arr.toReversed()` — allocate a fresh array of the same length and
    /// copy source elements in reverse order. Source is untouched. Mirrors the
    /// ES2023 semantics (`Array.prototype.toReversed`).
    pub(crate) fn emit_array_to_reversed(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
    ) -> Result<(), CompileError> {
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };
        let arena_idx = self
            .module_ctx
            .arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

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

        // Allocate new array (header + len*esize).
        let new_ptr = self.alloc_local(WasmType::I32);
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::LocalSet(new_ptr));
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Add);
        self.push(Instruction::GlobalSet(arena_idx));

        // Write header: length = capacity = len.
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // i = 0; while i < len: new[i] = src[len - 1 - i]; i++
        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        // dst = new_ptr + HEADER + i * esize
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);

        // load src[len - 1 - i]
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Sub);
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
        // store at dst
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg {
                offset: 0,
                align: 3,
                memory_index: 0,
            })),
            _ => self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            })),
        }

        // i++
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End);
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(new_ptr));
        Ok(())
    }

    /// Emit `arr.with(index, value)` — ES2023 immutable single-element update.
    /// Allocates a shallow clone, writes `value` at the (normalized) index, and
    /// returns the new array. Out-of-range indices trap, matching our
    /// bounds-check posture elsewhere (see `Array.at`) and the spec's
    /// `RangeError` in the absence of a real exception channel.
    pub(crate) fn emit_array_with(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };
        let arena_idx = self
            .module_ctx
            .arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

        // Evaluate args left-to-right: index, then value.
        let idx_local = self.alloc_local(WasmType::I32);
        let idx_ty = self.emit_expr(call.arguments[0].to_expression())?;
        if idx_ty == WasmType::F64 {
            self.push(Instruction::I32TruncF64S);
        }
        self.push(Instruction::LocalSet(idx_local));

        let val_local = self.alloc_local(elem_ty);
        let val_ty = self.emit_expr(call.arguments[1].to_expression())?;
        if val_ty != elem_ty {
            if elem_ty == WasmType::F64 && val_ty == WasmType::I32 {
                self.push(Instruction::F64ConvertI32S);
            } else {
                return Err(CompileError::type_err(format!(
                    "Array.with value has type {val_ty:?}, expected {elem_ty:?}"
                )));
            }
        }
        self.push(Instruction::LocalSet(val_local));

        // Evaluate and save array pointer, length.
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

        // Normalize negative index: if idx < 0: idx += len.
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32LtS);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(idx_local));
        self.push(Instruction::End);

        // Bounds check: if idx < 0 || idx >= len, trap.
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

        // Allocate new array (header + len*esize).
        let new_ptr = self.alloc_local(WasmType::I32);
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::LocalSet(new_ptr));
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Add);
        self.push(Instruction::GlobalSet(arena_idx));

        // Header: length = capacity = len.
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // memory.copy all elements: dst=new+HEADER, src=arr+HEADER, n=len*esize.
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        // new[idx] = value.
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(val_local));
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg {
                offset: 0,
                align: 3,
                memory_index: 0,
            })),
            _ => self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            })),
        }

        self.push(Instruction::LocalGet(new_ptr));
        Ok(())
    }

    /// Emit `arr.toSpliced(start, deleteCount?, ...items)` — ES2023 immutable
    /// splice. Allocates a fresh array of the resulting length (`len - delete
    /// + insert`), copies the prefix, writes inserted items, then copies the
    /// suffix. Source array is untouched; the return value is the NEW array
    /// (not the removed elements — that's `splice`'s contract, not this one).
    /// Shares argument defaulting/clamping semantics with `splice`.
    pub(crate) fn emit_array_to_spliced(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };

        let insert_count: i32 = call.arguments.len().saturating_sub(2) as i32;

        // Pre-evaluate insert items left-to-right into locals.
        let mut item_locals: Vec<u32> = Vec::with_capacity(insert_count as usize);
        for arg in call.arguments.iter().skip(2) {
            let expr = arg.to_expression();
            let ty = self.emit_expr(expr)?;
            if ty != elem_ty {
                if elem_ty == WasmType::F64 && ty == WasmType::I32 {
                    self.push(Instruction::F64ConvertI32S);
                } else {
                    return Err(CompileError::type_err(format!(
                        "Array.toSpliced insert item has type {ty:?}, expected {elem_ty:?}"
                    )));
                }
            }
            let local = self.alloc_local(elem_ty);
            self.push(Instruction::LocalSet(local));
            item_locals.push(local);
        }

        // Array pointer, length.
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

        // start: truncate f64→i32, normalize negative (+len), clamp to [0, len].
        let start_local = self.alloc_local(WasmType::I32);
        let ty = self.emit_expr(call.arguments[0].to_expression())?;
        if ty == WasmType::F64 {
            self.push(Instruction::I32TruncF64S);
        }
        self.push(Instruction::LocalSet(start_local));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32LtS);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(start_local));
        self.push(Instruction::End);
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32LtS);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(start_local));
        self.push(Instruction::End);
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GtS);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::LocalSet(start_local));
        self.push(Instruction::End);

        // deleteCount: default len-start when omitted; else clamp to [0, len-start].
        let delete_local = self.alloc_local(WasmType::I32);
        if call.arguments.len() >= 2 {
            let ty = self.emit_expr(call.arguments[1].to_expression())?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
            self.push(Instruction::LocalSet(delete_local));
            self.push(Instruction::LocalGet(delete_local));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(wasm_encoder::BlockType::Empty));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::LocalSet(delete_local));
            self.push(Instruction::End);
            self.push(Instruction::LocalGet(delete_local));
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::LocalGet(start_local));
            self.push(Instruction::I32Sub);
            self.push(Instruction::I32GtS);
            self.push(Instruction::If(wasm_encoder::BlockType::Empty));
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::LocalGet(start_local));
            self.push(Instruction::I32Sub);
            self.push(Instruction::LocalSet(delete_local));
            self.push(Instruction::End);
        } else {
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::LocalGet(start_local));
            self.push(Instruction::I32Sub);
            self.push(Instruction::LocalSet(delete_local));
        }

        // new_len = len - delete + insert_count
        let new_len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::LocalGet(delete_local));
        self.push(Instruction::I32Sub);
        self.push(Instruction::I32Const(insert_count));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(new_len_local));

        // Allocate new array sized to new_len.
        let new_ptr = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(new_len_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        let alloc_tmp = self.emit_arena_alloc_to_local(true)?;
        self.push(Instruction::LocalGet(alloc_tmp));
        self.push(Instruction::LocalSet(new_ptr));

        // Header: length = capacity = new_len.
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(new_len_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(new_len_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // Copy prefix: memory.copy(new+HEADER, arr+HEADER, start*esize).
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        // Write insert items at new + HEADER + (start + i) * esize.
        for (i, &item_local) in item_locals.iter().enumerate() {
            self.push(Instruction::LocalGet(new_ptr));
            self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalGet(start_local));
            self.push(Instruction::I32Const(i as i32));
            self.push(Instruction::I32Add);
            self.push(Instruction::I32Const(esize));
            self.push(Instruction::I32Mul);
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalGet(item_local));
            match elem_ty {
                WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg {
                    offset: 0,
                    align: 3,
                    memory_index: 0,
                })),
                _ => self.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                })),
            }
        }

        // Copy suffix:
        //   dst = new + HEADER + (start + insert_count) * esize
        //   src = arr + HEADER + (start + delete) * esize
        //   n   = (len - start - delete) * esize
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Const(insert_count));
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::LocalGet(delete_local));
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalGet(delete_local));
        self.push(Instruction::I32Sub);
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        self.push(Instruction::LocalGet(new_ptr));
        Ok(())
    }
}
