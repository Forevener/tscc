use oxc_ast::ast::*;
use wasm_encoder::{Instruction, ValType};

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

use super::ARRAY_HEADER_SIZE;

/// Recognize the `{length: <expr>}` single-property object literal that
/// `Array.from` accepts as a sequence-generation input. Returns the length
/// expression if the shape matches exactly; any additional properties (or a
/// shorthand / spread / getter / computed key) disqualify. Kept narrow on
/// purpose: once general object literals land, other shapes will route
/// through the regular object-expression path and this pattern will keep
/// firing only for the sequence-generation idiom.
fn extract_length_only_object<'a, 'b>(expr: &'b Expression<'a>) -> Option<&'b Expression<'a>> {
    match expr {
        Expression::ParenthesizedExpression(p) => extract_length_only_object(&p.expression),
        Expression::ObjectExpression(obj) => {
            if obj.properties.len() != 1 {
                return None;
            }
            let prop = match &obj.properties[0] {
                ObjectPropertyKind::ObjectProperty(p) => p,
                _ => return None,
            };
            if prop.shorthand || prop.method || prop.computed {
                return None;
            }
            let key_ok = match &prop.key {
                PropertyKey::StaticIdentifier(id) => id.name.as_str() == "length",
                PropertyKey::StringLiteral(s) => s.value.as_str() == "length",
                _ => false,
            };
            if !key_ok {
                return None;
            }
            Some(&prop.value)
        }
        _ => None,
    }
}

impl<'a> FuncContext<'a> {
    // ---- Phase 4: Arrays ----

    /// Emit arr.length (load i32 at arr+0)
    pub(crate) fn emit_array_property(
        &mut self,
        member: &StaticMemberExpression<'a>,
        prop: &str,
    ) -> Result<WasmType, CompileError> {
        match prop {
            "length" => {
                self.emit_expr(&member.object)?;
                self.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                Ok(WasmType::I32)
            }
            _ => Err(CompileError::codegen(format!(
                "Array has no property '{prop}' — supported: length"
            ))),
        }
    }

    /// Try to emit array method calls: arr.push(val)
    pub(crate) fn try_emit_array_method_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        let member = match &call.callee {
            Expression::StaticMemberExpression(m) => m,
            _ => return Ok(None),
        };

        let method_name = member.property.name.as_str();

        // Check if the object is a known array variable
        let elem_ty = match self.resolve_expr_array_elem(&member.object) {
            Some(ty) => ty,
            None => return Ok(None),
        };

