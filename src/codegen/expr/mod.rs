mod array;
mod assignment;
mod binary;
mod call;
mod class;
mod closure;
mod hash_table;
mod map;
mod member;
mod object;
mod set;
mod string;
mod tuple;

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
            Expression::TaggedTemplateExpression(tagged) => {
                self.emit_tagged_template(tagged)
            }
            Expression::ArrayExpression(arr) => self.emit_array_literal(arr, None),
            Expression::ObjectExpression(obj) => {
                let (ty, _) = self.emit_object_literal(obj, None)?;
                Ok(ty)
            }
            _ => {
                let err =
                    CompileError::unsupported(format!("expression type: {}", expr_kind_name(expr)));
                Err(err)
            }
        }
    }

    /// Like `emit_expr` but forwards an `expected` class-name hint to literal
    /// emitters that can consume one: object literals and tuple literals.
    /// For any other expression, `expected` is ignored — this is a
    /// semantic-free pass-through except at the literal sites.
    ///
    /// Direct callers (declarator, assignment, return, call-arg) usually
    /// reach `emit_object_literal` / `emit_tuple_literal` directly to get
    /// the resolved class back. This wrapper is the generic entry point
    /// and handles paren-wrapped forwarding.
    pub fn emit_expr_with_expected(
        &mut self,
        expr: &Expression<'a>,
        expected: Option<&str>,
    ) -> Result<WasmType, CompileError> {
        match expr {
            Expression::ObjectExpression(obj) => {
                let (ty, _) = self.emit_object_literal(obj, expected)?;
                Ok(ty)
            }
            Expression::ArrayExpression(arr) => {
                // Route tuple-typed array literals to the tuple emitter.
                if let Some(target) = expected
                    && self.is_tuple_shape(target)
                {
                    let (ty, _) = self.emit_tuple_literal(arr, target)?;
                    return Ok(ty);
                }
                self.emit_array_literal(arr, None)
            }
            Expression::ParenthesizedExpression(p) => {
                self.emit_expr_with_expected(&p.expression, expected)
            }
            _ => self.emit_expr(expr),
        }
    }

    /// True iff `name` is a registered tuple shape. Used at each
    /// expected-type hook point to decide whether an `ArrayExpression`
    /// routes through `emit_tuple_literal` or through the regular
    /// `emit_array_literal` path.
    pub(crate) fn is_tuple_shape(&self, name: &str) -> bool {
        self.module_ctx
            .shape_registry
            .by_name
            .get(name)
            .is_some_and(|&i| self.module_ctx.shape_registry.shapes[i].is_tuple)
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
        let target_class = crate::types::get_class_type_name_from_ts_type(
            &as_expr.type_annotation,
            Some(&self.module_ctx.shape_registry),
        );
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

pub(super) fn is_pure_rhs(expr: &Expression) -> bool {
    match expr {
        Expression::NumericLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::Identifier(_)
        | Expression::ThisExpression(_) => true,
        Expression::ParenthesizedExpression(p) => is_pure_rhs(&p.expression),
        Expression::UnaryExpression(u) => is_pure_rhs(&u.argument),
        Expression::TSAsExpression(a) => is_pure_rhs(&a.expression),
        Expression::StaticMemberExpression(m) => is_pure_rhs(&m.object),
        _ => false,
    }
}

pub(super) enum SlotRef<'a> {
    Field { name: &'a str },
    Tuple { index: usize, target: &'a str },
}

pub(super) fn widen_or_check(
    rhs_ty: WasmType,
    slot_ty: WasmType,
    slot: SlotRef,
    ctx: &mut FuncContext<'_>,
) -> Result<(), CompileError> {
    if rhs_ty == slot_ty {
        return Ok(());
    }
    if slot_ty == WasmType::F64 && rhs_ty == WasmType::I32 {
        ctx.push(Instruction::F64ConvertI32S);
        return Ok(());
    }
    Err(CompileError::type_err(match slot {
        SlotRef::Field { name } => format!(
            "object literal field '{name}' has type {rhs_ty:?}, expected {slot_ty:?}"
        ),
        SlotRef::Tuple { index, target } => format!(
            "tuple literal element {index} has type {rhs_ty:?}, tuple type '{target}' expects {slot_ty:?}"
        ),
    }))
}
