use std::collections::HashSet;

use oxc_ast::ast::*;
use wasm_encoder::{Function, Instruction, MemArg, ValType};

use super::module::ModuleContext;
use crate::types::WasmType;

/// String header size: 4 bytes for the length field.
pub const STRING_HEADER_SIZE: i32 = 4;

/// Names of all string runtime helpers, in registration order.
/// Note: `__str_concat` was removed in favor of inline fused allocation
/// (see `emit_fused_string_chain` in codegen/expr.rs), which avoids the N-1
/// intermediate allocations of the chained-call approach.
pub const STRING_HELPER_NAMES: &[&str] = &[
    "__str_eq",
    "__str_cmp",
    "__str_indexOf",
    "__str_slice",
    "__str_startsWith",
    "__str_endsWith",
    "__str_includes",
    "__str_toLower",
    "__str_toUpper",
    "__str_trim",
    "__str_trimStart",
    "__str_trimEnd",
    "__str_from_i32",
    "__str_from_f64",
    "__str_split",
    "__str_replace",
    "__str_parseInt",
    "__str_parseFloat",
    "__str_fromCharCode",
    "__str_repeat",
    "__str_padStart",
    "__str_padEnd",
    "__str_concat",
];

/// Register all string runtime helper functions in the module.
/// Must be called after Pass 2 (all user functions registered), before Pass 3 (codegen).
type HelperSig = (&'static str, Vec<(String, WasmType)>, WasmType);

pub fn register_string_helpers(ctx: &mut ModuleContext, used: &HashSet<String>) {
    let helpers: Vec<HelperSig> = vec![
        // __str_eq(a: i32, b: i32) -> i32
        ("__str_eq", vec![("a".into(), WasmType::I32), ("b".into(), WasmType::I32)], WasmType::I32),
        // __str_cmp(a: i32, b: i32) -> i32
        ("__str_cmp", vec![("a".into(), WasmType::I32), ("b".into(), WasmType::I32)], WasmType::I32),
        // __str_indexOf(haystack: i32, needle: i32) -> i32
        ("__str_indexOf", vec![("haystack".into(), WasmType::I32), ("needle".into(), WasmType::I32)], WasmType::I32),
        // __str_slice(s: i32, start: i32, end: i32) -> i32
        ("__str_slice", vec![("s".into(), WasmType::I32), ("start".into(), WasmType::I32), ("end".into(), WasmType::I32)], WasmType::I32),
        // __str_startsWith(s: i32, prefix: i32) -> i32
        ("__str_startsWith", vec![("s".into(), WasmType::I32), ("prefix".into(), WasmType::I32)], WasmType::I32),
        // __str_endsWith(s: i32, suffix: i32) -> i32
        ("__str_endsWith", vec![("s".into(), WasmType::I32), ("suffix".into(), WasmType::I32)], WasmType::I32),
        // __str_includes(s: i32, search: i32) -> i32
        ("__str_includes", vec![("s".into(), WasmType::I32), ("search".into(), WasmType::I32)], WasmType::I32),
        // __str_toLower(s: i32) -> i32
        ("__str_toLower", vec![("s".into(), WasmType::I32)], WasmType::I32),
        // __str_toUpper(s: i32) -> i32
        ("__str_toUpper", vec![("s".into(), WasmType::I32)], WasmType::I32),
        // __str_trim(s: i32) -> i32
        ("__str_trim", vec![("s".into(), WasmType::I32)], WasmType::I32),
        // __str_trimStart(s: i32) -> i32
        ("__str_trimStart", vec![("s".into(), WasmType::I32)], WasmType::I32),
        // __str_trimEnd(s: i32) -> i32
        ("__str_trimEnd", vec![("s".into(), WasmType::I32)], WasmType::I32),
        // __str_from_i32(n: i32) -> i32
        ("__str_from_i32", vec![("n".into(), WasmType::I32)], WasmType::I32),
        // __str_from_f64(n: f64) -> i32
        ("__str_from_f64", vec![("n".into(), WasmType::F64)], WasmType::I32),
        // __str_split(s: i32, delim: i32) -> i32 (returns Array<string> pointer)
        ("__str_split", vec![("s".into(), WasmType::I32), ("delim".into(), WasmType::I32)], WasmType::I32),
        // __str_replace(s: i32, search: i32, replacement: i32) -> i32
        ("__str_replace", vec![("s".into(), WasmType::I32), ("search".into(), WasmType::I32), ("replacement".into(), WasmType::I32)], WasmType::I32),
        // __str_parseInt(s: i32) -> i32
        ("__str_parseInt", vec![("s".into(), WasmType::I32)], WasmType::I32),
        // __str_parseFloat(s: i32) -> f64
        ("__str_parseFloat", vec![("s".into(), WasmType::I32)], WasmType::F64),
        // __str_fromCharCode(code: i32) -> i32
        ("__str_fromCharCode", vec![("code".into(), WasmType::I32)], WasmType::I32),
        // __str_repeat(s: i32, count: i32) -> i32
        ("__str_repeat", vec![("s".into(), WasmType::I32), ("count".into(), WasmType::I32)], WasmType::I32),
        // __str_padStart(s: i32, targetLen: i32, fill: i32) -> i32
        ("__str_padStart", vec![("s".into(), WasmType::I32), ("targetLen".into(), WasmType::I32), ("fill".into(), WasmType::I32)], WasmType::I32),
        // __str_padEnd(s: i32, targetLen: i32, fill: i32) -> i32
        ("__str_padEnd", vec![("s".into(), WasmType::I32), ("targetLen".into(), WasmType::I32), ("fill".into(), WasmType::I32)], WasmType::I32),
        // __str_concat(a: i32, b: i32) -> i32 — runtime 2-string concat for
        // Array.join. String `+` goes through emit_fused_string_chain and does
        // NOT use this helper.
        ("__str_concat", vec![("a".into(), WasmType::I32), ("b".into(), WasmType::I32)], WasmType::I32),
    ];

    for (name, params, ret) in helpers {
        if used.contains(name) {
            ctx.register_func(name, &params, ret, false).unwrap();
        }
    }
}

/// Compile bodies for the string helpers that were registered.
/// Iterates in the same `STRING_HELPER_NAMES` order used by `register_string_helpers`,
/// so the emitted function indices line up with registration.
pub fn compile_string_helpers(ctx: &ModuleContext, used: &HashSet<String>) -> Vec<Function> {
    let arena_idx = ctx.arena_ptr_global.unwrap();
    STRING_HELPER_NAMES
        .iter()
        .filter(|name| used.contains(**name))
        .map(|name| compile_helper(name, arena_idx))
        .collect()
}

/// Pre-codegen AST scan that returns the set of string runtime helpers the program
/// will actually call. Used to register only what's needed (tree-shaking).
///
/// Conservative by design: the scanner lacks type info, so it over-approximates for
/// `+`, `==`/`!=`, and comparison operators by enabling the matching helper whenever
/// the program contains both such an operator and at least one string-like literal.
/// Under-approximation would crash at codegen (get_func unwrap), so we prefer slight
/// bloat over correctness holes.
pub fn collect_used_helpers(program: &Program<'_>) -> HashSet<String> {
    let mut s = Scanner::default();
    for stmt in &program.body {
        s.walk_stmt(stmt);
    }
    s.into_set()
}

#[derive(Default)]
struct Scanner {
    has_string_literal: bool,
    has_template_with_expr: bool,
    has_plus: bool,
    has_eq_op: bool,
    has_cmp_op: bool,
    method_names: HashSet<String>,
    identifier_calls: HashSet<String>,
    has_string_from_char_code: bool,
}

impl Scanner {
    fn into_set(self) -> HashSet<String> {
        let mut used = HashSet::new();
        let add = |n: &str, set: &mut HashSet<String>| { set.insert(n.to_string()); };

        // Template literals with interpolated expressions coerce each expression to a
        // string via __str_from_i32 / __str_from_f64. The concat itself is fused inline
        // (see emit_fused_string_chain in codegen/expr.rs) and does NOT call __str_concat.
        if self.has_template_with_expr {
            add("__str_from_i32", &mut used);
            add("__str_from_f64", &mut used);
        }

        // Strings can also enter the program via `String.fromCharCode` or string-returning
        // methods (slice, toLowerCase, etc.). If any such source exists, treat the program
        // as "has strings" for operator-based helper inclusion.
        let string_returning_methods = [
            "slice", "substring", "toLowerCase", "toUpperCase",
            "trim", "trimStart", "trimEnd", "replace", "repeat", "padStart", "padEnd",
            "concat",
        ];
        let has_string_source = self.has_string_literal
            || self.has_string_from_char_code
            || string_returning_methods.iter().any(|m| self.method_names.contains(*m));

        // `+` with strings present: enable the coercion helpers so numeric operands
        // can be formatted. The concat is fused inline — no __str_concat call.
        if has_string_source && self.has_plus {
            add("__str_from_i32", &mut used);
            add("__str_from_f64", &mut used);
        }

        if has_string_source && self.has_eq_op {
            add("__str_eq", &mut used);
        }
        if has_string_source && self.has_cmp_op {
            add("__str_cmp", &mut used);
        }

        if self.has_string_from_char_code {
            add("__str_fromCharCode", &mut used);
        }

        let method_map: &[(&str, &str)] = &[
            ("indexOf", "__str_indexOf"),
            ("includes", "__str_includes"),
            ("startsWith", "__str_startsWith"),
            ("endsWith", "__str_endsWith"),
            ("slice", "__str_slice"),
            ("substring", "__str_slice"),
            ("toLowerCase", "__str_toLower"),
            ("toUpperCase", "__str_toUpper"),
            ("trim", "__str_trim"),
            ("trimStart", "__str_trimStart"),
            ("trimEnd", "__str_trimEnd"),
            ("split", "__str_split"),
            ("replace", "__str_replace"),
            ("repeat", "__str_repeat"),
            ("padStart", "__str_padStart"),
            ("padEnd", "__str_padEnd"),
        ];
        for (method, helper) in method_map {
            if self.method_names.contains(*method) {
                add(helper, &mut used);
            }
        }

        // `parseInt` / `parseFloat` can appear as bare identifiers OR as
        // `Number.parseInt` / `Number.parseFloat` (ES6 aliases). Method-name
        // presence is conservative — accepts any `.parseInt(...)` call.
        if self.identifier_calls.contains("parseInt") || self.method_names.contains("parseInt") {
            add("__str_parseInt", &mut used);
        }
        if self.identifier_calls.contains("parseFloat") || self.method_names.contains("parseFloat") {
            add("__str_parseFloat", &mut used);
        }

        // Array.join: runtime-loop concatenation needs __str_concat plus the
        // numeric stringifiers for non-string element arrays. Over-includes
        // for string-element arrays — cheap in tree-shaking terms.
        if self.method_names.contains("join") {
            add("__str_concat", &mut used);
            add("__str_from_i32", &mut used);
            add("__str_from_f64", &mut used);
        }

        // String.prototype.concat(other) — also needs the runtime 2-string
        // helper. Over-includes for the rare case of `[].concat` on arrays.
        if self.method_names.contains("concat") && has_string_source {
            add("__str_concat", &mut used);
        }

        used
    }

    fn walk_stmt(&mut self, stmt: &Statement<'_>) {
        match stmt {
            Statement::ExpressionStatement(s) => self.walk_expr(&s.expression),
            Statement::BlockStatement(b) => {
                for s in &b.body { self.walk_stmt(s); }
            }
            Statement::IfStatement(s) => {
                self.walk_expr(&s.test);
                self.walk_stmt(&s.consequent);
                if let Some(alt) = &s.alternate { self.walk_stmt(alt); }
            }
            Statement::WhileStatement(s) => {
                self.walk_expr(&s.test);
                self.walk_stmt(&s.body);
            }
            Statement::DoWhileStatement(s) => {
                self.walk_expr(&s.test);
                self.walk_stmt(&s.body);
            }
            Statement::ForStatement(s) => {
                if let Some(init) = &s.init {
                    match init {
                        ForStatementInit::VariableDeclaration(d) => self.walk_var_decl(d),
                        _ => self.walk_expr(init.to_expression()),
                    }
                }
                if let Some(test) = &s.test { self.walk_expr(test); }
                if let Some(update) = &s.update { self.walk_expr(update); }
                self.walk_stmt(&s.body);
            }
            Statement::ForOfStatement(s) => {
                if let ForStatementLeft::VariableDeclaration(d) = &s.left {
                    self.walk_var_decl(d);
                }
                self.walk_expr(&s.right);
                self.walk_stmt(&s.body);
            }
            Statement::ForInStatement(s) => {
                self.walk_expr(&s.right);
                self.walk_stmt(&s.body);
            }
            Statement::SwitchStatement(s) => {
                self.walk_expr(&s.discriminant);
                for case in &s.cases {
                    if let Some(t) = &case.test { self.walk_expr(t); }
                    for s in &case.consequent { self.walk_stmt(s); }
                }
            }
            Statement::ReturnStatement(s) => {
                if let Some(arg) = &s.argument { self.walk_expr(arg); }
            }
            Statement::ThrowStatement(s) => self.walk_expr(&s.argument),
            Statement::TryStatement(s) => {
                for st in &s.block.body { self.walk_stmt(st); }
                if let Some(h) = &s.handler {
                    for st in &h.body.body { self.walk_stmt(st); }
                }
                if let Some(f) = &s.finalizer {
                    for st in &f.body { self.walk_stmt(st); }
                }
            }
            Statement::LabeledStatement(s) => self.walk_stmt(&s.body),
            Statement::VariableDeclaration(d) => self.walk_var_decl(d),
            Statement::FunctionDeclaration(f) => {
                if let Some(body) = &f.body {
                    for st in &body.statements { self.walk_stmt(st); }
                }
            }
            Statement::ClassDeclaration(c) => {
                for element in &c.body.body {
                    if let ClassElement::MethodDefinition(m) = element
                        && let Some(body) = &m.value.body {
                            for st in &body.statements { self.walk_stmt(st); }
                        }
                }
            }
            Statement::ExportDefaultDeclaration(e) => {
                if let ExportDefaultDeclarationKind::FunctionDeclaration(f) = &e.declaration
                    && let Some(body) = &f.body {
                        for st in &body.statements { self.walk_stmt(st); }
                    }
            }
            Statement::ExportNamedDeclaration(e) => {
                if let Some(Declaration::FunctionDeclaration(f)) = &e.declaration
                    && let Some(body) = &f.body {
                        for st in &body.statements { self.walk_stmt(st); }
                    }
                if let Some(Declaration::VariableDeclaration(d)) = &e.declaration {
                    self.walk_var_decl(d);
                }
            }
            _ => {}
        }
    }

    fn walk_var_decl(&mut self, d: &VariableDeclaration<'_>) {
        for decl in &d.declarations {
            if let Some(init) = &decl.init { self.walk_expr(init); }
        }
    }

    fn walk_expr(&mut self, expr: &Expression<'_>) {
        match expr {
            Expression::StringLiteral(_) => { self.has_string_literal = true; }
            Expression::TemplateLiteral(t) => {
                self.has_string_literal = true;
                if !t.expressions.is_empty() {
                    self.has_template_with_expr = true;
                }
                for e in &t.expressions { self.walk_expr(e); }
            }
            Expression::BinaryExpression(b) => {
                use oxc_ast::ast::BinaryOperator as Op;
                match b.operator {
                    Op::Addition => self.has_plus = true,
                    Op::Equality | Op::Inequality
                    | Op::StrictEquality | Op::StrictInequality => self.has_eq_op = true,
                    Op::LessThan | Op::LessEqualThan
                    | Op::GreaterThan | Op::GreaterEqualThan => self.has_cmp_op = true,
                    _ => {}
                }
                self.walk_expr(&b.left);
                self.walk_expr(&b.right);
            }
            Expression::LogicalExpression(l) => {
                self.walk_expr(&l.left);
                self.walk_expr(&l.right);
            }
            Expression::UnaryExpression(u) => self.walk_expr(&u.argument),
            Expression::UpdateExpression(_) => {}
            Expression::AssignmentExpression(a) => {
                self.walk_expr(&a.right);
            }
            Expression::ConditionalExpression(c) => {
                self.walk_expr(&c.test);
                self.walk_expr(&c.consequent);
                self.walk_expr(&c.alternate);
            }
            Expression::CallExpression(c) => self.walk_call(c),
            Expression::NewExpression(n) => {
                for a in &n.arguments {
                    self.walk_expr(a.to_expression());
                }
            }
            Expression::ArrayExpression(a) => {
                for el in &a.elements {
                    if let ArrayExpressionElement::SpreadElement(s) = el {
                        self.walk_expr(&s.argument);
                    } else if let Some(e) = el.as_expression() {
                        self.walk_expr(e);
                    }
                }
            }
            Expression::ObjectExpression(o) => {
                for prop in &o.properties {
                    if let ObjectPropertyKind::ObjectProperty(p) = prop {
                        self.walk_expr(&p.value);
                    }
                }
            }
            Expression::ParenthesizedExpression(p) => self.walk_expr(&p.expression),
            Expression::ChainExpression(c) => {
                match &c.expression {
                    ChainElement::CallExpression(call) => self.walk_call(call),
                    ChainElement::StaticMemberExpression(m) => self.walk_expr(&m.object),
                    ChainElement::ComputedMemberExpression(m) => {
                        self.walk_expr(&m.object);
                        self.walk_expr(&m.expression);
                    }
                    _ => {}
                }
            }
            Expression::StaticMemberExpression(m) => self.walk_expr(&m.object),
            Expression::ComputedMemberExpression(m) => {
                self.walk_expr(&m.object);
                self.walk_expr(&m.expression);
            }
            Expression::ArrowFunctionExpression(a) => {
                for st in &a.body.statements { self.walk_stmt(st); }
            }
            Expression::TSAsExpression(a) => self.walk_expr(&a.expression),
            Expression::SequenceExpression(s) => {
                for e in &s.expressions { self.walk_expr(e); }
            }
            _ => {}
        }
    }

    fn walk_call(&mut self, call: &CallExpression<'_>) {
        match &call.callee {
            Expression::StaticMemberExpression(m) => {
                let method = m.property.name.as_str();
                self.method_names.insert(method.to_string());
                if let Expression::Identifier(obj) = &m.object
                    && obj.name.as_str() == "String"
                    && method == "fromCharCode" {
                        self.has_string_from_char_code = true;
                    }
                self.walk_expr(&m.object);
            }
            Expression::Identifier(ident) => {
                self.identifier_calls.insert(ident.name.as_str().to_string());
            }
            other => self.walk_expr(other),
        }
        for arg in &call.arguments {
            self.walk_expr(arg.to_expression());
        }
    }
}

fn compile_helper(name: &str, arena_idx: u32) -> Function {
    match name {
        "__str_eq" => build_str_eq(),
        "__str_cmp" => build_str_cmp(),
        "__str_indexOf" => build_str_index_of(),
        "__str_slice" => build_str_slice(arena_idx),
        "__str_startsWith" => build_str_starts_with(),
        "__str_endsWith" => build_str_ends_with(),
        "__str_includes" => build_str_includes(),
        "__str_toLower" => build_str_to_lower(arena_idx),
        "__str_toUpper" => build_str_to_upper(arena_idx),
        "__str_trim" => build_str_trim_impl(arena_idx, true, true),
        "__str_trimStart" => build_str_trim_impl(arena_idx, true, false),
        "__str_trimEnd" => build_str_trim_impl(arena_idx, false, true),
        "__str_from_i32" => build_str_from_i32(arena_idx),
        "__str_from_f64" => build_str_from_f64(arena_idx),
        "__str_split" => build_str_split(arena_idx),
        "__str_replace" => build_str_replace(arena_idx),
        "__str_parseInt" => build_str_parse_int(),
        "__str_parseFloat" => build_str_parse_float(),
        "__str_fromCharCode" => build_str_from_char_code(arena_idx),
        "__str_repeat" => build_str_repeat(arena_idx),
        "__str_padStart" => build_str_pad_start(arena_idx),
        "__str_padEnd" => build_str_pad_end(arena_idx),
        "__str_concat" => build_str_concat(arena_idx),
        _ => unreachable!("unknown string helper: {name}"),
    }
}

fn mem_load_i32(offset: u64) -> Instruction<'static> {
    Instruction::I32Load(MemArg { offset, align: 2, memory_index: 0 })
}

