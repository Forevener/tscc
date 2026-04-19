use std::collections::{HashMap, HashSet};

use oxc_ast::ast::*;
use wasm_encoder::{Function, Instruction, ValType};

use crate::error::{self, CompileError};
use crate::types::{self, ClosureSig, TypeBindings, WasmType};

use super::module::ModuleContext;

/// One emit slot in a function body. Most of tscc pushes `Instruction` values
/// one at a time; the splicer (L_splice) pastes a pre-rewritten helper body as
/// a single `RawBytes` chunk to avoid a wasmparser→wasm_encoder op-conversion
/// table that would grow unboundedly with every helper.
pub enum EmittedChunk {
    Instruction(Instruction<'static>),
    RawBytes(Vec<u8>),
}

pub struct FuncContext<'a> {
    pub module_ctx: &'a ModuleContext,
    pub locals: HashMap<String, (u32, WasmType)>,
    pub emitted: Vec<EmittedChunk>,
    local_types: Vec<ValType>,
    param_count: u32,
    #[allow(dead_code)]
    return_type: WasmType,
    /// Stack of (break_depth, continue_depth) for loops
    pub(crate) loop_stack: Vec<LoopLabels>,
    /// Current WASM block nesting depth
    pub(crate) block_depth: u32,
    /// If inside a method, the class name (for `this` resolution)
    pub this_class: Option<String>,
    /// Track which local variables hold class instances: var_name -> class_name
    pub local_class_types: HashMap<String, String>,
    /// Track which local variables hold arrays: var_name -> element WasmType
    pub local_array_elem_types: HashMap<String, WasmType>,
    /// Track which local array variables hold class-typed elements: var_name -> class_name
    pub local_array_elem_classes: HashMap<String, String>,
    /// Source text for error location reporting
    pub source: &'a str,
    /// Variables declared with `const` — assignment to these is a compile error
    pub const_locals: HashSet<String>,
    /// Track which local variables hold closures: var_name -> closure signature
    pub local_closure_sigs: HashMap<String, ClosureSig>,
    /// Variables that need boxing (captured by closure AND mutated) — stored via arena pointer
    pub boxed_vars: HashSet<String>,
    /// Original types of boxed variables (since their local holds an i32 pointer)
    pub boxed_var_types: HashMap<String, WasmType>,
    /// Track which local variables hold string pointers
    pub local_string_vars: HashSet<String>,
    /// Source map: (instruction_index, source_byte_offset) for DWARF debug info
    pub source_map: Vec<(usize, u32)>,
    /// When Some(idx), method calls that would normally re-evaluate their
    /// receiver expression use `LocalGet(idx)` instead. Used by optional-call
    /// codegen (`obj?.m()`) to null-check a receiver without double evaluation.
    pub method_receiver_override: Option<u32>,
    /// Type-parameter bindings for the surrounding monomorphized class or
    /// function. When a field/param/return annotation mentions a name present
    /// in this map, the binding substitutes for the annotation during type
    /// resolution. `None` for non-generic code.
    pub type_bindings: Option<TypeBindings>,
}

pub(crate) struct LoopLabels {
    pub(crate) break_depth: u32,
    pub(crate) continue_depth: u32,
}

impl<'a> FuncContext<'a> {
    pub fn new(
        module_ctx: &'a ModuleContext,
        params: &[(String, WasmType)],
        return_type: WasmType,
        source: &'a str,
    ) -> Self {
        let mut locals = HashMap::new();
        for (i, (name, ty)) in params.iter().enumerate() {
            locals.insert(name.clone(), (i as u32, *ty));
        }
        FuncContext {
            module_ctx,
            locals,
            emitted: Vec::new(),
            local_types: Vec::new(),
            param_count: params.len() as u32,
            return_type,
            loop_stack: Vec::new(),
            block_depth: 0,
            this_class: None,
            local_class_types: HashMap::new(),
            local_array_elem_types: HashMap::new(),
            local_array_elem_classes: HashMap::new(),
            source,
            const_locals: HashSet::new(),
            local_closure_sigs: HashMap::new(),
            boxed_vars: HashSet::new(),
            boxed_var_types: HashMap::new(),
            local_string_vars: HashSet::new(),
            source_map: Vec::new(),
            method_receiver_override: None,
            type_bindings: None,
        }
    }

