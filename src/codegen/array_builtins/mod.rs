mod predicate;
mod sort;
mod transform;

use oxc_ast::ast::*;
use oxc_span::GetSpan;
use wasm_encoder::Instruction;

use crate::error::CompileError;
use crate::types::WasmType;

pub(super) use super::expr::ARRAY_HEADER_SIZE;
use super::func::FuncContext;

/// Helper: get element size in bytes for an array element type.
pub(super) fn elem_size(ty: WasmType) -> Result<i32, CompileError> {
    match ty {
        WasmType::F64 => Ok(8),
        WasmType::I32 => Ok(4),
        _ => Err(CompileError::type_err("invalid array element type")),
    }
}

/// Helper: emit a load instruction for the given type at the address on the stack.
pub(super) fn emit_elem_load(func_ctx: &mut FuncContext, ty: WasmType) {
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
pub(super) fn emit_elem_store(func_ctx: &mut FuncContext, ty: WasmType) {
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
pub(super) fn emit_elem_addr(
    func_ctx: &mut FuncContext,
    arr_local: u32,
    idx_local: u32,
    esize: i32,
) {
    func_ctx.push(Instruction::LocalGet(arr_local));
    func_ctx.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
    func_ctx.push(Instruction::I32Add);
    func_ctx.push(Instruction::LocalGet(idx_local));
    func_ctx.push(Instruction::I32Const(esize));
    func_ctx.push(Instruction::I32Mul);
    func_ctx.push(Instruction::I32Add);
}

/// Helper: emit code to load arr.length (i32 at arr+0).
pub(super) fn emit_arr_length(func_ctx: &mut FuncContext, arr_local: u32) {
    func_ctx.push(Instruction::LocalGet(arr_local));
    func_ctx.push(Instruction::I32Load(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
}

/// Helper: emit inline push — store element at end, increment length.
/// Expects: result_local = array pointer, elem value on stack.
pub(super) fn emit_inline_push(
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
pub(super) fn emit_alloc_array(
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
pub(super) fn extract_arrow_params(
    arrow: &ArrowFunctionExpression,
) -> Result<Vec<String>, CompileError> {
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
pub(super) fn build_elem_index_bindings(
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
pub(crate) struct ArrowScope {
    param_names: Vec<String>,
    saved_locals: Vec<Option<(u32, WasmType)>>,
    saved_class_types: Vec<Option<String>>,
    saved_array_elem_types: Vec<Option<WasmType>>,
    saved_array_elem_classes: Vec<Option<String>>,
}

/// Set up arrow parameter bindings, returning scope info to restore later.
pub(crate) fn setup_arrow_scope<'a>(
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
pub(crate) fn restore_arrow_scope(func_ctx: &mut FuncContext, scope: ArrowScope) {
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
            "reduce" | "reduceRight" => {
                if call.arguments.len() != 2 {
                    return Err(CompileError::codegen(format!(
                        "Array.{method_name}() expects 2 arguments (callback, initialValue)"
                    )));
                }
                let reverse = method_name == "reduceRight";
                let result = self.emit_array_reduce(
                    &member.object,
                    elem_ty,
                    elem_class.as_deref(),
                    call.arguments[0].to_expression(),
                    call.arguments[1].to_expression(),
                    reverse,
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
                if call.arguments.len() > 1 {
                    return Err(CompileError::codegen(
                        "Array.sort() expects 0 or 1 arguments (optional comparator)",
                    ));
                }
                let callback = call.arguments.first().map(|a| a.to_expression());
                self.emit_array_sort(&member.object, elem_ty, elem_class.as_deref(), callback)?;
                // sort returns the same array (mutates in place)
                self.emit_expr(&member.object)?;
                Ok(Some(WasmType::I32))
            }
            "toSorted" => {
                if call.arguments.len() > 1 {
                    return Err(CompileError::codegen(
                        "Array.toSorted() expects 0 or 1 arguments (optional comparator)",
                    ));
                }
                let callback = call.arguments.first().map(|a| a.to_expression());
                self.emit_array_to_sorted(
                    &member.object,
                    elem_ty,
                    elem_class.as_deref(),
                    callback,
                )?;
                Ok(Some(WasmType::I32))
            }
            _ => Ok(None),
        }
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
            Expression::TSAsExpression(as_expr) => {
                // `x as f64` / `x as i32` resolves to the annotated type
                // regardless of the source expression. Falls back to recursing
                // into the source if the target annotation doesn't resolve.
                crate::types::resolve_ts_type(
                    &as_expr.type_annotation,
                    &self.module_ctx.class_names,
                )
                .or_else(|_| {
                    self.infer_expr_type(&as_expr.expression, arrow_params, elem_ty, elem_class)
                })
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