fn mem_store_i32(offset: u64) -> Instruction<'static> {
    Instruction::I32Store(MemArg { offset, align: 2, memory_index: 0 })
}

fn mem_load8_u(offset: u64) -> Instruction<'static> {
    Instruction::I32Load8U(MemArg { offset, align: 0, memory_index: 0 })
}

fn mem_store8(offset: u64) -> Instruction<'static> {
    Instruction::I32Store8(MemArg { offset, align: 0, memory_index: 0 })
}

// ============================================================
// __str_eq(a: i32, b: i32) -> i32
// Returns 1 if equal, 0 otherwise
// ============================================================
fn build_str_eq() -> Function {
    // Params: a=0, b=1
    // Locals: len_a=2, i=3, byte_a=4, byte_b=5
    let locals = vec![
        (1, ValType::I32), // len_a
        (1, ValType::I32), // i
        (1, ValType::I32), // byte_a
        (1, ValType::I32), // byte_b
    ];
    let mut func = Function::new(locals);
    let (a, b) = (0u32, 1);
    let (len_a, i) = (2u32, 3);

    // len_a = load(a)
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len_a));

    // if len_a != load(b): return 0
    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // i = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    // loop
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty)); // block (break target)
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));  // loop

    // if i >= len_a: break → return 1
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1)); // break to outer block

    // byte_a = load8_u(a + 4 + i)
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    // byte_b = load8_u(b + 4 + i)
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    // if byte_a != byte_b: return 0
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // i++
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));

    // br loop
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // end loop
    func.instruction(&Instruction::End); // end block

    // return 1
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_cmp(a: i32, b: i32) -> i32
// Lexicographic compare: returns -1, 0, or 1
// ============================================================
fn build_str_cmp() -> Function {
    // Params: a=0, b=1
    // Locals: len_a=2, len_b=3, min_len=4, i=5, byte_a=6, byte_b=7
    let locals = vec![
        (1, ValType::I32), // len_a
        (1, ValType::I32), // len_b
        (1, ValType::I32), // min_len
        (1, ValType::I32), // i
        (1, ValType::I32), // byte_a
        (1, ValType::I32), // byte_b
    ];
    let mut func = Function::new(locals);
    let (a, b) = (0u32, 1);
    let (len_a, len_b, min_len, i, byte_a, byte_b) = (2u32, 3, 4, 5, 6, 7);

    // len_a = load(a), len_b = load(b)
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len_a));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len_b));

    // min_len = min(len_a, len_b)
    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::LocalGet(len_b));
    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::LocalGet(len_b));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(min_len));

    // i = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    // loop: compare bytes
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    // if i >= min_len: break
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(min_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    // byte_a = load8_u(a+4+i)
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte_a));

    // byte_b = load8_u(b+4+i)
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte_b));

    // if byte_a < byte_b: return -1
    func.instruction(&Instruction::LocalGet(byte_a));
    func.instruction(&Instruction::LocalGet(byte_b));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // if byte_a > byte_b: return 1
    func.instruction(&Instruction::LocalGet(byte_a));
    func.instruction(&Instruction::LocalGet(byte_b));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // i++
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // end loop
    func.instruction(&Instruction::End); // end block

    // Compare lengths: if len_a < len_b return -1, if > return 1, else 0
    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::LocalGet(len_b));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::LocalGet(len_b));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_indexOf(haystack: i32, needle: i32) -> i32