    pub fn push(&mut self, inst: Instruction<'static>) {
        self.emitted.push(EmittedChunk::Instruction(inst));
    }

    /// Append pre-encoded opcode bytes as a single emit slot. Used by the
    /// L_splice splicer to paste a rewritten helper body without round-
    /// tripping every operator through the `Instruction` enum.
    pub fn push_raw_bytes(&mut self, bytes: Vec<u8>) {
        self.emitted.push(EmittedChunk::RawBytes(bytes));
    }

    /// Record a source location for the next chunk to be emitted. A raw-byte
    /// chunk maps to one source position for its entire byte range — fine,
    /// since the bytes come from a precompiled helper with no TS source.
    pub fn mark_loc(&mut self, source_offset: u32) {
        self.source_map.push((self.emitted.len(), source_offset));
    }

    pub fn alloc_local(&mut self, ty: WasmType) -> u32 {
        let vt = ty.to_val_type().unwrap_or(ValType::I32);
        let idx = self.param_count + self.local_types.len() as u32;
        self.local_types.push(vt);
        idx
    }

    pub fn declare_local(&mut self, name: &str, ty: WasmType) -> u32 {
        let idx = self.alloc_local(ty);
        self.locals.insert(name.to_string(), (idx, ty));
        idx
    }

    /// Emit arena allocation: pushes `size` (i32) on stack, returns pointer in a new local.
    /// If __arena_alloc is registered (overflow checking enabled), calls it.
    /// Otherwise, does inline bump (original behavior).
    pub fn emit_arena_alloc_to_local(&mut self, size_on_stack: bool) -> Result<u32, CompileError> {
        let arena_idx = self
            .module_ctx
            .arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;
        let ptr_local = self.alloc_local(WasmType::I32);

        if let Some(alloc_idx) = self.module_ctx.arena_alloc_func {
            // size is already on stack
            if !size_on_stack {
                return Err(CompileError::codegen(
                    "emit_arena_alloc_to_local requires size on stack",
                ));
            }
            self.push(Instruction::Call(alloc_idx));
            self.push(Instruction::LocalSet(ptr_local));
        } else {
            // Inline bump: ptr = arena_ptr; arena_ptr += size
            if size_on_stack {
                let size_local = self.alloc_local(WasmType::I32);
                self.push(Instruction::LocalSet(size_local));
                self.push(Instruction::GlobalGet(arena_idx));
                self.push(Instruction::LocalSet(ptr_local));
                self.push(Instruction::GlobalGet(arena_idx));
                self.push(Instruction::LocalGet(size_local));
                self.push(Instruction::I32Add);
                self.push(Instruction::GlobalSet(arena_idx));
            } else {
                return Err(CompileError::codegen(
                    "emit_arena_alloc_to_local requires size on stack",
                ));
            }
        }
        Ok(ptr_local)
    }

    /// Attach a source location to an error using a byte offset (from oxc Span).
    pub fn locate(&self, err: CompileError, offset: u32) -> CompileError {
        err.with_loc(error::offset_to_loc(self.source, offset))
    }

    /// Finish the function, returning the encoded WASM function and a source map.
    /// Source map entries are (byte_offset_in_func_body, source_byte_offset).
    pub fn finish(self) -> (Function, Vec<(u32, u32)>) {
        let local_groups: Vec<(u32, ValType)> =
            self.local_types.iter().map(|vt| (1, *vt)).collect();
        let mut func = Function::new(local_groups);

        // Track byte offset of the start of each chunk within the function body
        let mut chunk_byte_offsets: Vec<u32> = Vec::with_capacity(self.emitted.len());
        for chunk in &self.emitted {
            chunk_byte_offsets.push(func.byte_len() as u32);
            match chunk {
                EmittedChunk::Instruction(inst) => {
                    func.instruction(inst);
                }
                EmittedChunk::RawBytes(bytes) => {
                    func.raw(bytes.iter().copied());
                }
            }
        }
        func.instruction(&Instruction::End);

        // Convert source_map from chunk indices to byte offsets
        let byte_source_map: Vec<(u32, u32)> = self
            .source_map
            .iter()
            .filter_map(|&(chunk_idx, src_offset)| {
                chunk_byte_offsets
                    .get(chunk_idx)
                    .map(|&byte_off| (byte_off, src_offset))
            })
            .collect();

        (func, byte_source_map)
    }