        match method_name {
            "push" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen(
                        "Array.push() expects exactly 1 argument",
                    ));
                }
                self.emit_array_push(&member.object, elem_ty, call.arguments[0].to_expression())?;
                Ok(Some(WasmType::Void))
            }
            "pop" => {
                self.expect_args(call, 0, "Array.pop")?;
                self.emit_array_pop(&member.object, elem_ty)?;
                Ok(Some(elem_ty))
            }
            "indexOf" => {
                if !matches!(call.arguments.len(), 1 | 2) {
                    return Err(CompileError::codegen(
                        "Array.indexOf expects 1 or 2 arguments",
                    ));
                }
                let from = call.arguments.get(1).map(|a| a.to_expression());
                self.emit_array_index_of(
                    &member.object,
                    elem_ty,
                    call.arguments[0].to_expression(),
                    false,
                    from,
                )?;
                Ok(Some(WasmType::I32))
            }
            "lastIndexOf" => {
                if !matches!(call.arguments.len(), 1 | 2) {
                    return Err(CompileError::codegen(
                        "Array.lastIndexOf expects 1 or 2 arguments",
                    ));
                }
                let from = call.arguments.get(1).map(|a| a.to_expression());
                self.emit_array_index_of(
                    &member.object,
                    elem_ty,
                    call.arguments[0].to_expression(),
                    true,
                    from,
                )?;
                Ok(Some(WasmType::I32))
            }
            "includes" => {
                if !matches!(call.arguments.len(), 1 | 2) {
                    return Err(CompileError::codegen(
                        "Array.includes expects 1 or 2 arguments",
                    ));
                }
                let from = call.arguments.get(1).map(|a| a.to_expression());
                self.emit_array_index_of(
                    &member.object,
                    elem_ty,
                    call.arguments[0].to_expression(),
                    false,
                    from,
                )?;
                // Convert index to bool: (idx >= 0)
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32GeS);
                Ok(Some(WasmType::I32))
            }
            "reverse" => {
                self.expect_args(call, 0, "Array.reverse")?;
                self.emit_array_reverse(&member.object, elem_ty)?;
                Ok(Some(WasmType::I32))
            }
            "toReversed" => {
                self.expect_args(call, 0, "Array.toReversed")?;
                self.emit_array_to_reversed(&member.object, elem_ty)?;
                Ok(Some(WasmType::I32))
            }
            "toSpliced" => {
                if call.arguments.is_empty() {
                    return Err(CompileError::codegen(
                        "Array.toSpliced expects at least 1 argument (start)",
                    ));
                }
                self.emit_array_to_spliced(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "with" => {
                self.expect_args(call, 2, "Array.with")?;
                self.emit_array_with(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "at" => {
                self.expect_args(call, 1, "Array.at")?;
                self.emit_array_at(&member.object, elem_ty, call.arguments[0].to_expression())?;
                Ok(Some(elem_ty))
            }
            "fill" => {
                if !matches!(call.arguments.len(), 1..=3) {
                    return Err(CompileError::codegen("Array.fill expects 1-3 arguments"));
                }
                self.emit_array_fill(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "slice" => {
                if call.arguments.len() > 2 {
                    return Err(CompileError::codegen("Array.slice expects 0-2 arguments"));
                }
                self.emit_array_slice(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "concat" => {
                if call.arguments.is_empty() {
                    return Err(CompileError::codegen(
                        "Array.concat expects at least 1 argument",
                    ));
                }
                self.emit_array_concat(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "join" => {
                if !matches!(call.arguments.len(), 0 | 1) {
                    return Err(CompileError::codegen("Array.join expects 0 or 1 arguments"));
                }
                self.emit_array_join(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "splice" => {
                if call.arguments.is_empty() {
                    return Err(CompileError::codegen(
                        "Array.splice expects at least 1 argument (start)",
                    ));
                }
                self.emit_array_splice(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "shift" => {
                self.expect_args(call, 0, "Array.shift")?;
                self.emit_array_shift(&member.object, elem_ty)?;
                Ok(Some(elem_ty))
            }
            "unshift" => {
                self.emit_array_unshift(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "copyWithin" => {
                if !matches!(call.arguments.len(), 2 | 3) {
                    return Err(CompileError::codegen(
                        "Array.copyWithin expects 2 or 3 arguments (target, start, end?)",
                    ));
                }
                self.emit_array_copy_within(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            _ => Err(CompileError::codegen(format!(
                "Array has no method '{method_name}' — supported: push, pop, shift, unshift, indexOf, lastIndexOf, includes, reverse, toReversed, at, with, fill, slice, concat, join, splice, toSpliced, copyWithin, filter, map, forEach, reduce, reduceRight, sort, toSorted, find, findIndex, findLast, findLastIndex, some, every"
            ))),
        }
    }

    /// Emit arr.push(val) — store at end, increment length. Grows array via arena reallocation if at capacity.
    pub(crate) fn emit_array_push(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        val_expr: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let elem_size: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };

        let arena_idx = self
            .module_ctx
            .arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

        // Evaluate array pointer
        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(arr_local));

        // Load current length
        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(len_local));

        // Load current capacity
        let cap_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(cap_local));

        // If length >= capacity, grow the array
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::LocalGet(cap_local));
        self.push(Instruction::I32GeU);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        {
            // new_cap = if cap == 0 { 1 } else { cap * 2 }
            let new_cap_local = self.alloc_local(WasmType::I32);
            self.push(Instruction::LocalGet(cap_local));
            self.push(Instruction::I32Eqz);
            self.push(Instruction::If(wasm_encoder::BlockType::Result(
                ValType::I32,
            )));
            self.push(Instruction::I32Const(1));
            self.push(Instruction::Else);
            self.push(Instruction::LocalGet(cap_local));
            self.push(Instruction::I32Const(2));
            self.push(Instruction::I32Mul);
            self.push(Instruction::End);
            self.push(Instruction::LocalSet(new_cap_local));

            // Check if array is at the top of the arena (in-place grow possible).
            // arr_end = arr_ptr + 8 + cap * elem_size
            let arr_end_local = self.alloc_local(WasmType::I32);
            self.push(Instruction::LocalGet(arr_local));
            self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalGet(cap_local));
            self.push(Instruction::I32Const(elem_size));
            self.push(Instruction::I32Mul);
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(arr_end_local));

            self.push(Instruction::LocalGet(arr_end_local));
            self.push(Instruction::GlobalGet(arena_idx));
            self.push(Instruction::I32Eq);
            self.push(Instruction::If(wasm_encoder::BlockType::Empty));
            {
                // In-place grow: just bump arena_ptr by (new_cap - old_cap) * elem_size
                // extra = (new_cap - cap) * elem_size
                self.push(Instruction::GlobalGet(arena_idx));
                self.push(Instruction::LocalGet(new_cap_local));
                self.push(Instruction::LocalGet(cap_local));
                self.push(Instruction::I32Sub);
                self.push(Instruction::I32Const(elem_size));
                self.push(Instruction::I32Mul);
                self.push(Instruction::I32Add);
                self.push(Instruction::GlobalSet(arena_idx));

                // Update capacity in place: arr[4] = new_cap
                self.push(Instruction::LocalGet(arr_local));
                self.push(Instruction::LocalGet(new_cap_local));
                self.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset: 4,
                    align: 2,
                    memory_index: 0,
                }));
            }
            self.push(Instruction::Else);
            {
                // Copy-and-abandon: allocate new array, copy elements

                // new_size = 8 + new_cap * elem_size
                let new_size_local = self.alloc_local(WasmType::I32);
                self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
                self.push(Instruction::LocalGet(new_cap_local));
                self.push(Instruction::I32Const(elem_size));
                self.push(Instruction::I32Mul);
                self.push(Instruction::I32Add);
                self.push(Instruction::LocalSet(new_size_local));

                // new_ptr = __arena_ptr
                let new_ptr_local = self.alloc_local(WasmType::I32);
                self.push(Instruction::GlobalGet(arena_idx));
                self.push(Instruction::LocalSet(new_ptr_local));

                // __arena_ptr += new_size
                self.push(Instruction::GlobalGet(arena_idx));
                self.push(Instruction::LocalGet(new_size_local));
                self.push(Instruction::I32Add);
                self.push(Instruction::GlobalSet(arena_idx));

                // Copy old elements: memory.copy(new_ptr + 8, arr_local + 8, len * elem_size)
                self.push(Instruction::LocalGet(new_ptr_local));
                self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
                self.push(Instruction::I32Add);
                self.push(Instruction::LocalGet(arr_local));
                self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
                self.push(Instruction::I32Add);
                self.push(Instruction::LocalGet(len_local));
                self.push(Instruction::I32Const(elem_size));
                self.push(Instruction::I32Mul);
                self.push(Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });

                // Write header: new_ptr[0] = length
                self.push(Instruction::LocalGet(new_ptr_local));
                self.push(Instruction::LocalGet(len_local));
                self.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));

                // Write header: new_ptr[4] = new_cap
                self.push(Instruction::LocalGet(new_ptr_local));
                self.push(Instruction::LocalGet(new_cap_local));
                self.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset: 4,
                    align: 2,
                    memory_index: 0,
                }));

                // Update arr_local to point to new array
                self.push(Instruction::LocalGet(new_ptr_local));
                self.push(Instruction::LocalSet(arr_local));

                // Write back to the original variable if it's a simple identifier
                if let Expression::Identifier(ident) = arr_expr {
                    let name = ident.name.as_str();
                    if let Some(&(idx, _ty)) = self.locals.get(name) {
                        self.push(Instruction::LocalGet(new_ptr_local));
                        self.push(Instruction::LocalSet(idx));
                    }
                }
            }
            self.push(Instruction::End); // end in-place vs copy-and-abandon
        }
        self.push(Instruction::End); // end length >= capacity check

        // Compute element address: arr + 8 + length * elem_size
        let addr_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(elem_size));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(addr_local));

        // Store value. Thread the array's element class through to the arg
        // so that `arr.push([a, b])` with `arr: Array<[T, U]>` routes the
        // tuple literal through `emit_tuple_literal`, and `arr.push({x, y})`
        // with `arr: Array<Shape>` routes through `emit_object_literal`.
        // Without this, a bare `emit_expr` on the ArrayExpression would emit
        // `[a, b]` as a plain `Array<T>` (len/cap header + elems), which is
        // the wrong memory layout for a tuple slot.
        let elem_class = self.resolve_expr_array_elem_class(arr_expr);
        self.push(Instruction::LocalGet(addr_local));
        self.emit_expr_with_expected(val_expr, elem_class.as_deref())?;
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg {
                offset: 0,
                align: 3,
                memory_index: 0,
            })),
            WasmType::I32 => self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            })),
            _ => unreachable!(),
        }

        // Increment length: arr.length = length + 1
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));

        Ok(())
    }

    /// Emit `arr.pop()` — returns the last element and shrinks length by one.
    /// On an empty array we return a default value (0 / 0.0) to mirror the
    /// JS contract of "undefined on empty" without introducing a tagged type.
    pub(crate) fn emit_array_pop(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
    ) -> Result<(), CompileError> {
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };

        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(arr_local));

        // len = arr.length
        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(len_local));

        // if len == 0 -> return default
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Eqz);
        let bt = match elem_ty {
            WasmType::F64 => wasm_encoder::BlockType::Result(ValType::F64),
            _ => wasm_encoder::BlockType::Result(ValType::I32),
        };
        self.push(Instruction::If(bt));
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Const(0.0)),
            _ => self.push(Instruction::I32Const(0)),
        }
        self.push(Instruction::Else);

        // new_len = len - 1
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));

        // addr = arr + HEADER + (len-1) * esize
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        // load
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

        self.push(Instruction::End);
        Ok(())
    }

    /// Emit `arr.shift()` — returns arr[0] and shifts the tail down by one
    /// via a single `memory.copy` (the WASM spec handles overlap). Empty
    /// array returns the zero default, mirroring `pop`.
    pub(crate) fn emit_array_shift(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
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

        // if len == 0 → return default
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Eqz);
        let bt = match elem_ty {
            WasmType::F64 => wasm_encoder::BlockType::Result(ValType::F64),
            _ => wasm_encoder::BlockType::Result(ValType::I32),
        };
        self.push(Instruction::If(bt));
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Const(0.0)),
            _ => self.push(Instruction::I32Const(0)),
        }
        self.push(Instruction::Else);

        // result = arr[0]
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
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
        let result_local = self.alloc_local(elem_ty);
        self.push(Instruction::LocalSet(result_local));

        // memory.copy(dst=arr+HEADER, src=arr+HEADER+esize, n=(len-1)*esize)
        // Overlap is handled by the WASM spec.
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32 + esize));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        // arr.length = len - 1
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));

        self.push(Instruction::LocalGet(result_local));
        self.push(Instruction::End); // end of if/else
        Ok(())
    }

    /// Emit `arr.unshift(a, b, …)` — insert args at the front and return the
    /// new length. Mirrors `splice`'s grow/in-place fork: when new_len exceeds
    /// capacity we copy-and-abandon (writing the new pointer back to the
    /// source identifier), otherwise we memcpy the existing tail up by
    /// insert_count and store the items. Items are pre-evaluated into locals
    /// so any side-effects happen before mutation, matching JS spec order.
    pub(crate) fn emit_array_unshift(
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
        let insert_count: i32 = call.arguments.len() as i32;

        // Pre-evaluate insert items into locals (left-to-right per JS spec).
        let mut item_locals: Vec<u32> = Vec::with_capacity(insert_count as usize);
        for arg in &call.arguments {
            let ty = self.emit_expr(arg.to_expression())?;
            if ty != elem_ty {
                if elem_ty == WasmType::F64 && ty == WasmType::I32 {
                    self.push(Instruction::F64ConvertI32S);
                } else {
                    return Err(CompileError::type_err(format!(
                        "Array.unshift item has type {ty:?}, expected {elem_ty:?}"
                    )));
                }
            }
            let local = self.alloc_local(elem_ty);
            self.push(Instruction::LocalSet(local));
            item_locals.push(local);
        }

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
        let cap_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(cap_local));

        // new_len = len + insert_count
        let new_len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(insert_count));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(new_len_local));

        // if new_len > cap: copy-and-abandon reallocation, else: in-place shift.
        self.push(Instruction::LocalGet(new_len_local));
        self.push(Instruction::LocalGet(cap_local));
        self.push(Instruction::I32GtU);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        {
            // Allocate new buffer sized to new_len.
            self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
            self.push(Instruction::LocalGet(new_len_local));
            self.push(Instruction::I32Const(esize));
            self.push(Instruction::I32Mul);
            self.push(Instruction::I32Add);
            let new_ptr = self.emit_arena_alloc_to_local(true)?;

            // new_ptr[0] = new_len, new_ptr[4] = new_len (capacity snaps tight)
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

            // Copy existing tail: dst = new+HEADER+insert_count*esize,
            // src = arr+HEADER, n = len*esize.
            self.push(Instruction::LocalGet(new_ptr));
            self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32 + insert_count * esize));
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

            // Store inserted items at new+HEADER+i*esize.
            for (i, &item_local) in item_locals.iter().enumerate() {
                let item_offset = ARRAY_HEADER_SIZE as u64 + (i as u64) * (esize as u64);
                self.push(Instruction::LocalGet(new_ptr));
                self.push(Instruction::LocalGet(item_local));
                match elem_ty {
                    WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg {
                        offset: item_offset,
                        align: 3,
                        memory_index: 0,
                    })),
                    _ => self.push(Instruction::I32Store(wasm_encoder::MemArg {
                        offset: item_offset,
                        align: 2,
                        memory_index: 0,
                    })),
                }
            }

            // arr_local := new_ptr, and write back to the source variable if simple identifier.
            self.push(Instruction::LocalGet(new_ptr));
            self.push(Instruction::LocalSet(arr_local));
            if let Expression::Identifier(ident) = arr_expr {
                let name = ident.name.as_str();
                if let Some(&(idx, _ty)) = self.locals.get(name) {
                    self.push(Instruction::LocalGet(new_ptr));
                    self.push(Instruction::LocalSet(idx));
                }
            }
        }
        self.push(Instruction::Else);
        {
            // In-place: memory.copy shifts the tail up by insert_count elements.
            // Overlap is handled by the WASM spec (per-byte copy).
            if insert_count > 0 {
                // dst = arr+HEADER+insert_count*esize
                self.push(Instruction::LocalGet(arr_local));
                self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32 + insert_count * esize));
                self.push(Instruction::I32Add);
                // src = arr+HEADER
                self.push(Instruction::LocalGet(arr_local));
                self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
                self.push(Instruction::I32Add);
                // n = len * esize
                self.push(Instruction::LocalGet(len_local));
                self.push(Instruction::I32Const(esize));
                self.push(Instruction::I32Mul);
                self.push(Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }

            // Store items at arr+HEADER+i*esize.
            for (i, &item_local) in item_locals.iter().enumerate() {
                let item_offset = ARRAY_HEADER_SIZE as u64 + (i as u64) * (esize as u64);
                self.push(Instruction::LocalGet(arr_local));
                self.push(Instruction::LocalGet(item_local));
                match elem_ty {
                    WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg {
                        offset: item_offset,
                        align: 3,
                        memory_index: 0,
                    })),
                    _ => self.push(Instruction::I32Store(wasm_encoder::MemArg {
                        offset: item_offset,
                        align: 2,
                        memory_index: 0,
                    })),
                }
            }

            // arr[0] = new_len
            self.push(Instruction::LocalGet(arr_local));
            self.push(Instruction::LocalGet(new_len_local));
            self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
        }
        self.push(Instruction::End);

        // Result value: the new length.
        self.push(Instruction::LocalGet(new_len_local));
        Ok(())
    }

    /// Emit `arr.copyWithin(target, start, end?)` — shallow in-place copy of
    /// the `[start, end)` slice to the position beginning at `target`. ES §
    /// 23.1.3.4: mutates the array, returns it, length unchanged. Negative
    /// indices are normalized by adding `len`; all three are clamped to
    /// `[0, len]`. `count = min(end - start, len - target)` caps the copy at
    /// the trailing room; a single `memory.copy` handles overlap in either
    /// direction per the wasm spec.
    pub(crate) fn emit_array_copy_within(
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

        // Eval target, start, end — argcount ∈ {2, 3} (caller validated).
        let target_local = self.alloc_local(WasmType::I32);
        let start_local = self.alloc_local(WasmType::I32);
        let end_local = self.alloc_local(WasmType::I32);

        let ty = self.emit_expr(call.arguments[0].to_expression())?;
        if ty == WasmType::F64 {
            self.push(Instruction::I32TruncF64S);
        }
        self.push(Instruction::LocalSet(target_local));

        let ty = self.emit_expr(call.arguments[1].to_expression())?;
        if ty == WasmType::F64 {
            self.push(Instruction::I32TruncF64S);
        }
        self.push(Instruction::LocalSet(start_local));

        if call.arguments.len() == 3 {
            let ty = self.emit_expr(call.arguments[2].to_expression())?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
        } else {
            self.push(Instruction::LocalGet(len_local));
        }
        self.push(Instruction::LocalSet(end_local));

        // Normalize negatives: bound < 0 ⇒ bound += len.
        for &bound in &[target_local, start_local, end_local] {
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(wasm_encoder::BlockType::Empty));
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(bound));
            self.push(Instruction::End);
        }

        // Clamp each to [0, len].
        let clamp_to_len = |fc: &mut FuncContext<'a>, bound: u32| {
            fc.push(Instruction::LocalGet(bound));
            fc.push(Instruction::I32Const(0));
            fc.push(Instruction::I32LtS);
            fc.push(Instruction::If(wasm_encoder::BlockType::Empty));
            fc.push(Instruction::I32Const(0));
            fc.push(Instruction::LocalSet(bound));
            fc.push(Instruction::End);
            fc.push(Instruction::LocalGet(bound));
            fc.push(Instruction::LocalGet(len_local));
            fc.push(Instruction::I32GtS);
            fc.push(Instruction::If(wasm_encoder::BlockType::Empty));
            fc.push(Instruction::LocalGet(len_local));
            fc.push(Instruction::LocalSet(bound));
            fc.push(Instruction::End);
        };
        clamp_to_len(self, target_local);
        clamp_to_len(self, start_local);
        clamp_to_len(self, end_local);

        // count = min(end - start, len - target). Both terms can be ≤ 0 after
        // clamping (e.g. end < start); a guard below skips memory.copy in that
        // case since wasm's memory.copy with n=0 is legal but negative would
        // trap via u32 wrap.
        let count_local = self.alloc_local(WasmType::I32);
        let avail_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(end_local));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(count_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::LocalGet(target_local));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(avail_local));
        // count = count < avail ? count : avail
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::LocalGet(avail_local));
        self.push(Instruction::I32LtS);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Else);
        self.push(Instruction::LocalGet(avail_local));
        self.push(Instruction::LocalSet(count_local));
        self.push(Instruction::End);

        // if count > 0: memory.copy(dst, src, n)
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32GtS);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        // dst = arr + HEADER + target * esize
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(target_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        // src = arr + HEADER + start * esize
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        // n = count * esize
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        self.push(Instruction::End);

        // Return the same array pointer.
        self.push(Instruction::LocalGet(arr_local));
        Ok(())
    }

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

    /// Emit `arr.reverse()` — swap elements in place and leave the array
    /// pointer on the stack so `arr.reverse()` can chain or be assigned.
    pub(crate) fn emit_array_reverse(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
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

        let lo = self.alloc_local(WasmType::I32);
        let hi = self.alloc_local(WasmType::I32);
        let tmp_a = self.alloc_local(elem_ty);
        let tmp_b = self.alloc_local(elem_ty);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(lo));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(hi));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        // if lo >= hi, break
        self.push(Instruction::LocalGet(lo));
        self.push(Instruction::LocalGet(hi));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        // tmp_a = arr[lo]; tmp_b = arr[hi]
        let emit_addr = |fc: &mut FuncContext<'a>, idx: u32| {
            fc.push(Instruction::LocalGet(arr_local));
            fc.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
            fc.push(Instruction::I32Add);
            fc.push(Instruction::LocalGet(idx));
            fc.push(Instruction::I32Const(esize));
            fc.push(Instruction::I32Mul);
            fc.push(Instruction::I32Add);
        };
        emit_addr(self, lo);
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
        self.push(Instruction::LocalSet(tmp_a));
        emit_addr(self, hi);
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
        self.push(Instruction::LocalSet(tmp_b));

        // arr[lo] = tmp_b
        emit_addr(self, lo);
        self.push(Instruction::LocalGet(tmp_b));
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
        // arr[hi] = tmp_a
        emit_addr(self, hi);
        self.push(Instruction::LocalGet(tmp_a));
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

        // lo++, hi--
        self.push(Instruction::LocalGet(lo));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(lo));
        self.push(Instruction::LocalGet(hi));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(hi));
        self.push(Instruction::Br(0));

        self.push(Instruction::End);
        self.push(Instruction::End);

        // Return arr pointer
        self.push(Instruction::LocalGet(arr_local));
        Ok(())
    }

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

    /// Emit `arr.fill(value, start?, end?)` — in-place, leaves arr pointer
    /// on the stack. Negative start/end indices are normalized by adding len.
    pub(crate) fn emit_array_fill(
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

        let val_local = self.alloc_local(elem_ty);
        self.emit_expr(call.arguments[0].to_expression())?;
        self.push(Instruction::LocalSet(val_local));

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

        let start_local = self.alloc_local(WasmType::I32);
        let end_local = self.alloc_local(WasmType::I32);

        // start default = 0
        if call.arguments.len() >= 2 {
            let ty = self.emit_expr(call.arguments[1].to_expression())?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
        } else {
            self.push(Instruction::I32Const(0));
        }
        self.push(Instruction::LocalSet(start_local));
        // end default = len
        if call.arguments.len() == 3 {
            let ty = self.emit_expr(call.arguments[2].to_expression())?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
        } else {
            self.push(Instruction::LocalGet(len_local));
        }
        self.push(Instruction::LocalSet(end_local));

        // Normalize negatives: start < 0 -> start += len; end < 0 -> end += len
        for &bound in &[start_local, end_local] {
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(wasm_encoder::BlockType::Empty));
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(bound));
            self.push(Instruction::End);
        }

        // Clamp: if bound < lower → bound = lower; if bound > len → bound = len.
        let clamp = |fc: &mut FuncContext<'a>, bound: u32, lower: u32| {
            fc.push(Instruction::LocalGet(bound));
            fc.push(Instruction::LocalGet(lower));
            fc.push(Instruction::I32LtS);
            fc.push(Instruction::If(wasm_encoder::BlockType::Empty));
            fc.push(Instruction::LocalGet(lower));
            fc.push(Instruction::LocalSet(bound));
            fc.push(Instruction::End);
            fc.push(Instruction::LocalGet(bound));
            fc.push(Instruction::LocalGet(len_local));
            fc.push(Instruction::I32GtS);
            fc.push(Instruction::If(wasm_encoder::BlockType::Empty));
            fc.push(Instruction::LocalGet(len_local));
            fc.push(Instruction::LocalSet(bound));
            fc.push(Instruction::End);
        };
        let zero_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(zero_local));
        clamp(self, start_local, zero_local);
        clamp(self, end_local, start_local);

        // Loop i from start to end-1
        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(end_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        // arr[i] = val
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(i_local));
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

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(arr_local));
        Ok(())
    }

    /// Emit `arr.slice(start?, end?)` — allocates a new array and copies the
    /// selected range via memory.copy. Negative indices are normalized by
    /// adding len; both ends are clamped to [0, len].
    pub(crate) fn emit_array_slice(
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

        let start_local = self.alloc_local(WasmType::I32);
        let end_local = self.alloc_local(WasmType::I32);
        if !call.arguments.is_empty() {
            let ty = self.emit_expr(call.arguments[0].to_expression())?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
        } else {
            self.push(Instruction::I32Const(0));
        }
        self.push(Instruction::LocalSet(start_local));
        if call.arguments.len() == 2 {
            let ty = self.emit_expr(call.arguments[1].to_expression())?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
        } else {
            self.push(Instruction::LocalGet(len_local));
        }
        self.push(Instruction::LocalSet(end_local));

        // Normalize + clamp (same pattern as fill)
        for &bound in &[start_local, end_local] {
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(wasm_encoder::BlockType::Empty));
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(bound));
            self.push(Instruction::End);
        }
        // Clamp to [0, len]
        let clamp_to_len = |fc: &mut FuncContext<'a>, bound: u32| {
            // if bound < 0: bound = 0
            fc.push(Instruction::LocalGet(bound));
            fc.push(Instruction::I32Const(0));
            fc.push(Instruction::I32LtS);
            fc.push(Instruction::If(wasm_encoder::BlockType::Empty));
            fc.push(Instruction::I32Const(0));
            fc.push(Instruction::LocalSet(bound));
            fc.push(Instruction::End);
            // if bound > len: bound = len
            fc.push(Instruction::LocalGet(bound));
            fc.push(Instruction::LocalGet(len_local));
            fc.push(Instruction::I32GtS);
            fc.push(Instruction::If(wasm_encoder::BlockType::Empty));
            fc.push(Instruction::LocalGet(len_local));
            fc.push(Instruction::LocalSet(bound));
            fc.push(Instruction::End);
        };
        clamp_to_len(self, start_local);
        clamp_to_len(self, end_local);
        // if end < start: end = start
        self.push(Instruction::LocalGet(end_local));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32LtS);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::LocalSet(end_local));
        self.push(Instruction::End);

        // count = end - start
        let count_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(end_local));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(count_local));

        // Allocate new array via arena (header + count * esize)
        let new_ptr = self.alloc_local(WasmType::I32);
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::LocalSet(new_ptr));
        // bump arena by header + count*esize
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Add);
        self.push(Instruction::GlobalSet(arena_idx));

        // Write header: length=count, capacity=count
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // memory.copy(dst=new_ptr+HEADER, src=arr+HEADER+start*esize, n=count*esize)
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        self.push(Instruction::LocalGet(new_ptr));
        Ok(())
    }

    /// Emit `arr.concat(other)` — new array = this + other. Only the
    /// single-argument, same-element-type form is supported (richer overloads
    /// can be layered via the closure builtins in a later pass).
    /// `arr.concat(b, c, ...)` — variadic concat. Each argument must be an
    /// array of the same element type. Allocates once for the total length and
    /// memcpys each source in order. Single-argument calls are the common case
    /// but the variadic form mirrors the ES spec's overload (ignoring the
    /// non-array-arg form, which doesn't fit the typed subset).
    pub(crate) fn emit_array_concat(
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

        // Collect (ptr_local, len_local) pairs for the receiver and each arg.
        let mut sources: Vec<(u32, u32)> = Vec::with_capacity(call.arguments.len() + 1);
        let push_source = |fc: &mut FuncContext<'a>,
                               expr: &Expression<'a>|
         -> Result<(u32, u32), CompileError> {
            let ptr = fc.alloc_local(WasmType::I32);
            fc.emit_expr(expr)?;
            fc.push(Instruction::LocalSet(ptr));
            let len = fc.alloc_local(WasmType::I32);
            fc.push(Instruction::LocalGet(ptr));
            fc.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
            fc.push(Instruction::LocalSet(len));
            Ok((ptr, len))
        };
        sources.push(push_source(self, arr_expr)?);
        for arg in &call.arguments {
            sources.push(push_source(self, arg.to_expression())?);
        }

        // total_len = sum of all lengths.
        let total_len = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(sources[0].1));
        for (_, len) in sources.iter().skip(1) {
            self.push(Instruction::LocalGet(*len));
            self.push(Instruction::I32Add);
        }
        self.push(Instruction::LocalSet(total_len));

        // Allocate new array.
        let new_ptr = self.alloc_local(WasmType::I32);
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::LocalSet(new_ptr));
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(total_len));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Add);
        self.push(Instruction::GlobalSet(arena_idx));

        // Header: length = capacity = total_len.
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(total_len));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(total_len));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // Running byte offset within the new array's body, held in a local so
        // each copy step can advance it by the current source's byte length.
        let offset_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(offset_local));

        for (ptr, len) in &sources {
            // memory.copy(new_ptr + HEADER + offset, ptr + HEADER, len * esize)
            self.push(Instruction::LocalGet(new_ptr));
            self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalGet(offset_local));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalGet(*ptr));
            self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalGet(*len));
            self.push(Instruction::I32Const(esize));
            self.push(Instruction::I32Mul);
            self.push(Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });
            // offset += len * esize
            self.push(Instruction::LocalGet(offset_local));
            self.push(Instruction::LocalGet(*len));
            self.push(Instruction::I32Const(esize));
            self.push(Instruction::I32Mul);
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(offset_local));
        }

        self.push(Instruction::LocalGet(new_ptr));
        Ok(())
    }

    /// Emit `arr.join(sep?)` — stringifies each element (i32 via __str_from_i32,
    /// f64 via __str_from_f64, string elements pass through) and concatenates
    /// with `sep` (default ",") between them.
    pub(crate) fn emit_array_join(
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

        // Select element-stringifier helper
        let to_str_helper: &str = match elem_ty {
            WasmType::I32 => "__str_from_i32",
            WasmType::F64 => "__str_from_f64",
            _ => unreachable!(),
        };
        let to_str_idx = self
            .module_ctx
            .get_func(to_str_helper)
            .ok_or_else(|| {
                CompileError::codegen(format!(
                    "Array.join requires {to_str_helper} — ensure string runtime is registered"
                ))
            })?
            .0;
        let concat_idx = self
            .module_ctx
            .get_func("__str_concat")
            .ok_or_else(|| CompileError::codegen("Array.join requires __str_concat"))?
            .0;

        // sep: evaluate once
        let sep_local = self.alloc_local(WasmType::I32);
        if call.arguments.is_empty() {
            // Default separator "," — intern once
            let offset = self.module_ctx.alloc_static_string(",");
            self.push(Instruction::I32Const(offset as i32));
        } else {
            self.emit_expr(call.arguments[0].to_expression())?;
        }
        self.push(Instruction::LocalSet(sep_local));

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

        // result = "" (empty interned string)
        let empty_off = self.module_ctx.alloc_static_string("");
        let result_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(empty_off as i32));
        self.push(Instruction::LocalSet(result_local));

        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        // If i > 0, prepend sep: result = concat(result, sep)
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32GtS);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::LocalGet(result_local));
        self.push(Instruction::LocalGet(sep_local));
        self.push(Instruction::Call(concat_idx));
        self.push(Instruction::LocalSet(result_local));
        self.push(Instruction::End);

        // Load arr[i], stringify, concat
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
        self.push(Instruction::Call(to_str_idx));
        // concat result with element string
        let elem_str = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalSet(elem_str));
        self.push(Instruction::LocalGet(result_local));
        self.push(Instruction::LocalGet(elem_str));
        self.push(Instruction::Call(concat_idx));
        self.push(Instruction::LocalSet(result_local));

        // i++
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
    /// Emit `arr.splice(start, deleteCount?, ...items)` — returns a new array
    /// holding the removed elements, mutates the original in place (shrink or
    /// stable shift) when capacity allows, otherwise copy-and-abandons like
    /// push. Insert items are pre-evaluated into locals so their side-effects
    /// happen before any mutation, matching JS spec order.
    pub(crate) fn emit_array_splice(
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

        // Pre-evaluate insert items into locals (JS spec: arguments evaluated
        // left-to-right before mutation). Doing this upfront also keeps the
        // later shift/copy emission simple.
        let mut item_locals: Vec<u32> = Vec::with_capacity(insert_count as usize);
        for arg in call.arguments.iter().skip(2) {
            let expr = arg.to_expression();
            let ty = self.emit_expr(expr)?;
            if ty != elem_ty {
                if elem_ty == WasmType::F64 && ty == WasmType::I32 {
                    self.push(Instruction::F64ConvertI32S);
                } else {
                    return Err(CompileError::type_err(format!(
                        "Array.splice insert item has type {ty:?}, expected {elem_ty:?}"
                    )));
                }
            }
            let local = self.alloc_local(elem_ty);
            self.push(Instruction::LocalSet(local));
            item_locals.push(local);
        }

        // Evaluate and save array pointer.
        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(arr_local));

        // Load length and capacity.
        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(len_local));
        let cap_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(cap_local));

        // start: eval, truncate f64→i32, normalize negative (+len), clamp to [0, len].
        let start_local = self.alloc_local(WasmType::I32);
        let ty = self.emit_expr(call.arguments[0].to_expression())?;
        if ty == WasmType::F64 {
            self.push(Instruction::I32TruncF64S);
        }
        self.push(Instruction::LocalSet(start_local));
        // if start < 0: start += len
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32LtS);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(start_local));
        self.push(Instruction::End);
        // clamp start to [0, len]
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

        // deleteCount: default len-start; otherwise clamp to [0, len-start].
        let delete_local = self.alloc_local(WasmType::I32);
        if call.arguments.len() >= 2 {
            let ty = self.emit_expr(call.arguments[1].to_expression())?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
            self.push(Instruction::LocalSet(delete_local));
            // if delete < 0: delete = 0
            self.push(Instruction::LocalGet(delete_local));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(wasm_encoder::BlockType::Empty));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::LocalSet(delete_local));
            self.push(Instruction::End);
            // cap_left = len - start; if delete > cap_left: delete = cap_left
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
            // Default: remove everything from start onward.
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::LocalGet(start_local));
            self.push(Instruction::I32Sub);
            self.push(Instruction::LocalSet(delete_local));
        }

        // Allocate removed array [header + delete * esize]. Header is written
        // even when delete == 0 so the result is a valid empty array.
        let removed_ptr = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(delete_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        let tmp_ptr = self.emit_arena_alloc_to_local(true)?;
        self.push(Instruction::LocalGet(tmp_ptr));
        self.push(Instruction::LocalSet(removed_ptr));
        // length, capacity = delete
        self.push(Instruction::LocalGet(removed_ptr));
        self.push(Instruction::LocalGet(delete_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(removed_ptr));
        self.push(Instruction::LocalGet(delete_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));
        // memory.copy(removed + HEADER, arr + HEADER + start*esize, delete*esize)
        self.push(Instruction::LocalGet(removed_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(delete_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        // new_len = len - delete + insert_count
        let new_len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::LocalGet(delete_local));
        self.push(Instruction::I32Sub);
        self.push(Instruction::I32Const(insert_count));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(new_len_local));

        // if new_len > cap: copy-and-abandon path; else: in-place.
        self.push(Instruction::LocalGet(new_len_local));
        self.push(Instruction::LocalGet(cap_local));
        self.push(Instruction::I32GtU);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        {
            // Copy-and-abandon: allocate new buffer sized to new_len.
            let new_ptr = self.alloc_local(WasmType::I32);
            self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
            self.push(Instruction::LocalGet(new_len_local));
            self.push(Instruction::I32Const(esize));
            self.push(Instruction::I32Mul);
            self.push(Instruction::I32Add);
            let alloc_tmp = self.emit_arena_alloc_to_local(true)?;
            self.push(Instruction::LocalGet(alloc_tmp));
            self.push(Instruction::LocalSet(new_ptr));

            // new_ptr[0] = new_len, new_ptr[4] = new_len
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

            // Copy prefix: memory.copy(new + HEADER, arr + HEADER, start*esize)
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

            // Write insert items at new + HEADER + (start + i) * esize
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
            //   len = (len - start - delete) * esize
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

            // arr_local := new_ptr, and write back to the source variable if simple identifier.
            self.push(Instruction::LocalGet(new_ptr));
            self.push(Instruction::LocalSet(arr_local));
            if let Expression::Identifier(ident) = arr_expr {
                let name = ident.name.as_str();
                if let Some(&(idx, _ty)) = self.locals.get(name) {
                    self.push(Instruction::LocalGet(new_ptr));
                    self.push(Instruction::LocalSet(idx));
                }
            }
        }
        self.push(Instruction::Else);
        {
            // In-place: shift suffix then write items then update length.
            // memory.copy handles overlapping regions correctly per WASM spec.
            // Only shift when insert_count != delete (otherwise source-equals-
            // dest would be wasteful though harmless).
            if insert_count != 0 {
                // Always emit the shift: compile-time we don't know if delete==insert_count.
                self.push(Instruction::LocalGet(arr_local));
                self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
                self.push(Instruction::I32Add);
                self.push(Instruction::LocalGet(start_local));
                self.push(Instruction::I32Const(insert_count));
                self.push(Instruction::I32Add);
                self.push(Instruction::I32Const(esize));
                self.push(Instruction::I32Mul);
                self.push(Instruction::I32Add);
                // src = arr + HEADER + (start + delete) * esize
                self.push(Instruction::LocalGet(arr_local));
                self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
                self.push(Instruction::I32Add);
                self.push(Instruction::LocalGet(start_local));
                self.push(Instruction::LocalGet(delete_local));
                self.push(Instruction::I32Add);
                self.push(Instruction::I32Const(esize));
                self.push(Instruction::I32Mul);
                self.push(Instruction::I32Add);
                // n = (len - start - delete) * esize
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
            } else {
                // Pure shrink (or no-op when delete == 0): shift only when delete > 0.
                // We still need a shift when inserting 0 but deleting >0.
                self.push(Instruction::LocalGet(delete_local));
                self.push(Instruction::I32Eqz);
                self.push(Instruction::I32Eqz); // delete > 0?
                self.push(Instruction::If(wasm_encoder::BlockType::Empty));
                // dst = arr + HEADER + start * esize
                self.push(Instruction::LocalGet(arr_local));
                self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
                self.push(Instruction::I32Add);
                self.push(Instruction::LocalGet(start_local));
                self.push(Instruction::I32Const(esize));
                self.push(Instruction::I32Mul);
                self.push(Instruction::I32Add);
                // src = arr + HEADER + (start + delete) * esize
                self.push(Instruction::LocalGet(arr_local));
                self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
                self.push(Instruction::I32Add);
                self.push(Instruction::LocalGet(start_local));
                self.push(Instruction::LocalGet(delete_local));
                self.push(Instruction::I32Add);
                self.push(Instruction::I32Const(esize));
                self.push(Instruction::I32Mul);
                self.push(Instruction::I32Add);
                // n = (len - start - delete) * esize
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
                self.push(Instruction::End);
            }

            // Write insert items at arr + HEADER + (start + i) * esize
            for (i, &item_local) in item_locals.iter().enumerate() {
                self.push(Instruction::LocalGet(arr_local));
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

            // arr[0] = new_len
            self.push(Instruction::LocalGet(arr_local));
            self.push(Instruction::LocalGet(new_len_local));
            self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
        }
        self.push(Instruction::End);

        // Leave removed ptr on stack.
        self.push(Instruction::LocalGet(removed_ptr));
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

    /// `Array.of<T>(...items)` — construct a new array containing the argument
    /// list in order. Element type is taken from the explicit `<T>` when given,
    /// otherwise inferred from the first argument (same rule as an array
    /// literal). The empty `Array.of()` without `<T>` is a type error.
    pub(crate) fn emit_array_of(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        // Resolve element type: explicit <T> wins, else infer from arg 0.
        let elem_ty = if let Some(type_args) = call.type_arguments.as_ref()
            && let Some(first) = type_args.params.first()
        {
            crate::types::resolve_ts_type(first, &self.module_ctx.class_names)?
        } else if let Some(first) = call.arguments.first() {
            let (ty, _) = self.infer_init_type(first.to_expression())?;
            ty
        } else {
            return Err(CompileError::type_err(
                "Array.of() requires at least one argument or an explicit type: Array.of<T>()",
            ));
        };
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => {
                return Err(CompileError::type_err(
                    "Array.of element type must be i32 or f64",
                ));
            }
        };

        let count = call.arguments.len() as i32;
        let total = ARRAY_HEADER_SIZE as i32 + count * esize;
        self.push(Instruction::I32Const(total));
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        // length = count
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(count));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        // capacity = count
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(count));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        for (i, arg) in call.arguments.iter().enumerate() {
            self.push(Instruction::LocalGet(ptr_local));
            let ty = self.emit_expr(arg.to_expression())?;
            if ty != elem_ty {
                if elem_ty == WasmType::F64 && ty == WasmType::I32 {
                    self.push(Instruction::F64ConvertI32S);
                } else {
                    return Err(CompileError::type_err(format!(
                        "Array.of argument {i} has type {ty:?}, expected {elem_ty:?}"
                    )));
                }
            }
            let offset = (ARRAY_HEADER_SIZE as i32 + (i as i32) * esize) as u64;
            match elem_ty {
                WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg {
                    offset,
                    align: 3,
                    memory_index: 0,
                })),
                WasmType::I32 => self.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset,
                    align: 2,
                    memory_index: 0,
                })),
                _ => unreachable!(),
            }
        }

        self.push(Instruction::LocalGet(ptr_local));
        Ok(())
    }

    /// `Array.from(src)` — shallow clone of an existing array (same shape as
    /// `src.slice()`). `Array.from(src, mapFn)` — same shape as `src.map(fn)`.
    /// `Array.from({length: n}, mapFn)` — sequence-generation form, recognized
    /// as a narrow object-literal pattern (exactly one `length` property); the
    /// map function is required so the element type can be inferred from its
    /// return, and each invocation sees `value = 0` since the typed subset has
    /// no `undefined`. When general object literals arrive, this recognizer
    /// still fires only on the exact shape — richer literals fall through to
    /// the array-source path and error appropriately.
    pub(crate) fn emit_array_from(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        if call.arguments.is_empty() || call.arguments.len() > 2 {
            return Err(CompileError::codegen(
                "Array.from expects 1 or 2 arguments: Array.from(src) or Array.from(src, mapFn)",
            ));
        }
        let src_expr = call.arguments[0].to_expression();

        if let Some(len_expr) = extract_length_only_object(src_expr) {
            let map_fn = call.arguments.get(1).map(|a| a.to_expression()).ok_or_else(|| {
                CompileError::codegen(
                    "Array.from({length: n}) requires a mapping function as the second argument — without it the element type can't be inferred in the typed subset",
                )
            })?;
            return self.emit_array_from_length(call, len_expr, map_fn);
        }

        let src_elem = self.resolve_expr_array_elem(src_expr).ok_or_else(|| {
            CompileError::type_err(
                "Array.from source must be an array or a `{length: n}` object literal",
            )
        })?;
        let src_class = self.resolve_expr_array_elem_class(src_expr);

        if call.arguments.len() == 1 {
            self.emit_array_from_copy(src_expr, src_elem)
        } else {
            let map_fn = call.arguments[1].to_expression();
            self.emit_array_from_map(src_expr, src_elem, src_class.as_deref(), map_fn)
        }
    }

    /// `Array.from({length: n}, mapFn)` — allocate an array of length n,
    /// invoke mapFn(0, i) for each i in [0, n), write results. The `value`
    /// argument is always 0 (the typed subset has no `undefined`); idiomatic
    /// code writes `(_, i) => …` and ignores it.
    fn emit_array_from_length(
        &mut self,
        call: &CallExpression<'a>,
        len_expr: &Expression<'a>,
        map_fn: &Expression<'a>,
    ) -> Result<(), CompileError> {
        use crate::codegen::array_builtins::{eval_arrow_body, extract_arrow};

        let arrow = extract_arrow(map_fn)?;
        let mut params: Vec<String> = Vec::new();
        for p in &arrow.params.items {
            match &p.pattern {
                BindingPattern::BindingIdentifier(id) => params.push(id.name.as_str().to_string()),
                _ => {
                    return Err(CompileError::unsupported(
                        "Array.from({length}, fn): mapFn parameter must be a simple identifier",
                    ));
                }
            }
        }
        if params.is_empty() || params.len() > 2 {
            return Err(CompileError::codegen(
                "Array.from({length}, fn): mapFn must take 1 or 2 parameters (value, index)",
            ));
        }

        // Element type resolution: explicit `Array.from<T>(...)` wins, else
        // infer from the arrow body with value-param defaulted to i32.
        let elem_ty = if let Some(type_args) = call.type_arguments.as_ref()
            && let Some(first) = type_args.params.first()
        {
            crate::types::resolve_ts_type(first, &self.module_ctx.class_names)?
        } else {
            self.infer_arrow_result_type(arrow, &params, WasmType::I32, None)?
        };
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => {
                return Err(CompileError::type_err(
                    "Array.from({length}, fn): element type must be i32 or f64",
                ));
            }
        };

        // Evaluate length into a local (i32).
        let len_local = self.alloc_local(WasmType::I32);
        let len_ty = self.emit_expr(len_expr)?;
        if len_ty == WasmType::F64 {
            self.push(Instruction::I32TruncSatF64S);
        }
        self.push(Instruction::LocalSet(len_local));

        // Allocate result: header + len * esize. Capacity = len.
        let arena_idx = self
            .module_ctx
            .arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

        let result_ptr = self.alloc_local(WasmType::I32);
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::LocalSet(result_ptr));
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Add);
        self.push(Instruction::GlobalSet(arena_idx));

        // length = len
        self.push(Instruction::LocalGet(result_ptr));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        // capacity = len
        self.push(Instruction::LocalGet(result_ptr));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // value_local = 0 (placeholder for undefined)
        let value_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(value_local));

        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        // if i >= len, break
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        // Bind arrow params: value -> value_local (i32=0), index -> i_local
        let mut param_locals: Vec<(u32, WasmType)> = vec![(value_local, WasmType::I32)];
        let mut param_classes: Vec<Option<String>> = vec![None];
        if params.len() >= 2 {
            param_locals.push((i_local, WasmType::I32));
            param_classes.push(None);
        }
        let scope = crate::codegen::array_builtins::setup_arrow_scope(
            self,
            &params,
            &param_locals,
            &param_classes,
        );

        // Pre-compute destination address: result_ptr + HEADER + i*esize
        // so we can write the arrow's result without threading it through a
        // temp local. Interleaved with arrow evaluation: push addr first,
        // then push arrow body, then store.
        self.push(Instruction::LocalGet(result_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);

        let body_ty = eval_arrow_body(self, arrow)?;
        if body_ty != elem_ty {
            if elem_ty == WasmType::F64 && body_ty == WasmType::I32 {
                self.push(Instruction::F64ConvertI32S);
            } else {
                return Err(CompileError::type_err(format!(
                    "Array.from({{length}}, fn): mapFn returns {body_ty:?}, expected {elem_ty:?}"
                )));
            }
        }

        // Store element
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg {
                offset: 0,
                align: 3,
                memory_index: 0,
            })),
            WasmType::I32 => self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            })),
            _ => unreachable!(),
        }

        crate::codegen::array_builtins::restore_arrow_scope(self, scope);

        // i++
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End); // loop
        self.push(Instruction::End); // block

        self.push(Instruction::LocalGet(result_ptr));
        Ok(())
    }

    /// Shallow clone of `src` via a single memory.copy of header + elements.
    fn emit_array_from_copy(
        &mut self,
        src_expr: &Expression<'a>,
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

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(src_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(src_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(len_local));

        // Allocate header + len * esize; capacity = len.
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

        // Write header: length=len, capacity=len.
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

        // memory.copy(dst=new_ptr+HEADER, src=src_ptr+HEADER, n=len*esize)
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(src_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        self.push(Instruction::LocalGet(new_ptr));
        Ok(())
    }

    /// `Array.from(src, mapFn)` form — same shape as `src.map(mapFn)`, so we
    /// just delegate. The dispatcher already validated `src` is an array and
    /// resolved the element type.
    fn emit_array_from_map(
        &mut self,
        src_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        map_fn: &Expression<'a>,
    ) -> Result<(), CompileError> {
        self.emit_array_map(src_expr, elem_ty, elem_class, map_fn)?;
        Ok(())
    }

    pub(crate) fn emit_array_bounds_check(&mut self, arr_local: u32, idx_local: u32) {
        // if (index >= length) unreachable
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        })); // load length
        self.push(Instruction::I32GeU); // unsigned comparison: catches negative indices too
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Unreachable);
        self.push(Instruction::End);
    }
    pub fn resolve_expr_array_elem(&self, expr: &Expression<'a>) -> Option<WasmType> {
        match expr {
            Expression::Identifier(ident) => self
                .local_array_elem_types
                .get(ident.name.as_str())
                .copied(),
            // arr.filter() / arr.sort() / arr.splice() / arr.slice() / arr.concat()
            // return arrays with the same element type as source
            Expression::CallExpression(call) => {
                if let Expression::StaticMemberExpression(member) = &call.callee {
                    let method = member.property.name.as_str();
                    // Static Array.of<T>(…) / Array.from(src[, mapFn]).
                    if let Expression::Identifier(obj) = &member.object
                        && obj.name.as_str() == "Array"
                    {
                        return self.resolve_array_static_call_elem(call, method);
                    }
                    match method {
                        "filter" | "sort" | "splice" | "slice" | "concat" | "toReversed"
                        | "toSorted" | "toSpliced" | "with" => {
                            self.resolve_expr_array_elem(&member.object)
                        }
                        "map" => {
                            // map changes the element type — infer from arrow return
                            if let Some(arg) = call.arguments.first()
                                && let Some(arrow) =
                                    self.try_extract_arrow_expr(arg.to_expression())
                            {
                                let src_elem = self.resolve_expr_array_elem(&member.object)?;
                                let src_class = self.resolve_expr_array_elem_class(&member.object);
                                let params = arrow
                                    .params
                                    .items
                                    .iter()
                                    .filter_map(|p| match &p.pattern {
                                        BindingPattern::BindingIdentifier(id) => {
                                            Some(id.name.as_str().to_string())
                                        }
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>();
                                return self
                                    .infer_arrow_result_type(
                                        arrow,
                                        &params,
                                        src_elem,
                                        src_class.as_deref(),
                                    )
                                    .ok();
                            }
                            None
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Element type resolution for `Array.of<T>(...)` / `Array.from(src[, mapFn])`.
    /// Mirrors the rules used during emission so chained calls (e.g.
    /// `Array.from(xs).filter(...)`) can carry element-type tracking through
    /// the same call-expression dispatch path.
    fn resolve_array_static_call_elem(
        &self,
        call: &CallExpression<'a>,
        method: &str,
    ) -> Option<WasmType> {
        match method {
            "of" => {
                if let Some(type_args) = call.type_arguments.as_ref()
                    && let Some(first) = type_args.params.first()
                {
                    return crate::types::resolve_ts_type(first, &self.module_ctx.class_names)
                        .ok();
                }
                if let Some(first) = call.arguments.first() {
                    return self.infer_init_type(first.to_expression()).ok().map(|t| t.0);
                }
                None
            }
            "from" => {
                let src_expr = call.arguments.first()?.to_expression();

                // `Array.from({length: n}, mapFn)` — element type comes from
                // the explicit `<T>` if given, else from the mapFn return
                // inferred with value_ty defaulted to i32 (since `undefined`
                // isn't in the typed subset).
                if extract_length_only_object(src_expr).is_some() {
                    if let Some(type_args) = call.type_arguments.as_ref()
                        && let Some(first) = type_args.params.first()
                    {
                        return crate::types::resolve_ts_type(first, &self.module_ctx.class_names)
                            .ok();
                    }
                    let arrow = self.try_extract_arrow_expr(call.arguments.get(1)?.to_expression())?;
                    let params = arrow
                        .params
                        .items
                        .iter()
                        .filter_map(|p| match &p.pattern {
                            BindingPattern::BindingIdentifier(id) => {
                                Some(id.name.as_str().to_string())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    return self
                        .infer_arrow_result_type(arrow, &params, WasmType::I32, None)
                        .ok();
                }

                let src_elem = self.resolve_expr_array_elem(src_expr)?;
                // Form 1 (src only): element type preserved.
                // Form 2 (src, mapFn): inferred from the mapFn return type.
                if call.arguments.len() < 2 {
                    return Some(src_elem);
                }
                let src_class = self.resolve_expr_array_elem_class(src_expr);
                let arrow = self.try_extract_arrow_expr(call.arguments[1].to_expression())?;
                let params = arrow
                    .params
                    .items
                    .iter()
                    .filter_map(|p| match &p.pattern {
                        BindingPattern::BindingIdentifier(id) => {
                            Some(id.name.as_str().to_string())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                self.infer_arrow_result_type(arrow, &params, src_elem, src_class.as_deref())
                    .ok()
            }
            _ => None,
        }
    }

    /// Resolve the array element class name for an expression (if elements are class instances).
    pub fn resolve_expr_array_elem_class(&self, expr: &Expression<'a>) -> Option<String> {
        match expr {
            Expression::Identifier(ident) => self
                .local_array_elem_classes
                .get(ident.name.as_str())
                .cloned(),
            // Chained calls: filter/sort/splice/slice/concat preserve element class.
            // `Array.from(src)` also preserves it (shallow clone).
            Expression::CallExpression(call) => {
                if let Expression::StaticMemberExpression(member) = &call.callee {
                    let method = member.property.name.as_str();
                    if let Expression::Identifier(obj) = &member.object
                        && obj.name.as_str() == "Array"
                        && method == "from"
                        && call.arguments.len() == 1
                    {
                        return self.resolve_expr_array_elem_class(
                            call.arguments[0].to_expression(),
                        );
                    }
                    match method {
                        "filter" | "sort" | "splice" | "slice" | "concat" | "toReversed"
                        | "toSorted" | "toSpliced" | "with" => {
                            self.resolve_expr_array_elem_class(&member.object)
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}
