use oxc_ast::ast::*;
use wasm_encoder::{Instruction, MemArg};

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

/// A single piece in a fused string-concatenation chain.
/// Static pieces skip a header-load on the hot path; dynamic pieces read the length
/// from their runtime [len][bytes] header.
enum FusionPiece<'p, 'a: 'p> {
    Static { offset: u32, len: u32 },
    Expr(&'p Expression<'a>),
}

fn string_load_i32(offset: u64) -> Instruction<'static> {
    Instruction::I32Load(MemArg {
        offset,
        align: 2,
        memory_index: 0,
    })
}

fn string_store_i32(offset: u64) -> Instruction<'static> {
    Instruction::I32Store(MemArg {
        offset,
        align: 2,
        memory_index: 0,
    })
}

impl<'a> FuncContext<'a> {
    pub(crate) fn emit_string_literal(
        &mut self,
        lit: &StringLiteral,
    ) -> Result<WasmType, CompileError> {
        let offset = self.module_ctx.alloc_static_string(&lit.value);
        self.push(Instruction::I32Const(offset as i32));
        Ok(WasmType::I32)
    }

    pub(crate) fn emit_template_literal(
        &mut self,
        tpl: &TemplateLiteral<'a>,
    ) -> Result<WasmType, CompileError> {
        // Template literal: `abc${expr}def${expr2}ghi`
        // quasis = ["abc", "def", "ghi"], expressions = [expr, expr2]
        // Flatten to interleaved pieces [quasi_0, expr_0, quasi_1, expr_1, quasi_2]
        // (empty quasis dropped) and emit as a single fused allocation.

        let mut pieces: Vec<FusionPiece<'_, 'a>> =
            Vec::with_capacity(tpl.quasis.len() + tpl.expressions.len());
        for (i, quasi) in tpl.quasis.iter().enumerate() {
            let text = quasi.value.raw.as_str();
            if !text.is_empty() {
                let offset = self.module_ctx.alloc_static_string(text);
                pieces.push(FusionPiece::Static {
                    offset,
                    len: text.len() as u32,
                });
            }
            if i < tpl.expressions.len() {
                pieces.push(FusionPiece::Expr(&tpl.expressions[i]));
            }
        }

        self.emit_fused_string_chain(&pieces)
    }