// Returns byte offset of first occurrence, or -1
// ============================================================
fn build_str_index_of() -> Function {
    // Params: haystack=0, needle=1
    // Locals: h_len=2, n_len=3, limit=4, i=5, j=6, matched=7
    let locals = vec![
        (1, ValType::I32), // h_len
        (1, ValType::I32), // n_len
        (1, ValType::I32), // limit
        (1, ValType::I32), // i
        (1, ValType::I32), // j
        (1, ValType::I32), // matched
    ];
    let mut func = Function::new(locals);
    let (haystack, needle) = (0u32, 1);
    let (h_len, n_len, limit, i, j, matched) = (2u32, 3, 4, 5, 6, 7);

    // h_len = load(haystack), n_len = load(needle)
    func.instruction(&Instruction::LocalGet(haystack));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(h_len));
    func.instruction(&Instruction::LocalGet(needle));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(n_len));

    // if n_len == 0: return 0
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // if n_len > h_len: return -1
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::LocalGet(h_len));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // limit = h_len - n_len
    func.instruction(&Instruction::LocalGet(h_len));
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(limit));

    // i = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    // outer loop
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty)); // outer block
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));  // outer loop

    // if i > limit: break → return -1
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(limit));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::BrIf(1));

    // j = 0, matched = 1
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(matched));

    // inner loop: compare bytes
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    // if j >= n_len: break inner
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    // h_byte = load8_u(haystack + 4 + i + j)
    func.instruction(&Instruction::LocalGet(haystack));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    // n_byte = load8_u(needle + 4 + j)
    func.instruction(&Instruction::LocalGet(needle));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    // if h_byte != n_byte: matched=0, break inner
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::Br(2)); // break inner block
    func.instruction(&Instruction::End);

    // j++
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Br(0)); // continue inner loop
    func.instruction(&Instruction::End); // end inner loop
    func.instruction(&Instruction::End); // end inner block

    // if matched: return i
    func.instruction(&Instruction::LocalGet(matched));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // i++
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0)); // continue outer loop
    func.instruction(&Instruction::End); // end outer loop
    func.instruction(&Instruction::End); // end outer block

    // return -1
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_slice(s: i32, start: i32, end: i32) -> i32
// Arena-allocates a new string with bytes [start..end)
// ============================================================
fn build_str_slice(arena_idx: u32) -> Function {
    // Params: s=0, start=1, end=2
    // Locals: len=3, new_len=4, total=5, ptr=6
    let locals = vec![
        (1, ValType::I32), // len
        (1, ValType::I32), // new_len
        (1, ValType::I32), // total
        (1, ValType::I32), // ptr
    ];
    let mut func = Function::new(locals);
    let (s, start, end) = (0u32, 1, 2);
    let (len, new_len, total, ptr) = (3u32, 4, 5, 6);

    // len = load(s)
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len));

    // Clamp start: if start < 0: start = max(0, len + start)
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    // start = len + start
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(start));
    // if still < 0: start = 0
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(start));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Clamp start to len
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::LocalSet(start));
    func.instruction(&Instruction::End);

    // Clamp end: if end < 0: end = max(0, len + end)
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(end));
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(end));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Clamp end to len
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::LocalSet(end));
    func.instruction(&Instruction::End);

    // new_len = max(0, end - start)
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(new_len));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(new_len));
    func.instruction(&Instruction::End);

    // total = new_len + 4
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total));

    // ptr = arena_alloc(total)
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // store length
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&mem_store_i32(0));

    // memory.copy(ptr+4, s+4+start, new_len)
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_startsWith(s: i32, prefix: i32) -> i32
// ============================================================
fn build_str_starts_with() -> Function {
    // Params: s=0, prefix=1
    // Locals: s_len=2, p_len=3, i=4
    let locals = vec![
        (1, ValType::I32), // s_len
        (1, ValType::I32), // p_len
        (1, ValType::I32), // i
    ];
    let mut func = Function::new(locals);
    let (s, prefix) = (0u32, 1);
    let (s_len, p_len, i) = (2u32, 3, 4);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(s_len));
    func.instruction(&Instruction::LocalGet(prefix));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(p_len));

    // if p_len > s_len: return 0
    func.instruction(&Instruction::LocalGet(p_len));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // Compare first p_len bytes
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(p_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    // Compare bytes
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    func.instruction(&Instruction::LocalGet(prefix));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // end loop
    func.instruction(&Instruction::End); // end block

    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_endsWith(s: i32, suffix: i32) -> i32
// ============================================================
fn build_str_ends_with() -> Function {
    // Params: s=0, suffix=1
    // Locals: s_len=2, suf_len=3, offset=4, i=5
    let locals = vec![
        (1, ValType::I32), // s_len
        (1, ValType::I32), // suf_len
        (1, ValType::I32), // offset
        (1, ValType::I32), // i
    ];
    let mut func = Function::new(locals);
    let (s, suffix) = (0u32, 1);
    let (s_len, suf_len, offset, i) = (2u32, 3, 4, 5);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(s_len));
    func.instruction(&Instruction::LocalGet(suffix));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(suf_len));

    // if suf_len > s_len: return 0
    func.instruction(&Instruction::LocalGet(suf_len));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // offset = s_len - suf_len
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(suf_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(offset));

    // Compare last suf_len bytes
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(suf_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    // s byte at offset + i
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(offset));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    // suffix byte at i
    func.instruction(&Instruction::LocalGet(suffix));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_includes(s: i32, search: i32) -> i32
