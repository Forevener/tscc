use oxc_ast::ast::*;
use oxc_span::GetSpan;
use wasm_encoder::Instruction;

use crate::error::CompileError;
use crate::types::WasmType;

use super::expr::ARRAY_HEADER_SIZE;
use super::func::FuncContext;

/// Helper: get element size in bytes for an array element type.
fn elem_size(ty: WasmType) -> Result<i32, CompileError> {
    match ty {
        WasmType::F64 => Ok(8),
        WasmType::I32 => Ok(4),
        _ => Err(CompileError::type_err("invalid array element type")),
    }
}

/// Helper: emit a load instruction for the given type at the address on the stack.
fn emit_elem_load(func_ctx: &mut FuncContext, ty: WasmType) {
    match ty {
        WasmType::F64 => func_ctx.push(Instruction::F64Load(wasm_encoder::MemArg {
            offset: 0,
            align: 3,
            memory_index: 0,
        })),
        WasmType::I32 => func_ctx.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        })),
        _ => {}
    }
}

/// Helper: emit a store instruction for the given type.
/// Expects [addr, value] on the stack.
fn emit_elem_store(func_ctx: &mut FuncContext, ty: WasmType) {
    match ty {
        WasmType::F64 => func_ctx.push(Instruction::F64Store(wasm_encoder::MemArg {
            offset: 0,
            align: 3,
            memory_index: 0,
        })),
        WasmType::I32 => func_ctx.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        })),
        _ => {}
    }
}

/// Helper: emit code to compute element address: arr_ptr + HEADER + index * elem_size.
/// Leaves the address on the stack.
fn emit_elem_addr(func_ctx: &mut FuncContext, arr_local: u32, idx_local: u32, esize: i32) {
    func_ctx.push(Instruction::LocalGet(arr_local));
    func_ctx.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
    func_ctx.push(Instruction::I32Add);
    func_ctx.push(Instruction::LocalGet(idx_local));
    func_ctx.push(Instruction::I32Const(esize));
    func_ctx.push(Instruction::I32Mul);
    func_ctx.push(Instruction::I32Add);
}

/// Helper: emit code to load arr.length (i32 at arr+0).
fn emit_arr_length(func_ctx: &mut FuncContext, arr_local: u32) {
    func_ctx.push(Instruction::LocalGet(arr_local));
    func_ctx.push(Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
}

/// Helper: emit inline push — store element at end, increment length.
/// Expects: result_local = array pointer, elem value on stack.
fn emit_inline_push(
    func_ctx: &mut FuncContext,
    result_local: u32,
    elem_ty: WasmType,
) -> Result<(), CompileError> {
    let esize = elem_size(elem_ty)?;

    // Save the value to push
    let val_tmp = func_ctx.alloc_local(elem_ty);
    func_ctx.push(Instruction::LocalSet(val_tmp));

    // Load current length
    let len_tmp = func_ctx.alloc_local(WasmType::I32);
    emit_arr_length(func_ctx, result_local);
    func_ctx.push(Instruction::LocalSet(len_tmp));

    // Compute element address: result + 8 + length * elem_size
    emit_elem_addr(func_ctx, result_local, len_tmp, esize);

    // Store value
    func_ctx.push(Instruction::LocalGet(val_tmp));
    emit_elem_store(func_ctx, elem_ty);

    // Increment length
    func_ctx.push(Instruction::LocalGet(result_local));
    func_ctx.push(Instruction::LocalGet(len_tmp));
    func_ctx.push(Instruction::I32Const(1));
    func_ctx.push(Instruction::I32Add);
    func_ctx.push(Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));

    Ok(())
}

/// Helper: allocate a new array via arena bump.
/// Returns the local index holding the new array pointer.
fn emit_alloc_array(
    func_ctx: &mut FuncContext,
    capacity_local: u32,
    elem_ty: WasmType,
) -> Result<u32, CompileError> {
    let esize = elem_size(elem_ty)?;
    let arena_idx = func_ctx
        .module_ctx
        .arena_ptr_global
        .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

    // total_size = 8 + capacity * elem_size
    let size_local = func_ctx.alloc_local(WasmType::I32);
    func_ctx.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
    func_ctx.push(Instruction::LocalGet(capacity_local));
    func_ctx.push(Instruction::I32Const(esize));
    func_ctx.push(Instruction::I32Mul);
    func_ctx.push(Instruction::I32Add);
    func_ctx.push(Instruction::LocalSet(size_local));

    // ptr = __arena_ptr
    let ptr_local = func_ctx.alloc_local(WasmType::I32);
    func_ctx.push(Instruction::GlobalGet(arena_idx));
    func_ctx.push(Instruction::LocalSet(ptr_local));

    // __arena_ptr += total_size
    func_ctx.push(Instruction::GlobalGet(arena_idx));
    func_ctx.push(Instruction::LocalGet(size_local));
    func_ctx.push(Instruction::I32Add);
    func_ctx.push(Instruction::GlobalSet(arena_idx));

    // length = 0
    func_ctx.push(Instruction::LocalGet(ptr_local));
    func_ctx.push(Instruction::I32Const(0));
    func_ctx.push(Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));

    // capacity
    func_ctx.push(Instruction::LocalGet(ptr_local));
    func_ctx.push(Instruction::LocalGet(capacity_local));
    func_ctx.push(Instruction::I32Store(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));

    Ok(ptr_local)
}