    /// Infer the type of an expression without emitting WASM code.
    /// Used for type inference when no annotation is provided.
    pub fn infer_init_type(
        &self,
        expr: &Expression<'a>,
    ) -> Result<(WasmType, Option<String>), CompileError> {
        match expr {
            Expression::NumericLiteral(lit) => {
                if lit.raw.as_ref().is_some_and(|r| r.contains('.')) || lit.value.fract() != 0.0 {
                    Ok((WasmType::F64, None))
                } else {
                    Ok((WasmType::I32, None))
                }
            }
            Expression::BooleanLiteral(_) => Ok((WasmType::I32, None)),
            Expression::StringLiteral(_) => Ok((WasmType::I32, None)),
            Expression::NullLiteral(_) => Err(CompileError::type_err(
                "cannot infer type from null — add a type annotation",
            )),
            Expression::Identifier(ident) => {
                let name = ident.name.as_str();
                if let Some(&(_, ty)) = self.locals.get(name) {
                    let class = self.local_class_types.get(name).cloned();
                    Ok((ty, class))
                } else if let Some(&(_, ty)) = self.module_ctx.globals.get(name) {
                    let class = self.module_ctx.var_class_types.get(name).cloned();
                    Ok((ty, class))
                } else {
                    Err(CompileError::type_err(format!(
                        "cannot infer type from undefined variable '{name}'"
                    )))
                }
            }
            Expression::NewExpression(new_expr) => {
                if let Expression::Identifier(ident) = &new_expr.callee {
                    let class_name = ident.name.as_str();
                    if class_name == "Array" {
                        // Array<T> → i32, but we need the element type from type params
                        // For now, require annotation for arrays
                        return Err(CompileError::type_err(
                            "Array variables require a type annotation: Array<T>",
                        ));
                    }
                    if self.module_ctx.class_names.contains(class_name) {
                        return Ok((WasmType::I32, Some(class_name.to_string())));
                    }
                }
                Ok((WasmType::I32, None))
            }
            Expression::CallExpression(call) => {
                if let Expression::Identifier(ident) = &call.callee {
                    let name = ident.name.as_str();
                    // Type cast functions
                    if name == "f64" {
                        return Ok((WasmType::F64, None));
                    }
                    if name == "i32" {
                        return Ok((WasmType::I32, None));
                    }
                    // Look up function return type
                    if let Some((_, ret_ty)) = self.module_ctx.get_func(name) {
                        return Ok((ret_ty, None));
                    }
                    // Look up closure variable return type
                    if let Some(sig) = self.local_closure_sigs.get(name) {
                        return Ok((sig.return_type, None));
                    }
                }
                // Check for Math.* calls
                if let Expression::StaticMemberExpression(member) = &call.callee
                    && let Expression::Identifier(ident) = &member.object
                    && ident.name.as_str() == "Math"
                {
                    return Ok((WasmType::F64, None));
                }
                Err(CompileError::type_err(
                    "cannot infer type from this expression — add a type annotation",
                ))
            }
            Expression::BinaryExpression(bin) => {
                // Comparisons always return i32
                match bin.operator {
                    BinaryOperator::LessThan
                    | BinaryOperator::LessEqualThan
                    | BinaryOperator::GreaterThan
                    | BinaryOperator::GreaterEqualThan
                    | BinaryOperator::StrictEquality
                    | BinaryOperator::Equality
                    | BinaryOperator::StrictInequality
                    | BinaryOperator::Inequality => {
                        return Ok((WasmType::I32, None));
                    }
                    _ => {}
                }
                // For arithmetic, infer from left operand
                self.infer_init_type(&bin.left)
            }
            Expression::UnaryExpression(un) => match un.operator {
                UnaryOperator::LogicalNot | UnaryOperator::BitwiseNot => Ok((WasmType::I32, None)),
                _ => self.infer_init_type(&un.argument),
            },
            Expression::ParenthesizedExpression(paren) => self.infer_init_type(&paren.expression),
            Expression::ConditionalExpression(cond) => self.infer_init_type(&cond.consequent),
            Expression::StaticMemberExpression(member) => {
                // e.field — try to resolve class and field type
                let class_name = match self.resolve_expr_class(&member.object) {
                    Ok(name) => name,
                    Err(_) => {
                        return Err(CompileError::type_err(
                            "cannot infer type — add a type annotation",
                        ));
                    }
                };
                if let Some(layout) = self.module_ctx.class_registry.get(&class_name)
                    && let Some(&(_, field_ty)) =
                        layout.field_map.get(member.property.name.as_str())
                {
                    return Ok((field_ty, None));
                }
                Err(CompileError::type_err(
                    "cannot infer type — add a type annotation",
                ))
            }
            // Arrow functions are closure pointers (i32)
            Expression::ArrowFunctionExpression(_) => Ok((WasmType::I32, None)),
            // Array literals [a, b, c] are pointers into the arena. Element
            // type tracking happens at the var-decl layer where we have the
            // target name; the local itself is always an i32 handle.
            Expression::ArrayExpression(a) => {
                if a.elements.is_empty() {
                    Err(CompileError::type_err(
                        "cannot infer type of empty array literal — add a type annotation: `let x: number[] = []`",
                    ))
                } else {
                    Ok((WasmType::I32, None))
                }
            }
            _ => Err(CompileError::type_err(
                "cannot infer type from this expression — add a type annotation",
            )),
        }
    }