// Delegates to indexOf >= 0
// ============================================================
fn build_str_includes() -> Function {
    // This is a simple wrapper — we can't call indexOf from here since we don't know its index.
    // Instead, implement inline (same as indexOf but return 0/1).
    // Params: s=0, search=1
    // Locals: h_len=2, n_len=3, limit=4, i=5, j=6, matched=7
    let locals = vec![
        (1, ValType::I32),
        (1, ValType::I32),
        (1, ValType::I32),
        (1, ValType::I32),
        (1, ValType::I32),
        (1, ValType::I32),
    ];
    let mut func = Function::new(locals);
    let (s, search) = (0u32, 1);
    let (h_len, n_len, limit, i, j, matched) = (2u32, 3, 4, 5, 6, 7);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(h_len));
    func.instruction(&Instruction::LocalGet(search));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(n_len));

    // Empty needle: return 1
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // needle > haystack: return 0
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::LocalGet(h_len));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(h_len));
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(limit));

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(limit));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::BrIf(1));

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(matched));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    func.instruction(&Instruction::LocalGet(search));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(matched));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_toLower(s: i32) -> i32
// ============================================================
fn build_str_to_lower(arena_idx: u32) -> Function {
    build_case_convert(arena_idx, true)
}

// ============================================================
// __str_toUpper(s: i32) -> i32
// ============================================================
fn build_str_to_upper(arena_idx: u32) -> Function {
    build_case_convert(arena_idx, false)
}

