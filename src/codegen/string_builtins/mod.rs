mod compare;
mod convert;
mod search;
mod split_join;
mod transform;
use std::collections::HashSet;

use oxc_ast::ast::*;
use wasm_encoder::{Function, Instruction, MemArg};

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
    "__str_replaceAll",
    "__str_toFixed",
    "__str_toPrecision",
    "__str_lastIndexOf",
];

/// Register all string runtime helper functions in the module.
/// Must be called after Pass 2 (all user functions registered), before Pass 3 (codegen).
type HelperSig = (&'static str, Vec<(String, WasmType)>, WasmType);

pub fn register_string_helpers(ctx: &mut ModuleContext, used: &HashSet<String>) {
    let helpers: Vec<HelperSig> = vec![
        // __str_eq(a: i32, b: i32) -> i32
        (
            "__str_eq",
            vec![("a".into(), WasmType::I32), ("b".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_cmp(a: i32, b: i32) -> i32
        (
            "__str_cmp",
            vec![("a".into(), WasmType::I32), ("b".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_indexOf(haystack: i32, needle: i32) -> i32
        (
            "__str_indexOf",
            vec![
                ("haystack".into(), WasmType::I32),
                ("needle".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_lastIndexOf(haystack: i32, needle: i32) -> i32
        (
            "__str_lastIndexOf",
            vec![
                ("haystack".into(), WasmType::I32),
                ("needle".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_slice(s: i32, start: i32, end: i32) -> i32
        (
            "__str_slice",
            vec![
                ("s".into(), WasmType::I32),
                ("start".into(), WasmType::I32),
                ("end".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_startsWith(s: i32, prefix: i32) -> i32
        (
            "__str_startsWith",
            vec![
                ("s".into(), WasmType::I32),
                ("prefix".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_endsWith(s: i32, suffix: i32) -> i32
        (
            "__str_endsWith",
            vec![
                ("s".into(), WasmType::I32),
                ("suffix".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_includes(s: i32, search: i32) -> i32
        (
            "__str_includes",
            vec![
                ("s".into(), WasmType::I32),
                ("search".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_toLower(s: i32) -> i32
        (
            "__str_toLower",
            vec![("s".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_toUpper(s: i32) -> i32
        (
            "__str_toUpper",
            vec![("s".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_trim(s: i32) -> i32
        (
            "__str_trim",
            vec![("s".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_trimStart(s: i32) -> i32
        (
            "__str_trimStart",
            vec![("s".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_trimEnd(s: i32) -> i32
        (
            "__str_trimEnd",
            vec![("s".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_from_i32(n: i32) -> i32
        (
            "__str_from_i32",
            vec![("n".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_from_f64(n: f64) -> i32
        (
            "__str_from_f64",
            vec![("n".into(), WasmType::F64)],
            WasmType::I32,
        ),
        // __str_split(s: i32, delim: i32) -> i32 (returns Array<string> pointer)
        (
            "__str_split",
            vec![("s".into(), WasmType::I32), ("delim".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_replace(s: i32, search: i32, replacement: i32) -> i32
        (
            "__str_replace",
            vec![
                ("s".into(), WasmType::I32),
                ("search".into(), WasmType::I32),
                ("replacement".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_parseInt(s: i32) -> i32
        (
            "__str_parseInt",
            vec![("s".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_parseFloat(s: i32) -> f64
        (
            "__str_parseFloat",
            vec![("s".into(), WasmType::I32)],
            WasmType::F64,
        ),
        // __str_fromCharCode(code: i32) -> i32
        (
            "__str_fromCharCode",
            vec![("code".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_repeat(s: i32, count: i32) -> i32
        (
            "__str_repeat",
            vec![("s".into(), WasmType::I32), ("count".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_padStart(s: i32, targetLen: i32, fill: i32) -> i32
        (
            "__str_padStart",
            vec![
                ("s".into(), WasmType::I32),
                ("targetLen".into(), WasmType::I32),
                ("fill".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_padEnd(s: i32, targetLen: i32, fill: i32) -> i32
        (
            "__str_padEnd",
            vec![
                ("s".into(), WasmType::I32),
                ("targetLen".into(), WasmType::I32),
                ("fill".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_concat(a: i32, b: i32) -> i32 — runtime 2-string concat for
        // Array.join. String `+` goes through emit_fused_string_chain and does
        // NOT use this helper.
        (
            "__str_concat",
            vec![("a".into(), WasmType::I32), ("b".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_replaceAll(s: i32, search: i32, replacement: i32) -> i32
        (
            "__str_replaceAll",
            vec![
                ("s".into(), WasmType::I32),
                ("search".into(), WasmType::I32),
                ("replacement".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
        // __str_toFixed(n: f64, digits: i32) -> i32
        (
            "__str_toFixed",
            vec![("n".into(), WasmType::F64), ("digits".into(), WasmType::I32)],
            WasmType::I32,
        ),
        // __str_toPrecision(n: f64, precision: i32) -> i32
        (
            "__str_toPrecision",
            vec![
                ("n".into(), WasmType::F64),
                ("precision".into(), WasmType::I32),
            ],
            WasmType::I32,
        ),
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
        let add = |n: &str, set: &mut HashSet<String>| {
            set.insert(n.to_string());
        };

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
            "slice",
            "substring",
            "toLowerCase",
            "toUpperCase",
            "trim",
            "trimStart",
            "trimEnd",
            "replace",
            "replaceAll",
            "repeat",
            "padStart",
            "padEnd",
            "concat",
        ];
        let has_string_source = self.has_string_literal
            || self.has_string_from_char_code
            || string_returning_methods
                .iter()
                .any(|m| self.method_names.contains(*m));

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
            ("lastIndexOf", "__str_lastIndexOf"),
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
            ("replaceAll", "__str_replaceAll"),
            ("repeat", "__str_repeat"),
            ("padStart", "__str_padStart"),
            ("padEnd", "__str_padEnd"),
        ];
        for (method, helper) in method_map {
            if self.method_names.contains(*method) {
                add(helper, &mut used);
            }
        }

        // Number.prototype.toString() needs the coercion helpers.
        if self.method_names.contains("toString") {
            add("__str_from_i32", &mut used);
            add("__str_from_f64", &mut used);
        }

        // Number.prototype.toFixed(digits) needs the dedicated helper.
        if self.method_names.contains("toFixed") {
            add("__str_toFixed", &mut used);
        }

        // Number.prototype.toPrecision(digits) needs the dedicated helper.
        if self.method_names.contains("toPrecision") {
            add("__str_toPrecision", &mut used);
        }

        // `parseInt` / `parseFloat` can appear as bare identifiers OR as
        // `Number.parseInt` / `Number.parseFloat` (ES6 aliases). Method-name
        // presence is conservative — accepts any `.parseInt(...)` call.
        if self.identifier_calls.contains("parseInt") || self.method_names.contains("parseInt") {
            add("__str_parseInt", &mut used);
        }
        if self.identifier_calls.contains("parseFloat") || self.method_names.contains("parseFloat")
        {
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
                for s in &b.body {
                    self.walk_stmt(s);
                }
            }
            Statement::IfStatement(s) => {
                self.walk_expr(&s.test);
                self.walk_stmt(&s.consequent);
                if let Some(alt) = &s.alternate {
                    self.walk_stmt(alt);
                }
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
                if let Some(test) = &s.test {
                    self.walk_expr(test);
                }
                if let Some(update) = &s.update {
                    self.walk_expr(update);
                }
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
                    if let Some(t) = &case.test {
                        self.walk_expr(t);
                    }
                    for s in &case.consequent {
                        self.walk_stmt(s);
                    }
                }
            }
            Statement::ReturnStatement(s) => {
                if let Some(arg) = &s.argument {
                    self.walk_expr(arg);
                }
            }
            Statement::ThrowStatement(s) => self.walk_expr(&s.argument),
            Statement::TryStatement(s) => {
                for st in &s.block.body {
                    self.walk_stmt(st);
                }
                if let Some(h) = &s.handler {
                    for st in &h.body.body {
                        self.walk_stmt(st);
                    }
                }
                if let Some(f) = &s.finalizer {
                    for st in &f.body {
                        self.walk_stmt(st);
                    }
                }
            }
            Statement::LabeledStatement(s) => self.walk_stmt(&s.body),
            Statement::VariableDeclaration(d) => self.walk_var_decl(d),
            Statement::FunctionDeclaration(f) => {
                if let Some(body) = &f.body {
                    for st in &body.statements {
                        self.walk_stmt(st);
                    }
                }
            }
            Statement::ClassDeclaration(c) => {
                for element in &c.body.body {
                    if let ClassElement::MethodDefinition(m) = element
                        && let Some(body) = &m.value.body
                    {
                        for st in &body.statements {
                            self.walk_stmt(st);
                        }
                    }
                }
            }
            Statement::ExportDefaultDeclaration(e) => {
                if let ExportDefaultDeclarationKind::FunctionDeclaration(f) = &e.declaration
                    && let Some(body) = &f.body
                {
                    for st in &body.statements {
                        self.walk_stmt(st);
                    }
                }
            }
            Statement::ExportNamedDeclaration(e) => {
                if let Some(Declaration::FunctionDeclaration(f)) = &e.declaration
                    && let Some(body) = &f.body
                {
                    for st in &body.statements {
                        self.walk_stmt(st);
                    }
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
            if let Some(init) = &decl.init {
                self.walk_expr(init);
            }
        }
    }

    fn walk_expr(&mut self, expr: &Expression<'_>) {
        match expr {
            Expression::StringLiteral(_) => {
                self.has_string_literal = true;
            }
            Expression::TemplateLiteral(t) => {
                self.has_string_literal = true;
                if !t.expressions.is_empty() {
                    self.has_template_with_expr = true;
                }
                for e in &t.expressions {
                    self.walk_expr(e);
                }
            }
            Expression::BinaryExpression(b) => {
                use oxc_ast::ast::BinaryOperator as Op;
                match b.operator {
                    Op::Addition => self.has_plus = true,
                    Op::Equality | Op::Inequality | Op::StrictEquality | Op::StrictInequality => {
                        self.has_eq_op = true
                    }
                    Op::LessThan | Op::LessEqualThan | Op::GreaterThan | Op::GreaterEqualThan => {
                        self.has_cmp_op = true
                    }
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
            Expression::ChainExpression(c) => match &c.expression {
                ChainElement::CallExpression(call) => self.walk_call(call),
                ChainElement::StaticMemberExpression(m) => self.walk_expr(&m.object),
                ChainElement::ComputedMemberExpression(m) => {
                    self.walk_expr(&m.object);
                    self.walk_expr(&m.expression);
                }
                _ => {}
            },
            Expression::StaticMemberExpression(m) => self.walk_expr(&m.object),
            Expression::ComputedMemberExpression(m) => {
                self.walk_expr(&m.object);
                self.walk_expr(&m.expression);
            }
            Expression::ArrowFunctionExpression(a) => {
                for st in &a.body.statements {
                    self.walk_stmt(st);
                }
            }
            Expression::TSAsExpression(a) => self.walk_expr(&a.expression),
            Expression::SequenceExpression(s) => {
                for e in &s.expressions {
                    self.walk_expr(e);
                }
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
                    && method == "fromCharCode"
                {
                    self.has_string_from_char_code = true;
                }
                self.walk_expr(&m.object);
            }
            Expression::Identifier(ident) => {
                self.identifier_calls
                    .insert(ident.name.as_str().to_string());
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
        "__str_eq" => compare::build_str_eq(),
        "__str_cmp" => compare::build_str_cmp(),
        "__str_indexOf" => search::build_str_index_of(),
        "__str_lastIndexOf" | "__str_slice" => {
            super::precompiled::precompiled_function(name)
                .unwrap_or_else(|| panic!("precompiled {name} not found"))
        }
        "__str_startsWith" => search::build_str_starts_with(),
        "__str_endsWith" => search::build_str_ends_with(),
        "__str_includes" => search::build_str_includes(),
        "__str_toLower" => transform::build_str_to_lower(arena_idx),
        "__str_toUpper" => transform::build_str_to_upper(arena_idx),
        "__str_trim" => transform::build_str_trim_impl(arena_idx, true, true),
        "__str_trimStart" => transform::build_str_trim_impl(arena_idx, true, false),
        "__str_trimEnd" => transform::build_str_trim_impl(arena_idx, false, true),
        "__str_from_i32" => convert::build_str_from_i32(arena_idx),
        "__str_from_f64" => convert::build_str_from_f64(arena_idx),
        "__str_split" => split_join::build_str_split(arena_idx),
        "__str_replace" => transform::build_str_replace(arena_idx),
        "__str_parseInt" => convert::build_str_parse_int(),
        "__str_parseFloat" => convert::build_str_parse_float(),
        "__str_fromCharCode" => convert::build_str_from_char_code(arena_idx),
        "__str_repeat" => transform::build_str_repeat(arena_idx),
        "__str_padStart" => transform::build_str_pad_start(arena_idx),
        "__str_padEnd" => transform::build_str_pad_end(arena_idx),
        "__str_concat" => transform::build_str_concat(arena_idx),
        "__str_replaceAll" => transform::build_str_replace_all(arena_idx),
        "__str_toFixed" => convert::build_str_to_fixed(arena_idx),
        "__str_toPrecision" => convert::build_str_to_precision(arena_idx),
        _ => unreachable!("unknown string helper: {name}"),
    }
}

pub(super) fn mem_load_i32(offset: u64) -> Instruction<'static> {
    Instruction::I32Load(MemArg {
        offset,
        align: 2,
        memory_index: 0,
    })
}

pub(super) fn mem_store_i32(offset: u64) -> Instruction<'static> {
    Instruction::I32Store(MemArg {
        offset,
        align: 2,
        memory_index: 0,
    })
}

pub(super) fn mem_load8_u(offset: u64) -> Instruction<'static> {
    Instruction::I32Load8U(MemArg {
        offset,
        align: 0,
        memory_index: 0,
    })
}

pub(super) fn mem_store8(offset: u64) -> Instruction<'static> {
    Instruction::I32Store8(MemArg {
        offset,
        align: 0,
        memory_index: 0,
    })
}

/// Emit: (byte == 32 || byte == 9 || byte == 10 || byte == 13) → i32 on stack
pub(super) fn emit_is_whitespace(func: &mut Function, byte_local: u32) {
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