/// Extract the arrow function from a call argument.
/// Returns the ArrowFunctionExpression or an error.
pub fn extract_arrow<'a, 'b>(
    arg: &'b Expression<'a>,
) -> Result<&'b ArrowFunctionExpression<'a>, CompileError> {
    match arg {
        Expression::ArrowFunctionExpression(arrow) => Ok(arrow),
        Expression::ParenthesizedExpression(paren) => extract_arrow(&paren.expression),
        _ => Err(CompileError::unsupported(
            "array builtin requires an arrow function argument",
        )),
    }
}

/// Extract parameter names from an arrow function.
fn extract_arrow_params(arrow: &ArrowFunctionExpression) -> Result<Vec<String>, CompileError> {
    let mut names = Vec::new();
    for param in &arrow.params.items {
        let name = match &param.pattern {
            BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
            _ => return Err(CompileError::unsupported("destructured arrow param")),
        };
        names.push(name);
    }
    Ok(names)
}

/// Build the `param_locals` and `param_class_types` vectors for an arrow
/// callback that takes `(elem)` or `(elem, index)`.
/// `params` must have 1 or 2 entries; the second (if present) is bound to the
/// loop index local.
fn build_elem_index_bindings(
    params: &[String],
    elem_local: u32,
    elem_ty: WasmType,
    i_local: u32,
    elem_class: Option<&str>,
) -> (Vec<(u32, WasmType)>, Vec<Option<String>>) {
    let mut locals = vec![(elem_local, elem_ty)];
    let mut classes: Vec<Option<String>> = vec![elem_class.map(|s| s.to_string())];
    if params.len() >= 2 {
        locals.push((i_local, WasmType::I32));
        classes.push(None);
    }
    (locals, classes)
}

/// Evaluate an arrow function body inline.
/// For expression arrows (`x => expr`), evaluates the expression and returns its type.
/// For block arrows (`x => { stmts; return val; }`), evaluates statements.
/// Returns the type of the result left on the WASM stack.
pub(crate) fn eval_arrow_body<'a>(
    func_ctx: &mut FuncContext<'a>,
    arrow: &ArrowFunctionExpression<'a>,
) -> Result<WasmType, CompileError> {
    // Record source location for the arrow body (inline closures in array builtins)
    func_ctx.mark_loc(arrow.span().start);

    if arrow.expression {
        // Expression body: single statement that IS the return value
        if let Some(stmt) = arrow.body.statements.first()
            && let Statement::ExpressionStatement(expr_stmt) = stmt
        {
            return func_ctx.emit_expr(&expr_stmt.expression);
        }
        Err(CompileError::codegen("empty arrow expression body"))
    } else {
        // Block body: emit all statements.
        // The return value should be on the stack from a return statement.
        // For block arrows used in builtins, we handle return specially.
        for stmt in &arrow.body.statements {
            match stmt {
                Statement::ReturnStatement(ret) => {
                    func_ctx.mark_loc(ret.span.start);
                    if let Some(arg) = &ret.argument {
                        return func_ctx.emit_expr(arg);
                    }
                    return Ok(WasmType::Void);
                }
                _ => {
                    func_ctx.emit_statement(stmt)?;
                }
            }
        }
        Ok(WasmType::Void)
    }
}

/// Set up a temporary local for an arrow parameter and bind it to a value local.
/// Returns the previous binding (if any) so it can be restored after the arrow body.
struct ArrowScope {
    param_names: Vec<String>,
    saved_locals: Vec<Option<(u32, WasmType)>>,
    saved_class_types: Vec<Option<String>>,
    saved_array_elem_types: Vec<Option<WasmType>>,
    saved_array_elem_classes: Vec<Option<String>>,
}

/// Set up arrow parameter bindings, returning scope info to restore later.
fn setup_arrow_scope<'a>(
    func_ctx: &mut FuncContext<'a>,
    params: &[String],
    param_locals: &[(u32, WasmType)],
    param_class_types: &[Option<String>],
) -> ArrowScope {
    let mut scope = ArrowScope {
        param_names: params.to_vec(),
        saved_locals: Vec::new(),
        saved_class_types: Vec::new(),
        saved_array_elem_types: Vec::new(),
        saved_array_elem_classes: Vec::new(),
    };

    for (i, name) in params.iter().enumerate() {
        // Save existing bindings
        scope.saved_locals.push(func_ctx.locals.get(name).copied());
        scope
            .saved_class_types
            .push(func_ctx.local_class_types.get(name).cloned());
        scope
            .saved_array_elem_types
            .push(func_ctx.local_array_elem_types.get(name).copied());
        scope
            .saved_array_elem_classes
            .push(func_ctx.local_array_elem_classes.get(name).cloned());

        // Bind arrow parameter
        let (local_idx, local_ty) = param_locals[i];
        func_ctx.locals.insert(name.clone(), (local_idx, local_ty));

        // Set class type if applicable
        if let Some(class_name) = &param_class_types[i] {
            func_ctx
                .local_class_types
                .insert(name.clone(), class_name.clone());
        } else {
            func_ctx.local_class_types.remove(name);
        }

        // Clear array types for arrow params (they're elements, not arrays)
        func_ctx.local_array_elem_types.remove(name);
        func_ctx.local_array_elem_classes.remove(name);
    }

    scope
}