fn build_case_convert(arena_idx: u32, to_lower: bool) -> Function {
    // Params: s=0
    // Locals: len=1, ptr=2, total=3, i=4, byte=5
    let locals = vec![
        (1, ValType::I32), // len
        (1, ValType::I32), // ptr
        (1, ValType::I32), // total
        (1, ValType::I32), // i
        (1, ValType::I32), // byte
    ];
    let mut func = Function::new(locals);
    let s = 0u32;
    let (len, ptr, total, i, byte) = (1u32, 2, 3, 4, 5);

    // Range for conversion
    let (range_start, range_end, offset): (i32, i32, i32) = if to_lower {
        (65, 90, 32) // A-Z → +32
    } else {
        (97, 122, -32) // a-z → -32
    };

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len));

    // total = len + 4
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total));

    // ptr = arena_alloc(total)
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // store length
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&mem_store_i32(0));

    // i = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    // byte = load8_u(s + 4 + i)
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte));

    // if byte >= range_start && byte <= range_end: byte += offset
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(range_start));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(range_end));
    func.instruction(&Instruction::I32LeU);
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(offset));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(byte));
    func.instruction(&Instruction::End);

    // store8(ptr + 4 + i, byte)
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&mem_store8(0));

    // i++
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_trim / __str_trimStart / __str_trimEnd (s: i32) -> i32
// Parameterized by which side(s) to trim.
// ============================================================
fn build_str_trim_impl(arena_idx: u32, trim_left: bool, trim_right: bool) -> Function {
    // Params: s=0
    // Locals: len=1, start=2, end=3, byte=4, new_len=5, total=6, ptr=7
    let locals = vec![
        (1, ValType::I32), // len
        (1, ValType::I32), // start
        (1, ValType::I32), // end
        (1, ValType::I32), // byte
        (1, ValType::I32), // new_len
        (1, ValType::I32), // total
        (1, ValType::I32), // ptr
    ];
    let mut func = Function::new(locals);
    let s = 0u32;
    let (len, start, end, byte, new_len, total, ptr) = (1u32, 2, 3, 4, 5, 6, 7);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len));

    // start = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(start));

    // end = len
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::LocalSet(end));

    // Find start: skip whitespace from left
    if trim_left {
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::LocalGet(end));
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));

        func.instruction(&Instruction::LocalGet(s));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::I32Add);
        func.instruction(&mem_load8_u(0));
        func.instruction(&Instruction::LocalSet(byte));

        // Check whitespace: space(32), tab(9), LF(10), CR(13)
        emit_is_whitespace(&mut func, byte);
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1)); // not whitespace → break

        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(start));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    }

    // Find end: skip whitespace from right
    if trim_right {
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(end));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::I32LeU);
        func.instruction(&Instruction::BrIf(1));

        func.instruction(&Instruction::LocalGet(s));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(end));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::I32Add);
        func.instruction(&mem_load8_u(0));
        func.instruction(&Instruction::LocalSet(byte));

        emit_is_whitespace(&mut func, byte);
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1)); // not whitespace → break

        func.instruction(&Instruction::LocalGet(end));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(end));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    }

    // new_len = end - start
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(new_len));

    // total = new_len + 4
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total));

    // ptr = arena_alloc(total)
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&mem_store_i32(0));

    // memory.copy(ptr+4, s+4+start, new_len)
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

/// Emit: (byte == 32 || byte == 9 || byte == 10 || byte == 13) → i32 on stack
fn emit_is_whitespace(func: &mut Function, byte_local: u32) {
    func.instruction(&Instruction::LocalGet(byte_local));
    func.instruction(&Instruction::I32Const(32)); // space
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::LocalGet(byte_local));
    func.instruction(&Instruction::I32Const(9)); // tab
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::LocalGet(byte_local));
    func.instruction(&Instruction::I32Const(10)); // LF
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::LocalGet(byte_local));
    func.instruction(&Instruction::I32Const(13)); // CR
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::I32Or);
}