    /// Infer a ClosureSig from an ArrowFunctionExpression's parameter annotations and return type.
    pub fn infer_arrow_sig(&self, arrow: &ArrowFunctionExpression<'a>) -> Option<ClosureSig> {
        let mut param_types = Vec::new();
        for param in &arrow.params.items {
            let ty = if let Some(ann) = &param.type_annotation {
                types::resolve_type_annotation_with_classes(ann, &self.module_ctx.class_names)
                    .ok()?
            } else {
                return None;
            };
            param_types.push(ty);
        }
        let return_type = if let Some(ann) = &arrow.return_type {
            types::resolve_type_annotation_with_classes(ann, &self.module_ctx.class_names).ok()?
        } else {
            // Try to infer from expression body
            if arrow.expression {
                if let Some(Statement::ExpressionStatement(e)) = arrow.body.statements.first() {
                    self.infer_init_type(&e.expression).ok().map(|(ty, _)| ty)?
                } else {
                    return None;
                }
            } else {
                WasmType::Void
            }
        };
        Some(ClosureSig {
            param_types,
            return_type,
        })
    }
}

// ── Boxing analysis ─────────────────────────────────────────────────
// Identifies variables that need boxing: captured by a closure AND mutated anywhere.

/// Analyze a function body and return the set of variable names that need boxing.
pub fn analyze_boxed_vars(body: &[Statement]) -> HashSet<String> {
    let mut captured = HashSet::new(); // vars referenced inside arrow bodies
    let mut mutated = HashSet::new(); // vars assigned or updated anywhere

    for stmt in body {
        scan_stmt_for_boxing(stmt, &mut captured, &mut mutated, false);
    }

    // Intersection: only box vars that are both captured AND mutated
    captured.intersection(&mutated).cloned().collect()
}

