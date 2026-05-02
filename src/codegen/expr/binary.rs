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

        // `Symbol` is a compile-time-only token: tscc has no runtime `Symbol`
        // type. The only recognized form is `[Symbol.iterator]()` as a class
        // method computed key (handled in `classes::property_key_name`).
        // Catch any other use early so the user gets a precise hint rather
        // than the generic "undefined variable" error below.
        if name == "Symbol" {
            return Err(self.locate(
                CompileError::codegen(
                    "'Symbol' is a compile-time-only token; only `[Symbol.iterator]() {...}` as a class method key is recognized",
                ),
                ident.span.start,
            ));
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
        // `instanceof` short-circuits operand emission: the right operand is
        // a *type* identifier (class name), not a value, so feeding it to
        // `emit_expr` would fail. Phase 2 sub-phase 2.
        if bin.operator == BinaryOperator::Instanceof {
            return self.emit_instanceof(&bin.left, &bin.right, bin.span.start);
        }

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

    /// Lower `left instanceof RightClass` to a vtable-pointer comparison.
    ///
    /// The runtime test loads the vtable pointer from offset 0 of `left`
    /// (every polymorphic class instance carries one — see `expr/class.rs`)
    /// and compares it against `RightClass`'s `vtable_offset`. When
    /// `RightClass` has descendants the comparison fans out into an
    /// OR-chain over each matching class's `vtable_offset`; when `left` is
    /// statically a class union the chain is intersected with the union's
    /// member set so only reachable variants contribute to the test.
    ///
    /// Validation rules:
    /// - Right operand must be a bare identifier resolving to a registered,
    ///   polymorphic class (the polymorphism gate from sub-phase 1
    ///   guarantees this for class-union members).
    /// - Left operand's static type must be a registered class or class
    ///   union — anything else can't carry a vtable to inspect.
    /// - The matched set (right_class plus descendants, intersected with
    ///   left's union members when applicable) must be non-empty; an empty
    ///   set means the test is statically false because right_class can't
    ///   share runtime tags with anything left could be.
    pub(crate) fn emit_instanceof(
        &mut self,
        left: &Expression<'a>,
        right: &Expression<'a>,
        span_start: u32,
    ) -> Result<WasmType, CompileError> {
        let right_class = match right {
            Expression::Identifier(id) => id.name.as_str().to_string(),
            _ => {
                return Err(self.locate(
                    CompileError::type_err(
                        "right operand of `instanceof` must name a class — \
                         expressions, member access, and generic type args are \
                         not yet supported",
                    ),
                    span_start,
                ));
            }
        };
        let right_layout = self
            .module_ctx
            .class_registry
            .get(&right_class)
            .ok_or_else(|| {
                self.locate(
                    CompileError::type_err(format!(
                        "right operand of `instanceof` must name a registered class \
                         — '{right_class}' is not a class"
                    )),
                    span_start,
                )
            })?
            .clone();
        if !right_layout.is_polymorphic {
            return Err(self.locate(
                CompileError::type_err(format!(
                    "`instanceof {right_class}` requires '{right_class}' to be \
                     polymorphic — leaf classes carry no vtable pointer to \
                     inspect at runtime. Add a common base class so '{right_class}' \
                     participates in an inheritance hierarchy."
                )),
                span_start,
            ));
        }

        let left_class = self.resolve_expr_class(left).map_err(|_| {
            self.locate(
                CompileError::type_err(
                    "left operand of `instanceof` must have a class or class-union \
                     static type",
                ),
                span_start,
            )
        })?;

        // Build the matched set: every registered class whose `vtable_offset`
        // would satisfy `instanceof right_class` at runtime — i.e. itself or
        // a descendant. Walk the registry once; the size cap is the class
        // count, which is bounded.
        let mut matched: Vec<String> = self
            .module_ctx
            .class_registry
            .classes
            .keys()
            .filter(|c| {
                c.as_str() == right_class
                    || self
                        .module_ctx
                        .class_registry
                        .is_subclass_of(c, &right_class)
            })
            .cloned()
            .collect();

        // When `left` is a static class union, restrict the chain to the
        // union's class members — shapes and literals can never match
        // `instanceof` at runtime, and a class member outside the matched
        // descendant set can never satisfy the test either.
        if let Some(u) = self.module_ctx.union_registry.get_by_name(&left_class) {
            use crate::codegen::unions::UnionMember;
            use std::collections::HashSet;
            let union_class_members: HashSet<&str> = u
                .members
                .iter()
                .filter_map(|m| match m {
                    UnionMember::Shape(name)
                        if !self.module_ctx.shape_registry.by_name.contains_key(name) =>
                    {
                        Some(name.as_str())
                    }
                    _ => None,
                })
                .collect();
            matched.retain(|c| union_class_members.contains(c.as_str()));
        }
        // Stable order so generated wasm is deterministic across runs.
        matched.sort();

        if matched.is_empty() {
            return Err(self.locate(
                CompileError::type_err(format!(
                    "`instanceof {right_class}` is statically false on a value of \
                     type '{left_class}' — '{right_class}' is unrelated to every \
                     member of the static type. Did you mean a different class?"
                )),
                span_start,
            ));
        }

        let vtable_offsets: Vec<u32> = matched
            .iter()
            .map(|c| self.module_ctx.class_registry.classes[c].vtable_offset)
            .collect();

        // Emit `left` and load its vtable pointer (i32 at offset 0).
        let _left_ty = self.emit_expr(left)?;
        self.push(Instruction::I32Load(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));

        if vtable_offsets.len() == 1 {
            self.push(Instruction::I32Const(vtable_offsets[0] as i32));
            self.push(Instruction::I32Eq);
        } else {
            // Tee the vtable pointer to a temp so each comparison can re-read it.
            let vt_temp = self.alloc_local(WasmType::I32);
            self.push(Instruction::LocalTee(vt_temp));
            self.push(Instruction::I32Const(vtable_offsets[0] as i32));
            self.push(Instruction::I32Eq);
            for &off in &vtable_offsets[1..] {
                self.push(Instruction::LocalGet(vt_temp));
                self.push(Instruction::I32Const(off as i32));
                self.push(Instruction::I32Eq);
                self.push(Instruction::I32Or);
            }
        }

        Ok(WasmType::I32)
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