    /// Flatten a string-yielding `+` chain into a list of pieces for fused concatenation.
    /// Only recurses into sub-`+` nodes whose own result is string-typed, so that
    /// `(1 + 2) + "x"` correctly treats `1 + 2` as a single numeric piece.
    fn collect_string_plus_chain<'p>(
        &self,
        expr: &'p Expression<'a>,
        into: &mut Vec<FusionPiece<'p, 'a>>,
    ) {
        if let Expression::BinaryExpression(bin) = expr
            && bin.operator == BinaryOperator::Addition
            && self.resolve_expr_is_string(expr)
        {
            self.collect_string_plus_chain(&bin.left, into);
            self.collect_string_plus_chain(&bin.right, into);
            return;
        }
        // String literals: inline as static pieces so we skip a redundant header load.
        if let Expression::StringLiteral(s) = expr {
            let offset = self.module_ctx.alloc_static_string(&s.value);
            into.push(FusionPiece::Static {
                offset,
                len: s.value.len() as u32,
            });
            return;
        }
        into.push(FusionPiece::Expr(expr));
    }

    /// Emit a fused string chain: evaluate every piece into a local, sum their lengths
    /// at runtime, arena-allocate the combined buffer once, then memcpy each piece
    /// body into place. Replaces N-1 chained `__str_concat` calls with a single alloc.
    fn emit_fused_string_chain(
        &mut self,
        pieces: &[FusionPiece<'_, 'a>],
    ) -> Result<WasmType, CompileError> {
        // Degenerate cases: 0 pieces → empty string; 1 piece → just emit it.
        if pieces.is_empty() {
            let offset = self.module_ctx.alloc_static_string("");
            self.push(Instruction::I32Const(offset as i32));
            return Ok(WasmType::I32);
        }
        if pieces.len() == 1 {
            return match &pieces[0] {
                FusionPiece::Static { offset, .. } => {
                    self.push(Instruction::I32Const(*offset as i32));
                    Ok(WasmType::I32)
                }
                FusionPiece::Expr(e) => {
                    self.emit_expr_coerce_to_string(e)?;
                    Ok(WasmType::I32)
                }
            };
        }

        // Per-piece info: (ptr_local, known_static_len)
        // Static pieces know their length at compile time; dynamic ones load it at runtime.
        struct PieceInfo {
            ptr_local: u32,
            static_len: Option<u32>,
        }
        let mut infos: Vec<PieceInfo> = Vec::with_capacity(pieces.len());

        for piece in pieces {
            let ptr_local = self.alloc_local(WasmType::I32);
            match piece {
                FusionPiece::Static { offset, len } => {
                    self.push(Instruction::I32Const(*offset as i32));
                    self.push(Instruction::LocalSet(ptr_local));
                    infos.push(PieceInfo {
                        ptr_local,
                        static_len: Some(*len),
                    });
                }
                FusionPiece::Expr(e) => {
                    self.emit_expr_coerce_to_string(e)?;
                    self.push(Instruction::LocalSet(ptr_local));
                    infos.push(PieceInfo {
                        ptr_local,
                        static_len: None,
                    });
                }
            }
        }

        // total_len = static_sum + sum(load(dyn_ptr)) for each dynamic piece.
        let static_sum: u32 = infos.iter().filter_map(|p| p.static_len).sum();
        let total_len = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(static_sum as i32));
        for info in &infos {
            if info.static_len.is_none() {
                self.push(Instruction::LocalGet(info.ptr_local));
                self.push(string_load_i32(0));
                self.push(Instruction::I32Add);
            }
        }
        self.push(Instruction::LocalSet(total_len));

        // Arena-alloc total_len + 4 bytes.
        self.push(Instruction::LocalGet(total_len));
        self.push(Instruction::I32Const(4));
        self.push(Instruction::I32Add);
        let result_ptr = self.emit_arena_alloc_to_local(true)?;

        // Store header: result_ptr[0] = total_len
        self.push(Instruction::LocalGet(result_ptr));
        self.push(Instruction::LocalGet(total_len));
        self.push(string_store_i32(0));

        // dst = result_ptr + 4
        let dst = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(result_ptr));
        self.push(Instruction::I32Const(4));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(dst));

        // For each piece: memory.copy(dst, ptr+4, len); dst += len
        for info in &infos {
            // dst
            self.push(Instruction::LocalGet(dst));
            // src = ptr + 4
            self.push(Instruction::LocalGet(info.ptr_local));
            self.push(Instruction::I32Const(4));
            self.push(Instruction::I32Add);
            // len: static constant or load
            match info.static_len {
                Some(n) => self.push(Instruction::I32Const(n as i32)),
                None => {
                    self.push(Instruction::LocalGet(info.ptr_local));
                    self.push(string_load_i32(0));
                }
            }
            self.push(Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });

            // dst += len
            self.push(Instruction::LocalGet(dst));
            match info.static_len {
                Some(n) => self.push(Instruction::I32Const(n as i32)),
                None => {
                    self.push(Instruction::LocalGet(info.ptr_local));
                    self.push(string_load_i32(0));
                }
            }
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(dst));
        }

        self.push(Instruction::LocalGet(result_ptr));
        Ok(WasmType::I32)
    }

    /// Check if an expression evaluates to a string pointer.
    pub fn resolve_expr_is_string(&self, expr: &Expression<'a>) -> bool {
        match expr {
            Expression::StringLiteral(_) => true,
            Expression::TemplateLiteral(_) => true,
            Expression::Identifier(ident) => self.local_string_vars.contains(ident.name.as_str()),
            Expression::BinaryExpression(bin) => {
                // string + anything = string
                if bin.operator == BinaryOperator::Addition {
                    self.resolve_expr_is_string(&bin.left)
                        || self.resolve_expr_is_string(&bin.right)
                } else {
                    false
                }
            }
            Expression::CallExpression(call) => {
                if let Expression::StaticMemberExpression(member) = &call.callee {
                    // String.fromCharCode(...)
                    if let Expression::Identifier(obj) = &member.object
                        && obj.name.as_str() == "String"
                        && member.property.name.as_str() == "fromCharCode"
                    {
                        return true;
                    }
                    if self.resolve_expr_is_string(&member.object) {
                        let method = member.property.name.as_str();
                        // String methods that return strings
                        matches!(
                            method,
                            "charAt"
                                | "slice"
                                | "substring"
                                | "toLowerCase"
                                | "toUpperCase"
                                | "trim"
                                | "trimStart"
                                | "trimEnd"
                                | "replace"
                                | "replaceAll"
                                | "repeat"
                                | "padStart"
                                | "padEnd"
                                | "concat"
                        )
                    } else {
                        // Number instance methods that return strings
                        let method = member.property.name.as_str();
                        matches!(
                            method,
                            "toString" | "toFixed" | "toPrecision" | "toExponential"
                        )
                    }
                } else if let Expression::Identifier(ident) = &call.callee {
                    // Check if function returns string
                    self.module_ctx
                        .func_return_strings
                        .contains(ident.name.as_str())
                } else {
                    false
                }
            }
            Expression::ParenthesizedExpression(paren) => {
                self.resolve_expr_is_string(&paren.expression)
            }
            Expression::StaticMemberExpression(member) => {
                // Check if accessing a string field on a class instance
                if let Ok(class_name) = self.resolve_expr_class(&member.object)
                    && let Some(layout) = self.module_ctx.class_registry.get(&class_name)
                {
                    return layout
                        .field_string_types
                        .contains(member.property.name.as_str());
                }
                false
            }
            _ => false,
        }
    }

    pub(crate) fn emit_string_property(
        &mut self,
        member: &StaticMemberExpression<'a>,
        prop: &str,
    ) -> Result<WasmType, CompileError> {
        match prop {
            "length" => {
                self.emit_expr(&member.object)?;
                self.push(Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                Ok(WasmType::I32)
            }
            _ => Err(self.locate(
                CompileError::codegen(format!("string has no property '{prop}'")),
                member.property.span.start,
            )),
        }
    }

    /// Emit a non-string expression and coerce it to a string pointer.
    pub(crate) fn emit_expr_coerce_to_string(
        &mut self,
        expr: &Expression<'a>,
    ) -> Result<(), CompileError> {
        if self.resolve_expr_is_string(expr) {
            self.emit_expr(expr)?;
        } else {
            let ty = self.emit_expr(expr)?;
            match ty {
                WasmType::I32 => {
                    let (func_idx, _) = self.module_ctx.get_func("__str_from_i32").unwrap();
                    self.push(Instruction::Call(func_idx));
                }
                WasmType::F64 => {
                    let (func_idx, _) = self.module_ctx.get_func("__str_from_f64").unwrap();
                    self.push(Instruction::Call(func_idx));
                }
                _ => return Err(CompileError::type_err("cannot convert void to string")),
            }
        }
        Ok(())
    }

    pub(crate) fn emit_string_binary(
        &mut self,
        bin: &BinaryExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        match bin.operator {
            BinaryOperator::Addition => {
                // Fused concat: flatten the whole string-yielding `+` subtree into a
                // single allocation. Replaces N-1 chained `__str_concat` calls (each
                // allocating an intermediate string) with one arena-alloc plus memcpy.
                let mut pieces: Vec<FusionPiece<'_, 'a>> = Vec::new();
                self.collect_string_plus_chain(&bin.left, &mut pieces);
                self.collect_string_plus_chain(&bin.right, &mut pieces);
                self.emit_fused_string_chain(&pieces)
            }
            BinaryOperator::StrictEquality | BinaryOperator::Equality => {
                self.emit_expr(&bin.left)?;
                self.emit_expr(&bin.right)?;
                let (func_idx, _) = self
                    .module_ctx
                    .get_func("__str_eq")
                    .ok_or_else(|| CompileError::codegen("__str_eq not found"))?;
                self.push(Instruction::Call(func_idx));
                Ok(WasmType::I32)
            }
            BinaryOperator::StrictInequality | BinaryOperator::Inequality => {
                self.emit_expr(&bin.left)?;
                self.emit_expr(&bin.right)?;
                let (func_idx, _) = self
                    .module_ctx
                    .get_func("__str_eq")
                    .ok_or_else(|| CompileError::codegen("__str_eq not found"))?;
                self.push(Instruction::Call(func_idx));
                self.push(Instruction::I32Eqz); // negate
                Ok(WasmType::I32)
            }
            BinaryOperator::LessThan => {
                self.emit_expr(&bin.left)?;
                self.emit_expr(&bin.right)?;
                let (func_idx, _) = self
                    .module_ctx
                    .get_func("__str_cmp")
                    .ok_or_else(|| CompileError::codegen("__str_cmp not found"))?;
                self.push(Instruction::Call(func_idx));
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32LtS);
                Ok(WasmType::I32)
            }
            BinaryOperator::GreaterThan => {
                self.emit_expr(&bin.left)?;
                self.emit_expr(&bin.right)?;
                let (func_idx, _) = self
                    .module_ctx
                    .get_func("__str_cmp")
                    .ok_or_else(|| CompileError::codegen("__str_cmp not found"))?;
                self.push(Instruction::Call(func_idx));
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32GtS);
                Ok(WasmType::I32)
            }
            BinaryOperator::LessEqualThan => {
                self.emit_expr(&bin.left)?;
                self.emit_expr(&bin.right)?;
                let (func_idx, _) = self
                    .module_ctx
                    .get_func("__str_cmp")
                    .ok_or_else(|| CompileError::codegen("__str_cmp not found"))?;
                self.push(Instruction::Call(func_idx));
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32LeS);
                Ok(WasmType::I32)
            }
            BinaryOperator::GreaterEqualThan => {
                self.emit_expr(&bin.left)?;
                self.emit_expr(&bin.right)?;
                let (func_idx, _) = self
                    .module_ctx
                    .get_func("__str_cmp")
                    .ok_or_else(|| CompileError::codegen("__str_cmp not found"))?;
                self.push(Instruction::Call(func_idx));
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32GeS);
                Ok(WasmType::I32)
            }
            _ => Err(self.locate(
                CompileError::unsupported(format!("string operator {:?}", bin.operator)),
                bin.span.start,
            )),
        }
    }

    pub(crate) fn try_emit_string_method_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        let member = match &call.callee {
            Expression::StaticMemberExpression(m) => m,
            _ => return Ok(None),
        };

        if !self.resolve_expr_is_string(&member.object) {
            return Ok(None);
        }

        let method = member.property.name.as_str();
        match method {
            "charCodeAt" | "charAt" => {
                // Inline: bounds-checked load8_u — returns i32
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen(format!(
                        "{method} expects 1 argument"
                    )));
                }
                let str_local = self.alloc_local(WasmType::I32);
                self.emit_expr(&member.object)?;
                self.push(Instruction::LocalSet(str_local));

                let idx_local = self.alloc_local(WasmType::I32);
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::LocalSet(idx_local));

                self.emit_array_bounds_check(str_local, idx_local);

                self.push(Instruction::LocalGet(str_local));
                self.push(Instruction::I32Const(4)); // STRING_HEADER_SIZE
                self.push(Instruction::I32Add);
                self.push(Instruction::LocalGet(idx_local));
                self.push(Instruction::I32Add);
                self.push(Instruction::I32Load8U(MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                }));
                Ok(Some(WasmType::I32))
            }
            "indexOf" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen("indexOf expects 1 argument"));
                }
                self.emit_expr(&member.object)?;
                self.emit_expr(call.arguments[0].to_expression())?;
                let (func_idx, _) = self.module_ctx.get_func("__str_indexOf").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "lastIndexOf" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen("lastIndexOf expects 1 argument"));
                }
                self.emit_expr(&member.object)?;
                self.emit_expr(call.arguments[0].to_expression())?;
                let (func_idx, _) =
                    self.module_ctx.get_func("__str_lastIndexOf").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "includes" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen("includes expects 1 argument"));
                }
                self.emit_expr(&member.object)?;
                self.emit_expr(call.arguments[0].to_expression())?;
                let (func_idx, _) = self.module_ctx.get_func("__str_includes").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "startsWith" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen("startsWith expects 1 argument"));
                }
                self.emit_expr(&member.object)?;
                self.emit_expr(call.arguments[0].to_expression())?;
                let (func_idx, _) = self.module_ctx.get_func("__str_startsWith").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "endsWith" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen("endsWith expects 1 argument"));
                }
                self.emit_expr(&member.object)?;
                self.emit_expr(call.arguments[0].to_expression())?;
                let (func_idx, _) = self.module_ctx.get_func("__str_endsWith").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "slice" | "substring" => {
                if call.arguments.is_empty() || call.arguments.len() > 2 {
                    return Err(CompileError::codegen(format!(
                        "{method} expects 1-2 arguments"
                    )));
                }
                // Emit string pointer
                let str_local = self.alloc_local(WasmType::I32);
                self.emit_expr(&member.object)?;
                self.push(Instruction::LocalSet(str_local));

                // Emit start
                self.push(Instruction::LocalGet(str_local));
                self.emit_expr(call.arguments[0].to_expression())?;

                // Emit end (default to string length if omitted)
                if call.arguments.len() == 2 {
                    self.emit_expr(call.arguments[1].to_expression())?;
                } else {
                    // end = str.length
                    self.push(Instruction::LocalGet(str_local));
                    self.push(Instruction::I32Load(MemArg {
                        offset: 0,
                        align: 2,
                        memory_index: 0,
                    }));
                }

                let (func_idx, _) = self.module_ctx.get_func("__str_slice").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "toLowerCase" => {
                self.emit_expr(&member.object)?;
                let (func_idx, _) = self.module_ctx.get_func("__str_toLower").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "toUpperCase" => {
                self.emit_expr(&member.object)?;
                let (func_idx, _) = self.module_ctx.get_func("__str_toUpper").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "trim" => {
                self.emit_expr(&member.object)?;
                let (func_idx, _) = self.module_ctx.get_func("__str_trim").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "concat" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen(
                        "String.concat expects 1 argument (variadic concat not supported; use `a + b` or template literals)",
                    ));
                }
                self.emit_expr(&member.object)?;
                self.emit_expr(call.arguments[0].to_expression())?;
                let (func_idx, _) = self.module_ctx.get_func("__str_concat").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "trimStart" => {
                self.emit_expr(&member.object)?;
                let (func_idx, _) = self.module_ctx.get_func("__str_trimStart").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "trimEnd" => {
                self.emit_expr(&member.object)?;
                let (func_idx, _) = self.module_ctx.get_func("__str_trimEnd").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "at" | "codePointAt" => {
                // Negative-index-normalized byte load (returns i32 char code).
                // Matches tscc's charAt/charCodeAt convention of returning i32
                // instead of a single-char string. `codePointAt` is BMP-only
                // (identical to charCodeAt for values < 0x10000).
                // Negative indices count from the end. OOB traps (same as
                // charAt/charCodeAt); non-spec but consistent with tscc.
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen(format!(
                        "{method} expects 1 argument"
                    )));
                }
                let str_local = self.alloc_local(WasmType::I32);
                self.emit_expr(&member.object)?;
                self.push(Instruction::LocalSet(str_local));

                let idx_local = self.alloc_local(WasmType::I32);
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::LocalSet(idx_local));

                if method == "at" {
                    // Normalize negative index: if idx < 0, idx += length
                    self.push(Instruction::LocalGet(idx_local));
                    self.push(Instruction::I32Const(0));
                    self.push(Instruction::I32LtS);
                    self.push(Instruction::If(wasm_encoder::BlockType::Empty));
                    self.push(Instruction::LocalGet(idx_local));
                    self.push(Instruction::LocalGet(str_local));
                    self.push(Instruction::I32Load(MemArg {
                        offset: 0,
                        align: 2,
                        memory_index: 0,
                    }));
                    self.push(Instruction::I32Add);
                    self.push(Instruction::LocalSet(idx_local));
                    self.push(Instruction::End);
                }

                self.emit_array_bounds_check(str_local, idx_local);

                self.push(Instruction::LocalGet(str_local));
                self.push(Instruction::I32Const(4)); // STRING_HEADER_SIZE
                self.push(Instruction::I32Add);
                self.push(Instruction::LocalGet(idx_local));
                self.push(Instruction::I32Add);
                self.push(Instruction::I32Load8U(MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                }));
                Ok(Some(WasmType::I32))
            }
            "split" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen("split expects 1 argument"));
                }
                self.emit_expr(&member.object)?;
                self.emit_expr(call.arguments[0].to_expression())?;
                let (func_idx, _) = self.module_ctx.get_func("__str_split").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "replace" => {
                if call.arguments.len() != 2 {
                    return Err(CompileError::codegen("replace expects 2 arguments"));
                }
                self.emit_expr(&member.object)?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.emit_expr(call.arguments[1].to_expression())?;
                let (func_idx, _) = self.module_ctx.get_func("__str_replace").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "replaceAll" => {
                if call.arguments.len() != 2 {
                    return Err(CompileError::codegen("replaceAll expects 2 arguments"));
                }
                self.emit_expr(&member.object)?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.emit_expr(call.arguments[1].to_expression())?;
                let (func_idx, _) = self.module_ctx.get_func("__str_replaceAll").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "repeat" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen("repeat expects 1 argument"));
                }
                self.emit_expr(&member.object)?;
                self.emit_expr(call.arguments[0].to_expression())?;
                let (func_idx, _) = self.module_ctx.get_func("__str_repeat").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "padStart" => {
                if call.arguments.is_empty() || call.arguments.len() > 2 {
                    return Err(CompileError::codegen("padStart expects 1-2 arguments"));
                }
                self.emit_expr(&member.object)?;
                self.emit_expr(call.arguments[0].to_expression())?;
                if call.arguments.len() == 2 {
                    self.emit_expr(call.arguments[1].to_expression())?;
                } else {
                    // Default fill = " "
                    let offset = self.module_ctx.alloc_static_string(" ");
                    self.push(Instruction::I32Const(offset as i32));
                }
                let (func_idx, _) = self.module_ctx.get_func("__str_padStart").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "padEnd" => {
                if call.arguments.is_empty() || call.arguments.len() > 2 {
                    return Err(CompileError::codegen("padEnd expects 1-2 arguments"));
                }
                self.emit_expr(&member.object)?;
                self.emit_expr(call.arguments[0].to_expression())?;
                if call.arguments.len() == 2 {
                    self.emit_expr(call.arguments[1].to_expression())?;
                } else {
                    let offset = self.module_ctx.alloc_static_string(" ");
                    self.push(Instruction::I32Const(offset as i32));
                }
                let (func_idx, _) = self.module_ctx.get_func("__str_padEnd").unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            _ => Ok(None),
        }
    }

    /// Emit str[i] — bounds-checked byte access returning i32 char code.
    pub(crate) fn emit_string_index(
        &mut self,
        member: &ComputedMemberExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        // Evaluate string pointer and index
        let str_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&member.object)?;
        self.push(Instruction::LocalSet(str_local));

        let idx_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&member.expression)?;
        self.push(Instruction::LocalSet(idx_local));

        // Bounds check: if index >= length, trap
        self.emit_array_bounds_check(str_local, idx_local);

        // Compute byte address: str_ptr + 4 + index
        self.push(Instruction::LocalGet(str_local));
        self.push(Instruction::I32Const(4)); // STRING_HEADER_SIZE
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::I32Add);

        // Load single byte as unsigned i32
        self.push(Instruction::I32Load8U(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));

        Ok(WasmType::I32)
    }
}