fn scan_stmt_for_boxing<'a>(
    stmt: &Statement<'a>,
    captured: &mut HashSet<String>,
    mutated: &mut HashSet<String>,
    in_arrow: bool,
) {
    match stmt {
        Statement::ExpressionStatement(e) => {
            scan_expr_for_boxing(&e.expression, captured, mutated, in_arrow)
        }
        Statement::ReturnStatement(r) => {
            if let Some(arg) = &r.argument {
                scan_expr_for_boxing(arg, captured, mutated, in_arrow);
            }
        }
        Statement::VariableDeclaration(v) => {
            for decl in &v.declarations {
                if let Some(init) = &decl.init {
                    scan_expr_for_boxing(init, captured, mutated, in_arrow);
                }
            }
        }
        Statement::IfStatement(i) => {
            scan_expr_for_boxing(&i.test, captured, mutated, in_arrow);
            scan_stmt_for_boxing(&i.consequent, captured, mutated, in_arrow);
            if let Some(alt) = &i.alternate {
                scan_stmt_for_boxing(alt, captured, mutated, in_arrow);
            }
        }
        Statement::WhileStatement(w) => {
            scan_expr_for_boxing(&w.test, captured, mutated, in_arrow);
            scan_stmt_for_boxing(&w.body, captured, mutated, in_arrow);
        }
        Statement::DoWhileStatement(d) => {
            scan_stmt_for_boxing(&d.body, captured, mutated, in_arrow);
            scan_expr_for_boxing(&d.test, captured, mutated, in_arrow);
        }
        Statement::ForStatement(f) => {
            if let Some(ForStatementInit::VariableDeclaration(v)) = &f.init {
                for decl in &v.declarations {
                    if let Some(init) = &decl.init {
                        scan_expr_for_boxing(init, captured, mutated, in_arrow);
                    }
                }
            }
            if let Some(test) = &f.test {
                scan_expr_for_boxing(test, captured, mutated, in_arrow);
            }
            if let Some(update) = &f.update {
                scan_expr_for_boxing(update, captured, mutated, in_arrow);
            }
            scan_stmt_for_boxing(&f.body, captured, mutated, in_arrow);
        }
        Statement::ForOfStatement(f) => {
            scan_expr_for_boxing(&f.right, captured, mutated, in_arrow);
            scan_stmt_for_boxing(&f.body, captured, mutated, in_arrow);
        }
        Statement::BlockStatement(b) => {
            for s in &b.body {
                scan_stmt_for_boxing(s, captured, mutated, in_arrow);
            }
        }
        Statement::SwitchStatement(s) => {
            scan_expr_for_boxing(&s.discriminant, captured, mutated, in_arrow);
            for case in &s.cases {
                if let Some(test) = &case.test {
                    scan_expr_for_boxing(test, captured, mutated, in_arrow);
                }
                for s in &case.consequent {
                    scan_stmt_for_boxing(s, captured, mutated, in_arrow);
                }
            }
        }
        _ => {}
    }
}

