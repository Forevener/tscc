use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::classes::MethodSig;
use crate::codegen::func::{FuncContext, Refinement, peel_parens};
use crate::codegen::unions::UnionMember;
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
            // `Symbol.iterator` (and any future well-known Symbol member) is a
            // compile-time-only token — never a runtime value. It is recognized
            // only as a class-method computed key (`classes::property_key_name`).
            // Reject `const x = Symbol.iterator;` etc. with a precise hint.
            if obj_ident.name.as_str() == "Symbol" {
                return Err(self.locate(
                    CompileError::codegen(format!(
                        "'Symbol.{}' is a compile-time-only token, not a runtime value; \
                         use it only as a class-method computed key, e.g. `[Symbol.iterator]() {{ ... }}`",
                        member.property.name.as_str()
                    )),
                    member.span.start,
                ));
            }

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
            // Typed-array statics on the type identifier itself, e.g.
            // `Int32Array.BYTES_PER_ELEMENT`. Sub-phase 2 only ships
            // `BYTES_PER_ELEMENT`; future statics (`name`, etc.) slot
            // into this same dispatcher.
            if let Some(desc) =
                crate::codegen::typed_arrays::descriptor_for(obj_ident.name.as_str())
                && let Some(ty) = self.try_emit_typed_array_static_property(
                    desc,
                    member.property.name.as_str(),
                )?
            {
                return Ok(ty);
            }
        }

        let field_name = member.property.name.as_str();

        // Check if this is a string property access (str.length)
        if self.resolve_expr_is_string(&member.object) {
            return self.emit_string_property(member, field_name);
        }

        // Typed-array instance properties (length / byteLength). Routed
        // before the array path because typed-array locals don't appear in
        // `local_array_elem_types`, so the array path would error.
        if let Some(desc) = self.resolve_expr_typed_array(&member.object) {
            return self.emit_typed_array_property(desc, member, field_name);
        }

        // Check if this is an array property access (arr.length)
        if let Some(_elem_ty) = self.resolve_expr_array_elem(&member.object) {
            return self.emit_array_property(member, field_name);
        }

        // Determine the class of the object
        let class_name = self.resolve_expr_class(&member.object)?;

        // Union receiver (shared-field rule): emit a normal field load
        // when every (possibly refined) variant declares the field at
        // the same offset with the same WasmType. Variant-specific
        // fields require narrowing — the error message points the user
        // at the guard.
        if let Some(union) = self.module_ctx.union_registry.get_by_name(&class_name) {
            // Sub-phase 1.5.1: if the receiver is a refined identifier,
            // restrict the shared-field check to the refined member set.
            // `Never` is unreachable; reject member access there.
            let receiver_refinement =
                if let Expression::Identifier(ident) = peel_parens(&member.object) {
                    self.current_refinement_of(ident.name.as_str()).cloned()
                } else {
                    None
                };
            let (effective_members, refined): (Vec<UnionMember>, bool) =
                match receiver_refinement {
                    Some(Refinement::Never) => {
                        return Err(CompileError::type_err(format!(
                            "value of union '{class_name}' is unreachable here — every \
                             variant has been ruled out by prior narrowing"
                        )));
                    }
                    Some(Refinement::Subunion(members)) => (members, true),
                    // `Class(_)` is handled by `resolve_expr_class` above
                    // returning the refined class name, which takes the
                    // class-registry path further down — not this union arm.
                    Some(Refinement::Class(_)) | None => (union.members.clone(), false),
                };
            let (offset, ty) =
                resolve_shared_field_in_members(self, &effective_members, field_name)
                    .ok_or_else(|| {
                        let suffix = if refined {
                            " (after refinement)".to_string()
                        } else {
                            String::new()
                        };
                        CompileError::type_err(format!(
                            "field '{field_name}' is not shared across all variants of \
                             union '{class_name}'{suffix} — narrow the value with \
                             `if (x.kind === '...')` before accessing variant-specific \
                             fields"
                        ))
                    })?;
            self.emit_expr(&member.object)?;
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
                _ => return Err(CompileError::codegen("void field access")),
            }
            return Ok(ty);
        }

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
            let expected = layout.field_class_types.get(field_name).cloned();
            match value {
                Expression::ObjectExpression(obj) => {
                    self.emit_object_literal(obj, expected.as_deref())?;
                }
                Expression::ArrayExpression(arr)
                    if expected
                        .as_deref()
                        .is_some_and(|e| self.is_tuple_shape(e)) =>
                {
                    self.emit_tuple_literal(arr, expected.as_deref().unwrap())?;
                }
                _ => {
                    self.emit_expr(value)?;
                }
            }
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

        // Tuple indexed access: `t[N]` with a literal N → load field `_N`.
        // Non-literal indices on tuples are rejected — slots have per-position
        // types, so a dynamic index can't be checked at compile time.
        if let Some(ty) = self.try_emit_tuple_index(member)? {
            return Ok(ty);
        }

        // Typed-array element read: `ta[i]` where ta is Int32Array /
        // Float64Array / Uint8Array. Routed before the regular Array<T>
        // path because typed arrays are class-typed (not in
        // `local_array_elem_types`), so the Array<T> path would error.
        if let Some(desc) = self.resolve_expr_typed_array(&member.object) {
            return self.emit_typed_array_indexed_read(desc, member);
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
    /// If `t[N]` is a tuple access, emit it as a field load on `_N` and
    /// return the slot type. Returns `Ok(None)` when the receiver is not a
    /// tuple shape (so the caller can fall through to the array path).
    /// Returns a clear error when the receiver is a tuple but the index is
    /// non-literal or out of bounds.
    fn try_emit_tuple_index(
        &mut self,
        member: &ComputedMemberExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        let Ok(class_name) = self.resolve_expr_class(&member.object) else {
            return Ok(None);
        };
        let shape_idx = match self.module_ctx.shape_registry.by_name.get(&class_name) {
            Some(&i) => i,
            None => return Ok(None),
        };
        if !self.module_ctx.shape_registry.shapes[shape_idx].is_tuple {
            return Ok(None);
        }
        let arity = self.module_ctx.shape_registry.shapes[shape_idx].fields.len();
        let index = match tuple_literal_index(&member.expression) {
            Some(n) => n,
            None => {
                return Err(self.locate(
                    CompileError::type_err(format!(
                        "tuple `{class_name}` requires a literal numeric index; dynamic `t[i]` is \
                         not supported — use `Array<T>` if slots share a type"
                    )),
                    member.span.start,
                ));
            }
        };
        if index >= arity {
            return Err(self.locate(
                CompileError::type_err(format!(
                    "tuple index {index} out of bounds for `{class_name}` (arity {arity})"
                )),
                member.span.start,
            ));
        }
        let layout = self
            .module_ctx
            .class_registry
            .get(&class_name)
            .ok_or_else(|| CompileError::codegen(format!("tuple '{class_name}' not registered")))?
            .clone();
        let (_, offset, slot_ty) = layout.fields[index].clone();

        self.emit_expr(&member.object)?;
        match slot_ty {
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
            _ => return Err(CompileError::codegen("void tuple slot")),
        }
        Ok(Some(slot_ty))
    }

    pub(crate) fn emit_computed_member_assign(
        &mut self,
        member: &ComputedMemberExpression<'a>,
        value: &Expression<'a>,
        operator: AssignmentOperator,
    ) -> Result<WasmType, CompileError> {
        // Typed-array element write: `ta[i] = v` / `ta[i] += v` etc.
        if let Some(desc) = self.resolve_expr_typed_array(&member.object) {
            return self.emit_typed_array_indexed_write(desc, member, value, operator);
        }

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

/// Extract a non-negative integer literal from a tuple-index expression.
/// Returns `None` for anything dynamic — the caller turns that into a clear
/// "tuple requires literal index" error.
fn tuple_literal_index(expr: &Expression<'_>) -> Option<usize> {
    match expr {
        Expression::ParenthesizedExpression(p) => tuple_literal_index(&p.expression),
        Expression::NumericLiteral(lit) => {
            let v = lit.value;
            if v.fract() != 0.0 || v < 0.0 {
                return None;
            }
            // `v.fract() == 0.0` guarantees `v` fits cleanly in usize for any
            // plausible arity; upper bound enforced by the arity check.
            Some(v as usize)
        }
        _ => None,
    }
}

/// Shared-field rule over an explicit member list. Returns the
/// `(offset, WasmType)` of `field_name` only if every shape member
/// declares it at the same offset with the same type. Literal members
/// short-circuit to `None` (they have no fields). Used by
/// `emit_member_access` for both un-narrowed unions (full `members`)
/// and Sub-phase 1.5.1's refined sub-unions (`Refinement::Subunion`).
pub(crate) fn resolve_shared_field_in_members(
    ctx: &FuncContext<'_>,
    members: &[UnionMember],
    field_name: &str,
) -> Option<(u32, WasmType)> {
    let mut resolved: Option<(u32, WasmType)> = None;
    for m in members {
        match m {
            UnionMember::Shape(sn) => {
                let layout = ctx.module_ctx.class_registry.get(sn)?;
                let &(off, ty) = layout.field_map.get(field_name)?;
                match resolved {
                    None => resolved = Some((off, ty)),
                    Some((off0, ty0)) => {
                        if off0 != off || ty0 != ty {
                            return None;
                        }
                    }
                }
            }
            UnionMember::Literal(_) => return None,
        }
    }
    resolved
}

/// Why a shared-method lookup over a union failed. Distinguishes the
/// shapes a caller will want to surface as different diagnostics.
#[derive(Debug)]
pub(crate) enum SharedMethodIssue {
    /// A class variant lacks the method entirely (or there are no
    /// members at all). Carries the variant name so the caller can name
    /// it. Also produced for literal members.
    MissingOnVariant(String),
    /// A shape member is part of the union — shapes have no methods at
    /// all, so the rule can never be satisfied with this variant in the
    /// member set. Distinguished from `MissingOnVariant` so the caller
    /// can produce a more actionable diagnostic ("narrow with
    /// `instanceof <Class>` first") rather than the generic
    /// "Variant lacks the method" message.
    ShapeHasNoMethods(String),
    /// All variants declare the method but at differing vtable slots.
    /// Implies independent declarations without a common ancestor owning
    /// the method.
    SlotMismatch,
    /// Same slot across variants but parameter or return WasmTypes
    /// differ — invalid for a shared `call_indirect` site.
    SignatureMismatch,
}

/// Shared-method rule over an explicit member list. Mirrors
/// [`resolve_shared_field_in_members`] one-to-one but on vtable slots
/// instead of field offsets. Returns the slot index plus a representative
/// `MethodSig` (for synthesizing the `call_indirect` type and threading
/// `param_classes` hints into object-literal arguments). Each variant
/// must declare the method at the same slot with matching parameter and
/// return WasmTypes. Literal and method-less shape members are rejected
/// via `MissingOnVariant`.
pub(crate) fn resolve_shared_method_in_members(
    ctx: &FuncContext<'_>,
    members: &[UnionMember],
    method_name: &str,
) -> Result<(usize, MethodSig), SharedMethodIssue> {
    let mut resolved: Option<(usize, MethodSig)> = None;
    for m in members {
        match m {
            UnionMember::Shape(name) => {
                // Shape vs class: shapes register a synthetic
                // `ClassLayout` with empty `methods`/`vtable_method_map`,
                // so a generic "method not found" check would fire here.
                // Surface the distinction so the caller can steer the
                // user toward `instanceof <Class>` rather than implying
                // a method declaration would help.
                if ctx.module_ctx.shape_registry.by_name.contains_key(name) {
                    return Err(SharedMethodIssue::ShapeHasNoMethods(name.clone()));
                }
                let layout = ctx
                    .module_ctx
                    .class_registry
                    .get(name)
                    .ok_or_else(|| SharedMethodIssue::MissingOnVariant(name.clone()))?;
                let &slot = layout
                    .vtable_method_map
                    .get(method_name)
                    .ok_or_else(|| SharedMethodIssue::MissingOnVariant(name.clone()))?;
                let sig = layout
                    .methods
                    .get(method_name)
                    .ok_or_else(|| SharedMethodIssue::MissingOnVariant(name.clone()))?
                    .clone();
                match &resolved {
                    None => resolved = Some((slot, sig)),
                    Some((slot0, sig0)) => {
                        if *slot0 != slot {
                            return Err(SharedMethodIssue::SlotMismatch);
                        }
                        let p0: Vec<WasmType> =
                            sig0.params.iter().map(|(_, t)| *t).collect();
                        let p1: Vec<WasmType> = sig.params.iter().map(|(_, t)| *t).collect();
                        if p0 != p1 || sig0.return_type != sig.return_type {
                            return Err(SharedMethodIssue::SignatureMismatch);
                        }
                    }
                }
            }
            UnionMember::Literal(lit) => {
                return Err(SharedMethodIssue::MissingOnVariant(lit.canonical()));
            }
        }
    }
    resolved.ok_or(SharedMethodIssue::MissingOnVariant(String::from("(empty)")))
}