// ============================================================
// __str_from_i32(n: i32) -> i32
// Convert integer to decimal string. Handles negatives.
// Strategy: write digits backwards into a 12-byte scratch area,
// then copy to a properly sized arena string.
// ============================================================
fn build_str_from_i32(arena_idx: u32) -> Function {
    // Params: n=0
    // Locals: is_neg=1, abs_val=2, buf_start=3, pos=4, digit=5, len=6, ptr=7, total=8
    let locals = vec![
        (1, ValType::I32), // is_neg
        (1, ValType::I32), // abs_val
        (1, ValType::I32), // buf_start (scratch area in arena for digits)
        (1, ValType::I32), // pos (write position, counts from end)
        (1, ValType::I32), // digit
        (1, ValType::I32), // len
        (1, ValType::I32), // ptr
        (1, ValType::I32), // total
    ];
    let mut func = Function::new(locals);
    let n = 0u32;
    let (is_neg, abs_val, buf_start, pos, digit, len, ptr, total) = (1u32, 2, 3, 4, 5, 6, 7, 8);

    // Allocate 12-byte scratch buffer from arena (enough for -2147483648)
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(buf_start));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::I32Const(12));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // pos = 11 (write backwards from end of buffer)
    func.instruction(&Instruction::I32Const(11));
    func.instruction(&Instruction::LocalSet(pos));

    // Handle 0 specially
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    // Write '0' at pos, len=1
    func.instruction(&Instruction::LocalGet(buf_start));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(48)); // '0'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(len));
    // Skip the digit extraction loop
    func.instruction(&Instruction::Br(0)); // This exits the if block. We need a different flow.
    func.instruction(&Instruction::End);

    // Handle negative
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::LocalSet(is_neg));

    // abs_val = is_neg ? -n : n  (careful: -INT_MIN overflows, but we handle it via unsigned div)
    func.instruction(&Instruction::LocalGet(is_neg));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(abs_val));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::LocalSet(abs_val));
    func.instruction(&Instruction::End);

    // Extract digits: loop while abs_val > 0 (but only if n != 0)
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::I32Eqz); // n != 0
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    // if abs_val == 0: break
    func.instruction(&Instruction::LocalGet(abs_val));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));

    // digit = abs_val % 10
    func.instruction(&Instruction::LocalGet(abs_val));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32RemU);
    func.instruction(&Instruction::LocalSet(digit));

    // abs_val = abs_val / 10
    func.instruction(&Instruction::LocalGet(abs_val));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32DivU);
    func.instruction(&Instruction::LocalSet(abs_val));

    // store digit char at buf_start + pos
    func.instruction(&Instruction::LocalGet(buf_start));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(digit));
    func.instruction(&Instruction::I32Const(48)); // '0'
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_store8(0));

    // pos--
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(pos));

    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // end loop
    func.instruction(&Instruction::End); // end block

    // If negative, write '-'
    func.instruction(&Instruction::LocalGet(is_neg));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(buf_start));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(45)); // '-'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::End);

    // len = 11 - pos
    func.instruction(&Instruction::I32Const(11));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(len));

    func.instruction(&Instruction::End); // end if n != 0

    // Now allocate the actual string: ptr = arena_alloc(4 + len)
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total));

    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // Store length
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&mem_store_i32(0));

    // Copy digits from scratch: memory.copy(ptr+4, buf_start+pos+1, len)
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(buf_start));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_from_f64(n: f64) -> i32
// Convert f64 to decimal string.
// Strategy: convert integer part via i32 path, append fractional
// digits (up to 6 significant, strip trailing zeros).
// ============================================================
fn build_str_from_f64(arena_idx: u32) -> Function {
    // This is complex. We'll use a simpler approach:
    // 1. If the value has no fractional part, convert as i32
    // 2. Otherwise: handle sign, integer part, '.', fractional part (up to 6 digits)
    //
    // We'll write the string character-by-character into arena memory.

    // Params: n=0
    // Locals: is_neg=1, int_part=2, frac_val=3, buf=4, pos=5, digit=6, ptr=7,
    //         abs_val=8, temp=9, frac_digits=10, len=11
    let locals = vec![
        (1, ValType::I32),   // is_neg
        (1, ValType::I32),   // int_part
        (1, ValType::I32),   // frac_val (fractional part * 1000000 as i32)
        (1, ValType::I32),   // buf
        (1, ValType::I32),   // pos (write position from start)
        (1, ValType::I32),   // digit
        (1, ValType::I32),   // ptr (result string)
        (1, ValType::I32),   // abs_int
        (1, ValType::I32),   // temp (for reversing digits)
        (1, ValType::I32),   // digit_start
        (1, ValType::I32),   // len
        (1, ValType::F64),   // abs_f
    ];
    let mut func = Function::new(locals);
    let n = 0u32;
    let (is_neg, int_part, frac_val, buf, pos, digit, ptr, abs_int, temp, digit_start, len, abs_f)
        = (1u32, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12);

    // Allocate 32-byte scratch buffer
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(buf));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::I32Const(32));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // pos = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(pos));

    // Check negative
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::F64Const(0.0f64));
    func.instruction(&Instruction::F64Lt);
    func.instruction(&Instruction::LocalSet(is_neg));

    // abs_f = is_neg ? -n : n
    func.instruction(&Instruction::LocalGet(is_neg));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::F64Neg);
    func.instruction(&Instruction::LocalSet(abs_f));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::LocalSet(abs_f));
    func.instruction(&Instruction::End);

    // Write '-' if negative
    func.instruction(&Instruction::LocalGet(is_neg));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::I32Const(45)); // '-'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::End);

    // int_part = trunc(abs_f) as i32
    func.instruction(&Instruction::LocalGet(abs_f));
    func.instruction(&Instruction::F64Floor);
    func.instruction(&Instruction::I32TruncF64U);
    func.instruction(&Instruction::LocalSet(int_part));

    // Write integer part digits (reverse order then flip)
    // digit_start = pos (remember where digits start for reversing)
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::LocalSet(digit_start));

    // Handle 0 integer part
    func.instruction(&Instruction::LocalGet(int_part));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(48)); // '0'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::Else);

    // Write digits of int_part (forward: extract digits, store in reverse order, then reverse)
    func.instruction(&Instruction::LocalGet(int_part));
    func.instruction(&Instruction::LocalSet(abs_int));
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32RemU);
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(digit));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(digit));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32DivU);
    func.instruction(&Instruction::LocalSet(abs_int));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Reverse the integer digits in place [digit_start..pos)
    // Use temp for swapping. left=digit_start, right=pos-1
    func.instruction(&Instruction::LocalGet(digit_start));
    func.instruction(&Instruction::LocalSet(abs_int)); // reuse as left
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(temp)); // right

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(abs_int)); // left
    func.instruction(&Instruction::LocalGet(temp)); // right
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    // swap buf[left] and buf[right]
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(digit)); // save left char
    // buf[left] = buf[right]
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&mem_store8(0));
    // buf[right] = saved left char
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(digit));
    func.instruction(&mem_store8(0));
    // left++, right--
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(abs_int));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(temp));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::End); // end if int_part == 0 else

    // Check if there's a fractional part
    // frac_val = round((abs_f - floor(abs_f)) * 1000000) as i32
    func.instruction(&Instruction::LocalGet(abs_f));
    func.instruction(&Instruction::LocalGet(abs_f));
    func.instruction(&Instruction::F64Floor);
    func.instruction(&Instruction::F64Sub);
    func.instruction(&Instruction::F64Const(1000000.0f64));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::F64Nearest); // round
    func.instruction(&Instruction::I32TruncF64U);
    func.instruction(&Instruction::LocalSet(frac_val));

    // If frac_val > 0, write '.' and digits
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));

    // Strip trailing zeros from frac_val
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32RemU);
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::I32Eqz); // non-zero remainder? break
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32DivU);
    func.instruction(&Instruction::LocalSet(frac_val));
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1)); // if became 0, break
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Write '.'
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(46)); // '.'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(pos));

    // Write frac digits (backwards then reverse, same pattern)
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::LocalSet(digit_start));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32RemU);
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(digit));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(digit));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32DivU);
    func.instruction(&Instruction::LocalSet(frac_val));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Reverse frac digits
    func.instruction(&Instruction::LocalGet(digit_start));
    func.instruction(&Instruction::LocalSet(abs_int));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(temp));
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(digit));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(digit));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(abs_int));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(temp));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::End); // end if frac_val > 0

    // len = pos
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::LocalSet(len));

    // Allocate final string: ptr = arena_alloc(4 + len)
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // Store length
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&mem_store_i32(0));

    // Copy from buf to ptr+4
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_split(s: i32, delim: i32) -> i32 (Array<string>)
// Returns arena-allocated array of string pointers.
// Array layout: [length:i32][capacity:i32][elements:i32...]
// ============================================================
fn build_str_split(arena_idx: u32) -> Function {
    // Params: s=0, delim=1
    // Locals: s_len=2, d_len=3, arr=4, count=5, start=6, i=7, j=8, matched=9,
    //         seg_len=10, seg_ptr=11, cap=12
    let locals = vec![
        (1, ValType::I32), // s_len
        (1, ValType::I32), // d_len
        (1, ValType::I32), // arr (array pointer)
        (1, ValType::I32), // count (number of segments found)
        (1, ValType::I32), // start (start of current segment)
        (1, ValType::I32), // i (scan position)
        (1, ValType::I32), // j (inner loop)
        (1, ValType::I32), // matched
        (1, ValType::I32), // seg_len
        (1, ValType::I32), // seg_ptr
        (1, ValType::I32), // cap (initial capacity)
    ];
    let mut func = Function::new(locals);
    let (s, delim) = (0u32, 1);
    let (s_len, d_len, arr, count, start, i, j, matched, seg_len, seg_ptr, cap)
        = (2u32, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(s_len));
    func.instruction(&Instruction::LocalGet(delim));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(d_len));

    // Allocate array with initial capacity 8. Array header = 8 bytes.
    func.instruction(&Instruction::I32Const(8));
    func.instruction(&Instruction::LocalSet(cap));
    // arr = arena_alloc(8 + cap * 4)
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(arr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::I32Const(8)); // header
    func.instruction(&Instruction::LocalGet(cap));
    func.instruction(&Instruction::I32Const(4)); // element size
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    // arr.length = 0, arr.capacity = cap
    func.instruction(&Instruction::LocalGet(arr));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&mem_store_i32(0));
    func.instruction(&Instruction::LocalGet(arr));
    func.instruction(&Instruction::LocalGet(cap));
    func.instruction(&Instruction::I32Store(MemArg { offset: 4, align: 2, memory_index: 0 }));

    // count = 0, start = 0, i = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(count));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(start));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    // Scan loop
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    // if i > s_len - d_len: break (but handle d_len=0 / i >= s_len)
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(d_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::BrIf(1));

    // Check if delimiter matches at position i
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(j));

    // Empty delimiter: don't match (prevent infinite loop)
    func.instruction(&Instruction::LocalGet(d_len));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::Else);

    // Inner compare loop
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalGet(d_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    func.instruction(&Instruction::LocalGet(delim));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::Br(2)); // break inner
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::End); // end if d_len == 0

    // If matched: emit segment [start..i), advance i past delimiter
    func.instruction(&Instruction::LocalGet(matched));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));

    // seg_len = i - start
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(seg_len));

    // Allocate segment string
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(seg_ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(seg_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(seg_ptr));
    func.instruction(&Instruction::LocalGet(seg_len));
    func.instruction(&mem_store_i32(0));
    // Copy bytes
    func.instruction(&Instruction::LocalGet(seg_ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(seg_len));
    func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

    // Store seg_ptr in arr[count]
    func.instruction(&Instruction::LocalGet(arr));
    func.instruction(&Instruction::I32Const(8)); // array header
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(seg_ptr));
    func.instruction(&mem_store_i32(0));

    // count++
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(count));

    // start = i + d_len
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(d_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(start));
    // i = start
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::LocalSet(i));

    func.instruction(&Instruction::Br(1)); // continue outer loop
    func.instruction(&Instruction::End); // end if matched

    // Not matched: i++
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // end loop
    func.instruction(&Instruction::End); // end block

    // Emit final segment [start..s_len)
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(seg_len));

    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(seg_ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(seg_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(seg_ptr));
    func.instruction(&Instruction::LocalGet(seg_len));
    func.instruction(&mem_store_i32(0));
    func.instruction(&Instruction::LocalGet(seg_ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(seg_len));
    func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

    func.instruction(&Instruction::LocalGet(arr));
    func.instruction(&Instruction::I32Const(8));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(seg_ptr));
    func.instruction(&mem_store_i32(0));

    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(count));

    // Update array length
    func.instruction(&Instruction::LocalGet(arr));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&mem_store_i32(0));

    func.instruction(&Instruction::LocalGet(arr));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_replace(s: i32, search: i32, replacement: i32) -> i32
