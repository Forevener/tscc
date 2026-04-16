use std::collections::HashSet;

use oxc_ast::ast::*;
use wasm_encoder::{Instruction, MemArg, ValType};

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::{ClosureSig, WasmType};

impl<'a> FuncContext<'a> {
    pub(crate) fn try_extract_arrow_expr<'b>(
        &self,
        expr: &'b Expression<'a>,
    ) -> Option<&'b ArrowFunctionExpression<'a>> {
        match expr {
            Expression::ArrowFunctionExpression(arrow) => Some(arrow),
            Expression::ParenthesizedExpression(paren) => {
                self.try_extract_arrow_expr(&paren.expression)
            }
            _ => None,
        }
    }
    // ── Boxed variable helpers ────────────────────────────────────────────

    /// Emit a load from a boxed variable. Assumes ptr is on the stack.
    pub(crate) fn emit_boxed_load(&mut self, ty: WasmType) {
        match ty {
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
    }

    /// Emit a store to a boxed variable. Assumes [ptr, value] are on the stack.
    pub(crate) fn emit_boxed_store(&mut self, ty: WasmType) {
        match ty {
            WasmType::F64 => self.push(Instruction::F64Store(MemArg {
                offset: 0,
                align: 3,
                memory_index: 0,
            })),
            _ => self.push(Instruction::I32Store(MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            })),
        }
    }

    // ── First-class closure support ──────────────────────────────────────

    /// Emit a call to a closure variable via call_indirect.
    /// Loads func_table_idx and env_ptr from the closure struct, pushes env_ptr + args, then call_indirect.
    pub(crate) fn emit_closure_call(
        &mut self,
        var_name: &str,
        sig: &ClosureSig,
        call: &CallExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        let closure_local = self
            .locals
            .get(var_name)
            .ok_or_else(|| {
                CompileError::codegen(format!("undefined closure variable '{var_name}'"))
            })?
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
        let type_idx = self
            .module_ctx
            .get_or_add_type_sig(call_params, call_results);

        // Push func_table_idx and call_indirect
        self.push(Instruction::LocalGet(func_idx_local));
        self.push(Instruction::CallIndirect {
            type_index: type_idx,
            table_index: 0,
        });

        Ok(sig.return_type)
    }

    /// Emit an arrow function as a first-class closure value.
    /// Result: i32 pointer to arena-allocated closure struct [func_table_idx: i32, env_ptr: i32].
    pub(crate) fn emit_arrow_closure(
        &mut self,
        arrow: &ArrowFunctionExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        // 1. Extract arrow parameter names and types
        let mut arrow_param_names = Vec::new();
        let mut arrow_param_types = Vec::new();
        for param in &arrow.params.items {
            let pname = match &param.pattern {
                BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
                _ => return Err(CompileError::unsupported("destructured closure parameter")),
            };
            let pty = if let Some(ann) = &param.type_annotation {
                crate::types::resolve_type_annotation_with_classes(
                    ann,
                    &self.module_ctx.class_names,
                )?
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
        let mut lifted_params: Vec<(String, WasmType)> =
            vec![("__env_ptr".to_string(), WasmType::I32)];
        for (pname, pty) in arrow_param_names.iter().zip(arrow_param_types.iter()) {
            lifted_params.push((pname.clone(), *pty));
        }

        // 5. Compile the arrow body in a new FuncContext
        let mut lifted_ctx =
            FuncContext::new(self.module_ctx, &lifted_params, return_type, self.source);

        // Set up captured variable access: each capture is loaded from env at its offset
        // We create locals in the lifted function and pre-load them from the env struct
        for cap in &captures {
            let cap_local = lifted_ctx.declare_local(&cap.name, cap.wasm_type);
            // Propagate class/array/closure metadata
            if let Some(ref cn) = cap.class_name {
                lifted_ctx
                    .local_class_types
                    .insert(cap.name.clone(), cn.clone());
            }
            if let Some(et) = cap.array_elem_type {
                lifted_ctx
                    .local_array_elem_types
                    .insert(cap.name.clone(), et);
            }
            if let Some(ref ec) = cap.array_elem_class {
                lifted_ctx
                    .local_array_elem_classes
                    .insert(cap.name.clone(), ec.clone());
            }
            if let Some(sig) = self.local_closure_sigs.get(&cap.name) {
                lifted_ctx
                    .local_closure_sigs
                    .insert(cap.name.clone(), sig.clone());
            }
            // Propagate boxed var status — the captured pointer is itself a box pointer
            if let Some(&boxed_ty) = self.boxed_var_types.get(&cap.name) {
                lifted_ctx
                    .boxed_var_types
                    .insert(cap.name.clone(), boxed_ty);
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
                if result_ty != return_type
                    && return_type == WasmType::F64
                    && result_ty == WasmType::I32
                {
                    lifted_ctx.push(Instruction::F64ConvertI32S);
                } else if result_ty != return_type
                    && return_type == WasmType::I32
                    && result_ty == WasmType::F64
                {
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
        let arena_idx = self
            .module_ctx
            .arena_ptr_global
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
            for s in &b.body {
                collect_local_decls_from_stmt(s, out);
            }
        }
        Statement::IfStatement(i) => {
            collect_local_decls_from_stmt(&i.consequent, out);
            if let Some(alt) = &i.alternate {
                collect_local_decls_from_stmt(alt, out);
            }
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
            if let Some(test) = &f.test {
                collect_identifiers_from_expr(test, out);
            }
            if let Some(update) = &f.update {
                collect_identifiers_from_expr(update, out);
            }
            collect_identifiers_from_stmt(&f.body, out);
        }
        Statement::BlockStatement(b) => {
            for s in &b.body {
                collect_identifiers_from_stmt(s, out);
            }
        }
        _ => {}
    }
}

fn collect_identifiers_from_expr<'a>(expr: &Expression<'a>, out: &mut HashSet<&'a str>) {
    match expr {
        Expression::Identifier(ident) => {
            out.insert(ident.name.as_str());
        }
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
            for arg in &c.arguments {
                collect_identifiers_from_expr(arg.to_expression(), out);
            }
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
            for arg in &n.arguments {
                collect_identifiers_from_expr(arg.to_expression(), out);
            }
        }
        Expression::TSAsExpression(a) => collect_identifiers_from_expr(&a.expression, out),
        Expression::ChainExpression(c) => match &c.expression {
            ChainElement::StaticMemberExpression(m) => {
                collect_identifiers_from_expr(&m.object, out)
            }
            ChainElement::ComputedMemberExpression(m) => {
                collect_identifiers_from_expr(&m.object, out);
                collect_identifiers_from_expr(&m.expression, out);
            }
            ChainElement::CallExpression(c) => {
                collect_identifiers_from_expr(&c.callee, out);
                for arg in &c.arguments {
                    collect_identifiers_from_expr(arg.to_expression(), out);
                }
            }
            _ => {}
        },
        _ => {}
    }
}
