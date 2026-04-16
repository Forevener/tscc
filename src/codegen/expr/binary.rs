use oxc_ast::ast::*;
use wasm_encoder::{Instruction, MemArg, ValType};

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

impl<'a> FuncContext<'a> {
    pub(crate) fn emit_identifier(
        &mut self,
        ident: &IdentifierReference,
    ) -> Result<WasmType, CompileError> {
        let name = ident.name.as_str();

        // `undefined` is accepted as an alias for `null` in typed context:
        // both lower to the i32 sentinel 0 (null pointer / zero i32). No
        // distinct runtime representation exists for undefined in our model.
        if name == "undefined" {
            self.push(Instruction::I32Const(0));
            return Ok(WasmType::I32);
        }

        // Global numeric constants: `NaN` and `Infinity` are ECMAScript globals
        // that evaluate to f64 literals. Shadowing is impossible because we
        // reject them as variable names elsewhere (they are reserved words in
        // strict mode). Negative infinity is expressed as `-Infinity` via the
        // unary minus path.
        if name == "NaN" {
            self.push(Instruction::F64Const(f64::NAN));
            return Ok(WasmType::F64);
        }
        if name == "Infinity" {
            self.push(Instruction::F64Const(f64::INFINITY));
            return Ok(WasmType::F64);
        }

        // Check boxed variables first — load through pointer
        if let Some(&actual_ty) = self.boxed_var_types.get(name) {
            let &(ptr_idx, _) = self.locals.get(name).unwrap();
            self.push(Instruction::LocalGet(ptr_idx));
            match actual_ty {
                WasmType::F64 => self.push(Instruction::F64Load(MemArg {
                    offset: 0,
                    align: 3,
                    memory_index: 0,
                })),
                _ => self.push(Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                })),
            }
            return Ok(actual_ty);
        }

        // Check locals first
        if let Some(&(idx, ty)) = self.locals.get(name) {
            self.push(Instruction::LocalGet(idx));
            return Ok(ty);
        }

        // Check globals
        if let Some(&(idx, ty)) = self.module_ctx.globals.get(name) {
            self.push(Instruction::GlobalGet(idx));
            return Ok(ty);
        }

        // true/false handled by BooleanLiteral
        Err(self.locate(
            CompileError::codegen(format!("undefined variable '{name}'")),
            ident.span.start,
        ))
    }

    pub(crate) fn emit_binary(
        &mut self,
        bin: &BinaryExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        // Check for string operations BEFORE emitting operands
        let left_is_string = self.resolve_expr_is_string(&bin.left);
        let right_is_string = self.resolve_expr_is_string(&bin.right);
        if left_is_string || right_is_string {
            return self.emit_string_binary(bin);
        }

        let left_ty = self.emit_expr(&bin.left)?;
        let right_ty = self.emit_expr(&bin.right)?;

        if left_ty != right_ty {
            return Err(self.locate(
                CompileError::type_err(format!(
                    "type mismatch in binary expression: {left_ty:?} vs {right_ty:?}"
                )),
                bin.span.start,
            ));
        }

        let ty = left_ty;
        let is_f64 = ty == WasmType::F64;

        match bin.operator {
            BinaryOperator::Addition => {
                self.push(if is_f64 {
                    Instruction::F64Add
                } else {
                    Instruction::I32Add
                });
                Ok(ty)
            }
            BinaryOperator::Subtraction => {
                self.push(if is_f64 {
                    Instruction::F64Sub
                } else {
                    Instruction::I32Sub
                });
                Ok(ty)
            }
            BinaryOperator::Multiplication => {
                self.push(if is_f64 {
                    Instruction::F64Mul
                } else {
                    Instruction::I32Mul
                });
                Ok(ty)
            }
            BinaryOperator::Division => {
                self.push(if is_f64 {
                    Instruction::F64Div
                } else {
                    Instruction::I32DivS
                });
                Ok(ty)
            }
            BinaryOperator::Remainder => {
                if is_f64 {
                    return Err(CompileError::unsupported(
                        "f64 remainder (%) not supported in WASM",
                    ));
                }
                self.push(Instruction::I32RemS);
                Ok(WasmType::I32)
            }
            BinaryOperator::LessThan => {
                self.push(if is_f64 {
                    Instruction::F64Lt
                } else {
                    Instruction::I32LtS
                });
                Ok(WasmType::I32)
            }
            BinaryOperator::LessEqualThan => {
                self.push(if is_f64 {
                    Instruction::F64Le
                } else {
                    Instruction::I32LeS
                });
                Ok(WasmType::I32)
            }
            BinaryOperator::GreaterThan => {
                self.push(if is_f64 {
                    Instruction::F64Gt
                } else {
                    Instruction::I32GtS
                });
                Ok(WasmType::I32)
            }
            BinaryOperator::GreaterEqualThan => {
                self.push(if is_f64 {
                    Instruction::F64Ge
                } else {
                    Instruction::I32GeS
                });
                Ok(WasmType::I32)
            }
            BinaryOperator::StrictEquality | BinaryOperator::Equality => {
                self.push(if is_f64 {
                    Instruction::F64Eq
                } else {
                    Instruction::I32Eq
                });
                Ok(WasmType::I32)
            }
            BinaryOperator::StrictInequality | BinaryOperator::Inequality => {
                self.push(if is_f64 {
                    Instruction::F64Ne
                } else {
                    Instruction::I32Ne
                });
                Ok(WasmType::I32)
            }
            BinaryOperator::BitwiseAnd => {
                if is_f64 {
                    return Err(CompileError::type_err("bitwise & on f64"));
                }
                self.push(Instruction::I32And);
                Ok(WasmType::I32)
            }
            BinaryOperator::BitwiseOR => {
                if is_f64 {
                    return Err(CompileError::type_err("bitwise | on f64"));
                }
                self.push(Instruction::I32Or);
                Ok(WasmType::I32)
            }
            BinaryOperator::ShiftLeft => {
                if is_f64 {
                    return Err(CompileError::type_err("shift on f64"));
                }
                self.push(Instruction::I32Shl);
                Ok(WasmType::I32)
            }
            BinaryOperator::ShiftRight => {
                if is_f64 {
                    return Err(CompileError::type_err("shift on f64"));
                }
                self.push(Instruction::I32ShrS);
                Ok(WasmType::I32)
            }
            BinaryOperator::ShiftRightZeroFill => {
                if is_f64 {
                    return Err(CompileError::type_err("shift on f64"));
                }
                self.push(Instruction::I32ShrU);
                Ok(WasmType::I32)
            }
            _ => Err(CompileError::unsupported(format!(
                "binary operator {:?}",
                bin.operator
            ))),
        }
    }

    pub(crate) fn emit_logical(
        &mut self,
        log: &LogicalExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        match log.operator {
            LogicalOperator::And => {
                // Short-circuit: if left is 0, result is 0; else evaluate right
                let left_ty = self.emit_expr(&log.left)?;
                // Duplicate the value by using a local
                let tmp = self.alloc_local(WasmType::I32);
                self.push(Instruction::LocalTee(tmp));
                self.push(Instruction::If(wasm_encoder::BlockType::Result(
                    ValType::I32,
                )));
                let right_ty = self.emit_expr(&log.right)?;
                if right_ty != WasmType::I32 {
                    return Err(CompileError::type_err(
                        "logical && requires i32/bool operands",
                    ));
                }
                self.push(Instruction::Else);
                self.push(Instruction::I32Const(0));
                self.push(Instruction::End);
                let _ = left_ty;
                Ok(WasmType::I32)
            }
            LogicalOperator::Or => {
                // Short-circuit: if left is nonzero, result is left; else evaluate right
                let left_ty = self.emit_expr(&log.left)?;
                let tmp = self.alloc_local(WasmType::I32);
                self.push(Instruction::LocalTee(tmp));
                self.push(Instruction::If(wasm_encoder::BlockType::Result(
                    ValType::I32,
                )));
                self.push(Instruction::LocalGet(tmp));
                self.push(Instruction::Else);
                let right_ty = self.emit_expr(&log.right)?;
                if right_ty != WasmType::I32 {
                    return Err(CompileError::type_err(
                        "logical || requires i32/bool operands",
                    ));
                }
                self.push(Instruction::End);
                let _ = left_ty;
                Ok(WasmType::I32)
            }
            LogicalOperator::Coalesce => {
                // val ?? default → if val != 0 then val else default
                // In our type system, null = 0 for i32 pointers
                let left_ty = self.emit_expr(&log.left)?;
                let tmp = self.alloc_local(WasmType::I32);
                self.push(Instruction::LocalTee(tmp));
                self.push(Instruction::If(wasm_encoder::BlockType::Result(
                    ValType::I32,
                )));
                self.push(Instruction::LocalGet(tmp));
                self.push(Instruction::Else);
                let right_ty = self.emit_expr(&log.right)?;
                if right_ty != WasmType::I32 {
                    return Err(CompileError::type_err(
                        "nullish coalescing (??) requires i32 operands",
                    ));
                }
                self.push(Instruction::End);
                let _ = left_ty;
                Ok(WasmType::I32)
            }
        }
    }

    pub(crate) fn emit_unary(
        &mut self,
        un: &UnaryExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        let ty = self.emit_expr(&un.argument)?;
        match un.operator {
            UnaryOperator::UnaryNegation => {
                match ty {
                    WasmType::I32 => {
                        // 0 - x
                        // We need to rearrange: push 0 first, then x, then sub
                        // But x is already on stack. Use a temp local.
                        let tmp = self.alloc_local(WasmType::I32);
                        self.push(Instruction::LocalSet(tmp));
                        self.push(Instruction::I32Const(0));
                        self.push(Instruction::LocalGet(tmp));
                        self.push(Instruction::I32Sub);
                        Ok(WasmType::I32)
                    }
                    WasmType::F64 => {
                        self.push(Instruction::F64Neg);
                        Ok(WasmType::F64)
                    }
                    _ => Err(CompileError::type_err("cannot negate void")),
                }
            }
            UnaryOperator::LogicalNot => {
                self.push(Instruction::I32Eqz);
                Ok(WasmType::I32)
            }
            UnaryOperator::BitwiseNot => {
                if ty != WasmType::I32 {
                    return Err(CompileError::type_err("bitwise ~ requires i32"));
                }
                self.push(Instruction::I32Const(-1));
                self.push(Instruction::I32Xor);
                Ok(WasmType::I32)
            }
            _ => Err(CompileError::unsupported(format!(
                "unary operator {:?}",
                un.operator
            ))),
        }
    }
}
