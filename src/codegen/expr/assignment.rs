use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

impl<'a> FuncContext<'a> {
    pub(crate) fn emit_assignment(
        &mut self,
        assign: &AssignmentExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        // Handle obj.field = val or this.field = val
        if let AssignmentTarget::StaticMemberExpression(member) = &assign.left {
            return self.emit_member_assign(member, &assign.right, assign.operator);
        }

        // Handle arr[i] = val
        if let AssignmentTarget::ComputedMemberExpression(member) = &assign.left {
            return self.emit_computed_member_assign(member, &assign.right, assign.operator);
        }

        let target_name = match &assign.left {
            AssignmentTarget::AssignmentTargetIdentifier(ident) => ident.name.as_str(),
            _ => return Err(CompileError::unsupported("complex assignment target")),
        };

        if self.const_locals.contains(target_name) {
            return Err(CompileError::type_err(format!(
                "cannot assign to const variable '{target_name}'"
            )));
        }

        // If not a local, try mutable globals
        if !self.locals.contains_key(target_name)
            && let Some(&(g_idx, g_ty)) = self.module_ctx.globals.get(target_name)
        {
            if !self.module_ctx.mutable_globals.contains(target_name) {
                return Err(CompileError::type_err(format!(
                    "cannot assign to const global '{target_name}'"
                )));
            }
            return self.emit_global_assign(
                target_name,
                g_idx,
                g_ty,
                &assign.right,
                assign.operator,
            );
        }

        let &(idx, _local_ty) = self
            .locals
            .get(target_name)
            .ok_or_else(|| CompileError::codegen(format!("undefined variable '{target_name}'")))?;
        let is_boxed = self.boxed_var_types.contains_key(target_name);
        let ty = if is_boxed {
            *self.boxed_var_types.get(target_name).unwrap()
        } else {
            _local_ty
        };

        // Helper closure-like pattern: emit the value to store, then write it
        match assign.operator {
            AssignmentOperator::Assign => {
                if is_boxed {
                    self.push(Instruction::LocalGet(idx)); // ptr
                }
                let expr_ty = match &assign.right {
                    Expression::ObjectExpression(obj) => {
                        let expected = self.local_class_types.get(target_name).cloned();
                        let (t, _) = self.emit_object_literal(obj, expected.as_deref())?;
                        t
                    }
                    Expression::ArrayExpression(arr) => {
                        let target_class = self.local_class_types.get(target_name).cloned();
                        if let Some(t) = target_class.as_deref()
                            && self.is_tuple_shape(t)
                        {
                            let (et, _) = self.emit_tuple_literal(arr, t)?;
                            et
                        } else {
                            self.emit_expr_coerced(
                                &assign.right,
                                target_class.as_deref(),
                            )?
                        }
                    }
                    _ => {
                        let target_class = self.local_class_types.get(target_name).cloned();
                        self.emit_expr_coerced(&assign.right, target_class.as_deref())?
                    }
                };
                if expr_ty != ty {
                    return Err(CompileError::type_err(format!(
                        "cannot assign {expr_ty:?} to {ty:?} variable '{target_name}'"
                    )));
                }
                if is_boxed {
                    self.emit_boxed_store(ty);
                } else {
                    self.push(Instruction::LocalSet(idx));
                }
                Ok(WasmType::Void)
            }
            AssignmentOperator::Addition
            | AssignmentOperator::Subtraction
            | AssignmentOperator::Multiplication
            | AssignmentOperator::Division => {
                if is_boxed {
                    self.push(Instruction::LocalGet(idx)); // ptr (for the store)
                    // Load current value
                    self.push(Instruction::LocalGet(idx)); // ptr (for the load)
                    self.emit_boxed_load(ty);
                } else {
                    self.push(Instruction::LocalGet(idx));
                }
                self.emit_expr(&assign.right)?;
                let op = match (assign.operator, ty) {
                    (AssignmentOperator::Addition, WasmType::F64) => Instruction::F64Add,
                    (AssignmentOperator::Addition, _) => Instruction::I32Add,
                    (AssignmentOperator::Subtraction, WasmType::F64) => Instruction::F64Sub,
                    (AssignmentOperator::Subtraction, _) => Instruction::I32Sub,
                    (AssignmentOperator::Multiplication, WasmType::F64) => Instruction::F64Mul,
                    (AssignmentOperator::Multiplication, _) => Instruction::I32Mul,
                    (AssignmentOperator::Division, WasmType::F64) => Instruction::F64Div,
                    (AssignmentOperator::Division, _) => Instruction::I32DivS,
                    _ => unreachable!(),
                };
                self.push(op);
                if is_boxed {
                    self.emit_boxed_store(ty);
                } else {
                    self.push(Instruction::LocalSet(idx));
                }
                Ok(WasmType::Void)
            }
            _ => Err(CompileError::unsupported(format!(
                "assignment operator {:?}",
                assign.operator
            ))),
        }
    }

