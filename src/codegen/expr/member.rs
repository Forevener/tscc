use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

use super::{ARRAY_HEADER_SIZE, math_constant, number_constant};

impl<'a> FuncContext<'a> {
    pub(crate) fn emit_member_access(
        &mut self,
        member: &StaticMemberExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        // Check for enum member access: EnumName.MemberName → global lookup
        if let Expression::Identifier(obj_ident) = &member.object {
            let enum_member_key = format!(
                "{}.{}",
                obj_ident.name.as_str(),
                member.property.name.as_str()
            );
            if let Some(&(idx, ty)) = self.module_ctx.globals.get(&enum_member_key) {
                self.push(Instruction::GlobalGet(idx));
                return Ok(ty);
            }

            // Math.<CONSTANT> → inline f64 literal (ECMAScript standard values)
            if obj_ident.name.as_str() == "Math"
                && let Some(val) = math_constant(member.property.name.as_str())
            {
                self.push(Instruction::F64Const(val));
                return Ok(WasmType::F64);
            }
            // Number.<CONSTANT> → inline f64 literal
            if obj_ident.name.as_str() == "Number"
                && let Some(val) = number_constant(member.property.name.as_str())
            {
                self.push(Instruction::F64Const(val));
                return Ok(WasmType::F64);
            }
        }

        let field_name = member.property.name.as_str();

        // Check if this is a string property access (str.length)
        if self.resolve_expr_is_string(&member.object) {
            return self.emit_string_property(member, field_name);
        }

        // Check if this is an array property access (arr.length)
        if let Some(_elem_ty) = self.resolve_expr_array_elem(&member.object) {
            return self.emit_array_property(member, field_name);
        }

        // Determine the class of the object
        let class_name = self.resolve_expr_class(&member.object)?;
        let layout = self
            .module_ctx
            .class_registry
            .get(&class_name)
            .ok_or_else(|| CompileError::codegen(format!("unknown class '{class_name}'")))?;

        let &(offset, ty) = layout.field_map.get(field_name).ok_or_else(|| {
            CompileError::codegen(format!("class '{class_name}' has no field '{field_name}'"))
        })?;

        // Emit the object pointer
        self.emit_expr(&member.object)?;

        // Load the field
        match ty {
            WasmType::F64 => {
                self.push(Instruction::F64Load(wasm_encoder::MemArg {
                    offset: offset as u64,
                    align: 3,
                    memory_index: 0,
                }));
            }
            WasmType::I32 => {
                self.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: offset as u64,
                    align: 2,
                    memory_index: 0,
                }));
            }
            _ => return Err(CompileError::codegen("void field access")),
        }

        Ok(ty)
    }

    pub(crate) fn emit_member_assign(
        &mut self,
        member: &StaticMemberExpression<'a>,
        value: &Expression<'a>,
        operator: AssignmentOperator,
    ) -> Result<WasmType, CompileError> {
        let field_name = member.property.name.as_str();
        let class_name = self.resolve_expr_class(&member.object)?;
        let layout = self
            .module_ctx
            .class_registry
            .get(&class_name)
            .ok_or_else(|| CompileError::codegen(format!("unknown class '{class_name}'")))?;
        let &(offset, ty) = layout.field_map.get(field_name).ok_or_else(|| {
            CompileError::codegen(format!("class '{class_name}' has no field '{field_name}'"))
        })?;

        // Emit: object pointer, value, then store
        self.emit_expr(&member.object)?; // address

        if operator == AssignmentOperator::Assign {
            self.emit_expr(value)?;
        } else {
            // For compound assignment (+=, etc): load current, compute, then store
            // We need the address twice — use a temp local
            let addr_tmp = self.alloc_local(WasmType::I32);
            self.push(Instruction::LocalTee(addr_tmp));
            // Load current value
            match ty {
                WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg {
                    offset: offset as u64,
                    align: 3,
                    memory_index: 0,
                })),
                WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: offset as u64,
                    align: 2,
                    memory_index: 0,
                })),
                _ => return Err(CompileError::codegen("void field")),
            }
            self.emit_expr(value)?;
            let is_f64 = ty == WasmType::F64;
            match operator {
                AssignmentOperator::Addition => self.push(if is_f64 {
                    Instruction::F64Add
                } else {
                    Instruction::I32Add
                }),
                AssignmentOperator::Subtraction => self.push(if is_f64 {
                    Instruction::F64Sub
                } else {
                    Instruction::I32Sub
                }),
                AssignmentOperator::Multiplication => self.push(if is_f64 {
                    Instruction::F64Mul
                } else {
                    Instruction::I32Mul
                }),
                AssignmentOperator::Division => self.push(if is_f64 {
                    Instruction::F64Div
                } else {
                    Instruction::I32DivS
                }),
                _ => return Err(CompileError::unsupported("compound member assignment")),
            }
            // Now we need the address back for the store — swap stack order
            // Actually, we need addr on stack before the value. Let me restructure.
            // Store the computed value in a temp, reload addr, then store
            let val_tmp = self.alloc_local(ty);
            self.push(Instruction::LocalSet(val_tmp));
            self.push(Instruction::LocalGet(addr_tmp));
            self.push(Instruction::LocalGet(val_tmp));
        }

        match ty {
            WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg {
                offset: offset as u64,
                align: 3,
                memory_index: 0,
            })),
            WasmType::I32 => self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: offset as u64,
                align: 2,
                memory_index: 0,
            })),
            _ => return Err(CompileError::codegen("void field store")),
        }

        Ok(WasmType::Void)
    }
    /// Emit arr[i] — bounds-checked element read.
    pub(crate) fn emit_computed_member_access(
        &mut self,
        member: &ComputedMemberExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        // String indexing: str[i] → i32 char code (byte value)
        if self.resolve_expr_is_string(&member.object) {
            return self.emit_string_index(member);
        }

        let elem_ty = self
            .resolve_expr_array_elem(&member.object)
            .ok_or_else(|| {
                CompileError::codegen(
                    "computed member access (arr[i]) only supported on Array<T> or string",
                )
            })?;
        let elem_size: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };

        // Evaluate array pointer and index, save to locals for reuse
        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&member.object)?;
        self.push(Instruction::LocalSet(arr_local));

        let idx_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&member.expression)?;
        self.push(Instruction::LocalSet(idx_local));

        // Bounds check: if index >= length, trap
        self.emit_array_bounds_check(arr_local, idx_local);

        // Compute element address: arr + 8 + index * elem_size
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::I32Const(elem_size));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);

        // Load element
        match elem_ty {
            WasmType::F64 => {
                self.push(Instruction::F64Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 3,
                    memory_index: 0,
                }));
            }
            WasmType::I32 => {
                self.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
            }
            _ => unreachable!(),
        }

        Ok(elem_ty)
    }
    pub(crate) fn emit_computed_member_assign(
        &mut self,
        member: &ComputedMemberExpression<'a>,
        value: &Expression<'a>,
        operator: AssignmentOperator,
    ) -> Result<WasmType, CompileError> {
        let elem_ty = self
            .resolve_expr_array_elem(&member.object)
            .ok_or_else(|| {
                CompileError::codegen(
                    "computed member assignment (arr[i] = val) only supported on Array<T>",
                )
            })?;
        let elem_size: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };

        // Evaluate array pointer and index
        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&member.object)?;
        self.push(Instruction::LocalSet(arr_local));

        let idx_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&member.expression)?;
        self.push(Instruction::LocalSet(idx_local));

        // Bounds check
        self.emit_array_bounds_check(arr_local, idx_local);

        // Compute element address: arr + 8 + index * elem_size
        let addr_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::I32Const(elem_size));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(addr_local));

        // Store value
        self.push(Instruction::LocalGet(addr_local));

        if operator == AssignmentOperator::Assign {
            self.emit_expr(value)?;
        } else {
            // Compound assignment: load current, compute, then store
            self.push(Instruction::LocalGet(addr_local));
            match elem_ty {
                WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 3,
                    memory_index: 0,
                })),
                WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                })),
                _ => unreachable!(),
            }
            self.emit_expr(value)?;
            let is_f64 = elem_ty == WasmType::F64;
            match operator {
                AssignmentOperator::Addition => self.push(if is_f64 {
                    Instruction::F64Add
                } else {
                    Instruction::I32Add
                }),
                AssignmentOperator::Subtraction => self.push(if is_f64 {
                    Instruction::F64Sub
                } else {
                    Instruction::I32Sub
                }),
                AssignmentOperator::Multiplication => self.push(if is_f64 {
                    Instruction::F64Mul
                } else {
                    Instruction::I32Mul
                }),
                AssignmentOperator::Division => self.push(if is_f64 {
                    Instruction::F64Div
                } else {
                    Instruction::I32DivS
                }),
                _ => {
                    return Err(CompileError::unsupported(
                        "compound array element assignment",
                    ));
                }
            }
        }

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

        Ok(WasmType::Void)
    }
    /// Emit optional chaining: `target?.hp` → `if target != 0 { target.hp } else { 0 }`
    pub(crate) fn emit_chain_expression(
        &mut self,
        chain: &ChainExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        match &chain.expression {
            ChainElement::StaticMemberExpression(member) => {
                self.emit_optional_member_access(member)
            }
            ChainElement::ComputedMemberExpression(member) => {
                // target?.[i] — optional computed access
                let elem_ty = self
                    .resolve_expr_array_elem(&member.object)
                    .ok_or_else(|| {
                        CompileError::codegen(
                            "optional computed access (?.[]) only supported on Array<T>",
                        )
                    })?;

                // Evaluate object, check for null
                let obj_local = self.alloc_local(WasmType::I32);
                self.emit_expr(&member.object)?;
                self.push(Instruction::LocalTee(obj_local));

                let result_vt = elem_ty.to_val_type().unwrap_or(wasm_encoder::ValType::I32);
                self.push(Instruction::If(wasm_encoder::BlockType::Result(result_vt)));

                // Non-null path: evaluate the full computed member access
                // Re-emit as a regular computed access but using the saved local
                self.push(Instruction::LocalGet(obj_local));
                let idx_local = self.alloc_local(WasmType::I32);
                self.emit_expr(&member.expression)?;
                self.push(Instruction::LocalSet(idx_local));

                let elem_size: i32 = match elem_ty {
                    WasmType::F64 => 8,
                    WasmType::I32 => 4,
                    _ => return Err(CompileError::type_err("invalid array element type")),
                };

                // Bounds check
                self.emit_array_bounds_check(obj_local, idx_local);

                // Load element
                self.push(Instruction::LocalGet(obj_local));
                self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
                self.push(Instruction::I32Add);
                self.push(Instruction::LocalGet(idx_local));
                self.push(Instruction::I32Const(elem_size));
                self.push(Instruction::I32Mul);
                self.push(Instruction::I32Add);
                match elem_ty {
                    WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg {
                        offset: 0,
                        align: 3,
                        memory_index: 0,
                    })),
                    WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                        offset: 0,
                        align: 2,
                        memory_index: 0,
                    })),
                    _ => unreachable!(),
                }

                self.push(Instruction::Else);
                // Null path: return zero
                match elem_ty {
                    WasmType::F64 => self.push(Instruction::F64Const(0.0f64)),
                    WasmType::I32 => self.push(Instruction::I32Const(0)),
                    _ => self.push(Instruction::I32Const(0)),
                }
                self.push(Instruction::End);

                Ok(elem_ty)
            }
            ChainElement::CallExpression(call) => {
                // Supported shape: `obj?.method(args...)` where callee is an
                // optional static-member expression on a class instance.
                // Bare `fn?.()` (optional call on a value) is not supported.
                let member = match &call.callee {
                    Expression::StaticMemberExpression(m) if m.optional => m,
                    _ => {
                        return Err(CompileError::unsupported(
                            "optional call must be `obj?.method(...)` on a class instance",
                        ));
                    }
                };
                self.emit_optional_method_call(member, call)
            }
            _ => Err(CompileError::unsupported(
                "unsupported chain expression type",
            )),
        }
    }

    /// Emit `obj?.method(args)` — null-safe method call.
    /// If obj is 0 (null), returns the zero value of the method's return type;
    /// otherwise dispatches to the method (static or vtable) without re-evaluating obj.
    pub(crate) fn emit_optional_method_call(
        &mut self,
        member: &StaticMemberExpression<'a>,
        call: &CallExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        // Resolve class + method to learn the return type (for the if-block's
        // result signature and for picking a zero value on the null branch).
        let class_name = self.resolve_expr_class(&member.object).map_err(|_| {
            CompileError::unsupported(
                "optional method call requires a statically-typed class receiver",
            )
        })?;
        let method_name = member.property.name.as_str();
        let ret_ty = {
            let mut found = None;
            let mut cur = class_name.clone();
            loop {
                let key = format!("{cur}.{method_name}");
                if let Some(&(_, ret)) = self.module_ctx.method_map.get(&key) {
                    found = Some(ret);
                    break;
                }
                match self
                    .module_ctx
                    .class_registry
                    .get(&cur)
                    .and_then(|l| l.parent.clone())
                {
                    Some(p) => cur = p,
                    None => break,
                }
            }
            found.ok_or_else(|| {
                CompileError::codegen(format!(
                    "class '{class_name}' has no method '{method_name}'"
                ))
            })?
        };

        // Evaluate the receiver once into a local.
        let recv_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&member.object)?;
        self.push(Instruction::LocalSet(recv_local));

        // if (recv != 0) { call method } else { zero }
        self.push(Instruction::LocalGet(recv_local));
        match ret_ty.to_val_type() {
            Some(vt) => {
                self.push(Instruction::If(wasm_encoder::BlockType::Result(vt)));
                let prev = self.method_receiver_override.replace(recv_local);
                let result = self.try_emit_method_call(call);
                self.method_receiver_override = prev;
                result?;
                self.push(Instruction::Else);
                match ret_ty {
                    WasmType::F64 => self.push(Instruction::F64Const(0.0f64)),
                    _ => self.push(Instruction::I32Const(0)),
                }
                self.push(Instruction::End);
            }
            None => {
                // Void return — wrap call in a plain if block, no else needed
                self.push(Instruction::If(wasm_encoder::BlockType::Empty));
                let prev = self.method_receiver_override.replace(recv_local);
                let result = self.try_emit_method_call(call);
                self.method_receiver_override = prev;
                result?;
                self.push(Instruction::End);
            }
        }
        Ok(ret_ty)
    }

    /// Emit `target?.field` — null-safe field access.
    /// If target is 0 (null), returns 0/0.0 instead of loading the field.
    pub(crate) fn emit_optional_member_access(
        &mut self,
        member: &StaticMemberExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        let field_name = member.property.name.as_str();

        // Check if this is an array .length access
        if let Some(_elem_ty) = self.resolve_expr_array_elem(&member.object)
            && field_name == "length"
        {
            // target?.length
            let obj_local = self.alloc_local(WasmType::I32);
            self.emit_expr(&member.object)?;
            self.push(Instruction::LocalTee(obj_local));
            self.push(Instruction::If(wasm_encoder::BlockType::Result(
                wasm_encoder::ValType::I32,
            )));
            self.push(Instruction::LocalGet(obj_local));
            self.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
            self.push(Instruction::Else);
            self.push(Instruction::I32Const(0));
            self.push(Instruction::End);
            return Ok(WasmType::I32);
        }

        // Resolve class and field info
        let class_name = self.resolve_expr_class(&member.object)?;
        let layout = self
            .module_ctx
            .class_registry
            .get(&class_name)
            .ok_or_else(|| CompileError::codegen(format!("unknown class '{class_name}'")))?;
        let &(offset, field_ty) = layout.field_map.get(field_name).ok_or_else(|| {
            CompileError::codegen(format!("class '{class_name}' has no field '{field_name}'"))
        })?;

        let result_vt = field_ty.to_val_type().unwrap_or(wasm_encoder::ValType::I32);

        // Evaluate object, check for null (0)
        let obj_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&member.object)?;
        self.push(Instruction::LocalTee(obj_local));

        // if (obj != 0) { load field } else { zero }
        self.push(Instruction::If(wasm_encoder::BlockType::Result(result_vt)));
        self.push(Instruction::LocalGet(obj_local));
        match field_ty {
            WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg {
                offset: offset as u64,
                align: 3,
                memory_index: 0,
            })),
            WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: offset as u64,
                align: 2,
                memory_index: 0,
            })),
            _ => return Err(CompileError::codegen("void field in optional access")),
        }
        self.push(Instruction::Else);
        match field_ty {
            WasmType::F64 => self.push(Instruction::F64Const(0.0f64)),
            WasmType::I32 => self.push(Instruction::I32Const(0)),
            _ => self.push(Instruction::I32Const(0)),
        }
        self.push(Instruction::End);

        Ok(field_ty)
    }
}
