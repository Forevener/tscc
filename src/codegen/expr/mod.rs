mod array;
mod assignment;
mod binary;
mod call;
mod class;
mod closure;
mod map;
mod member;
mod string;

use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::error::CompileError;
use crate::types::WasmType;

use super::func::FuncContext;

/// Array header: [length: i32 (4B)] [capacity: i32 (4B)]
pub const ARRAY_HEADER_SIZE: u32 = 8;
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
            Expression::ComputedMemberExpression(member) => {
                self.emit_computed_member_access(member)
            }
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
            Expression::ArrayExpression(arr) => self.emit_array_literal(arr, None),
            _ => {
                let span_start = match expr {
                    Expression::ObjectExpression(o) => o.span.start,
                    _ => 0,
                };
                let err =
                    CompileError::unsupported(format!("expression type: {}", expr_kind_name(expr)));
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
    fn emit_as_cast(&mut self, as_expr: &TSAsExpression<'a>) -> Result<WasmType, CompileError> {
        // Check for class downcast/upcast first
        let target_class = crate::types::get_class_type_name_from_ts_type(&as_expr.type_annotation);
        if let Some(ref target_name) = target_class
            && self.module_ctx.class_names.contains(target_name)
        {
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
        let target_ty =
            crate::types::resolve_ts_type(&as_expr.type_annotation, &self.module_ctx.class_names)?;

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

    fn emit_conditional(
        &mut self,
        cond: &ConditionalExpression<'a>,
    ) -> Result<WasmType, CompileError> {
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