    /// Emit assignment to a mutable WASM global.
    pub(crate) fn emit_global_assign(
        &mut self,
        name: &str,
        g_idx: u32,
        ty: WasmType,
        rhs: &Expression<'a>,
        op: AssignmentOperator,
    ) -> Result<WasmType, CompileError> {
        match op {
            AssignmentOperator::Assign => {
                let expr_ty = self.emit_expr(rhs)?;
                if expr_ty != ty {
                    return Err(CompileError::type_err(format!(
                        "cannot assign {expr_ty:?} to {ty:?} global '{name}'"
                    )));
                }
                self.push(Instruction::GlobalSet(g_idx));
                Ok(WasmType::Void)
            }
            AssignmentOperator::Addition
            | AssignmentOperator::Subtraction
            | AssignmentOperator::Multiplication
            | AssignmentOperator::Division => {
                self.push(Instruction::GlobalGet(g_idx));
                self.emit_expr(rhs)?;
                let instr = match (op, ty) {
                    (AssignmentOperator::Addition, WasmType::F64) => Instruction::F64Add,
                    (AssignmentOperator::Addition, _) => Instruction::I32Add,
                    (AssignmentOperator::Subtraction, WasmType::F64) => Instruction::F64Sub,
                    (AssignmentOperator::Subtraction, _) => Instruction::I32Sub,
                    (AssignmentOperator::Multiplication, WasmType::F64) => Instruction::F64Mul,
                    (AssignmentOperator::Multiplication, _) => Instruction::I32Mul,
                    (AssignmentOperator::Division, WasmType::F64) => Instruction::F64Div,
                    (AssignmentOperator::Division, _) => Instruction::I32DivS,
                    _ => unreachable!(),
                };
                self.push(instr);
                self.push(Instruction::GlobalSet(g_idx));
                Ok(WasmType::Void)
            }
            _ => Err(CompileError::unsupported(format!(
                "assignment operator {op:?} on global"
            ))),
        }
    }