fn scan_expr_for_boxing<'a>(
    expr: &Expression<'a>,
    captured: &mut HashSet<String>,
    mutated: &mut HashSet<String>,
    in_arrow: bool,
) {
    match expr {
        Expression::Identifier(ident) => {
            if in_arrow {
                captured.insert(ident.name.as_str().to_string());
            }
        }
        Expression::AssignmentExpression(a) => {
            if let AssignmentTarget::AssignmentTargetIdentifier(ident) = &a.left {
                mutated.insert(ident.name.as_str().to_string());
                if in_arrow {
                    captured.insert(ident.name.as_str().to_string());
                }
            }
            scan_expr_for_boxing(&a.right, captured, mutated, in_arrow);
        }
        Expression::UpdateExpression(u) => {
            if let SimpleAssignmentTarget::AssignmentTargetIdentifier(ident) = &u.argument {
                mutated.insert(ident.name.as_str().to_string());
                if in_arrow {
                    captured.insert(ident.name.as_str().to_string());
                }
            }
        }
        Expression::ArrowFunctionExpression(arrow) => {
            // Collect arrow parameter names to exclude from captures
            let param_names: HashSet<String> = arrow
                .params
                .items
                .iter()
                .filter_map(|p| match &p.pattern {
                    BindingPattern::BindingIdentifier(id) => Some(id.name.as_str().to_string()),
                    _ => None,
                })
                .collect();

            // Scan the arrow body with in_arrow=true
            let mut arrow_captured = HashSet::new();
            let mut arrow_mutated = HashSet::new();
            for stmt in &arrow.body.statements {
                scan_stmt_for_boxing(stmt, &mut arrow_captured, &mut arrow_mutated, true);
            }

            // Collect locally declared variables inside the arrow body
            let mut arrow_locals = HashSet::new();
            for stmt in &arrow.body.statements {
                collect_local_decls(stmt, &mut arrow_locals);
            }

            // Remove arrow params AND local declarations — they're not outer-scope variables
            for p in &param_names {
                arrow_captured.remove(p);
                arrow_mutated.remove(p);
            }
            for local in &arrow_locals {
                arrow_captured.remove(local);
                arrow_mutated.remove(local);
            }

            // Merge: only truly outer-scoped captures/mutations propagate up
            captured.extend(arrow_captured);
            mutated.extend(arrow_mutated);
        }
        Expression::BinaryExpression(b) => {
            scan_expr_for_boxing(&b.left, captured, mutated, in_arrow);
            scan_expr_for_boxing(&b.right, captured, mutated, in_arrow);
        }
        Expression::LogicalExpression(l) => {
            scan_expr_for_boxing(&l.left, captured, mutated, in_arrow);
            scan_expr_for_boxing(&l.right, captured, mutated, in_arrow);
        }
        Expression::UnaryExpression(u) => {
            scan_expr_for_boxing(&u.argument, captured, mutated, in_arrow)
        }
        Expression::CallExpression(c) => {
            scan_expr_for_boxing(&c.callee, captured, mutated, in_arrow);
            for arg in &c.arguments {
                scan_expr_for_boxing(arg.to_expression(), captured, mutated, in_arrow);
            }
        }
        Expression::StaticMemberExpression(m) => {
            scan_expr_for_boxing(&m.object, captured, mutated, in_arrow)
        }
        Expression::ComputedMemberExpression(m) => {
            scan_expr_for_boxing(&m.object, captured, mutated, in_arrow);
            scan_expr_for_boxing(&m.expression, captured, mutated, in_arrow);
        }
        Expression::ConditionalExpression(c) => {
            scan_expr_for_boxing(&c.test, captured, mutated, in_arrow);
            scan_expr_for_boxing(&c.consequent, captured, mutated, in_arrow);
            scan_expr_for_boxing(&c.alternate, captured, mutated, in_arrow);
        }
        Expression::ParenthesizedExpression(p) => {
            scan_expr_for_boxing(&p.expression, captured, mutated, in_arrow)
        }
        Expression::NewExpression(n) => {
            for arg in &n.arguments {
                scan_expr_for_boxing(arg.to_expression(), captured, mutated, in_arrow);
            }
        }
        Expression::TSAsExpression(a) => {
            scan_expr_for_boxing(&a.expression, captured, mutated, in_arrow)
        }
        Expression::ChainExpression(c) => match &c.expression {
            ChainElement::StaticMemberExpression(m) => {
                scan_expr_for_boxing(&m.object, captured, mutated, in_arrow)
            }
            ChainElement::ComputedMemberExpression(m) => {
                scan_expr_for_boxing(&m.object, captured, mutated, in_arrow);
                scan_expr_for_boxing(&m.expression, captured, mutated, in_arrow);
            }
            ChainElement::CallExpression(c) => {
                scan_expr_for_boxing(&c.callee, captured, mutated, in_arrow);
                for arg in &c.arguments {
                    scan_expr_for_boxing(arg.to_expression(), captured, mutated, in_arrow);
                }
            }
            _ => {}
        },
        _ => {}
    }
}

/// Collect variable names declared in a statement (for excluding arrow-internal decls).
fn collect_local_decls(stmt: &Statement, out: &mut HashSet<String>) {
    match stmt {
        Statement::VariableDeclaration(v) => {
            for decl in &v.declarations {
                if let BindingPattern::BindingIdentifier(ident) = &decl.id {
                    out.insert(ident.name.as_str().to_string());
                }
            }
        }
        Statement::BlockStatement(b) => {
            for s in &b.body {
                collect_local_decls(s, out);
            }
        }
        Statement::IfStatement(i) => {
            collect_local_decls(&i.consequent, out);
            if let Some(alt) = &i.alternate {
                collect_local_decls(alt, out);
            }
        }
        Statement::ForStatement(f) => {
            if let Some(ForStatementInit::VariableDeclaration(v)) = &f.init {
                for decl in &v.declarations {
                    if let BindingPattern::BindingIdentifier(ident) = &decl.id {
                        out.insert(ident.name.as_str().to_string());
                    }
                }
            }
            collect_local_decls(&f.body, out);
        }
        Statement::WhileStatement(w) => collect_local_decls(&w.body, out),
        Statement::DoWhileStatement(d) => collect_local_decls(&d.body, out),
        _ => {}
    }
}
