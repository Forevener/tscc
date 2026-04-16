use std::collections::HashSet;

use oxc_ast::ast::*;
use wasm_encoder::{Instruction, MemArg, ValType};

use crate::error::CompileError;
use crate::types::{ClosureSig, WasmType};

use super::func::FuncContext;

/// Array header: [length: i32 (4B)] [capacity: i32 (4B)]
pub const ARRAY_HEADER_SIZE: u32 = 8;

/// A single piece in a fused string-concatenation chain.
/// Static pieces skip a header-load on the hot path; dynamic pieces read the length
/// from their runtime [len][bytes] header.
enum FusionPiece<'p, 'a: 'p> {
    Static { offset: u32, len: u32 },
    Expr(&'p Expression<'a>),
}

fn string_load_i32(offset: u64) -> Instruction<'static> {
    Instruction::I32Load(MemArg { offset, align: 2, memory_index: 0 })
}

fn string_store_i32(offset: u64) -> Instruction<'static> {
    Instruction::I32Store(MemArg { offset, align: 2, memory_index: 0 })
}

impl<'a> FuncContext<'a> {
    pub fn emit_expr(&mut self, expr: &Expression<'a>) -> Result<WasmType, CompileError> {
        match expr {
            Expression::NumericLiteral(lit) => self.emit_numeric_literal(lit),
            Expression::BooleanLiteral(lit) => {
                self.push(Instruction::I32Const(if lit.value { 1 } else { 0 }));
                Ok(WasmType::I32)
            }
            Expression::Identifier(ident) => self.emit_identifier(ident),
            Expression::BinaryExpression(bin) => self.emit_binary(bin),
            Expression::LogicalExpression(log) => self.emit_logical(log),
            Expression::UnaryExpression(un) => self.emit_unary(un),
            Expression::CallExpression(call) => self.emit_call(call),
            Expression::AssignmentExpression(assign) => self.emit_assignment(assign),
            Expression::ParenthesizedExpression(paren) => self.emit_expr(&paren.expression),
            Expression::UpdateExpression(update) => self.emit_update(update),
            Expression::NewExpression(new_expr) => self.emit_new(new_expr),
            Expression::StaticMemberExpression(member) => self.emit_member_access(member),
            Expression::ComputedMemberExpression(member) => self.emit_computed_member_access(member),
            Expression::ChainExpression(chain) => self.emit_chain_expression(chain),
            Expression::ConditionalExpression(cond) => self.emit_conditional(cond),
            Expression::ThisExpression(_) => self.emit_this(),
            Expression::NullLiteral(_) => {
                self.push(Instruction::I32Const(0));
                Ok(WasmType::I32)
            }
            Expression::TSAsExpression(as_expr) => self.emit_as_cast(as_expr),
            Expression::ArrowFunctionExpression(arrow) => self.emit_arrow_closure(arrow),
            Expression::StringLiteral(s) => self.emit_string_literal(s),
            Expression::TemplateLiteral(tpl) => self.emit_template_literal(tpl),
            _ => {
                let span_start = match expr {
                    Expression::ArrayExpression(a) => a.span.start,
                    Expression::ObjectExpression(o) => o.span.start,
                    _ => 0,
                };
                let err = CompileError::unsupported(format!(
                    "expression type: {}", expr_kind_name(expr)
                ));
                if span_start > 0 {
                    Err(self.locate(err, span_start))
                } else {
                    Err(err)
                }
            }
        }
    }

    fn emit_numeric_literal(&mut self, lit: &NumericLiteral) -> Result<WasmType, CompileError> {
        // If the literal has no fractional part and fits in i32, emit as i32
        // Otherwise emit as f64
        let val = lit.value;
        if val.fract() == 0.0 && val >= i32::MIN as f64 && val <= i32::MAX as f64 {
            // Check if the raw source contains a dot — if so, treat as f64
            if lit.raw.as_ref().is_some_and(|r| r.contains('.')) {
                self.push(Instruction::F64Const(val));
                Ok(WasmType::F64)
            } else {
                self.push(Instruction::I32Const(val as i32));
                Ok(WasmType::I32)
            }
        } else {
            self.push(Instruction::F64Const(val));
            Ok(WasmType::F64)
        }
    }

    fn emit_string_literal(&mut self, lit: &StringLiteral) -> Result<WasmType, CompileError> {
        let offset = self.module_ctx.alloc_static_string(&lit.value);
        self.push(Instruction::I32Const(offset as i32));
        Ok(WasmType::I32)
    }

    fn emit_template_literal(&mut self, tpl: &TemplateLiteral<'a>) -> Result<WasmType, CompileError> {
        // Template literal: `abc${expr}def${expr2}ghi`
        // quasis = ["abc", "def", "ghi"], expressions = [expr, expr2]
        // Flatten to interleaved pieces [quasi_0, expr_0, quasi_1, expr_1, quasi_2]
        // (empty quasis dropped) and emit as a single fused allocation.

        let mut pieces: Vec<FusionPiece<'_, 'a>> = Vec::with_capacity(tpl.quasis.len() + tpl.expressions.len());
        for (i, quasi) in tpl.quasis.iter().enumerate() {
            let text = quasi.value.raw.as_str();
            if !text.is_empty() {
                let offset = self.module_ctx.alloc_static_string(text);
                pieces.push(FusionPiece::Static { offset, len: text.len() as u32 });
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
            into.push(FusionPiece::Static { offset, len: s.value.len() as u32 });
            return;
        }
        into.push(FusionPiece::Expr(expr));
    }

    /// Emit a fused string chain: evaluate every piece into a local, sum their lengths
    /// at runtime, arena-allocate the combined buffer once, then memcpy each piece
    /// body into place. Replaces N-1 chained `__str_concat` calls with a single alloc.
    fn emit_fused_string_chain(&mut self, pieces: &[FusionPiece<'_, 'a>]) -> Result<WasmType, CompileError> {
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
                    infos.push(PieceInfo { ptr_local, static_len: Some(*len) });
                }
                FusionPiece::Expr(e) => {
                    self.emit_expr_coerce_to_string(e)?;
                    self.push(Instruction::LocalSet(ptr_local));
                    infos.push(PieceInfo { ptr_local, static_len: None });
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
            self.push(Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

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
            Expression::Identifier(ident) => {
                self.local_string_vars.contains(ident.name.as_str())
            }
            Expression::BinaryExpression(bin) => {
                // string + anything = string
                if bin.operator == BinaryOperator::Addition {
                    self.resolve_expr_is_string(&bin.left) || self.resolve_expr_is_string(&bin.right)
                } else {
                    false
                }
            }
            Expression::CallExpression(call) => {
                if let Expression::StaticMemberExpression(member) = &call.callee {
                    // String.fromCharCode(...)
                    if let Expression::Identifier(obj) = &member.object
                        && obj.name.as_str() == "String" && member.property.name.as_str() == "fromCharCode" {
                            return true;
                        }
                    if self.resolve_expr_is_string(&member.object) {
                        let method = member.property.name.as_str();
                        // String methods that return strings
                        matches!(method, "charAt" | "slice" | "substring" | "toLowerCase" | "toUpperCase" | "trim" | "trimStart" | "trimEnd" | "replace" | "repeat" | "padStart" | "padEnd" | "concat")
                    } else {
                        false
                    }
                } else if let Expression::Identifier(ident) = &call.callee {
                    // Check if function returns string
                    self.module_ctx.func_return_strings.contains(ident.name.as_str())
                } else {
                    false
                }
            }
            Expression::ParenthesizedExpression(paren) => self.resolve_expr_is_string(&paren.expression),
            Expression::StaticMemberExpression(member) => {
                // Check if accessing a string field on a class instance
                if let Ok(class_name) = self.resolve_expr_class(&member.object)
                    && let Some(layout) = self.module_ctx.class_registry.get(&class_name) {
                        return layout.field_string_types.contains(member.property.name.as_str());
                    }
                false
            }
            _ => false,
        }
    }

    fn emit_string_property(&mut self, member: &StaticMemberExpression<'a>, prop: &str) -> Result<WasmType, CompileError> {
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
    fn emit_expr_coerce_to_string(&mut self, expr: &Expression<'a>) -> Result<(), CompileError> {
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

    fn emit_string_binary(&mut self, bin: &BinaryExpression<'a>) -> Result<WasmType, CompileError> {
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
                let (func_idx, _) = self.module_ctx.get_func("__str_eq")
                    .ok_or_else(|| CompileError::codegen("__str_eq not found"))?;
                self.push(Instruction::Call(func_idx));
                Ok(WasmType::I32)
            }
            BinaryOperator::StrictInequality | BinaryOperator::Inequality => {
                self.emit_expr(&bin.left)?;
                self.emit_expr(&bin.right)?;
                let (func_idx, _) = self.module_ctx.get_func("__str_eq")
                    .ok_or_else(|| CompileError::codegen("__str_eq not found"))?;
                self.push(Instruction::Call(func_idx));
                self.push(Instruction::I32Eqz); // negate
                Ok(WasmType::I32)
            }
            BinaryOperator::LessThan => {
                self.emit_expr(&bin.left)?;
                self.emit_expr(&bin.right)?;
                let (func_idx, _) = self.module_ctx.get_func("__str_cmp")
                    .ok_or_else(|| CompileError::codegen("__str_cmp not found"))?;
                self.push(Instruction::Call(func_idx));
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32LtS);
                Ok(WasmType::I32)
            }
            BinaryOperator::GreaterThan => {
                self.emit_expr(&bin.left)?;
                self.emit_expr(&bin.right)?;
                let (func_idx, _) = self.module_ctx.get_func("__str_cmp")
                    .ok_or_else(|| CompileError::codegen("__str_cmp not found"))?;
                self.push(Instruction::Call(func_idx));
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32GtS);
                Ok(WasmType::I32)
            }
            BinaryOperator::LessEqualThan => {
                self.emit_expr(&bin.left)?;
                self.emit_expr(&bin.right)?;
                let (func_idx, _) = self.module_ctx.get_func("__str_cmp")
                    .ok_or_else(|| CompileError::codegen("__str_cmp not found"))?;
                self.push(Instruction::Call(func_idx));
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32LeS);
                Ok(WasmType::I32)
            }
            BinaryOperator::GreaterEqualThan => {
                self.emit_expr(&bin.left)?;
                self.emit_expr(&bin.right)?;
                let (func_idx, _) = self.module_ctx.get_func("__str_cmp")
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

    fn try_emit_string_method_call(&mut self, call: &CallExpression<'a>) -> Result<Option<WasmType>, CompileError> {
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
                    return Err(CompileError::codegen(format!("{method} expects 1 argument")));
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
                self.push(Instruction::I32Load8U(MemArg { offset: 0, align: 0, memory_index: 0 }));
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
                    return Err(CompileError::codegen(format!("{method} expects 1-2 arguments")));
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
                    self.push(Instruction::I32Load(MemArg { offset: 0, align: 2, memory_index: 0 }));
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
                    return Err(CompileError::codegen("String.concat expects 1 argument (variadic concat not supported; use `a + b` or template literals)"));
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
                    return Err(CompileError::codegen(format!("{method} expects 1 argument")));
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
                    self.push(Instruction::I32Load(MemArg { offset: 0, align: 2, memory_index: 0 }));
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
                self.push(Instruction::I32Load8U(MemArg { offset: 0, align: 0, memory_index: 0 }));
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
    fn emit_string_index(&mut self, member: &ComputedMemberExpression<'a>) -> Result<WasmType, CompileError> {
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

    fn emit_identifier(&mut self, ident: &IdentifierReference) -> Result<WasmType, CompileError> {
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
                    offset: 0, align: 3, memory_index: 0,
                })),
                _ => self.push(Instruction::I32Load(MemArg {
                    offset: 0, align: 2, memory_index: 0,
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

    fn emit_binary(&mut self, bin: &BinaryExpression<'a>) -> Result<WasmType, CompileError> {
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
                self.push(if is_f64 { Instruction::F64Add } else { Instruction::I32Add });
                Ok(ty)
            }
            BinaryOperator::Subtraction => {
                self.push(if is_f64 { Instruction::F64Sub } else { Instruction::I32Sub });
                Ok(ty)
            }
            BinaryOperator::Multiplication => {
                self.push(if is_f64 { Instruction::F64Mul } else { Instruction::I32Mul });
                Ok(ty)
            }
            BinaryOperator::Division => {
                self.push(if is_f64 { Instruction::F64Div } else { Instruction::I32DivS });
                Ok(ty)
            }
            BinaryOperator::Remainder => {
                if is_f64 {
                    return Err(CompileError::unsupported("f64 remainder (%) not supported in WASM"));
                }
                self.push(Instruction::I32RemS);
                Ok(WasmType::I32)
            }
            BinaryOperator::LessThan => {
                self.push(if is_f64 { Instruction::F64Lt } else { Instruction::I32LtS });
                Ok(WasmType::I32)
            }
            BinaryOperator::LessEqualThan => {
                self.push(if is_f64 { Instruction::F64Le } else { Instruction::I32LeS });
                Ok(WasmType::I32)
            }
            BinaryOperator::GreaterThan => {
                self.push(if is_f64 { Instruction::F64Gt } else { Instruction::I32GtS });
                Ok(WasmType::I32)
            }
            BinaryOperator::GreaterEqualThan => {
                self.push(if is_f64 { Instruction::F64Ge } else { Instruction::I32GeS });
                Ok(WasmType::I32)
            }
            BinaryOperator::StrictEquality | BinaryOperator::Equality => {
                self.push(if is_f64 { Instruction::F64Eq } else { Instruction::I32Eq });
                Ok(WasmType::I32)
            }
            BinaryOperator::StrictInequality | BinaryOperator::Inequality => {
                self.push(if is_f64 { Instruction::F64Ne } else { Instruction::I32Ne });
                Ok(WasmType::I32)
            }
            BinaryOperator::BitwiseAnd => {
                if is_f64 { return Err(CompileError::type_err("bitwise & on f64")); }
                self.push(Instruction::I32And);
                Ok(WasmType::I32)
            }
            BinaryOperator::BitwiseOR => {
                if is_f64 { return Err(CompileError::type_err("bitwise | on f64")); }
                self.push(Instruction::I32Or);
                Ok(WasmType::I32)
            }
            BinaryOperator::ShiftLeft => {
                if is_f64 { return Err(CompileError::type_err("shift on f64")); }
                self.push(Instruction::I32Shl);
                Ok(WasmType::I32)
            }
            BinaryOperator::ShiftRight => {
                if is_f64 { return Err(CompileError::type_err("shift on f64")); }
                self.push(Instruction::I32ShrS);
                Ok(WasmType::I32)
            }
            BinaryOperator::ShiftRightZeroFill => {
                if is_f64 { return Err(CompileError::type_err("shift on f64")); }
                self.push(Instruction::I32ShrU);
                Ok(WasmType::I32)
            }
            _ => Err(CompileError::unsupported(format!(
                "binary operator {:?}", bin.operator
            ))),
        }
    }

    fn emit_logical(&mut self, log: &LogicalExpression<'a>) -> Result<WasmType, CompileError> {
        match log.operator {
            LogicalOperator::And => {
                // Short-circuit: if left is 0, result is 0; else evaluate right
                let left_ty = self.emit_expr(&log.left)?;
                // Duplicate the value by using a local
                let tmp = self.alloc_local(WasmType::I32);
                self.push(Instruction::LocalTee(tmp));
                self.push(Instruction::If(wasm_encoder::BlockType::Result(ValType::I32)));
                let right_ty = self.emit_expr(&log.right)?;
                if right_ty != WasmType::I32 {
                    return Err(CompileError::type_err("logical && requires i32/bool operands"));
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
                self.push(Instruction::If(wasm_encoder::BlockType::Result(ValType::I32)));
                self.push(Instruction::LocalGet(tmp));
                self.push(Instruction::Else);
                let right_ty = self.emit_expr(&log.right)?;
                if right_ty != WasmType::I32 {
                    return Err(CompileError::type_err("logical || requires i32/bool operands"));
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
                self.push(Instruction::If(wasm_encoder::BlockType::Result(ValType::I32)));
                self.push(Instruction::LocalGet(tmp));
                self.push(Instruction::Else);
                let right_ty = self.emit_expr(&log.right)?;
                if right_ty != WasmType::I32 {
                    return Err(CompileError::type_err("nullish coalescing (??) requires i32 operands"));
                }
                self.push(Instruction::End);
                let _ = left_ty;
                Ok(WasmType::I32)
            }
        }
    }

    fn emit_unary(&mut self, un: &UnaryExpression<'a>) -> Result<WasmType, CompileError> {
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
                "unary operator {:?}", un.operator
            ))),
        }
    }