// Replace first occurrence of search with replacement.
// ============================================================
fn build_str_replace(arena_idx: u32) -> Function {
    // Strategy: find indexOf(search), if not found return s,
    // otherwise build: s[0..idx] + replacement + s[idx+search.len..]
    // Params: s=0, search=1, replacement=2
    // Locals: s_len=3, search_len=4, repl_len=5, idx=6, i=7, j=8, matched=9,
    //         new_len=10, ptr=11, limit=12
    let locals = vec![
        (1, ValType::I32), // s_len
        (1, ValType::I32), // search_len
        (1, ValType::I32), // repl_len
        (1, ValType::I32), // idx (found position)
        (1, ValType::I32), // i
        (1, ValType::I32), // j
        (1, ValType::I32), // matched
        (1, ValType::I32), // new_len
        (1, ValType::I32), // ptr
        (1, ValType::I32), // limit
    ];
    let mut func = Function::new(locals);
    let (s, search, replacement) = (0u32, 1, 2);
    let (s_len, search_len, repl_len, idx, i, j, matched, new_len, ptr, limit)
        = (3u32, 4, 5, 6, 7, 8, 9, 10, 11, 12);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(s_len));
    func.instruction(&Instruction::LocalGet(search));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(search_len));
    func.instruction(&Instruction::LocalGet(replacement));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(repl_len));

    // Find first occurrence (inline indexOf logic)
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::LocalSet(idx));

    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32LeU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(limit));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(limit));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::BrIf(1));

    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(j));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalGet(search));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(matched));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalSet(idx));
    func.instruction(&Instruction::Br(2)); // break outer search loop
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::End); // end if search_len <= s_len

    // If not found, return s
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // Build result: s[0..idx] + replacement + s[idx+search_len..]
    // new_len = idx + repl_len + (s_len - idx - search_len)
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalGet(repl_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(new_len));

    // Allocate
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&mem_store_i32(0));

    // Copy part 1: s[0..idx]
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

    // Copy part 2: replacement
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(replacement));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(repl_len));
    func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

    // Copy part 3: s[idx+search_len..]
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(repl_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_parseInt(s: i32) -> i32
// Parse decimal integer from string. Handles leading whitespace, sign.
// Returns 0 on invalid input (matches simplified JS behavior).
// ============================================================
fn build_str_parse_int() -> Function {
    // Params: s=0
    // Locals: len=1, i=2, byte=3, sign=4, result=5
    let locals = vec![
        (1, ValType::I32), // len
        (1, ValType::I32), // i
        (1, ValType::I32), // byte
        (1, ValType::I32), // sign
        (1, ValType::I32), // result
    ];
    let mut func = Function::new(locals);
    let s = 0u32;
    let (len, i, byte, sign, result) = (1u32, 2, 3, 4, 5);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(sign));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(result));

    // Skip whitespace
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte));
    emit_is_whitespace(&mut func, byte);
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Check sign
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte));
    // '-'
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(45));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::LocalSet(sign));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Else);
    // '+'
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(43));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Parse digits
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte));
    // if byte < '0' || byte > '9': break
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(57));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::BrIf(1));
    // result = result * 10 + (byte - '0')
    func.instruction(&Instruction::LocalGet(result));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(result));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // return result * sign
    func.instruction(&Instruction::LocalGet(result));
    func.instruction(&Instruction::LocalGet(sign));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_parseFloat(s: i32) -> f64
