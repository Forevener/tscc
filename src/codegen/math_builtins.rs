//! Transcendental math functions lowered to host imports.
//!
//! Wasm-native ops (sqrt, abs, ceil, floor, trunc, nearest, min, max, copysign)
//! are bit-exact by the wasm spec and stay inline (see expr.rs::try_emit_math_call).
//! These functions, by contrast, have no wasm instruction and require a host
//! implementation. tscc declares them as imports named `__tscc_<name>` in the
//! configured `host_module` — embedders wire them to libm, the host's f64
//! intrinsics, or any deterministic backend.
//!
//! Tree-shaking: only methods actually referenced by the program are imported.
//! A program that calls only `Math.sin` produces a module that imports only
//! `__tscc_sin` — `__tscc_cos` etc. are not declared.

use std::collections::HashSet;

use oxc_ast::ast::*;
use wasm_encoder::{Function, Instruction, ValType};

use super::module::{GlobalInit, ModuleContext};
use crate::types::WasmType;

/// Name of the exported mutable i64 global holding the PCG32 RNG state.
/// Embedders must seed this with a nonzero value before calling code that
/// uses Math.random(). State of 0 produces a low-quality (but valid) sequence
/// because the PCG increment is added unconditionally at each step.
pub const RNG_STATE_GLOBAL: &str = "__rng_state";

/// Name of the helper function that advances the PCG32 state and returns
/// the next f64 in [0, 1). Called by Math.random().
pub const RNG_NEXT_FUNC: &str = "__rng_next";

/// PCG32 multiplier (Numerical Recipes constant — standard for PCG XSH-RR 64/32).
const PCG_MULT: i64 = 6364136223846793005_u64 as i64;
/// PCG32 increment (must be odd; standard recommended value).
const PCG_INC: i64 = 1442695040888963407_u64 as i64;

/// Transcendental math functions: (Math method name, arity).
/// Arity is 1 or 2; the import has the matching f64 signature.
pub const MATH_TRANSCENDENTALS: &[(&str, u8)] = &[
    ("sin", 1), ("cos", 1), ("tan", 1),
    ("asin", 1), ("acos", 1), ("atan", 1),
    ("sinh", 1), ("cosh", 1), ("tanh", 1),
    ("asinh", 1), ("acosh", 1), ("atanh", 1),
    ("log", 1), ("log2", 1), ("log10", 1),
    ("log1p", 1),
    ("exp", 1), ("expm1", 1), ("cbrt", 1),
    ("atan2", 2), ("pow", 2),
];

/// Returns true if `method` is a transcendental that lowers to a host import.
pub fn is_transcendental(method: &str) -> bool {
    MATH_TRANSCENDENTALS.iter().any(|(name, _)| *name == method)
}

/// Returns the import name for a transcendental (e.g. "sin" -> "__tscc_sin").
pub fn import_name(method: &str) -> String {
    format!("__tscc_{method}")
}

/// Walk the program AST and collect the set of transcendental method names
/// referenced via `Math.<name>(...)`. Used to register only the host imports
/// the program actually needs.
pub fn collect_used_transcendentals(program: &Program<'_>) -> HashSet<String> {
    let mut s = Scanner::default();
    for stmt in &program.body {
        s.walk_stmt(stmt);
    }
    s.used
}

/// Returns true if the program contains at least one `Math.random()` call.
/// Drives lazy registration of the RNG state global and step function.
pub fn program_uses_random(program: &Program<'_>) -> bool {
    let mut s = Scanner::default();
    for stmt in &program.body {
        s.walk_stmt(stmt);
    }
    s.uses_random
}

/// Register the RNG state global (i64, mutable, exported) and the `__rng_next`
/// helper function. Idempotent within a module — call once if Math.random is used.
pub fn register_rng(ctx: &mut ModuleContext) {
    // i64 mutable global, initialized to 0. Embedder seeds via the exported
    // `__rng_state`. We bypass add_global (which assumes I32/F64 WasmType)
    // and write directly to the context fields.
    let state_idx = ctx.next_global_index_internal();
    ctx.globals.insert(RNG_STATE_GLOBAL.to_string(), (state_idx, WasmType::I32 /* placeholder; emit uses GlobalInit::I64 */));
    ctx.global_inits.push(GlobalInit::I64(0));
    ctx.mutable_globals.insert(RNG_STATE_GLOBAL.to_string());
    ctx.exported_globals.push((RNG_STATE_GLOBAL.to_string(), state_idx));

    // __rng_next() -> f64
    ctx.register_func(RNG_NEXT_FUNC, &[], WasmType::F64, false).unwrap();
}