    pub(crate) fn emit_update(
        &mut self,
        update: &UpdateExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        let name = match &update.argument {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(ident) => ident.name.as_str(),
            _ => return Err(CompileError::unsupported("complex update target")),
        };

        // Check const
        if self.const_locals.contains(name) {
            return Err(CompileError::type_err(format!(
                "cannot modify const variable '{name}'"
            )));
        }

        // Handle mutable globals
        if !self.locals.contains_key(name)
            && let Some(&(g_idx, g_ty)) = self.module_ctx.globals.get(name)
        {
            if !self.module_ctx.mutable_globals.contains(name) {
                return Err(CompileError::type_err(format!(
                    "cannot modify const global '{name}'"
                )));
            }
            if g_ty != WasmType::I32 {
                return Err(CompileError::type_err("++/-- only supported on i32"));
            }
            let delta = match update.operator {
                UpdateOperator::Increment => Instruction::I32Add,
                UpdateOperator::Decrement => Instruction::I32Sub,
            };
            if update.prefix {
                // ++g: compute new, store, leave new on stack
                self.push(Instruction::GlobalGet(g_idx));
                self.push(Instruction::I32Const(1));
                self.push(delta);
                let tmp = self.alloc_local(WasmType::I32);
                self.push(Instruction::LocalTee(tmp));
                self.push(Instruction::GlobalSet(g_idx));
                self.push(Instruction::LocalGet(tmp));
            } else {
                // g++: leave old on stack, store new
                let old = self.alloc_local(WasmType::I32);
                self.push(Instruction::GlobalGet(g_idx));
                self.push(Instruction::LocalTee(old));
                self.push(Instruction::I32Const(1));
                self.push(delta);
                self.push(Instruction::GlobalSet(g_idx));
                self.push(Instruction::LocalGet(old));
            }
            return Ok(WasmType::I32);
        }

        let &(idx, _local_ty) = self
            .locals
            .get(name)
            .ok_or_else(|| CompileError::codegen(format!("undefined variable '{name}'")))?;
        let is_boxed = self.boxed_var_types.contains_key(name);
        let ty = if is_boxed {
            *self.boxed_var_types.get(name).unwrap()
        } else {
            _local_ty
        };

        if ty != WasmType::I32 {
            return Err(CompileError::type_err("++/-- only supported on i32"));
        }

        if is_boxed {
            if update.prefix {
                // ++i: ptr, load, +1, store; then load again for result
                self.push(Instruction::LocalGet(idx)); // ptr for store
                self.push(Instruction::LocalGet(idx)); // ptr for load
                self.emit_boxed_load(WasmType::I32);
                self.push(Instruction::I32Const(1));
                match update.operator {
                    UpdateOperator::Increment => self.push(Instruction::I32Add),
                    UpdateOperator::Decrement => self.push(Instruction::I32Sub),
                }
                // Stack: [ptr, new_value]. Duplicate new_value before store
                let tmp = self.alloc_local(WasmType::I32);
                self.push(Instruction::LocalTee(tmp));
                self.emit_boxed_store(WasmType::I32);
                self.push(Instruction::LocalGet(tmp));
            } else {
                // i++: load old value, then store incremented
                let old_val = self.alloc_local(WasmType::I32);
                self.push(Instruction::LocalGet(idx));
                self.emit_boxed_load(WasmType::I32);
                self.push(Instruction::LocalSet(old_val));
                // Store new value
                self.push(Instruction::LocalGet(idx)); // ptr
                self.push(Instruction::LocalGet(old_val));
                self.push(Instruction::I32Const(1));
                match update.operator {
                    UpdateOperator::Increment => self.push(Instruction::I32Add),
                    UpdateOperator::Decrement => self.push(Instruction::I32Sub),
                }
                self.emit_boxed_store(WasmType::I32);
                // Return old value
                self.push(Instruction::LocalGet(old_val));
            }
            Ok(WasmType::I32)
        } else if update.prefix {
            // ++i: increment first, return new value
            self.push(Instruction::LocalGet(idx));
            self.push(Instruction::I32Const(1));
            match update.operator {
                UpdateOperator::Increment => self.push(Instruction::I32Add),
                UpdateOperator::Decrement => self.push(Instruction::I32Sub),
            }
            self.push(Instruction::LocalTee(idx)); // store and keep on stack
            Ok(WasmType::I32)
        } else {
            // i++: return old value, then increment
            self.push(Instruction::LocalGet(idx)); // old value stays on stack
            self.push(Instruction::LocalGet(idx));
            self.push(Instruction::I32Const(1));
            match update.operator {
                UpdateOperator::Increment => self.push(Instruction::I32Add),
                UpdateOperator::Decrement => self.push(Instruction::I32Sub),
            }
            self.push(Instruction::LocalSet(idx)); // store new value
            Ok(WasmType::I32)
        }
    }
}