    fn emit_call(&mut self, call: &CallExpression<'a>) -> Result<WasmType, CompileError> {
        // Handle super(args) — call parent constructor
        if matches!(&call.callee, Expression::Super(_)) {
            return self.emit_super_constructor_call(call);
        }

        // Handle super.method(args) — static dispatch to parent method
        if let Expression::StaticMemberExpression(member) = &call.callee
            && matches!(&member.object, Expression::Super(_)) {
                return self.emit_super_method_call(member.property.name.as_str(), call);
            }

        // Check for Number.<static> calls
        if let Some(result) = self.try_emit_number_call(call)? {
            return Ok(result);
        }

        // Check for Array.<static> calls (Array.isArray)
        if let Some(result) = self.try_emit_array_static_call(call)? {
            return Ok(result);
        }

        // Check for Math.* member calls first
        if let Some(result) = self.try_emit_math_call(call)? {
            return Ok(result);
        }

        // Check for array builtin calls (filter, map, forEach, reduce, sort)
        if let Some(result) = self.try_emit_array_builtin(call)? {
            return Ok(result);
        }

        // Check for String.fromCharCode(code)
        if let Expression::StaticMemberExpression(member) = &call.callee
            && let Expression::Identifier(obj) = &member.object
                && obj.name.as_str() == "String" && member.property.name.as_str() == "fromCharCode"
                    && call.arguments.len() == 1 {
                        self.emit_expr(call.arguments[0].to_expression())?;
                        let (func_idx, _) = self.module_ctx.get_func("__str_fromCharCode").unwrap();
                        self.push(Instruction::Call(func_idx));
                        return Ok(WasmType::I32);
                    }

        // Check for string method calls (str.indexOf, str.slice, etc.)
        if let Some(result) = self.try_emit_string_method_call(call)? {
            return Ok(result);
        }

        // Check for array method calls (arr.push, etc.)
        if let Some(result) = self.try_emit_array_method_call(call)? {
            return Ok(result);
        }

        // Check for obj.method(args) calls
        if let Some(result) = self.try_emit_method_call(call)? {
            return Ok(result);
        }

        let callee_name = match &call.callee {
            Expression::Identifier(ident) => ident.name.as_str(),
            _ => return Err(CompileError::unsupported("non-identifier callee")),
        };

        // Type cast: f64(x) -> f64.convert_i32_s, i32(x) -> i32.trunc_f64_s
        if callee_name == "f64" && call.arguments.len() == 1 {
            let arg_ty = self.emit_expr(call.arguments[0].to_expression())?;
            if arg_ty == WasmType::I32 {
                self.push(Instruction::F64ConvertI32S);
            }
            return Ok(WasmType::F64);
        }
        if callee_name == "i32" && call.arguments.len() == 1 {
            let arg_ty = self.emit_expr(call.arguments[0].to_expression())?;
            if arg_ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
            return Ok(WasmType::I32);
        }

        // Global isNaN(x): NaN !== NaN, so `x != x` (F64Ne with self).
        // Only f64 can be NaN in our model; i32 is always "not NaN".
        if callee_name == "isNaN" && call.arguments.len() == 1 {
            let ty = self.emit_expr(call.arguments[0].to_expression())?;
            match ty {
                WasmType::F64 => {
                    let tmp = self.alloc_local(WasmType::F64);
                    self.push(Instruction::LocalTee(tmp));
                    self.push(Instruction::LocalGet(tmp));
                    self.push(Instruction::F64Ne);
                }
                WasmType::I32 => {
                    self.push(Instruction::Drop);
                    self.push(Instruction::I32Const(0));
                }
                _ => return Err(CompileError::type_err("isNaN requires a numeric argument")),
            }
            return Ok(WasmType::I32);
        }
        // Global isFinite(x): (x - x) == 0 iff x is finite. finite-finite=0,
        // NaN-NaN=NaN, ±Inf-±Inf=NaN. F64Eq with 0.0 yields 1 for finite, 0
        // otherwise. i32 values are always finite.
        if callee_name == "isFinite" && call.arguments.len() == 1 {
            let ty = self.emit_expr(call.arguments[0].to_expression())?;
            match ty {
                WasmType::F64 => {
                    let tmp = self.alloc_local(WasmType::F64);
                    self.push(Instruction::LocalTee(tmp));
                    self.push(Instruction::LocalGet(tmp));
                    self.push(Instruction::F64Sub);
                    self.push(Instruction::F64Const(0.0));
                    self.push(Instruction::F64Eq);
                }
                WasmType::I32 => {
                    self.push(Instruction::Drop);
                    self.push(Instruction::I32Const(1));
                }
                _ => return Err(CompileError::type_err("isFinite requires a numeric argument")),
            }
            return Ok(WasmType::I32);
        }

        // parseInt(s) -> i32
        if callee_name == "parseInt" && call.arguments.len() == 1 {
            self.emit_expr(call.arguments[0].to_expression())?;
            let (func_idx, _) = self.module_ctx.get_func("__str_parseInt").unwrap();
            self.push(Instruction::Call(func_idx));
            return Ok(WasmType::I32);
        }
        // parseFloat(s) -> f64
        if callee_name == "parseFloat" && call.arguments.len() == 1 {
            self.emit_expr(call.arguments[0].to_expression())?;
            let (func_idx, _) = self.module_ctx.get_func("__str_parseFloat").unwrap();
            self.push(Instruction::Call(func_idx));
            return Ok(WasmType::F64);
        }

        // Memory intrinsics: load_f64(offset), load_i32(offset), store_f64(offset, val), store_i32(offset, val)
        if let Some(result) = self.try_emit_memory_intrinsic(callee_name, call)? {
            return Ok(result);
        }

        // Static data allocation: __static_alloc(size) -> i32 offset (compile-time constant)
        if callee_name == "__static_alloc" && call.arguments.len() == 1 {
            return self.emit_static_alloc(call);
        }

        // Check if callee is a closure variable
        if let Some(sig) = self.local_closure_sigs.get(callee_name).cloned() {
            return self.emit_closure_call(callee_name, &sig, call);
        }

        // Look up function
        let (func_idx, ret_ty) = self.module_ctx.get_func(callee_name)
            .ok_or_else(|| self.locate(
                CompileError::codegen(format!("undefined function '{callee_name}'")),
                call.span.start,
            ))?;

        // Emit arguments
        for arg in &call.arguments {
            self.emit_expr(arg.to_expression())?;
        }

        self.push(Instruction::Call(func_idx));
        Ok(ret_ty)
    }