// Parse decimal float from string. Handles sign, integer, '.', fractional.
// ============================================================
fn build_str_parse_float() -> Function {
    // Params: s=0
    // Locals: len=1, i=2, byte=3, sign=4, int_part=5, frac_part=6, frac_div=7
    let locals = vec![
        (1, ValType::I32), // len
        (1, ValType::I32), // i
        (1, ValType::I32), // byte
        (1, ValType::F64), // sign
        (1, ValType::F64), // int_part
        (1, ValType::F64), // frac_part
        (1, ValType::F64), // frac_div
    ];
    let mut func = Function::new(locals);
    let s = 0u32;
    let (len, i, byte, sign, int_part, frac_part, frac_div) = (1u32, 2, 3, 4, 5, 6, 7);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::F64Const(1.0f64));
    func.instruction(&Instruction::LocalSet(sign));
    func.instruction(&Instruction::F64Const(0.0f64));
    func.instruction(&Instruction::LocalSet(int_part));
    func.instruction(&Instruction::F64Const(0.0f64));
    func.instruction(&Instruction::LocalSet(frac_part));
    func.instruction(&Instruction::F64Const(1.0f64));
    func.instruction(&Instruction::LocalSet(frac_div));

    // Skip whitespace
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte));
    emit_is_whitespace(&mut func, byte);
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Check sign
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte));
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(45)); // '-'
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::F64Const(-1.0f64));
    func.instruction(&Instruction::LocalSet(sign));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(43)); // '+'
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Parse integer part
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte));
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(57));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(int_part));
    func.instruction(&Instruction::F64Const(10.0f64));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::F64ConvertI32U);
    func.instruction(&Instruction::F64Add);
    func.instruction(&Instruction::LocalSet(int_part));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Check for '.'
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::I32Const(46)); // '.'
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));

    // Parse fractional digits
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte));
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(57));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(frac_div));
    func.instruction(&Instruction::F64Const(10.0f64));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::LocalSet(frac_div));
    func.instruction(&Instruction::LocalGet(frac_part));
    func.instruction(&Instruction::F64Const(10.0f64));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::F64ConvertI32U);
    func.instruction(&Instruction::F64Add);
    func.instruction(&Instruction::LocalSet(frac_part));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::End); // end if '.'
    func.instruction(&Instruction::End); // end if i < len

    // result = sign * (int_part + frac_part / frac_div)
    func.instruction(&Instruction::LocalGet(sign));
    func.instruction(&Instruction::LocalGet(int_part));
    func.instruction(&Instruction::LocalGet(frac_part));
    func.instruction(&Instruction::LocalGet(frac_div));
    func.instruction(&Instruction::F64Div);
    func.instruction(&Instruction::F64Add);
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_fromCharCode(code: i32) -> i32
// Create a 1-character string from a char code.
// ============================================================
fn build_str_from_char_code(arena_idx: u32) -> Function {
    // Params: code=0
    // Locals: ptr=1
    let locals = vec![(1, ValType::I32)]; // ptr
    let mut func = Function::new(locals);
    let (code, ptr) = (0u32, 1);

    // Allocate 5 bytes: 4 (length header) + 1 (byte)
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::I32Const(5));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // Store length = 1
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&mem_store_i32(0));

    // Store byte
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(code));
    func.instruction(&mem_store8(0));

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_repeat(s: i32, count: i32) -> i32
// ============================================================
fn build_str_repeat(arena_idx: u32) -> Function {
    // Params: s=0, count=1
    // Locals: s_len=2, new_len=3, ptr=4, i=5
    let locals = vec![
        (1, ValType::I32), // s_len
        (1, ValType::I32), // new_len
        (1, ValType::I32), // ptr
        (1, ValType::I32), // i
    ];
    let mut func = Function::new(locals);
    let (s, count) = (0u32, 1);
    let (s_len, new_len, ptr, i) = (2u32, 3, 4, 5);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(s_len));

    // new_len = s_len * count
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalSet(new_len));

    // Allocate
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&mem_store_i32(0));

    // Copy s_len bytes `count` times
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_padStart(s: i32, targetLen: i32, fill: i32) -> i32
// ============================================================
fn build_str_pad_start(arena_idx: u32) -> Function {
    build_pad(arena_idx, true)
}

// ============================================================
// __str_padEnd(s: i32, targetLen: i32, fill: i32) -> i32
// ============================================================
fn build_str_pad_end(arena_idx: u32) -> Function {
    build_pad(arena_idx, false)
}

fn build_pad(arena_idx: u32, pad_start: bool) -> Function {
    // Params: s=0, targetLen=1, fill=2
    // Locals: s_len=3, fill_len=4, pad_needed=5, ptr=6, i=7, fill_byte=8
    let locals = vec![
        (1, ValType::I32), // s_len
        (1, ValType::I32), // fill_len
        (1, ValType::I32), // pad_needed
        (1, ValType::I32), // ptr
        (1, ValType::I32), // i
        (1, ValType::I32), // fill_byte
    ];
    let mut func = Function::new(locals);
    let (s, target_len, fill) = (0u32, 1, 2);
    let (s_len, fill_len, pad_needed, ptr, i, fill_byte) = (3u32, 4, 5, 6, 7, 8);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(s_len));
    func.instruction(&Instruction::LocalGet(fill));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(fill_len));

    // If s_len >= targetLen, return s unchanged
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(target_len));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // pad_needed = targetLen - s_len
    func.instruction(&Instruction::LocalGet(target_len));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(pad_needed));

    // Allocate result: targetLen + 4
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(target_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(target_len));
    func.instruction(&mem_store_i32(0));

    if pad_start {
        // Write padding bytes first (cycling through fill string)
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(pad_needed));
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));
        // fill_byte = load8_u(fill + 4 + (i % fill_len))
        func.instruction(&Instruction::LocalGet(fill));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(fill_len));
        func.instruction(&Instruction::I32RemU);
        func.instruction(&Instruction::I32Add);
        func.instruction(&mem_load8_u(0));
        func.instruction(&Instruction::LocalSet(fill_byte));
        // store8(ptr + 4 + i, fill_byte)
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(fill_byte));
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        // Copy original string after padding
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(pad_needed));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(s));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(s_len));
        func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
    } else {
        // Copy original string first
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(s));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(s_len));
        func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

        // Write padding bytes after (cycling through fill string)
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(pad_needed));
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(fill));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(fill_len));
        func.instruction(&Instruction::I32RemU);
        func.instruction(&Instruction::I32Add);
        func.instruction(&mem_load8_u(0));
        func.instruction(&Instruction::LocalSet(fill_byte));
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(s_len));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(fill_byte));
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    }

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

/// `__str_concat(a, b) -> c` — allocate a new string with bytes of `a` then
/// `b`. Used by `Array.join` at runtime where the number of concatenations is
/// only known at runtime. Compile-time chains go through `emit_fused_string_chain`.
fn build_str_concat(arena_idx: u32) -> Function {
    let locals = vec![(4, ValType::I32)];
    let mut func = Function::new(locals);
    let (a, b, a_len, b_len, total, ptr) = (0u32, 1, 2, 3, 4, 5);

    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(a_len));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(b_len));

    func.instruction(&Instruction::LocalGet(a_len));
    func.instruction(&Instruction::LocalGet(b_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total));

    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&mem_store_i32(0));

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(a_len));
    func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(a_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(b_len));
    func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