/// Compile the body of `__rng_next`. Implements the PCG32 XSH-RR 64/32 step:
///
/// ```text
/// oldstate = state
/// state    = oldstate * 6364136223846793005 + 1442695040888963407
/// xorshifted = u32(((oldstate >> 18) ^ oldstate) >> 27)
/// rot        = u32(oldstate >> 59) & 31
/// output_u32 = rotr(xorshifted, rot)
/// return f64(output_u32) * 2^-32     // result in [0, 1)
/// ```
///
/// `2^-32 = 2.3283064365386963e-10` is exactly representable in f64, so the
/// multiplication is exact (no rounding error from the conversion path).
pub fn compile_rng_next(ctx: &ModuleContext) -> Function {
    let state_global_idx = ctx.globals[RNG_STATE_GLOBAL].0;

    // Locals: $old (i64), $xorshifted (i32), $rot (i32)
    // wasm_encoder Function takes (count, ty) groups.
    let mut func = Function::new([(1, ValType::I64), (2, ValType::I32)]);
    let local_old: u32 = 0;
    let local_xorshifted: u32 = 1;
    let local_rot: u32 = 2;

    // oldstate = state
    func.instruction(&Instruction::GlobalGet(state_global_idx));
    func.instruction(&Instruction::LocalSet(local_old));

    // state = oldstate * MULT + INC
    func.instruction(&Instruction::LocalGet(local_old));
    func.instruction(&Instruction::I64Const(PCG_MULT));
    func.instruction(&Instruction::I64Mul);
    func.instruction(&Instruction::I64Const(PCG_INC));
    func.instruction(&Instruction::I64Add);
    func.instruction(&Instruction::GlobalSet(state_global_idx));

    // xorshifted = i32_wrap(((oldstate >> 18) ^ oldstate) >> 27)
    func.instruction(&Instruction::LocalGet(local_old));
    func.instruction(&Instruction::I64Const(18));
    func.instruction(&Instruction::I64ShrU);
    func.instruction(&Instruction::LocalGet(local_old));
    func.instruction(&Instruction::I64Xor);
    func.instruction(&Instruction::I64Const(27));
    func.instruction(&Instruction::I64ShrU);
    func.instruction(&Instruction::I32WrapI64);
    func.instruction(&Instruction::LocalSet(local_xorshifted));

    // rot = i32_wrap(oldstate >> 59) & 31
    func.instruction(&Instruction::LocalGet(local_old));
    func.instruction(&Instruction::I64Const(59));
    func.instruction(&Instruction::I64ShrU);
    func.instruction(&Instruction::I32WrapI64);
    func.instruction(&Instruction::I32Const(31));
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::LocalSet(local_rot));

    // output_u32 = rotr(xorshifted, rot)
    func.instruction(&Instruction::LocalGet(local_xorshifted));
    func.instruction(&Instruction::LocalGet(local_rot));
    func.instruction(&Instruction::I32Rotr);

    // f64 result = u32 * 2^-32
    func.instruction(&Instruction::F64ConvertI32U);
    // 1.0 / 4294967296.0 = 2^-32, exact in f64
    func.instruction(&Instruction::F64Const(2.3283064365386963e-10));
    func.instruction(&Instruction::F64Mul);

    func.instruction(&Instruction::End);
    func
}

#[derive(Default)]
struct Scanner {
    used: HashSet<String>,
    uses_random: bool,
}

impl Scanner {
    fn note_call(&mut self, callee: &Expression<'_>) {
        if let Expression::StaticMemberExpression(member) = callee
            && let Expression::Identifier(obj) = &member.object
            && obj.name.as_str() == "Math"
        {
            let m = member.property.name.as_str();
            if is_transcendental(m) {
                self.used.insert(m.to_string());
            } else if m == "random" {
                self.uses_random = true;
            }
        }
    }