    fn try_emit_memory_intrinsic(&mut self, name: &str, call: &CallExpression<'a>) -> Result<Option<WasmType>, CompileError> {
        match name {
            "load_f64" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen("load_f64 expects 1 argument (offset)"));
                }
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 3, // 2^3 = 8 byte alignment
                    memory_index: 0,
                }));
                Ok(Some(WasmType::F64))
            }
            "load_i32" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen("load_i32 expects 1 argument (offset)"));
                }
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2, // 2^2 = 4 byte alignment
                    memory_index: 0,
                }));
                Ok(Some(WasmType::I32))
            }
            "store_f64" => {
                if call.arguments.len() != 2 {
                    return Err(CompileError::codegen("store_f64 expects 2 arguments (offset, value)"));
                }
                self.emit_expr(call.arguments[0].to_expression())?;
                self.emit_expr(call.arguments[1].to_expression())?;
                self.push(Instruction::F64Store(wasm_encoder::MemArg {
                    offset: 0,
                    align: 3,
                    memory_index: 0,
                }));
                Ok(Some(WasmType::Void))
            }
            "store_i32" => {
                if call.arguments.len() != 2 {
                    return Err(CompileError::codegen("store_i32 expects 2 arguments (offset, value)"));
                }
                self.emit_expr(call.arguments[0].to_expression())?;
                self.emit_expr(call.arguments[1].to_expression())?;
                self.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                Ok(Some(WasmType::Void))
            }
            _ => Ok(None),
        }
    }

    /// Dispatch Number.<static> calls. Returns Some if recognized, None otherwise.
    ///
    /// Semantics per ECMA-262 §21.1.2:
    /// - `Number.isNaN(x)`: true only when x is the NaN value. Our f64 inputs
    ///   compare via `x != x`; i32 inputs are never NaN.
    /// - `Number.isFinite(x)`: true when x is finite. Unlike global `isFinite`,
    ///   it does not coerce — but in our typed world the inputs are already
    ///   numeric, so it behaves the same.
    /// - `Number.isInteger(x)`: finite AND `x == floor(x)`. i32 values are
    ///   always integers.
    /// - `Number.isSafeInteger(x)`: integer AND |x| ≤ 2^53 − 1.
    // Compile-time Array.isArray: typed subset resolves this statically since
    // arrays are arena pointers with no runtime tag.
    fn try_emit_array_static_call(&mut self, call: &CallExpression<'a>) -> Result<Option<WasmType>, CompileError> {
        let (obj_name, method_name) = match &call.callee {
            Expression::StaticMemberExpression(member) => {
                let obj = match &member.object {
                    Expression::Identifier(ident) => ident.name.as_str(),
                    _ => return Ok(None),
                };
                (obj, member.property.name.as_str())
            }
            _ => return Ok(None),
        };
        if obj_name != "Array" {
            return Ok(None);
        }
        match method_name {
            "isArray" => {
                self.expect_args(call, 1, "Array.isArray")?;
                let is_arr = self.expr_is_array(call.arguments[0].to_expression());
                self.push(Instruction::I32Const(if is_arr { 1 } else { 0 }));
                Ok(Some(WasmType::I32))
            }
            _ => Err(CompileError::unsupported(format!("Array.{method_name} is not supported"))),
        }
    }

    fn expr_is_array(&self, expr: &Expression<'a>) -> bool {
        match expr {
            Expression::ArrayExpression(_) => true,
            Expression::Identifier(ident) => {
                self.local_array_elem_types.contains_key(ident.name.as_str())
            }
            Expression::NewExpression(new) => {
                if let Expression::Identifier(id) = &new.callee {
                    id.name.as_str() == "Array"
                } else {
                    false
                }
            }
            Expression::ParenthesizedExpression(paren) => self.expr_is_array(&paren.expression),
            _ => false,
        }
    }

    fn try_emit_number_call(&mut self, call: &CallExpression<'a>) -> Result<Option<WasmType>, CompileError> {
        let (obj_name, method_name) = match &call.callee {
            Expression::StaticMemberExpression(member) => {
                let obj = match &member.object {
                    Expression::Identifier(ident) => ident.name.as_str(),
                    _ => return Ok(None),
                };
                (obj, member.property.name.as_str())
            }
            _ => return Ok(None),
        };
        if obj_name != "Number" {
            return Ok(None);
        }

        self.expect_args(call, 1, &format!("Number.{method_name}"))?;
        let ty = self.emit_expr(call.arguments[0].to_expression())?;

        match method_name {
            "isNaN" => {
                match ty {
                    WasmType::F64 => {
                        let tmp = self.alloc_local(WasmType::F64);
                        self.push(Instruction::LocalTee(tmp));
                        self.push(Instruction::LocalGet(tmp));
                        self.push(Instruction::F64Ne);
                    }
                    WasmType::I32 => {
                        self.push(Instruction::Drop);
                        self.push(Instruction::I32Const(0));
                    }
                    _ => return Err(CompileError::type_err("Number.isNaN requires a numeric argument")),
                }
                Ok(Some(WasmType::I32))
            }
            "isFinite" => {
                match ty {
                    WasmType::F64 => {
                        let tmp = self.alloc_local(WasmType::F64);
                        self.push(Instruction::LocalTee(tmp));
                        self.push(Instruction::LocalGet(tmp));
                        self.push(Instruction::F64Sub);
                        self.push(Instruction::F64Const(0.0));
                        self.push(Instruction::F64Eq);
                    }
                    WasmType::I32 => {
                        self.push(Instruction::Drop);
                        self.push(Instruction::I32Const(1));
                    }
                    _ => return Err(CompileError::type_err("Number.isFinite requires a numeric argument")),
                }
                Ok(Some(WasmType::I32))
            }
            "isInteger" => {
                match ty {
                    WasmType::I32 => {
                        self.push(Instruction::Drop);
                        self.push(Instruction::I32Const(1));
                    }
                    WasmType::F64 => {
                        // finite(x) && x == floor(x)
                        // Compute (x - x == 0) && (x == trunc(x)). Use trunc so
                        // ±Inf maps to ±Inf (not itself after floor for Inf),
                        // and the finite guard is a separate check.
                        let x = self.alloc_local(WasmType::F64);
                        self.push(Instruction::LocalSet(x));
                        // finite check
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::F64Sub);
                        self.push(Instruction::F64Const(0.0));
                        self.push(Instruction::F64Eq);
                        // integer check: x == trunc(x)
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::F64Trunc);
                        self.push(Instruction::F64Eq);
                        self.push(Instruction::I32And);
                    }
                    _ => return Err(CompileError::type_err("Number.isInteger requires a numeric argument")),
                }
                Ok(Some(WasmType::I32))
            }
            "isSafeInteger" => {
                match ty {
                    WasmType::I32 => {
                        // All i32 fit in ±(2^53 − 1), so just `true`.
                        self.push(Instruction::Drop);
                        self.push(Instruction::I32Const(1));
                    }
                    WasmType::F64 => {
                        let x = self.alloc_local(WasmType::F64);
                        self.push(Instruction::LocalSet(x));
                        // finite(x)
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::F64Sub);
                        self.push(Instruction::F64Const(0.0));
                        self.push(Instruction::F64Eq);
                        // x == trunc(x)
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::F64Trunc);
                        self.push(Instruction::F64Eq);
                        self.push(Instruction::I32And);
                        // abs(x) <= 2^53 − 1
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::F64Abs);
                        self.push(Instruction::F64Const(9_007_199_254_740_991.0));
                        self.push(Instruction::F64Le);
                        self.push(Instruction::I32And);
                    }
                    _ => return Err(CompileError::type_err("Number.isSafeInteger requires a numeric argument")),
                }
                Ok(Some(WasmType::I32))
            }
            // Number.parseInt / Number.parseFloat — ES6 aliases for the global
            // parseInt/parseFloat. The arg is a string (i32 pointer); reuse the
            // existing __str_parseInt / __str_parseFloat helpers.
            "parseInt" | "parseFloat" => {
                if ty != WasmType::I32 {
                    return Err(CompileError::type_err(format!(
                        "Number.{method_name} requires a string argument"
                    )));
                }
                let helper = if method_name == "parseInt" { "__str_parseInt" } else { "__str_parseFloat" };
                let (func_idx, _) = self.module_ctx.get_func(helper).ok_or_else(|| {
                    CompileError::codegen(format!("{helper} helper not registered"))
                })?;
                self.push(Instruction::Call(func_idx));
                let ret = if method_name == "parseInt" { WasmType::I32 } else { WasmType::F64 };
                Ok(Some(ret))
            }
            _ => Err(CompileError::codegen(format!(
                "Number.{method_name} is not a supported builtin"
            ))),
        }
    }

    fn try_emit_math_call(&mut self, call: &CallExpression<'a>) -> Result<Option<WasmType>, CompileError> {
        let (obj_name, method_name) = match &call.callee {
            Expression::StaticMemberExpression(member) => {
                let obj = match &member.object {
                    Expression::Identifier(ident) => ident.name.as_str(),
                    _ => return Ok(None),
                };
                (obj, member.property.name.as_str())
            }
            _ => return Ok(None),
        };

        if obj_name != "Math" {
            return Ok(None);
        }

        match method_name {
            // Single-argument math functions -> WASM f64 instructions
            "sqrt" => {
                self.expect_args(call, 1, "Math.sqrt")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Sqrt);
                Ok(Some(WasmType::F64))
            }
            "abs" => {
                self.expect_args(call, 1, "Math.abs")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Abs);
                Ok(Some(WasmType::F64))
            }
            "ceil" => {
                self.expect_args(call, 1, "Math.ceil")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Ceil);
                Ok(Some(WasmType::F64))
            }
            "floor" => {
                self.expect_args(call, 1, "Math.floor")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Floor);
                Ok(Some(WasmType::F64))
            }
            "trunc" => {
                self.expect_args(call, 1, "Math.trunc")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Trunc);
                Ok(Some(WasmType::F64))
            }
            "nearest" => {
                self.expect_args(call, 1, "Math.nearest")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Nearest);
                Ok(Some(WasmType::F64))
            }
            // Math.round(x) rounds half toward +Infinity per JS spec.
            // Lowered to floor(x + 0.5) — does NOT use F64Nearest (half-to-even).
            "round" => {
                self.expect_args(call, 1, "Math.round")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Const(0.5f64.into()));
                self.push(Instruction::F64Add);
                self.push(Instruction::F64Floor);
                Ok(Some(WasmType::F64))
            }
            // Two-argument math functions -> WASM f64 instructions
            "min" => {
                self.expect_args(call, 2, "Math.min")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.emit_expr(call.arguments[1].to_expression())?;
                self.push(Instruction::F64Min);
                Ok(Some(WasmType::F64))
            }
            "max" => {
                self.expect_args(call, 2, "Math.max")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.emit_expr(call.arguments[1].to_expression())?;
                self.push(Instruction::F64Max);
                Ok(Some(WasmType::F64))
            }
            // copysign
            "copysign" => {
                self.expect_args(call, 2, "Math.copysign")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.emit_expr(call.arguments[1].to_expression())?;
                self.push(Instruction::F64Copysign);
                Ok(Some(WasmType::F64))
            }
            // Math.sign(x): preserves ±0 and NaN; returns ±1 otherwise.
            // Evaluates x once via a local to stay correct with side-effecting args.
            "sign" => {
                self.expect_args(call, 1, "Math.sign")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                let l = self.alloc_local(WasmType::F64);
                self.push(Instruction::LocalSet(l));
                // val1 = x (used when x is ±0 or NaN)
                self.push(Instruction::LocalGet(l));
                // val2 = copysign(1.0, x)
                self.push(Instruction::F64Const(1.0));
                self.push(Instruction::LocalGet(l));
                self.push(Instruction::F64Copysign);
                // cond = (x == 0.0) | (x != x)
                self.push(Instruction::LocalGet(l));
                self.push(Instruction::F64Const(0.0));
                self.push(Instruction::F64Eq);
                self.push(Instruction::LocalGet(l));
                self.push(Instruction::LocalGet(l));
                self.push(Instruction::F64Ne);
                self.push(Instruction::I32Or);
                self.push(Instruction::Select);
                Ok(Some(WasmType::F64))
            }
            // Math.hypot(x, y): naive sqrt(x*x + y*y).
            // Note: overflows when x*x or y*y exceed f64 range. Adequate for
            // typical game-space coordinates; if you need libm's scaled
            // algorithm, call __tscc_hypot directly under --math=libm.
            "hypot" => {
                self.expect_args(call, 2, "Math.hypot")?;
                let lx = self.alloc_local(WasmType::F64);
                let ly = self.alloc_local(WasmType::F64);
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::LocalSet(lx));
                self.emit_expr(call.arguments[1].to_expression())?;
                self.push(Instruction::LocalSet(ly));
                self.push(Instruction::LocalGet(lx));
                self.push(Instruction::LocalGet(lx));
                self.push(Instruction::F64Mul);
                self.push(Instruction::LocalGet(ly));
                self.push(Instruction::LocalGet(ly));
                self.push(Instruction::F64Mul);
                self.push(Instruction::F64Add);
                self.push(Instruction::F64Sqrt);
                Ok(Some(WasmType::F64))
            }
            // Math.fround(x): round x to the nearest 32-bit float. Emitted as
            // an f64→f32→f64 round-trip, which is bit-exact to the ECMA-262
            // definition of fround on any platform (IEEE-754 round-nearest).
            "fround" => {
                self.expect_args(call, 1, "Math.fround")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F32DemoteF64);
                self.push(Instruction::F64PromoteF32);
                Ok(Some(WasmType::F64))
            }
            // Math.clz32(x): number of leading zero bits in the 32-bit int
            // representation of x. ECMA coerces x via ToUint32; we require an
            // i32 input in our typed world and emit `i32.clz` directly.
            "clz32" => {
                self.expect_args(call, 1, "Math.clz32")?;
                let ty = self.emit_expr(call.arguments[0].to_expression())?;
                match ty {
                    WasmType::I32 => {}
                    WasmType::F64 => self.push(Instruction::I32TruncSatF64S),
                    _ => return Err(CompileError::type_err("Math.clz32 requires a numeric argument")),
                }
                self.push(Instruction::I32Clz);
                Ok(Some(WasmType::I32))
            }
            // Math.imul(a, b): C-style 32-bit multiply. Operands are truncated
            // to i32 (if f64), then multiplied with wraparound (i32.mul).
            "imul" => {
                self.expect_args(call, 2, "Math.imul")?;
                for arg in &call.arguments {
                    let ty = self.emit_expr(arg.to_expression())?;
                    match ty {
                        WasmType::I32 => {}
                        WasmType::F64 => self.push(Instruction::I32TruncSatF64S),
                        _ => return Err(CompileError::type_err("Math.imul requires numeric arguments")),
                    }
                }
                self.push(Instruction::I32Mul);
                Ok(Some(WasmType::I32))
            }
            // Math.random() — call the lazily-emitted PCG32 step function.
            // Embedder controls the seed via the exported `__rng_state` global.
            "random" => {
                self.expect_args(call, 0, "Math.random")?;
                let (func_idx, _) = self.module_ctx
                    .get_func(super::math_builtins::RNG_NEXT_FUNC)
                    .expect("__rng_next not registered — scanner missed Math.random()");
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::F64))
            }
            // Transcendentals (sin, cos, log, exp, pow, ...): lower to host
            // imports declared in Pass 1a (see codegen/module.rs). The scanner
            // ensures only referenced ones are imported, so get_func will
            // succeed here for any transcendental that reached codegen.
            other if super::math_builtins::is_transcendental(other) => {
                let arity = super::math_builtins::MATH_TRANSCENDENTALS
                    .iter()
                    .find(|(n, _)| *n == other)
                    .map(|(_, a)| *a as usize)
                    .unwrap();
                self.expect_args(call, arity, &format!("Math.{other}"))?;
                for arg in &call.arguments {
                    self.emit_expr(arg.to_expression())?;
                }
                let import_name = super::math_builtins::import_name(other);
                let (func_idx, _) = self.module_ctx.get_func(&import_name).unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::F64))
            }
            _ => Err(CompileError::unsupported(format!(
                "Math.{method_name} is not a supported builtin"
            ))),
        }
    }

    fn expect_args(&self, call: &CallExpression, expected: usize, name: &str) -> Result<(), CompileError> {
        if call.arguments.len() != expected {
            return Err(CompileError::codegen(format!(
                "{name} expects {expected} argument(s), got {}", call.arguments.len()
            )));
        }
        Ok(())
    }

    fn emit_static_alloc(&mut self, call: &CallExpression<'a>) -> Result<WasmType, CompileError> {
        // __static_alloc(size) -> i32 constant (compile-time offset)
        let size = match &call.arguments[0].to_expression() {
            Expression::NumericLiteral(lit) => lit.value as u32,
            _ => return Err(CompileError::codegen("__static_alloc size must be a numeric literal")),
        };
        let offset = self.module_ctx.alloc_static(size);
        self.push(Instruction::I32Const(offset as i32));
        Ok(WasmType::I32)
    }

    fn emit_assignment(&mut self, assign: &AssignmentExpression<'a>) -> Result<WasmType, CompileError> {
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
            return Err(CompileError::type_err(format!("cannot assign to const variable '{target_name}'")));
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
            return self.emit_global_assign(target_name, g_idx, g_ty, &assign.right, assign.operator);
        }

        let &(idx, _local_ty) = self.locals.get(target_name)
            .ok_or_else(|| CompileError::codegen(format!("undefined variable '{target_name}'")))?;
        let is_boxed = self.boxed_var_types.contains_key(target_name);
        let ty = if is_boxed { *self.boxed_var_types.get(target_name).unwrap() } else { _local_ty };

        // Helper closure-like pattern: emit the value to store, then write it
        match assign.operator {
            AssignmentOperator::Assign => {
                if is_boxed {
                    self.push(Instruction::LocalGet(idx)); // ptr
                }
                let expr_ty = self.emit_expr(&assign.right)?;
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
            AssignmentOperator::Addition | AssignmentOperator::Subtraction |
            AssignmentOperator::Multiplication | AssignmentOperator::Division => {
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
                "assignment operator {:?}", assign.operator
            ))),
        }
    }

    /// Emit assignment to a mutable WASM global.
    fn emit_global_assign(
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

    fn emit_update(&mut self, update: &UpdateExpression<'a>) -> Result<WasmType, CompileError> {
        let name = match &update.argument {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(ident) => ident.name.as_str(),
            _ => return Err(CompileError::unsupported("complex update target")),
        };

        // Check const
        if self.const_locals.contains(name) {
            return Err(CompileError::type_err(format!("cannot modify const variable '{name}'")));
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

        let &(idx, _local_ty) = self.locals.get(name)
            .ok_or_else(|| CompileError::codegen(format!("undefined variable '{name}'")))?;
        let is_boxed = self.boxed_var_types.contains_key(name);
        let ty = if is_boxed { *self.boxed_var_types.get(name).unwrap() } else { _local_ty };

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

    // ---- Phase 3: Classes ----

    fn emit_new(&mut self, new_expr: &NewExpression<'a>) -> Result<WasmType, CompileError> {
        let class_name = match &new_expr.callee {
            Expression::Identifier(ident) => ident.name.as_str(),
            _ => return Err(CompileError::unsupported("non-identifier new target")),
        };

        // Handle new Array<T>(capacity)
        if class_name == "Array" {
            return self.emit_new_array(new_expr);
        }

        let layout = self.module_ctx.class_registry.get(class_name)
            .ok_or_else(|| CompileError::codegen(format!("unknown class '{class_name}'")))?;
        let size = layout.size;

        // Allocate object via arena
        self.push(Instruction::I32Const(size as i32));
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        // Write vtable pointer at offset 0 for polymorphic classes
        if layout.is_polymorphic && !layout.vtable_methods.is_empty() {
            self.push(Instruction::LocalGet(ptr_local));
            self.push(Instruction::I32Const(layout.vtable_offset as i32));
            self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
        }

        // Call constructor if it exists
        let ctor_key = format!("{class_name}.constructor");
        if let Some(&(func_idx, _)) = self.module_ctx.method_map.get(&ctor_key) {
            // Push this pointer
            self.push(Instruction::LocalGet(ptr_local));
            // Push constructor arguments
            for arg in &new_expr.arguments {
                self.emit_expr(arg.to_expression())?;
            }
            self.push(Instruction::Call(func_idx));
            self.push(Instruction::Drop); // constructor returns this, but we already have it
        }

        // Return pointer
        self.push(Instruction::LocalGet(ptr_local));
        Ok(WasmType::I32)
    }

    /// Emit `new Array<T>(capacity)` — arena-allocate array with header + element space.
    /// Layout: [length: i32 (4B)] [capacity: i32 (4B)] [elements...]
    fn emit_new_array(&mut self, new_expr: &NewExpression<'a>) -> Result<WasmType, CompileError> {
        if new_expr.arguments.len() != 1 {
            return Err(CompileError::codegen("new Array<T>(capacity) requires exactly 1 argument"));
        }

        // Determine element type from type_parameters on the NewExpression
        let elem_type = self.resolve_new_array_elem_type(new_expr)?;
        let elem_size: u32 = match elem_type {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("Array element type must be i32 or f64")),
        };

        // Evaluate capacity argument
        let cap_local = self.alloc_local(WasmType::I32);
        let arg_ty = self.emit_expr(new_expr.arguments[0].to_expression())?;
        if arg_ty != WasmType::I32 {
            return Err(CompileError::type_err("Array capacity must be i32"));
        }
        self.push(Instruction::LocalSet(cap_local));

        // Compute total size: 8 (header) + capacity * elem_size
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(cap_local));
        self.push(Instruction::I32Const(elem_size as i32));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        // Store length = 0 at ptr+0
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));

        // Store capacity at ptr+4
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::LocalGet(cap_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // Return pointer
        self.push(Instruction::LocalGet(ptr_local));
        Ok(WasmType::I32)
    }

    /// Extract the element type from `new Array<T>(...)` type parameters.
    fn resolve_new_array_elem_type(&self, new_expr: &NewExpression<'a>) -> Result<WasmType, CompileError> {
        if let Some(type_params) = &new_expr.type_arguments
            && let Some(first) = type_params.params.first() {
                return crate::types::resolve_ts_type(first, &self.module_ctx.class_names);
            }
        Err(CompileError::type_err("new Array requires a type parameter: new Array<f64>(n)"))
    }

    fn emit_member_access(&mut self, member: &StaticMemberExpression<'a>) -> Result<WasmType, CompileError> {
        // Check for enum member access: EnumName.MemberName → global lookup
        if let Expression::Identifier(obj_ident) = &member.object {
            let enum_member_key = format!("{}.{}", obj_ident.name.as_str(), member.property.name.as_str());
            if let Some(&(idx, ty)) = self.module_ctx.globals.get(&enum_member_key) {
                self.push(Instruction::GlobalGet(idx));
                return Ok(ty);
            }

            // Math.<CONSTANT> → inline f64 literal (ECMAScript standard values)
            if obj_ident.name.as_str() == "Math"
                && let Some(val) = math_constant(member.property.name.as_str()) {
                self.push(Instruction::F64Const(val));
                return Ok(WasmType::F64);
            }
            // Number.<CONSTANT> → inline f64 literal
            if obj_ident.name.as_str() == "Number"
                && let Some(val) = number_constant(member.property.name.as_str()) {
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
        let layout = self.module_ctx.class_registry.get(&class_name)
            .ok_or_else(|| CompileError::codegen(format!("unknown class '{class_name}'")))?;

        let &(offset, ty) = layout.field_map.get(field_name)
            .ok_or_else(|| CompileError::codegen(format!(
                "class '{class_name}' has no field '{field_name}'"
            )))?;

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

    fn emit_member_assign(
        &mut self,
        member: &StaticMemberExpression<'a>,
        value: &Expression<'a>,
        operator: AssignmentOperator,
    ) -> Result<WasmType, CompileError> {
        let field_name = member.property.name.as_str();
        let class_name = self.resolve_expr_class(&member.object)?;
        let layout = self.module_ctx.class_registry.get(&class_name)
            .ok_or_else(|| CompileError::codegen(format!("unknown class '{class_name}'")))?;
        let &(offset, ty) = layout.field_map.get(field_name)
            .ok_or_else(|| CompileError::codegen(format!(
                "class '{class_name}' has no field '{field_name}'"
            )))?;

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
                WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg { offset: offset as u64, align: 3, memory_index: 0 })),
                WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: offset as u64, align: 2, memory_index: 0 })),
                _ => return Err(CompileError::codegen("void field")),
            }
            self.emit_expr(value)?;
            let is_f64 = ty == WasmType::F64;
            match operator {
                AssignmentOperator::Addition => self.push(if is_f64 { Instruction::F64Add } else { Instruction::I32Add }),
                AssignmentOperator::Subtraction => self.push(if is_f64 { Instruction::F64Sub } else { Instruction::I32Sub }),
                AssignmentOperator::Multiplication => self.push(if is_f64 { Instruction::F64Mul } else { Instruction::I32Mul }),
                AssignmentOperator::Division => self.push(if is_f64 { Instruction::F64Div } else { Instruction::I32DivS }),
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
            WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg { offset: offset as u64, align: 3, memory_index: 0 })),
            WasmType::I32 => self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: offset as u64, align: 2, memory_index: 0 })),
            _ => return Err(CompileError::codegen("void field store")),
        }

        Ok(WasmType::Void)
    }

    // ---- Phase 4: Arrays ----

    /// Emit arr.length (load i32 at arr+0)
    fn emit_array_property(&mut self, member: &StaticMemberExpression<'a>, prop: &str) -> Result<WasmType, CompileError> {
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
    fn try_emit_array_method_call(&mut self, call: &CallExpression<'a>) -> Result<Option<WasmType>, CompileError> {
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
                    return Err(CompileError::codegen("Array.push() expects exactly 1 argument"));
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
                self.expect_args(call, 1, "Array.indexOf")?;
                self.emit_array_index_of(&member.object, elem_ty, call.arguments[0].to_expression(), false)?;
                Ok(Some(WasmType::I32))
            }
            "lastIndexOf" => {
                self.expect_args(call, 1, "Array.lastIndexOf")?;
                self.emit_array_index_of(&member.object, elem_ty, call.arguments[0].to_expression(), true)?;
                Ok(Some(WasmType::I32))
            }
            "includes" => {
                self.expect_args(call, 1, "Array.includes")?;
                self.emit_array_index_of(&member.object, elem_ty, call.arguments[0].to_expression(), false)?;
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
            "at" => {
                self.expect_args(call, 1, "Array.at")?;
                self.emit_array_at(&member.object, elem_ty, call.arguments[0].to_expression())?;
                Ok(Some(elem_ty))
            }
            "fill" => {
                if !matches!(call.arguments.len(), 1 | 2 | 3) {
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
                self.expect_args(call, 1, "Array.concat")?;
                self.emit_array_concat(&member.object, elem_ty, call.arguments[0].to_expression())?;
                Ok(Some(WasmType::I32))
            }
            "join" => {
                if !matches!(call.arguments.len(), 0 | 1) {
                    return Err(CompileError::codegen("Array.join expects 0 or 1 arguments"));
                }
                self.emit_array_join(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            _ => Err(CompileError::codegen(format!(
                "Array has no method '{method_name}' — supported: push, pop, indexOf, lastIndexOf, includes, reverse, at, fill, slice, concat, join, filter, map, forEach, reduce, sort, find, findIndex, findLast, findLastIndex, some, every"
            ))),
        }
    }

    /// Emit arr.push(val) — store at end, increment length. Grows array via arena reallocation if at capacity.
    fn emit_array_push(&mut self, arr_expr: &Expression<'a>, elem_ty: WasmType, val_expr: &Expression<'a>) -> Result<(), CompileError> {
        let elem_size: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };

        let arena_idx = self.module_ctx.arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

        // Evaluate array pointer
        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(arr_local));

        // Load current length
        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
        self.push(Instruction::LocalSet(len_local));

        // Load current capacity
        let cap_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 4, align: 2, memory_index: 0 }));
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
            self.push(Instruction::If(wasm_encoder::BlockType::Result(ValType::I32)));
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
                self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 4, align: 2, memory_index: 0 }));
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
                self.push(Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

                // Write header: new_ptr[0] = length
                self.push(Instruction::LocalGet(new_ptr_local));
                self.push(Instruction::LocalGet(len_local));
                self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));

                // Write header: new_ptr[4] = new_cap
                self.push(Instruction::LocalGet(new_ptr_local));
                self.push(Instruction::LocalGet(new_cap_local));
                self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 4, align: 2, memory_index: 0 }));

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

        // Store value
        self.push(Instruction::LocalGet(addr_local));
        self.emit_expr(val_expr)?;
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
            WasmType::I32 => self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
            _ => unreachable!(),
        }

        // Increment length: arr.length = length + 1
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));

        Ok(())
    }

    /// Emit `arr.pop()` — returns the last element and shrinks length by one.
    /// On an empty array we return a default value (0 / 0.0) to mirror the
    /// JS contract of "undefined on empty" without introducing a tagged type.
    fn emit_array_pop(&mut self, arr_expr: &Expression<'a>, elem_ty: WasmType) -> Result<(), CompileError> {
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
        self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
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
        self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));

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
            WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
            _ => self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
        }

        self.push(Instruction::End);
        Ok(())
    }

    /// Emit `arr.indexOf(x)` or `arr.lastIndexOf(x)` — linear scan returning
    /// the first (or last, if `reverse`) matching index, or -1 when absent.
    /// Uses strict equality: f64 compares via F64Eq (so NaN ≠ NaN, matching JS).
    fn emit_array_index_of(&mut self, arr_expr: &Expression<'a>, elem_ty: WasmType, needle_expr: &Expression<'a>, reverse: bool) -> Result<(), CompileError> {
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };

        // Evaluate needle once
        let needle_local = self.alloc_local(elem_ty);
        self.emit_expr(needle_expr)?;
        self.push(Instruction::LocalSet(needle_local));

        // Evaluate array
        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(arr_local));

        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
        self.push(Instruction::LocalSet(len_local));

        let i_local = self.alloc_local(WasmType::I32);
        let result_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(-1));
        self.push(Instruction::LocalSet(result_local));

        // i = reverse ? len-1 : 0
        if reverse {
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::I32Const(1));
            self.push(Instruction::I32Sub);
        } else {
            self.push(Instruction::I32Const(0));
        }
        self.push(Instruction::LocalSet(i_local));

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
            WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
            _ => self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
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
    fn emit_array_reverse(&mut self, arr_expr: &Expression<'a>, elem_ty: WasmType) -> Result<(), CompileError> {
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
        self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
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
            WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
            _ => self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
        }
        self.push(Instruction::LocalSet(tmp_a));
        emit_addr(self, hi);
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
            _ => self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
        }
        self.push(Instruction::LocalSet(tmp_b));

        // arr[lo] = tmp_b
        emit_addr(self, lo);
        self.push(Instruction::LocalGet(tmp_b));
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
            _ => self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
        }
        // arr[hi] = tmp_a
        emit_addr(self, hi);
        self.push(Instruction::LocalGet(tmp_a));
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
            _ => self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
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

    /// Emit `arr.at(i)` — negative-index lookup. Traps on out-of-range to
    /// match our bounds-check posture; callers wanting "undefined on OOB" can
    /// guard with length first.
    fn emit_array_at(&mut self, arr_expr: &Expression<'a>, elem_ty: WasmType, idx_expr: &Expression<'a>) -> Result<(), CompileError> {
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
        self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
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
            WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
            _ => self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
        }
        Ok(())
    }

    /// Emit `arr.fill(value, start?, end?)` — in-place, leaves arr pointer
    /// on the stack. Negative start/end indices are normalized by adding len.
    fn emit_array_fill(&mut self, arr_expr: &Expression<'a>, elem_ty: WasmType, call: &CallExpression<'a>) -> Result<(), CompileError> {
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
        self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
        self.push(Instruction::LocalSet(len_local));

        let start_local = self.alloc_local(WasmType::I32);
        let end_local = self.alloc_local(WasmType::I32);

        // start default = 0
        if call.arguments.len() >= 2 {
            let ty = self.emit_expr(call.arguments[1].to_expression())?;
            if ty == WasmType::F64 { self.push(Instruction::I32TruncF64S); }
        } else {
            self.push(Instruction::I32Const(0));
        }
        self.push(Instruction::LocalSet(start_local));
        // end default = len
        if call.arguments.len() == 3 {
            let ty = self.emit_expr(call.arguments[2].to_expression())?;
            if ty == WasmType::F64 { self.push(Instruction::I32TruncF64S); }
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
            WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
            _ => self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
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
    fn emit_array_slice(&mut self, arr_expr: &Expression<'a>, elem_ty: WasmType, call: &CallExpression<'a>) -> Result<(), CompileError> {
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };
        let arena_idx = self.module_ctx.arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(arr_local));

        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
        self.push(Instruction::LocalSet(len_local));

        let start_local = self.alloc_local(WasmType::I32);
        let end_local = self.alloc_local(WasmType::I32);
        if !call.arguments.is_empty() {
            let ty = self.emit_expr(call.arguments[0].to_expression())?;
            if ty == WasmType::F64 { self.push(Instruction::I32TruncF64S); }
        } else {
            self.push(Instruction::I32Const(0));
        }
        self.push(Instruction::LocalSet(start_local));
        if call.arguments.len() == 2 {
            let ty = self.emit_expr(call.arguments[1].to_expression())?;
            if ty == WasmType::F64 { self.push(Instruction::I32TruncF64S); }
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
        self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 4, align: 2, memory_index: 0 }));

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
        self.push(Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

        self.push(Instruction::LocalGet(new_ptr));
        Ok(())
    }

    /// Emit `arr.concat(other)` — new array = this + other. Only the
    /// single-argument, same-element-type form is supported (richer overloads
    /// can be layered via the closure builtins in a later pass).
    fn emit_array_concat(&mut self, arr_expr: &Expression<'a>, elem_ty: WasmType, other_expr: &Expression<'a>) -> Result<(), CompileError> {
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };
        let arena_idx = self.module_ctx.arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

        let a_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(a_local));
        let b_local = self.alloc_local(WasmType::I32);
        self.emit_expr(other_expr)?;
        self.push(Instruction::LocalSet(b_local));

        let a_len = self.alloc_local(WasmType::I32);
        let b_len = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(a_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
        self.push(Instruction::LocalSet(a_len));
        self.push(Instruction::LocalGet(b_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
        self.push(Instruction::LocalSet(b_len));

        let total_len = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(a_len));
        self.push(Instruction::LocalGet(b_len));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(total_len));

        // Allocate
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

        // Header
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(total_len));
        self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(total_len));
        self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 4, align: 2, memory_index: 0 }));

        // Copy a
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(a_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(a_len));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
        // Copy b
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(a_len));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(b_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(b_len));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

        self.push(Instruction::LocalGet(new_ptr));
        Ok(())
    }

    /// Emit `arr.join(sep?)` — stringifies each element (i32 via __str_from_i32,
    /// f64 via __str_from_f64, string elements pass through) and concatenates
    /// with `sep` (default ",") between them.
    fn emit_array_join(&mut self, arr_expr: &Expression<'a>, elem_ty: WasmType, call: &CallExpression<'a>) -> Result<(), CompileError> {
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
        let to_str_idx = self.module_ctx.get_func(to_str_helper)
            .ok_or_else(|| CompileError::codegen(format!(
                "Array.join requires {to_str_helper} — ensure string runtime is registered"
            )))?.0;
        let concat_idx = self.module_ctx.get_func("__str_concat")
            .ok_or_else(|| CompileError::codegen("Array.join requires __str_concat"))?.0;

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
        self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
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
            WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
            _ => self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
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

    /// Emit arr[i] — bounds-checked element read.
    fn emit_computed_member_access(&mut self, member: &ComputedMemberExpression<'a>) -> Result<WasmType, CompileError> {
        // String indexing: str[i] → i32 char code (byte value)
        if self.resolve_expr_is_string(&member.object) {
            return self.emit_string_index(member);
        }

        let elem_ty = self.resolve_expr_array_elem(&member.object)
            .ok_or_else(|| CompileError::codegen(
                "computed member access (arr[i]) only supported on Array<T> or string"
            ))?;
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

    /// Emit bounds check: if index >= arr.length, unreachable (trap).
    fn emit_array_bounds_check(&mut self, arr_local: u32, idx_local: u32) {
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

    /// Emit arr[i] = val — bounds-checked element write.
    fn emit_computed_member_assign(
        &mut self,
        member: &ComputedMemberExpression<'a>,
        value: &Expression<'a>,
        operator: AssignmentOperator,
    ) -> Result<WasmType, CompileError> {
        let elem_ty = self.resolve_expr_array_elem(&member.object)
            .ok_or_else(|| CompileError::codegen(
                "computed member assignment (arr[i] = val) only supported on Array<T>"
            ))?;
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
                WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
                WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
                _ => unreachable!(),
            }
            self.emit_expr(value)?;
            let is_f64 = elem_ty == WasmType::F64;
            match operator {
                AssignmentOperator::Addition => self.push(if is_f64 { Instruction::F64Add } else { Instruction::I32Add }),
                AssignmentOperator::Subtraction => self.push(if is_f64 { Instruction::F64Sub } else { Instruction::I32Sub }),
                AssignmentOperator::Multiplication => self.push(if is_f64 { Instruction::F64Mul } else { Instruction::I32Mul }),
                AssignmentOperator::Division => self.push(if is_f64 { Instruction::F64Div } else { Instruction::I32DivS }),
                _ => return Err(CompileError::unsupported("compound array element assignment")),
            }
        }

        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
            WasmType::I32 => self.push(Instruction::I32Store(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
            _ => unreachable!(),
        }

        Ok(WasmType::Void)
    }

    /// Resolve the array element type for an expression (if it's a known array variable or
    /// the result of an array-returning operation like .filter() or .map()).
    pub fn resolve_expr_array_elem(&self, expr: &Expression<'a>) -> Option<WasmType> {
        match expr {
            Expression::Identifier(ident) => {
                self.local_array_elem_types.get(ident.name.as_str()).copied()
            }
            // arr.filter() / arr.sort() return arrays with the same element type as source
            Expression::CallExpression(call) => {
                if let Expression::StaticMemberExpression(member) = &call.callee {
                    let method = member.property.name.as_str();
                    match method {
                        "filter" | "sort" => self.resolve_expr_array_elem(&member.object),
                        "map" => {
                            // map changes the element type — infer from arrow return
                            if let Some(arg) = call.arguments.first()
                                && let Some(arrow) = self.try_extract_arrow_expr(arg.to_expression()) {
                                    let src_elem = self.resolve_expr_array_elem(&member.object)?;
                                    let src_class = self.resolve_expr_array_elem_class(&member.object);
                                    let params = arrow.params.items.iter()
                                        .filter_map(|p| match &p.pattern {
                                            BindingPattern::BindingIdentifier(id) => Some(id.name.as_str().to_string()),
                                            _ => None,
                                        })
                                        .collect::<Vec<_>>();
                                    return self.infer_arrow_result_type(arrow, &params, src_elem, src_class.as_deref()).ok();
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

    /// Resolve the array element class name for an expression (if elements are class instances).
    pub fn resolve_expr_array_elem_class(&self, expr: &Expression<'a>) -> Option<String> {
        match expr {
            Expression::Identifier(ident) => {
                self.local_array_elem_classes.get(ident.name.as_str()).cloned()
            }
            // Chained calls: arr.filter() preserves element class
            Expression::CallExpression(call) => {
                if let Expression::StaticMemberExpression(member) = &call.callee {
                    let method = member.property.name.as_str();
                    match method {
                        "filter" | "sort" => self.resolve_expr_array_elem_class(&member.object),
                        _ => None,
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Helper: extract ArrowFunctionExpression from an expression (unwrapping parens).
    fn try_extract_arrow_expr<'b>(&self, expr: &'b Expression<'a>) -> Option<&'b ArrowFunctionExpression<'a>> {
        match expr {
            Expression::ArrowFunctionExpression(arrow) => Some(arrow),
            Expression::ParenthesizedExpression(paren) => self.try_extract_arrow_expr(&paren.expression),
            _ => None,
        }
    }

    /// Emit optional chaining: `target?.hp` → `if target != 0 { target.hp } else { 0 }`
    fn emit_chain_expression(&mut self, chain: &ChainExpression<'a>) -> Result<WasmType, CompileError> {
        match &chain.expression {
            ChainElement::StaticMemberExpression(member) => {
                self.emit_optional_member_access(member)
            }
            ChainElement::ComputedMemberExpression(member) => {
                // target?.[i] — optional computed access
                let elem_ty = self.resolve_expr_array_elem(&member.object)
                    .ok_or_else(|| CompileError::codegen(
                        "optional computed access (?.[]) only supported on Array<T>"
                    ))?;

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
                    WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
                    WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
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
                    _ => return Err(CompileError::unsupported(
                        "optional call must be `obj?.method(...)` on a class instance"
                    )),
                };
                self.emit_optional_method_call(member, call)
            }
            _ => Err(CompileError::unsupported("unsupported chain expression type")),
        }
    }

    /// Emit `obj?.method(args)` — null-safe method call.
    /// If obj is 0 (null), returns the zero value of the method's return type;
    /// otherwise dispatches to the method (static or vtable) without re-evaluating obj.
    fn emit_optional_method_call(
        &mut self,
        member: &StaticMemberExpression<'a>,
        call: &CallExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        // Resolve class + method to learn the return type (for the if-block's
        // result signature and for picking a zero value on the null branch).
        let class_name = self.resolve_expr_class(&member.object)
            .map_err(|_| CompileError::unsupported(
                "optional method call requires a statically-typed class receiver"
            ))?;
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
                match self.module_ctx.class_registry.get(&cur).and_then(|l| l.parent.clone()) {
                    Some(p) => cur = p,
                    None => break,
                }
            }
            found.ok_or_else(|| CompileError::codegen(format!(
                "class '{class_name}' has no method '{method_name}'"
            )))?
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
    fn emit_optional_member_access(&mut self, member: &StaticMemberExpression<'a>) -> Result<WasmType, CompileError> {
        let field_name = member.property.name.as_str();

        // Check if this is an array .length access
        if let Some(_elem_ty) = self.resolve_expr_array_elem(&member.object)
            && field_name == "length" {
                // target?.length
                let obj_local = self.alloc_local(WasmType::I32);
                self.emit_expr(&member.object)?;
                self.push(Instruction::LocalTee(obj_local));
                self.push(Instruction::If(wasm_encoder::BlockType::Result(wasm_encoder::ValType::I32)));
                self.push(Instruction::LocalGet(obj_local));
                self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
                self.push(Instruction::Else);
                self.push(Instruction::I32Const(0));
                self.push(Instruction::End);
                return Ok(WasmType::I32);
            }

        // Resolve class and field info
        let class_name = self.resolve_expr_class(&member.object)?;
        let layout = self.module_ctx.class_registry.get(&class_name)
            .ok_or_else(|| CompileError::codegen(format!("unknown class '{class_name}'")))?;
        let &(offset, field_ty) = layout.field_map.get(field_name)
            .ok_or_else(|| CompileError::codegen(format!(
                "class '{class_name}' has no field '{field_name}'"
            )))?;

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
                offset: offset as u64, align: 3, memory_index: 0,
            })),
            WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: offset as u64, align: 2, memory_index: 0,
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

    /// Emit ternary: `cond ? then : else`
    /// Emit `expr as T` — type cast via TSAsExpression.
    fn emit_as_cast(&mut self, as_expr: &TSAsExpression<'a>) -> Result<WasmType, CompileError> {
        // Check for class downcast/upcast first
        let target_class = crate::types::get_class_type_name_from_ts_type(&as_expr.type_annotation);
        if let Some(ref target_name) = target_class
            && self.module_ctx.class_names.contains(target_name) {
                // Class cast — validate hierarchy
                if let Ok(src_class) = self.resolve_expr_class(&as_expr.expression) {
                    let reg = &self.module_ctx.class_registry;
                    let valid = src_class == *target_name
                        || reg.is_subclass_of(target_name, &src_class)  // downcast
                        || reg.is_subclass_of(&src_class, target_name); // upcast
                    if !valid {
                        return Err(CompileError::type_err(format!(
                            "cannot cast '{src_class}' to '{target_name}': not in the same inheritance hierarchy"
                        )));
                    }
                }
                // Emit the expression (pointer) — no WASM instructions needed
                self.emit_expr(&as_expr.expression)?;
                return Ok(WasmType::I32);
            }

        let src_ty = self.emit_expr(&as_expr.expression)?;
        let target_ty = crate::types::resolve_ts_type(&as_expr.type_annotation, &self.module_ctx.class_names)?;

        match (src_ty, target_ty) {
            (a, b) if a == b => Ok(a), // no-op cast
            (WasmType::I32, WasmType::F64) => {
                self.push(Instruction::F64ConvertI32S);
                Ok(WasmType::F64)
            }
            (WasmType::F64, WasmType::I32) => {
                self.push(Instruction::I32TruncF64S);
                Ok(WasmType::I32)
            }
            _ => Err(CompileError::type_err(format!(
                "unsupported cast: {src_ty:?} as {target_ty:?}"
            ))),
        }
    }

    fn emit_conditional(&mut self, cond: &ConditionalExpression<'a>) -> Result<WasmType, CompileError> {
        self.emit_expr(&cond.test)?;

        self.push(Instruction::If(wasm_encoder::BlockType::Empty));

        let then_ty = self.emit_expr(&cond.consequent)?;
        let result_local = self.alloc_local(then_ty);
        self.push(Instruction::LocalSet(result_local));

        self.push(Instruction::Else);

        let else_ty = self.emit_expr(&cond.alternate)?;
        if else_ty != then_ty {
            return Err(CompileError::type_err(format!(
                "ternary branches have different types: {then_ty:?} vs {else_ty:?}"
            )));
        }
        self.push(Instruction::LocalSet(result_local));

        self.push(Instruction::End);

        self.push(Instruction::LocalGet(result_local));
        Ok(then_ty)
    }

    fn emit_this(&mut self) -> Result<WasmType, CompileError> {
        if self.this_class.is_none() {
            return Err(CompileError::codegen("`this` used outside of a method"));
        }
        // `this` is always local 0 in methods
        self.push(Instruction::LocalGet(0));
        Ok(WasmType::I32)
    }

    fn try_emit_method_call(&mut self, call: &CallExpression<'a>) -> Result<Option<WasmType>, CompileError> {
        let member = match &call.callee {
            Expression::StaticMemberExpression(m) => m,
            _ => return Ok(None),
        };

        let method_name = member.property.name.as_str();

        // Resolve the class of the object
        let class_name = match self.resolve_expr_class(&member.object) {
            Ok(name) => name,
            Err(_) => return Ok(None), // Not a class method call, let it fall through
        };

        // Look up the method — may be inherited from a parent class.
        // Walk up the parent chain checking method_map (which has entries only for declared methods).
        let (func_idx, ret_ty) = {
            let mut found = None;
            let mut cur = class_name.clone();
            loop {
                let key = format!("{cur}.{method_name}");
                if let Some(&v) = self.module_ctx.method_map.get(&key) {
                    found = Some(v);
                    break;
                }
                if let Some(layout) = self.module_ctx.class_registry.get(&cur) {
                    if let Some(ref parent) = layout.parent {
                        cur = parent.clone();
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            found.ok_or_else(|| CompileError::codegen(format!(
                "class '{class_name}' has no method '{method_name}'"
            )))?
        };

        // Check if this class is polymorphic (uses vtable dispatch)
        let layout = self.module_ctx.class_registry.get(&class_name);
        let is_polymorphic = layout.is_some_and(|l| l.is_polymorphic);

        if is_polymorphic {
            // Vtable dispatch via call_indirect
            let vtable_slot = layout.unwrap().vtable_method_map.get(method_name)
                .ok_or_else(|| CompileError::codegen(format!(
                    "method '{method_name}' not in vtable of '{class_name}'"
                )))?;

            // Emit this pointer, save to temp for vtable lookup. If an optional-call
            // override is set, use the pre-evaluated receiver local instead.
            if let Some(recv_local) = self.method_receiver_override {
                self.push(Instruction::LocalGet(recv_local));
            } else {
                self.emit_expr(&member.object)?;
            }
            let this_tmp = self.alloc_local(WasmType::I32);
            self.push(Instruction::LocalTee(this_tmp));

            // Emit arguments
            for arg in &call.arguments {
                self.emit_expr(arg.to_expression())?;
            }

            // Load vtable pointer from this (offset 0)
            self.push(Instruction::LocalGet(this_tmp));
            self.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));

            // Load table index from vtable at slot offset
            self.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: (*vtable_slot as u64) * 4,
                align: 2,
                memory_index: 0,
            }));

            // Build type signature for call_indirect: (this: i32, params...) -> ret
            // layout.methods includes inherited entries, so we can look up directly
            let method_sig = layout.unwrap().methods.get(method_name).unwrap();

            let mut param_types = vec![ValType::I32]; // this
            for (_pname, pty) in &method_sig.params {
                if let Some(vt) = pty.to_val_type() {
                    param_types.push(vt);
                }
            }
            let result_types = super::wasm_types::wasm_results(method_sig.return_type);

            let type_idx = self.module_ctx.get_or_add_type_sig(param_types, result_types);
            self.push(Instruction::CallIndirect { type_index: type_idx, table_index: 0 });

            Ok(Some(ret_ty))
        } else {
            // Static dispatch (non-polymorphic class)
            if let Some(recv_local) = self.method_receiver_override {
                self.push(Instruction::LocalGet(recv_local));
            } else {
                self.emit_expr(&member.object)?; // this
            }
            for arg in &call.arguments {
                self.emit_expr(arg.to_expression())?;
            }
            self.push(Instruction::Call(func_idx));

            Ok(Some(ret_ty))
        }
    }

    /// Emit `super(args)` — call parent constructor with `this` pointer.
    fn emit_super_constructor_call(&mut self, call: &CallExpression<'a>) -> Result<WasmType, CompileError> {
        let this_class = self.this_class.as_ref()
            .ok_or_else(|| CompileError::codegen("super() used outside of a method"))?
            .clone();
        let parent = self.module_ctx.class_registry.get(&this_class)
            .and_then(|l| l.parent.clone())
            .ok_or_else(|| CompileError::codegen(format!(
                "super() used in class '{this_class}' which has no parent"
            )))?;

        let ctor_key = format!("{parent}.constructor");
        if let Some(&(func_idx, _)) = self.module_ctx.method_map.get(&ctor_key) {
            // Parent has an explicit constructor — call it
            self.push(Instruction::LocalGet(0));
            for arg in &call.arguments {
                self.emit_expr(arg.to_expression())?;
            }
            self.push(Instruction::Call(func_idx));
            self.push(Instruction::Drop); // constructor returns this, but we already have it
        } else if !call.arguments.is_empty() {
            return Err(CompileError::codegen(format!(
                "parent class '{parent}' has no constructor, but super() was called with arguments"
            )));
        }
        // else: parent has no constructor and super() has no args — no-op

        Ok(WasmType::Void)
    }

    /// Emit `super.method(args)` — static dispatch to parent's method (bypasses vtable).
    fn emit_super_method_call(&mut self, method_name: &str, call: &CallExpression<'a>) -> Result<WasmType, CompileError> {
        let this_class = self.this_class.as_ref()
            .ok_or_else(|| CompileError::codegen("super.method() used outside of a method"))?
            .clone();
        let parent = self.module_ctx.class_registry.get(&this_class)
            .and_then(|l| l.parent.clone())
            .ok_or_else(|| CompileError::codegen(format!(
                "super.method() used in class '{this_class}' which has no parent"
            )))?;

        // Resolve method — may be on parent or grandparent
        let owner = self.module_ctx.class_registry.resolve_method_owner(&parent, method_name)
            .ok_or_else(|| CompileError::codegen(format!(
                "parent class '{parent}' has no method '{method_name}'"
            )))?;
        let key = format!("{owner}.{method_name}");
        let &(func_idx, ret_ty) = self.module_ctx.method_map.get(&key)
            .ok_or_else(|| CompileError::codegen(format!(
                "method '{method_name}' not found in parent chain of '{this_class}'"
            )))?;

        // Static dispatch: this + args + Call
        self.push(Instruction::LocalGet(0)); // this
        for arg in &call.arguments {
            self.emit_expr(arg.to_expression())?;
        }
        self.push(Instruction::Call(func_idx));

        Ok(ret_ty)
    }

    /// Resolve which class an expression refers to (for member access / method calls).
    /// Supports: identifiers, `this`, `new ClassName()`, `obj.field` (if field is a class),
    /// `obj.method()` (if method returns a class), and function calls returning classes.
    pub fn resolve_expr_class(&self, expr: &Expression<'a>) -> Result<String, CompileError> {
        match expr {
            Expression::Identifier(ident) => {
                let name = ident.name.as_str();
                if let Some(class_name) = self.local_class_types.get(name) {
                    return Ok(class_name.clone());
                }
                Err(CompileError::codegen(format!(
                    "cannot resolve class type of variable '{name}'"
                )))
            }
            Expression::ThisExpression(_) => {
                self.this_class.clone().ok_or_else(|| {
                    CompileError::codegen("`this` used outside of a method")
                })
            }
            // new ClassName(...) → class is ClassName
            Expression::NewExpression(new_expr) => {
                if let Expression::Identifier(ident) = &new_expr.callee {
                    let name = ident.name.as_str();
                    if self.module_ctx.class_names.contains(name) {
                        return Ok(name.to_string());
                    }
                }
                Err(CompileError::codegen("cannot resolve class type of new expression"))
            }
            // obj.field → if the field's type is a class, resolve it
            Expression::StaticMemberExpression(member) => {
                let parent_class = self.resolve_expr_class(&member.object)?;
                let layout = self.module_ctx.class_registry.get(&parent_class)
                    .ok_or_else(|| CompileError::codegen(format!("unknown class '{parent_class}'")))?;
                let field_name = member.property.name.as_str();
                if let Some(field_class) = layout.field_class_types.get(field_name) {
                    return Ok(field_class.clone());
                }
                Err(CompileError::codegen(format!(
                    "field '{field_name}' of class '{parent_class}' is not a class instance"
                )))
            }
            // obj.method() → if method returns a class, resolve it
            Expression::CallExpression(call) => {
                if let Expression::StaticMemberExpression(member) = &call.callee {
                    // Try method call: obj.method()
                    if let Ok(obj_class) = self.resolve_expr_class(&member.object) {
                        let method_name = member.property.name.as_str();
                        if let Some(layout) = self.module_ctx.class_registry.get(&obj_class)
                            && let Some(method_sig) = layout.methods.get(method_name)
                                && let Some(ref ret_class) = method_sig.return_class {
                                    return Ok(ret_class.clone());
                                }
                    }
                }
                // Try free function call: funcName()
                if let Expression::Identifier(ident) = &call.callee {
                    let name = ident.name.as_str();
                    // Check module-level function return class types
                    if let Some(class_name) = self.module_ctx.var_class_types.get(name) {
                        return Ok(class_name.clone());
                    }
                }
                Err(CompileError::codegen("cannot resolve class type of call expression"))
            }
            Expression::ParenthesizedExpression(paren) => {
                self.resolve_expr_class(&paren.expression)
            }
            // (expr as ClassName) → target class
            Expression::TSAsExpression(as_expr) => {
                if let Some(class_name) = crate::types::get_class_type_name_from_ts_type(&as_expr.type_annotation)
                    && self.module_ctx.class_names.contains(&class_name) {
                        return Ok(class_name);
                    }
                Err(CompileError::codegen("cannot resolve class type of as-expression"))
            }
            _ => Err(CompileError::codegen("cannot resolve class type of expression")),
        }
    }

    // ── Boxed variable helpers ────────────────────────────────────────────

    /// Emit a load from a boxed variable. Assumes ptr is on the stack.
    fn emit_boxed_load(&mut self, ty: WasmType) {
        match ty {
            WasmType::F64 => self.push(Instruction::F64Load(MemArg {
                offset: 0, align: 3, memory_index: 0,
            })),
            _ => self.push(Instruction::I32Load(MemArg {
                offset: 0, align: 2, memory_index: 0,
            })),
        }
    }

    /// Emit a store to a boxed variable. Assumes [ptr, value] are on the stack.
    fn emit_boxed_store(&mut self, ty: WasmType) {
        match ty {
            WasmType::F64 => self.push(Instruction::F64Store(MemArg {
                offset: 0, align: 3, memory_index: 0,
            })),
            _ => self.push(Instruction::I32Store(MemArg {
                offset: 0, align: 2, memory_index: 0,
            })),
        }
    }

    // ── First-class closure support ──────────────────────────────────────

    /// Emit a call to a closure variable via call_indirect.
    /// Loads func_table_idx and env_ptr from the closure struct, pushes env_ptr + args, then call_indirect.
    fn emit_closure_call(&mut self, var_name: &str, sig: &ClosureSig, call: &CallExpression<'a>) -> Result<WasmType, CompileError> {
        let closure_local = self.locals.get(var_name)
            .ok_or_else(|| CompileError::codegen(format!("undefined closure variable '{var_name}'")))?
            .0;

        // Load func_table_idx from closure struct offset 0
        let func_idx_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(closure_local));
        self.push(Instruction::I32Load(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(func_idx_local));

        // Load env_ptr from closure struct offset 4
        let env_ptr_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(closure_local));
        self.push(Instruction::I32Load(MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(env_ptr_local));

        // Push env_ptr as first argument
        self.push(Instruction::LocalGet(env_ptr_local));

        // Push remaining arguments
        for arg in &call.arguments {
            self.emit_expr(arg.to_expression())?;
        }

        // Get/register the type signature for call_indirect
        // The call-site type includes env_ptr: i32 as first param
        let mut call_params = vec![ValType::I32]; // env_ptr
        for pt in &sig.param_types {
            if let Some(vt) = (*pt).to_val_type() {
                call_params.push(vt);
            }
        }
        let call_results: Vec<ValType> = sig.return_type.to_val_type().into_iter().collect();
        let type_idx = self.module_ctx.get_or_add_type_sig(call_params, call_results);

        // Push func_table_idx and call_indirect
        self.push(Instruction::LocalGet(func_idx_local));
        self.push(Instruction::CallIndirect { type_index: type_idx, table_index: 0 });

        Ok(sig.return_type)
    }

    /// Emit an arrow function as a first-class closure value.
    /// Result: i32 pointer to arena-allocated closure struct [func_table_idx: i32, env_ptr: i32].
    fn emit_arrow_closure(&mut self, arrow: &ArrowFunctionExpression<'a>) -> Result<WasmType, CompileError> {
        // 1. Extract arrow parameter names and types
        let mut arrow_param_names = Vec::new();
        let mut arrow_param_types = Vec::new();
        for param in &arrow.params.items {
            let pname = match &param.pattern {
                BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
                _ => return Err(CompileError::unsupported("destructured closure parameter")),
            };
            let pty = if let Some(ann) = &param.type_annotation {
                crate::types::resolve_type_annotation_with_classes(ann, &self.module_ctx.class_names)?
            } else {
                return Err(CompileError::type_err(format!(
                    "closure parameter '{pname}' requires a type annotation"
                )));
            };
            arrow_param_names.push(pname);
            arrow_param_types.push(pty);
        }

        // 2. Determine return type
        let return_type = if let Some(ann) = &arrow.return_type {
            crate::types::resolve_type_annotation_with_classes(ann, &self.module_ctx.class_names)?
        } else if arrow.expression {
            // Infer from expression body
            if let Some(Statement::ExpressionStatement(e)) = arrow.body.statements.first() {
                self.infer_init_type(&e.expression)
                    .map(|(ty, _)| ty)
                    .unwrap_or(WasmType::Void)
            } else {
                WasmType::Void
            }
        } else {
            WasmType::Void
        };

        // 3. Capture analysis — find variables referenced in the body that exist in enclosing scope
        let mut referenced = HashSet::new();
        collect_identifiers_from_body(&arrow.body, &mut referenced);
        // Remove arrow params — they're not captures
        for name in &arrow_param_names {
            referenced.remove(name.as_str());
        }
        // Remove well-known non-variable names
        referenced.remove("Math");

        struct CapturedVar {
            name: String,
            wasm_type: WasmType,
            local_index: u32,
            env_offset: u32,
            class_name: Option<String>,
            array_elem_type: Option<WasmType>,
            array_elem_class: Option<String>,
        }

        let mut captures = Vec::new();
        let mut env_offset: u32 = 0;
        for name in &referenced {
            if let Some(&(local_idx, wasm_ty)) = self.locals.get(*name) {
                let class_name = self.local_class_types.get(*name).cloned();
                let array_elem_type = self.local_array_elem_types.get(*name).copied();
                let array_elem_class = self.local_array_elem_classes.get(*name).cloned();
                // Align offset for f64
                if wasm_ty == WasmType::F64 {
                    env_offset = (env_offset + 7) & !7;
                }
                captures.push(CapturedVar {
                    name: name.to_string(),
                    wasm_type: wasm_ty,
                    local_index: local_idx,
                    env_offset,
                    class_name,
                    array_elem_type,
                    array_elem_class,
                });
                env_offset += if wasm_ty == WasmType::F64 { 8 } else { 4 };
            }
            // If not in locals, might be a global or function — ignore (accessible without capture)
        }
        let env_size = env_offset;

        // 4. Build the lifted function's parameter list: [env_ptr: i32, ...arrow_params]
        let mut lifted_params: Vec<(String, WasmType)> = vec![("__env_ptr".to_string(), WasmType::I32)];
        for (pname, pty) in arrow_param_names.iter().zip(arrow_param_types.iter()) {
            lifted_params.push((pname.clone(), *pty));
        }

        // 5. Compile the arrow body in a new FuncContext
        let mut lifted_ctx = FuncContext::new(self.module_ctx, &lifted_params, return_type, self.source);

        // Set up captured variable access: each capture is loaded from env at its offset
        // We create locals in the lifted function and pre-load them from the env struct
        for cap in &captures {
            let cap_local = lifted_ctx.declare_local(&cap.name, cap.wasm_type);
            // Propagate class/array/closure metadata
            if let Some(ref cn) = cap.class_name {
                lifted_ctx.local_class_types.insert(cap.name.clone(), cn.clone());
            }
            if let Some(et) = cap.array_elem_type {
                lifted_ctx.local_array_elem_types.insert(cap.name.clone(), et);
            }
            if let Some(ref ec) = cap.array_elem_class {
                lifted_ctx.local_array_elem_classes.insert(cap.name.clone(), ec.clone());
            }
            if let Some(sig) = self.local_closure_sigs.get(&cap.name) {
                lifted_ctx.local_closure_sigs.insert(cap.name.clone(), sig.clone());
            }
            // Propagate boxed var status — the captured pointer is itself a box pointer
            if let Some(&boxed_ty) = self.boxed_var_types.get(&cap.name) {
                lifted_ctx.boxed_var_types.insert(cap.name.clone(), boxed_ty);
            }
            // Emit: cap_local = load(env_ptr + offset)
            lifted_ctx.push(Instruction::LocalGet(0)); // env_ptr is param 0
            match cap.wasm_type {
                WasmType::F64 => {
                    lifted_ctx.push(Instruction::F64Load(MemArg {
                        offset: cap.env_offset as u64,
                        align: 3,
                        memory_index: 0,
                    }));
                }
                _ => {
                    lifted_ctx.push(Instruction::I32Load(MemArg {
                        offset: cap.env_offset as u64,
                        align: 2,
                        memory_index: 0,
                    }));
                }
            }
            lifted_ctx.push(Instruction::LocalSet(cap_local));
        }

        // Compile the arrow body
        if arrow.expression {
            // Expression body: single expression that produces the return value
            if let Some(Statement::ExpressionStatement(expr_stmt)) = arrow.body.statements.first() {
                lifted_ctx.mark_loc(expr_stmt.span.start);
                let result_ty = lifted_ctx.emit_expr(&expr_stmt.expression)?;
                // Auto-convert if needed
                if result_ty != return_type && return_type == WasmType::F64 && result_ty == WasmType::I32 {
                    lifted_ctx.push(Instruction::F64ConvertI32S);
                } else if result_ty != return_type && return_type == WasmType::I32 && result_ty == WasmType::F64 {
                    lifted_ctx.push(Instruction::I32TruncF64S);
                }
            }
        } else {
            // Block body: statements with explicit return
            for stmt in &arrow.body.statements {
                lifted_ctx.emit_statement(stmt)?;
            }
        }

        let (lifted_func, lifted_source_map) = lifted_ctx.finish();

        // 6. Register the lifted function in the module's function table
        let mut wasm_params = vec![ValType::I32]; // env_ptr
        for pty in &arrow_param_types {
            if let Some(vt) = pty.to_val_type() {
                wasm_params.push(vt);
            }
        }
        let wasm_results: Vec<ValType> = return_type.to_val_type().into_iter().collect();
        let table_idx = self.module_ctx.register_closure_func(
            wasm_params,
            wasm_results,
            lifted_func,
            lifted_source_map,
        );

        // 7. Emit code in the ORIGINAL function to build the closure struct
        let arena_idx = self.module_ctx.arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

        // Allocate env struct (if captures exist)
        let env_ptr_local = self.alloc_local(WasmType::I32);
        if env_size > 0 {
            // env_ptr = arena_ptr
            self.push(Instruction::GlobalGet(arena_idx));
            self.push(Instruction::LocalSet(env_ptr_local));
            // arena_ptr += env_size (aligned to 8)
            let aligned_env_size = (env_size + 7) & !7;
            self.push(Instruction::GlobalGet(arena_idx));
            self.push(Instruction::I32Const(aligned_env_size as i32));
            self.push(Instruction::I32Add);
            self.push(Instruction::GlobalSet(arena_idx));

            // Copy captured values into env struct
            for cap in &captures {
                self.push(Instruction::LocalGet(env_ptr_local));
                self.push(Instruction::LocalGet(cap.local_index));
                match cap.wasm_type {
                    WasmType::F64 => {
                        self.push(Instruction::F64Store(MemArg {
                            offset: cap.env_offset as u64,
                            align: 3,
                            memory_index: 0,
                        }));
                    }
                    _ => {
                        self.push(Instruction::I32Store(MemArg {
                            offset: cap.env_offset as u64,
                            align: 2,
                            memory_index: 0,
                        }));
                    }
                }
            }
        } else {
            // No captures — env_ptr = 0
            self.push(Instruction::I32Const(0));
            self.push(Instruction::LocalSet(env_ptr_local));
        }

        // Allocate closure struct (8 bytes): [func_table_idx: i32, env_ptr: i32]
        let closure_ptr_local = self.alloc_local(WasmType::I32);
        // closure_ptr = arena_ptr
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::LocalSet(closure_ptr_local));
        // arena_ptr += 8
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::I32Const(8));
        self.push(Instruction::I32Add);
        self.push(Instruction::GlobalSet(arena_idx));

        // Store func_table_idx at closure_ptr + 0
        self.push(Instruction::LocalGet(closure_ptr_local));
        self.push(Instruction::I32Const(table_idx as i32));
        self.push(Instruction::I32Store(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));

        // Store env_ptr at closure_ptr + 4
        self.push(Instruction::LocalGet(closure_ptr_local));
        self.push(Instruction::LocalGet(env_ptr_local));
        self.push(Instruction::I32Store(MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // Result: closure pointer on the stack
        self.push(Instruction::LocalGet(closure_ptr_local));
        Ok(WasmType::I32)
    }
}

/// Recursively collect all IdentifierReference names from a function body,
/// excluding variables declared within the body (to avoid false shadow captures).
fn collect_identifiers_from_body<'a>(body: &FunctionBody<'a>, out: &mut HashSet<&'a str>) {
    let mut local_decls = HashSet::new();
    for stmt in &body.statements {
        collect_local_decls_from_stmt(stmt, &mut local_decls);
        collect_identifiers_from_stmt(stmt, out);
    }
    // Remove locally declared names — they're not captures from the outer scope
    for name in &local_decls {
        out.remove(name);
    }
}

/// Collect variable names declared in statements (for shadow-capture exclusion).
fn collect_local_decls_from_stmt<'a>(stmt: &Statement<'a>, out: &mut HashSet<&'a str>) {
    match stmt {
        Statement::VariableDeclaration(v) => {
            for decl in &v.declarations {
                if let BindingPattern::BindingIdentifier(ident) = &decl.id {
                    out.insert(ident.name.as_str());
                }
            }
        }
        Statement::BlockStatement(b) => {
            for s in &b.body { collect_local_decls_from_stmt(s, out); }
        }
        Statement::IfStatement(i) => {
            collect_local_decls_from_stmt(&i.consequent, out);
            if let Some(alt) = &i.alternate { collect_local_decls_from_stmt(alt, out); }
        }
        Statement::ForStatement(f) => {
            if let Some(ForStatementInit::VariableDeclaration(v)) = &f.init {
                for decl in &v.declarations {
                    if let BindingPattern::BindingIdentifier(ident) = &decl.id {
                        out.insert(ident.name.as_str());
                    }
                }
            }
            collect_local_decls_from_stmt(&f.body, out);
        }
        Statement::WhileStatement(w) => collect_local_decls_from_stmt(&w.body, out),
        Statement::DoWhileStatement(d) => collect_local_decls_from_stmt(&d.body, out),
        _ => {}
    }
}

fn collect_identifiers_from_stmt<'a>(stmt: &Statement<'a>, out: &mut HashSet<&'a str>) {
    match stmt {
        Statement::ExpressionStatement(e) => collect_identifiers_from_expr(&e.expression, out),
        Statement::ReturnStatement(r) => {
            if let Some(arg) = &r.argument {
                collect_identifiers_from_expr(arg, out);
            }
        }
        Statement::VariableDeclaration(v) => {
            for decl in &v.declarations {
                if let Some(init) = &decl.init {
                    collect_identifiers_from_expr(init, out);
                }
            }
        }
        Statement::IfStatement(i) => {
            collect_identifiers_from_expr(&i.test, out);
            collect_identifiers_from_stmt(&i.consequent, out);
            if let Some(alt) = &i.alternate {
                collect_identifiers_from_stmt(alt, out);
            }
        }
        Statement::WhileStatement(w) => {
            collect_identifiers_from_expr(&w.test, out);
            collect_identifiers_from_stmt(&w.body, out);
        }
        Statement::ForStatement(f) => {
            if let Some(ForStatementInit::VariableDeclaration(v)) = &f.init {
                for decl in &v.declarations {
                    if let Some(init) = &decl.init {
                        collect_identifiers_from_expr(init, out);
                    }
                }
            }
            if let Some(test) = &f.test { collect_identifiers_from_expr(test, out); }
            if let Some(update) = &f.update { collect_identifiers_from_expr(update, out); }
            collect_identifiers_from_stmt(&f.body, out);
        }
        Statement::BlockStatement(b) => {
            for s in &b.body { collect_identifiers_from_stmt(s, out); }
        }
        _ => {}
    }
}

fn collect_identifiers_from_expr<'a>(expr: &Expression<'a>, out: &mut HashSet<&'a str>) {
    match expr {
        Expression::Identifier(ident) => { out.insert(ident.name.as_str()); }
        Expression::BinaryExpression(b) => {
            collect_identifiers_from_expr(&b.left, out);
            collect_identifiers_from_expr(&b.right, out);
        }
        Expression::LogicalExpression(l) => {
            collect_identifiers_from_expr(&l.left, out);
            collect_identifiers_from_expr(&l.right, out);
        }
        Expression::UnaryExpression(u) => collect_identifiers_from_expr(&u.argument, out),
        Expression::CallExpression(c) => {
            collect_identifiers_from_expr(&c.callee, out);
            for arg in &c.arguments { collect_identifiers_from_expr(arg.to_expression(), out); }
        }
        Expression::AssignmentExpression(a) => {
            if let AssignmentTarget::AssignmentTargetIdentifier(ident) = &a.left {
                out.insert(ident.name.as_str());
            }
            collect_identifiers_from_expr(&a.right, out);
        }
        Expression::StaticMemberExpression(m) => collect_identifiers_from_expr(&m.object, out),
        Expression::ComputedMemberExpression(m) => {
            collect_identifiers_from_expr(&m.object, out);
            collect_identifiers_from_expr(&m.expression, out);
        }
        Expression::ConditionalExpression(c) => {
            collect_identifiers_from_expr(&c.test, out);
            collect_identifiers_from_expr(&c.consequent, out);
            collect_identifiers_from_expr(&c.alternate, out);
        }
        Expression::ParenthesizedExpression(p) => collect_identifiers_from_expr(&p.expression, out),
        Expression::UpdateExpression(u) => {
            if let SimpleAssignmentTarget::AssignmentTargetIdentifier(ident) = &u.argument {
                out.insert(ident.name.as_str());
            }
        }
        Expression::NewExpression(n) => {
            for arg in &n.arguments { collect_identifiers_from_expr(arg.to_expression(), out); }
        }
        Expression::TSAsExpression(a) => collect_identifiers_from_expr(&a.expression, out),
        Expression::ChainExpression(c) => {
            match &c.expression {
                ChainElement::StaticMemberExpression(m) => collect_identifiers_from_expr(&m.object, out),
                ChainElement::ComputedMemberExpression(m) => {
                    collect_identifiers_from_expr(&m.object, out);
                    collect_identifiers_from_expr(&m.expression, out);
                }
                ChainElement::CallExpression(c) => {
                    collect_identifiers_from_expr(&c.callee, out);
                    for arg in &c.arguments { collect_identifiers_from_expr(arg.to_expression(), out); }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn expr_kind_name(expr: &Expression) -> &'static str {
    match expr {
        Expression::NumericLiteral(_) => "NumericLiteral",
        Expression::BooleanLiteral(_) => "BooleanLiteral",
        Expression::StringLiteral(_) => "StringLiteral",
        Expression::Identifier(_) => "Identifier",
        Expression::BinaryExpression(_) => "BinaryExpression",
        Expression::LogicalExpression(_) => "LogicalExpression",
        Expression::UnaryExpression(_) => "UnaryExpression",
        Expression::CallExpression(_) => "CallExpression",
        Expression::AssignmentExpression(_) => "AssignmentExpression",
        Expression::ParenthesizedExpression(_) => "ParenthesizedExpression",
        Expression::UpdateExpression(_) => "UpdateExpression",
        Expression::ConditionalExpression(_) => "ConditionalExpression",
        Expression::StaticMemberExpression(_) => "MemberExpression",
        Expression::ComputedMemberExpression(_) => "ComputedMemberExpression",
        Expression::NewExpression(_) => "NewExpression",
        Expression::ThisExpression(_) => "ThisExpression",
        Expression::ArrayExpression(_) => "ArrayExpression",
        Expression::ObjectExpression(_) => "ObjectExpression",
        Expression::ArrowFunctionExpression(_) => "ArrowFunctionExpression",
        Expression::NullLiteral(_) => "NullLiteral",
        _ => "Unknown",
    }
}

/// ECMAScript Math.<CONSTANT> values (nearest-f64 to the mathematical reals).
/// Bit-exact across platforms — these are compile-time literals, not computed.
pub(crate) fn math_constant(name: &str) -> Option<f64> {
    Some(match name {
        "PI" => std::f64::consts::PI,
        "E" => std::f64::consts::E,
        "LN2" => std::f64::consts::LN_2,
        "LN10" => std::f64::consts::LN_10,
        "LOG2E" => std::f64::consts::LOG2_E,
        "LOG10E" => std::f64::consts::LOG10_E,
        "SQRT2" => std::f64::consts::SQRT_2,
        "SQRT1_2" => std::f64::consts::FRAC_1_SQRT_2,
        _ => return None,
    })
}

/// ECMAScript Number.<CONSTANT> values. Emitted as inline f64 literals.
/// MAX/MIN_SAFE_INTEGER are exact integers representable in f64 (±(2^53 − 1)).
/// EPSILON is 2^-52. MAX_VALUE / MIN_VALUE match ECMA-262 §21.1.2 semantics
/// (largest finite / smallest positive denormal).
pub(crate) fn number_constant(name: &str) -> Option<f64> {
    Some(match name {
        "MAX_SAFE_INTEGER" => 9_007_199_254_740_991.0,
        "MIN_SAFE_INTEGER" => -9_007_199_254_740_991.0,
        "MAX_VALUE" => f64::MAX,
        "MIN_VALUE" => 5e-324, // smallest positive denormal (ECMA-262)
        "EPSILON" => f64::EPSILON,
        "POSITIVE_INFINITY" => f64::INFINITY,
        "NEGATIVE_INFINITY" => f64::NEG_INFINITY,
        "NaN" => f64::NAN,
        _ => return None,
    })
}