/// Restore the previous variable bindings after arrow body evaluation.
fn restore_arrow_scope(func_ctx: &mut FuncContext, scope: ArrowScope) {
    for (i, name) in scope.param_names.iter().enumerate() {
        // Restore locals
        if let Some(prev) = scope.saved_locals[i] {
            func_ctx.locals.insert(name.clone(), prev);
        } else {
            func_ctx.locals.remove(name);
        }

        // Restore class types
        if let Some(prev) = &scope.saved_class_types[i] {
            func_ctx
                .local_class_types
                .insert(name.clone(), prev.clone());
        } else {
            func_ctx.local_class_types.remove(name);
        }

        // Restore array elem types
        if let Some(prev) = scope.saved_array_elem_types[i] {
            func_ctx.local_array_elem_types.insert(name.clone(), prev);
        } else {
            func_ctx.local_array_elem_types.remove(name);
        }

        // Restore array elem classes
        if let Some(prev) = &scope.saved_array_elem_classes[i] {
            func_ctx
                .local_array_elem_classes
                .insert(name.clone(), prev.clone());
        } else {
            func_ctx.local_array_elem_classes.remove(name);
        }
    }
}

// ---- Array builtin implementations ----

impl<'a> FuncContext<'a> {
    /// Try to emit an array builtin method call.
    /// Returns Some(type) if this was a recognized builtin, None otherwise.
    pub fn try_emit_array_builtin(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
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

        let elem_class = self.resolve_expr_array_elem_class(&member.object);

        match method_name {
            "filter" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen("Array.filter() expects 1 argument"));
                }
                let result = self.emit_array_filter(
                    &member.object,
                    elem_ty,
                    elem_class.as_deref(),
                    call.arguments[0].to_expression(),
                )?;
                Ok(Some(result))
            }
            "map" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen("Array.map() expects 1 argument"));
                }
                let result = self.emit_array_map(
                    &member.object,
                    elem_ty,
                    elem_class.as_deref(),
                    call.arguments[0].to_expression(),
                )?;
                Ok(Some(result))
            }
            "forEach" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen("Array.forEach() expects 1 argument"));
                }
                self.emit_array_foreach(
                    &member.object,
                    elem_ty,
                    elem_class.as_deref(),
                    call.arguments[0].to_expression(),
                )?;
                Ok(Some(WasmType::Void))
            }
            "reduce" => {
                if call.arguments.len() != 2 {
                    return Err(CompileError::codegen(
                        "Array.reduce() expects 2 arguments (callback, initialValue)",
                    ));
                }
                let result = self.emit_array_reduce(
                    &member.object,
                    elem_ty,
                    elem_class.as_deref(),
                    call.arguments[0].to_expression(),
                    call.arguments[1].to_expression(),
                )?;
                Ok(Some(result))
            }
            "some" | "every" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen(format!(
                        "Array.{method_name}() expects 1 argument"
                    )));
                }
                let all = method_name == "every";
                let result = self.emit_array_some_every(
                    &member.object,
                    elem_ty,
                    elem_class.as_deref(),
                    call.arguments[0].to_expression(),
                    all,
                )?;
                Ok(Some(result))
            }
            "find" | "findIndex" | "findLast" | "findLastIndex" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen(format!(
                        "Array.{method_name}() expects 1 argument"
                    )));
                }
                let reverse = method_name.starts_with("findLast");
                let return_index = method_name.ends_with("Index");
                let result = self.emit_array_find(
                    &member.object,
                    elem_ty,
                    elem_class.as_deref(),
                    call.arguments[0].to_expression(),
                    reverse,
                    return_index,
                )?;
                Ok(Some(result))
            }
            "sort" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen(
                        "Array.sort() expects 1 argument (comparator)",
                    ));
                }
                self.emit_array_sort(
                    &member.object,
                    elem_ty,
                    elem_class.as_deref(),
                    call.arguments[0].to_expression(),
                )?;
                // sort returns the same array (mutates in place)
                self.emit_expr(&member.object)?;
                Ok(Some(WasmType::I32))
            }
            _ => Ok(None),
        }
    }

    /// `arr.some(pred)` / `arr.every(pred)` — short-circuit scan. `some`
    /// returns 1 on first truthy predicate, 0 otherwise. `every` returns 0
    /// on first falsy predicate, 1 otherwise.
    fn emit_array_some_every(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: &Expression<'a>,
        all: bool,
    ) -> Result<WasmType, CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_arrow_params(arrow)?;
        if params.is_empty() || params.len() > 2 {
            return Err(CompileError::codegen(
                "some/every predicate must take 1-2 parameters",
            ));
        }
        let esize = elem_size(elem_ty)?;

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len = self.alloc_local(WasmType::I32);
        emit_arr_length(self, src_local);
        self.push(Instruction::LocalSet(src_len));

        let result_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(if all { 1 } else { 0 }));
        self.push(Instruction::LocalSet(result_local));

        let i_local = self.alloc_local(WasmType::I32);
        let elem_local = self.alloc_local(elem_ty);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(src_len));
        self.push(Instruction::I32GeU);
        self.push(Instruction::BrIf(1));

        emit_elem_addr(self, src_local, i_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(elem_local));

        let (param_locals, param_classes) =
            build_elem_index_bindings(&params, elem_local, elem_ty, i_local, elem_class);
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);
        let pred_ty = eval_arrow_body(self, arrow)?;
        if pred_ty != WasmType::I32 {
            return Err(CompileError::type_err(
                "some/every predicate must return i32/bool",
            ));
        }
        restore_arrow_scope(self, scope);

        if all {
            // If !pred: result = 0; break
            self.push(Instruction::I32Eqz);
            self.push(Instruction::If(wasm_encoder::BlockType::Empty));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::LocalSet(result_local));
            self.push(Instruction::Br(2));
            self.push(Instruction::End);
        } else {
            // If pred: result = 1; break
            self.push(Instruction::If(wasm_encoder::BlockType::Empty));
            self.push(Instruction::I32Const(1));
            self.push(Instruction::LocalSet(result_local));
            self.push(Instruction::Br(2));
            self.push(Instruction::End);
        }

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(result_local));
        Ok(WasmType::I32)
    }

    /// `arr.find(pred)` / `arr.findIndex(pred)` / `arr.findLast(pred)` /
    /// `arr.findLastIndex(pred)` — linear search returning the element or
    /// index of the first match (or last match, if `reverse`).
    ///
    /// When nothing matches:
    /// - `find*Index` returns -1 (matches JS).
    /// - `find` / `findLast` returns a default value (0 / 0.0) because our
    ///   typed world has no undefined for numeric or class pointer cells.
    ///   Scripts that need "not found" discrimination should use `findIndex`
    ///   or guard with `.some()`.
    fn emit_array_find(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: &Expression<'a>,
        reverse: bool,
        return_index: bool,
    ) -> Result<WasmType, CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_arrow_params(arrow)?;
        if params.is_empty() || params.len() > 2 {
            return Err(CompileError::codegen(
                "find predicate must take 1-2 parameters",
            ));
        }
        let esize = elem_size(elem_ty)?;

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len = self.alloc_local(WasmType::I32);
        emit_arr_length(self, src_local);
        self.push(Instruction::LocalSet(src_len));

        let i_local = self.alloc_local(WasmType::I32);
        let elem_local = self.alloc_local(elem_ty);
        let found_idx = self.alloc_local(WasmType::I32);
        let found_val = self.alloc_local(elem_ty);
        self.push(Instruction::I32Const(-1));
        self.push(Instruction::LocalSet(found_idx));
        match elem_ty {
            WasmType::F64 => {
                self.push(Instruction::F64Const(0.0));
                self.push(Instruction::LocalSet(found_val));
            }
            _ => {
                self.push(Instruction::I32Const(0));
                self.push(Instruction::LocalSet(found_val));
            }
        }

        if reverse {
            self.push(Instruction::LocalGet(src_len));
            self.push(Instruction::I32Const(1));
            self.push(Instruction::I32Sub);
        } else {
            self.push(Instruction::I32Const(0));
        }
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        self.push(Instruction::LocalGet(i_local));
        if reverse {
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
        } else {
            self.push(Instruction::LocalGet(src_len));
            self.push(Instruction::I32GeS);
        }
        self.push(Instruction::BrIf(1));

        emit_elem_addr(self, src_local, i_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(elem_local));

        let (param_locals, param_classes) =
            build_elem_index_bindings(&params, elem_local, elem_ty, i_local, elem_class);
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);
        let pred_ty = eval_arrow_body(self, arrow)?;
        if pred_ty != WasmType::I32 {
            return Err(CompileError::type_err(
                "find predicate must return i32/bool",
            ));
        }
        restore_arrow_scope(self, scope);

        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalSet(found_idx));
        self.push(Instruction::LocalGet(elem_local));
        self.push(Instruction::LocalSet(found_val));
        self.push(Instruction::Br(2));
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(if reverse { -1 } else { 1 }));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End);
        self.push(Instruction::End);

        if return_index {
            self.push(Instruction::LocalGet(found_idx));
            Ok(WasmType::I32)
        } else {
            self.push(Instruction::LocalGet(found_val));
            Ok(elem_ty)
        }
    }

    /// arr.filter(e => predicate) — returns a new array with elements where predicate is truthy.
    fn emit_array_filter(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: &Expression<'a>,
    ) -> Result<WasmType, CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_arrow_params(arrow)?;
        if params.is_empty() || params.len() > 2 {
            return Err(CompileError::codegen(
                "filter callback must have 1-2 parameters",
            ));
        }
        let esize = elem_size(elem_ty)?;

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len = self.alloc_local(WasmType::I32);
        emit_arr_length(self, src_local);
        self.push(Instruction::LocalSet(src_len));

        let result_local = emit_alloc_array(self, src_len, elem_ty)?;

        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        let elem_local = self.alloc_local(elem_ty);

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(src_len));
        self.push(Instruction::I32GeU);
        self.push(Instruction::BrIf(1));

        emit_elem_addr(self, src_local, i_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(elem_local));

        let (param_locals, param_classes) =
            build_elem_index_bindings(&params, elem_local, elem_ty, i_local, elem_class);
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);

        let pred_ty = eval_arrow_body(self, arrow)?;
        if pred_ty != WasmType::I32 {
            return Err(CompileError::type_err(
                "filter predicate must return i32/bool",
            ));
        }

        // Restore scope
        restore_arrow_scope(self, scope);

        // If truthy, push element to result
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::LocalGet(elem_local));
        emit_inline_push(self, result_local, elem_ty)?;
        self.push(Instruction::End);

        // i++
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End); // end loop
        self.push(Instruction::End); // end block

        // Return result array pointer
        self.push(Instruction::LocalGet(result_local));
        Ok(WasmType::I32)
    }

    /// arr.map(e => expr) — returns a new array with transformed elements.
    fn emit_array_map(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: &Expression<'a>,
    ) -> Result<WasmType, CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_arrow_params(arrow)?;
        if params.is_empty() || params.len() > 2 {
            return Err(CompileError::codegen(
                "map callback must have 1-2 parameters",
            ));
        }
        let esize = elem_size(elem_ty)?;

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len = self.alloc_local(WasmType::I32);
        emit_arr_length(self, src_local);
        self.push(Instruction::LocalSet(src_len));

        // We need to figure out the result element type by evaluating the arrow
        // on a dummy element. For now, we'll allocate with the same elem type
        // and determine the actual type during body evaluation.
        // Actually, we allocate the result after knowing the type. Let's do a
        // two-pass or just use a reasonable approach: evaluate the arrow body
        // once and track the type. Since we're inlining, we'll do it in the loop.

        // Temp local for element value
        let elem_local = self.alloc_local(elem_ty);

        // We need to determine the result element type first.
        // For the common case, we can infer it from the arrow's return type annotation
        // or from the first evaluation. Let's use a practical approach: if the arrow
        // param type is a class and we're accessing a field, we know the result type.
        // For now, allocate the result array assuming same elem_size as source.
        // We'll fix up if needed once we know the result type from first eval.

        // Actually, the cleanest approach: always allocate with i32 element type initially,
        // then use f64 if the mapped result is f64. Since we're inlining, we know
        // the result type at the point we push. Let's determine it upfront by
        // checking the arrow body type.

        // Pre-allocate result with max possible element size (f64=8).
        // The actual push will use the correct type.
        let result_elem_ty = self.infer_arrow_result_type(arrow, &params, elem_ty, elem_class)?;
        let result_esize = elem_size(result_elem_ty)?;
        let _ = result_esize; // used indirectly via emit_inline_push

        let result_local = emit_alloc_array(self, src_len, result_elem_ty)?;

        // Loop
        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        // if i >= src_len, break
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(src_len));
        self.push(Instruction::I32GeU);
        self.push(Instruction::BrIf(1));

        // Load element: src[i]
        emit_elem_addr(self, src_local, i_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(elem_local));

        let (param_locals, param_classes) =
            build_elem_index_bindings(&params, elem_local, elem_ty, i_local, elem_class);
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);

        // Evaluate arrow body — result value is on the stack
        let _result_ty = eval_arrow_body(self, arrow)?;

        // Restore scope
        restore_arrow_scope(self, scope);

        // Push result to output array
        emit_inline_push(self, result_local, result_elem_ty)?;

        // i++
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End); // end loop
        self.push(Instruction::End); // end block

        // Return result array pointer
        self.push(Instruction::LocalGet(result_local));
        Ok(WasmType::I32)
    }

    /// arr.forEach(e => { ... }) — execute callback for each element, no result.
    fn emit_array_foreach(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_arrow_params(arrow)?;
        if params.is_empty() || params.len() > 2 {
            return Err(CompileError::codegen(
                "forEach callback must have 1-2 parameters",
            ));
        }
        let esize = elem_size(elem_ty)?;

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len = self.alloc_local(WasmType::I32);
        emit_arr_length(self, src_local);
        self.push(Instruction::LocalSet(src_len));

        let elem_local = self.alloc_local(elem_ty);
        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(src_len));
        self.push(Instruction::I32GeU);
        self.push(Instruction::BrIf(1));

        emit_elem_addr(self, src_local, i_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(elem_local));

        let (param_locals, param_classes) =
            build_elem_index_bindings(&params, elem_local, elem_ty, i_local, elem_class);
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);

        let body_ty = eval_arrow_body(self, arrow)?;
        if body_ty != WasmType::Void {
            self.push(Instruction::Drop);
        }

        // Restore scope
        restore_arrow_scope(self, scope);

        // i++
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End); // end loop
        self.push(Instruction::End); // end block

        Ok(())
    }

    /// arr.reduce((acc, e) => expr, initialValue) — fold array to a single value.
    fn emit_array_reduce(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: &Expression<'a>,
        init_expr: &Expression<'a>,
    ) -> Result<WasmType, CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_arrow_params(arrow)?;
        if params.len() != 2 {
            return Err(CompileError::codegen(
                "reduce callback must have exactly 2 parameters (acc, elem)",
            ));
        }
        let esize = elem_size(elem_ty)?;

        // Evaluate initial value
        let acc_ty = self.emit_expr(init_expr)?;
        let acc_local = self.alloc_local(acc_ty);
        self.push(Instruction::LocalSet(acc_local));

        // Evaluate source array
        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len = self.alloc_local(WasmType::I32);
        emit_arr_length(self, src_local);
        self.push(Instruction::LocalSet(src_len));

        let elem_local = self.alloc_local(elem_ty);
        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        // if i >= src_len, break
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(src_len));
        self.push(Instruction::I32GeU);
        self.push(Instruction::BrIf(1));

        // Load element
        emit_elem_addr(self, src_local, i_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(elem_local));

        // Set up arrow scope: bind (acc, elem)
        let scope = setup_arrow_scope(
            self,
            &params,
            &[(acc_local, acc_ty), (elem_local, elem_ty)],
            &[None, elem_class.map(|s| s.to_string())],
        );

        // Evaluate arrow body — result is the new accumulator
        let body_ty = eval_arrow_body(self, arrow)?;
        if body_ty != acc_ty {
            return Err(CompileError::type_err(format!(
                "reduce callback returns {body_ty:?} but accumulator is {acc_ty:?}"
            )));
        }

        // Restore scope
        restore_arrow_scope(self, scope);

        // Update accumulator
        self.push(Instruction::LocalSet(acc_local));

        // i++
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End); // end loop
        self.push(Instruction::End); // end block

        // Return accumulator
        self.push(Instruction::LocalGet(acc_local));
        Ok(acc_ty)
    }

    /// arr.sort((a, b) => a - b) — in-place bottom-up iterative merge sort using comparator.
    /// O(n log n) via arena-allocated temp buffer.
    fn emit_array_sort(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let arrow = extract_arrow(callback)?;
        let params = extract_arrow_params(arrow)?;
        if params.len() != 2 {
            return Err(CompileError::codegen(
                "sort comparator must have exactly 2 parameters",
            ));
        }
        let esize = elem_size(elem_ty)?;

        // Evaluate array pointer
        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(arr_local));

        // Load length
        let len_local = self.alloc_local(WasmType::I32);
        emit_arr_length(self, arr_local);
        self.push(Instruction::LocalSet(len_local));

        // Allocate temp buffer via arena (same capacity as arr)
        let tmp_local = emit_alloc_array(self, len_local, elem_ty)?;

        // Merge sort locals
        let width_local = self.alloc_local(WasmType::I32);
        let i_local = self.alloc_local(WasmType::I32);
        let mid_local = self.alloc_local(WasmType::I32);
        let right_local = self.alloc_local(WasmType::I32);
        let l_local = self.alloc_local(WasmType::I32);
        let r_local = self.alloc_local(WasmType::I32);
        let k_local = self.alloc_local(WasmType::I32);
        let a_local = self.alloc_local(elem_ty);
        let b_local = self.alloc_local(elem_ty);
        let copy_idx = self.alloc_local(WasmType::I32);

        // width = 1
        self.push(Instruction::I32Const(1));
        self.push(Instruction::LocalSet(width_local));

        // === Outer loop: while width < len ===
        self.push(Instruction::Block(wasm_encoder::BlockType::Empty)); // $outer_break (depth 0 from block = br(1) from loop)
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty)); // $outer_loop

        // if width >= len, break
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break outer

        // i = 0
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        // === Inner loop: while i < len ===
        self.push(Instruction::Block(wasm_encoder::BlockType::Empty)); // $inner_break
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty)); // $inner_loop

        // if i >= len, break inner
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break inner

        // mid = min(i + width, len)
        // Emit: i + width
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Add);
        // Emit: min(i+width, len) using select: a, b, a < b => if true pick a else b
        self.push(Instruction::LocalGet(len_local));
        // Stack: [i+width, len]. We need: a, b, cond => select picks a if cond!=0
        // We want min(i+width, len): pick i+width if i+width < len, else pick len
        // select(a, b, cond): returns a if cond != 0, b otherwise
        // So: select(i+width, len, i+width < len)
        // But stack is [i+width, len] right now — need to dup for comparison.
        // Easier to use locals:
        self.push(Instruction::LocalSet(mid_local)); // mid_local = len (temporarily)
        // Recompute i+width and do comparison
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Add);
        // Stack: [i+width]
        self.push(Instruction::LocalGet(mid_local)); // [i+width, len]
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Add); // [i+width, len, i+width]
        self.push(Instruction::LocalGet(mid_local)); // [i+width, len, i+width, len]
        self.push(Instruction::I32LtS); // [i+width, len, i+width < len]
        self.push(Instruction::Select); // min(i+width, len)
        self.push(Instruction::LocalSet(mid_local));

        // right = min(i + 2*width, len)
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Const(2));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        // Stack: [i+2w]
        self.push(Instruction::LocalGet(len_local)); // [i+2w, len]
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Const(2));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add); // [i+2w, len, i+2w]
        self.push(Instruction::LocalGet(len_local)); // [i+2w, len, i+2w, len]
        self.push(Instruction::I32LtS); // [i+2w, len, i+2w < len]
        self.push(Instruction::Select); // min(i+2w, len)
        self.push(Instruction::LocalSet(right_local));

        // l = i (which is left)
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalSet(l_local));

        // r = mid
        self.push(Instruction::LocalGet(mid_local));
        self.push(Instruction::LocalSet(r_local));

        // k = i (which is left)
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalSet(k_local));

        // === Merge loop: while l < mid && r < right ===
        self.push(Instruction::Block(wasm_encoder::BlockType::Empty)); // $merge_break
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty)); // $merge_loop

        // if l >= mid, break merge
        self.push(Instruction::LocalGet(l_local));
        self.push(Instruction::LocalGet(mid_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break merge

        // if r >= right, break merge
        self.push(Instruction::LocalGet(r_local));
        self.push(Instruction::LocalGet(right_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break merge

        // Load arr[l] into a_local
        emit_elem_addr(self, arr_local, l_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(a_local));

        // Load arr[r] into b_local
        emit_elem_addr(self, arr_local, r_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(b_local));

        // Evaluate compare(a, b) via inline arrow body
        let scope = setup_arrow_scope(
            self,
            &params,
            &[(a_local, elem_ty), (b_local, elem_ty)],
            &[
                elem_class.map(|s| s.to_string()),
                elem_class.map(|s| s.to_string()),
            ],
        );

        let cmp_ty = eval_arrow_body(self, arrow)?;

        restore_arrow_scope(self, scope);

        // if compare(a, b) <= 0: copy arr[l] to tmp[k], l++
        // else: copy arr[r] to tmp[k], r++
        match cmp_ty {
            WasmType::I32 => {
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32LeS);
            }
            WasmType::F64 => {
                self.push(Instruction::F64Const(0.0f64));
                self.push(Instruction::F64Le);
            }
            _ => {
                return Err(CompileError::type_err(
                    "sort comparator must return i32 or f64",
                ));
            }
        }

        // if-else: comparator <= 0 means take from left
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));

        // tmp[k] = a_local (arr[l])
        emit_elem_addr(self, tmp_local, k_local, esize);
        self.push(Instruction::LocalGet(a_local));
        emit_elem_store(self, elem_ty);
        // l++
        self.push(Instruction::LocalGet(l_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(l_local));

        self.push(Instruction::Else);

        // tmp[k] = b_local (arr[r])
        emit_elem_addr(self, tmp_local, k_local, esize);
        self.push(Instruction::LocalGet(b_local));
        emit_elem_store(self, elem_ty);
        // r++
        self.push(Instruction::LocalGet(r_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(r_local));

        self.push(Instruction::End); // end if-else

        // k++
        self.push(Instruction::LocalGet(k_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(k_local));

        self.push(Instruction::Br(0)); // continue merge loop
        self.push(Instruction::End); // end merge loop
        self.push(Instruction::End); // end merge block

        // === Copy remaining left elements: while l < mid ===
        self.push(Instruction::Block(wasm_encoder::BlockType::Empty)); // $left_break
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty)); // $left_loop

        self.push(Instruction::LocalGet(l_local));
        self.push(Instruction::LocalGet(mid_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break left

        // tmp[k] = arr[l]
        emit_elem_addr(self, tmp_local, k_local, esize);
        emit_elem_addr(self, arr_local, l_local, esize);
        emit_elem_load(self, elem_ty);
        emit_elem_store(self, elem_ty);

        // l++
        self.push(Instruction::LocalGet(l_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(l_local));
        // k++
        self.push(Instruction::LocalGet(k_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(k_local));

        self.push(Instruction::Br(0)); // continue left loop
        self.push(Instruction::End); // end left loop
        self.push(Instruction::End); // end left block

        // === Copy remaining right elements: while r < right ===
        self.push(Instruction::Block(wasm_encoder::BlockType::Empty)); // $right_break
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty)); // $right_loop

        self.push(Instruction::LocalGet(r_local));
        self.push(Instruction::LocalGet(right_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break right

        // tmp[k] = arr[r]
        emit_elem_addr(self, tmp_local, k_local, esize);
        emit_elem_addr(self, arr_local, r_local, esize);
        emit_elem_load(self, elem_ty);
        emit_elem_store(self, elem_ty);

        // r++
        self.push(Instruction::LocalGet(r_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(r_local));
        // k++
        self.push(Instruction::LocalGet(k_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(k_local));

        self.push(Instruction::Br(0)); // continue right loop
        self.push(Instruction::End); // end right loop
        self.push(Instruction::End); // end right block

        // i += 2 * width
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Const(2));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Br(0)); // continue inner loop
        self.push(Instruction::End); // end inner loop
        self.push(Instruction::End); // end inner block

        // === Copy tmp data back to arr: for copy_idx = 0; copy_idx < len; copy_idx++ ===
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(copy_idx));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty)); // $copy_break
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty)); // $copy_loop

        self.push(Instruction::LocalGet(copy_idx));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break copy

        // arr[copy_idx] = tmp[copy_idx]
        emit_elem_addr(self, arr_local, copy_idx, esize);
        emit_elem_addr(self, tmp_local, copy_idx, esize);
        emit_elem_load(self, elem_ty);
        emit_elem_store(self, elem_ty);

        // copy_idx++
        self.push(Instruction::LocalGet(copy_idx));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(copy_idx));

        self.push(Instruction::Br(0)); // continue copy loop
        self.push(Instruction::End); // end copy loop
        self.push(Instruction::End); // end copy block

        // width *= 2
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Const(2));
        self.push(Instruction::I32Mul);
        self.push(Instruction::LocalSet(width_local));

        self.push(Instruction::Br(0)); // continue outer loop
        self.push(Instruction::End); // end outer loop
        self.push(Instruction::End); // end outer block

        Ok(())
    }

    /// Infer the return type of an arrow function without actually emitting code.
    /// This is a lightweight type check for map() to know the result array element type.
    pub fn infer_arrow_result_type(
        &self,
        arrow: &ArrowFunctionExpression<'a>,
        params: &[String],
        elem_ty: WasmType,
        elem_class: Option<&str>,
    ) -> Result<WasmType, CompileError> {
        // If the arrow has a return type annotation, use it
        if let Some(ret_ann) = &arrow.return_type {
            return crate::types::resolve_type_annotation_with_classes(
                ret_ann,
                &self.module_ctx.class_names,
            );
        }

        // For expression arrows, try to infer from the body expression
        if arrow.expression
            && let Some(stmt) = arrow.body.statements.first()
            && let Statement::ExpressionStatement(expr_stmt) = stmt
        {
            return self.infer_expr_type(&expr_stmt.expression, params, elem_ty, elem_class);
        }

        // Default to same type as input element
        Ok(elem_ty)
    }

    /// Lightweight type inference for expressions without emitting code.
    fn infer_expr_type(
        &self,
        expr: &Expression<'a>,
        arrow_params: &[String],
        elem_ty: WasmType,
        elem_class: Option<&str>,
    ) -> Result<WasmType, CompileError> {
        match expr {
            Expression::NumericLiteral(lit) => {
                if lit.raw.as_ref().is_some_and(|r| r.contains('.')) || lit.value.fract() != 0.0 {
                    Ok(WasmType::F64)
                } else {
                    Ok(WasmType::I32)
                }
            }
            Expression::BooleanLiteral(_) => Ok(WasmType::I32),
            Expression::Identifier(ident) => {
                let name = ident.name.as_str();
                // Check if it's an arrow param
                if arrow_params.contains(&name.to_string()) {
                    return Ok(elem_ty);
                }
                // Check locals
                if let Some(&(_, ty)) = self.locals.get(name) {
                    return Ok(ty);
                }
                // Check globals
                if let Some(&(_, ty)) = self.module_ctx.globals.get(name) {
                    return Ok(ty);
                }
                Ok(WasmType::I32) // fallback
            }
            Expression::StaticMemberExpression(member) => {
                // e.field — check if it's an arrow param with a class type
                if let Expression::Identifier(ident) = &member.object {
                    let name = ident.name.as_str();
                    if arrow_params.contains(&name.to_string())
                        && let Some(class_name) = elem_class
                        && let Some(layout) = self.module_ctx.class_registry.get(class_name)
                        && let Some(&(_, ty)) = layout.field_map.get(member.property.name.as_str())
                    {
                        return Ok(ty);
                    }
                }
                Ok(WasmType::I32) // fallback for pointers
            }
            Expression::BinaryExpression(bin) => {
                let left = self.infer_expr_type(&bin.left, arrow_params, elem_ty, elem_class)?;
                // Comparison operators always return i32
                match bin.operator {
                    BinaryOperator::LessThan
                    | BinaryOperator::LessEqualThan
                    | BinaryOperator::GreaterThan
                    | BinaryOperator::GreaterEqualThan
                    | BinaryOperator::StrictEquality
                    | BinaryOperator::Equality
                    | BinaryOperator::StrictInequality
                    | BinaryOperator::Inequality => Ok(WasmType::I32),
                    _ => Ok(left), // arithmetic preserves type
                }
            }
            Expression::UnaryExpression(un) => match un.operator {
                UnaryOperator::LogicalNot => Ok(WasmType::I32),
                _ => self.infer_expr_type(&un.argument, arrow_params, elem_ty, elem_class),
            },
            Expression::ParenthesizedExpression(paren) => {
                self.infer_expr_type(&paren.expression, arrow_params, elem_ty, elem_class)
            }
            Expression::CallExpression(call) => {
                // Check for method calls that have known return types
                if let Expression::StaticMemberExpression(member) = &call.callee
                    && let Expression::Identifier(ident) = &member.object
                    && ident.name.as_str() == "Math"
                {
                    return Ok(WasmType::F64);
                }
                // Check function return type
                if let Expression::Identifier(ident) = &call.callee
                    && let Some((_, ret_ty)) = self.module_ctx.get_func(ident.name.as_str())
                {
                    return Ok(ret_ty);
                }
                Ok(WasmType::I32) // fallback
            }
            _ => Ok(elem_ty), // fallback
        }
    }
}