    fn walk_stmt(&mut self, stmt: &Statement<'_>) {
        match stmt {
            Statement::ExpressionStatement(es) => self.walk_expr(&es.expression),
            Statement::VariableDeclaration(vd) => {
                for d in &vd.declarations {
                    if let Some(init) = &d.init {
                        self.walk_expr(init);
                    }
                }
            }
            Statement::ReturnStatement(r) => {
                if let Some(e) = &r.argument {
                    self.walk_expr(e);
                }
            }
            Statement::IfStatement(i) => {
                self.walk_expr(&i.test);
                self.walk_stmt(&i.consequent);
                if let Some(alt) = &i.alternate {
                    self.walk_stmt(alt);
                }
            }
            Statement::BlockStatement(b) => {
                for s in &b.body {
                    self.walk_stmt(s);
                }
            }
            Statement::WhileStatement(w) => {
                self.walk_expr(&w.test);
                self.walk_stmt(&w.body);
            }
            Statement::DoWhileStatement(w) => {
                self.walk_expr(&w.test);
                self.walk_stmt(&w.body);
            }
            Statement::ForStatement(f) => {
                if let Some(ForStatementInit::VariableDeclaration(vd)) = &f.init {
                    for d in &vd.declarations {
                        if let Some(init) = &d.init {
                            self.walk_expr(init);
                        }
                    }
                } else if let Some(init) = &f.init {
                    if let Some(e) = init.as_expression() {
                        self.walk_expr(e);
                    }
                }
                if let Some(test) = &f.test {
                    self.walk_expr(test);
                }
                if let Some(update) = &f.update {
                    self.walk_expr(update);
                }
                self.walk_stmt(&f.body);
            }
            Statement::ForOfStatement(f) => {
                self.walk_expr(&f.right);
                self.walk_stmt(&f.body);
            }
            Statement::SwitchStatement(s) => {
                self.walk_expr(&s.discriminant);
                for c in &s.cases {
                    if let Some(t) = &c.test {
                        self.walk_expr(t);
                    }
                    for s in &c.consequent {
                        self.walk_stmt(s);
                    }
                }
            }
            Statement::FunctionDeclaration(f) => {
                if let Some(body) = &f.body {
                    for s in &body.statements {
                        self.walk_stmt(s);
                    }
                }
            }
            Statement::ExportNamedDeclaration(e) => {
                if let Some(decl) = &e.declaration {
                    match decl {
                        Declaration::FunctionDeclaration(f) => {
                            if let Some(body) = &f.body {
                                for s in &body.statements {
                                    self.walk_stmt(s);
                                }
                            }
                        }
                        Declaration::VariableDeclaration(vd) => {
                            for d in &vd.declarations {
                                if let Some(init) = &d.init {
                                    self.walk_expr(init);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Statement::ExportDefaultDeclaration(e) => {
                if let oxc_ast::ast::ExportDefaultDeclarationKind::FunctionDeclaration(f) = &e.declaration {
                    if let Some(body) = &f.body {
                        for s in &body.statements {
                            self.walk_stmt(s);
                        }
                    }
                }
            }
            Statement::ClassDeclaration(c) => {
                for elem in &c.body.body {
                    if let ClassElement::MethodDefinition(m) = elem {
                        if let Some(body) = &m.value.body {
                            for s in &body.statements {
                                self.walk_stmt(s);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn walk_expr(&mut self, expr: &Expression<'_>) {
        match expr {
            Expression::CallExpression(c) => {
                self.note_call(&c.callee);
                self.walk_expr(&c.callee);
                for arg in &c.arguments {
                    if let Some(e) = arg.as_expression() {
                        self.walk_expr(e);
                    }
                }
            }
            Expression::BinaryExpression(b) => {
                self.walk_expr(&b.left);
                self.walk_expr(&b.right);
            }
            Expression::LogicalExpression(l) => {
                self.walk_expr(&l.left);
                self.walk_expr(&l.right);
            }
            Expression::UnaryExpression(u) => self.walk_expr(&u.argument),
            Expression::UpdateExpression(_) => {}
            Expression::AssignmentExpression(a) => self.walk_expr(&a.right),
            Expression::ConditionalExpression(c) => {
                self.walk_expr(&c.test);
                self.walk_expr(&c.consequent);
                self.walk_expr(&c.alternate);
            }
            Expression::ParenthesizedExpression(p) => self.walk_expr(&p.expression),
            Expression::ArrayExpression(a) => {
                for el in &a.elements {
                    if let Some(e) = el.as_expression() {
                        self.walk_expr(e);
                    }
                }
            }
            Expression::StaticMemberExpression(m) => self.walk_expr(&m.object),
            Expression::ComputedMemberExpression(m) => {
                self.walk_expr(&m.object);
                self.walk_expr(&m.expression);
            }
            Expression::TemplateLiteral(t) => {
                for e in &t.expressions {
                    self.walk_expr(e);
                }
            }
            Expression::ArrowFunctionExpression(a) => {
                for s in &a.body.statements {
                    self.walk_stmt(s);
                }
            }
            Expression::NewExpression(n) => {
                for arg in &n.arguments {
                    if let Some(e) = arg.as_expression() {
                        self.walk_expr(e);
                    }
                }
            }
            _ => {}
        }
    }
}
