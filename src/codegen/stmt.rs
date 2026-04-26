use oxc_ast::ast::*;
use oxc_span::GetSpan;
use wasm_encoder::{BlockType, Instruction};

use crate::error::CompileError;
use crate::types::{self, WasmType};

use super::func::{FuncContext, LoopLabels};

impl<'a> FuncContext<'a> {
    pub fn emit_statement(&mut self, stmt: &Statement<'a>) -> Result<(), CompileError> {
        // Record source location for debug info (skip empty statements)
        if !matches!(stmt, Statement::EmptyStatement(_)) {
            self.mark_loc(stmt.span().start);
        }
        match stmt {
            Statement::ExpressionStatement(expr_stmt) => {
                let ty = self.emit_expr(&expr_stmt.expression)?;
                // Drop the value if the expression left something on the stack
                if ty != WasmType::Void {
                    self.push(Instruction::Drop);
                }
                Ok(())
            }
            Statement::VariableDeclaration(var_decl) => self.emit_var_declaration(var_decl),
            Statement::ReturnStatement(ret) => self.emit_return(ret),
            Statement::IfStatement(if_stmt) => self.emit_if(if_stmt),
            Statement::WhileStatement(while_stmt) => self.emit_while(while_stmt),
            Statement::ForStatement(for_stmt) => self.emit_for(for_stmt),
            Statement::BlockStatement(block) => {
                for stmt in &block.body {
                    self.emit_statement(stmt)?;
                }
                Ok(())
            }
            Statement::ForOfStatement(for_of) => self.emit_for_of(for_of),
            Statement::SwitchStatement(switch) => self.emit_switch(switch),
            Statement::DoWhileStatement(do_while) => self.emit_do_while(do_while),
            Statement::BreakStatement(_) => self.emit_break(),
            Statement::ContinueStatement(_) => self.emit_continue(),
            Statement::EmptyStatement(_) => Ok(()),
            _ => Err(CompileError::unsupported("unsupported statement type")),
        }
    }

    fn emit_var_declaration(
        &mut self,
        var_decl: &VariableDeclaration<'a>,
    ) -> Result<(), CompileError> {
        for declarator in &var_decl.declarations {
            self.emit_var_declarator(declarator)?;
            // Track const for immutability enforcement
            if var_decl.kind == VariableDeclarationKind::Const
                && let BindingPattern::BindingIdentifier(ident) = &declarator.id
            {
                self.const_locals.insert(ident.name.as_str().to_string());
            }
        }
        Ok(())
    }

    pub fn emit_var_declarator(
        &mut self,
        decl: &VariableDeclarator<'a>,
    ) -> Result<(), CompileError> {
        match &decl.id {
            BindingPattern::BindingIdentifier(_) => self.emit_simple_var_declarator(decl),
            BindingPattern::ObjectPattern(obj_pat) => self.emit_object_destructuring(obj_pat, decl),
            BindingPattern::ArrayPattern(arr_pat) => self.emit_array_destructuring(arr_pat, decl),
            _ => Err(CompileError::unsupported(
                "assignment pattern with default value in destructuring",
            )),
        }
    }

    fn emit_simple_var_declarator(
        &mut self,
        decl: &VariableDeclarator<'a>,
    ) -> Result<(), CompileError> {
        let name = match &decl.id {
            BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
            _ => unreachable!(),
        };

        // Resolve type from annotation (on VariableDeclarator, not BindingPattern)
        let mut annotated_array_elem_ty: Option<WasmType> = None;
        let ty = if let Some(ann) = &decl.type_annotation {
            // Track closure signature from function type annotation
            if let Some(sig) = types::get_closure_sig(
                ann,
                &self.module_ctx.class_names,
                &self.module_ctx.non_i32_union_wasm_types,
            ) {
                self.local_closure_sigs.insert(name.clone(), sig);
            }
            // Track class type for property access resolution (threading
            // type_bindings so generic-param annotations inside monomorphized
            // methods resolve to the mangled instantiation).
            if let Some(class_name) = types::get_class_type_name_with_bindings(
                ann,
                self.type_bindings.as_ref(),
                Some(&self.module_ctx.shape_registry),
                Some(&self.module_ctx.union_registry),
            ) {
                self.local_class_types.insert(name.clone(), class_name);
            }
            // Track array element type
            if let Some(elem_ty) = types::get_array_element_type(
                ann,
                &self.module_ctx.class_names,
                &self.module_ctx.non_i32_union_wasm_types,
            )
            {
                self.local_array_elem_types.insert(name.clone(), elem_ty);
                annotated_array_elem_ty = Some(elem_ty);
                // Track array element class if applicable
                if let Some(elem_class) = types::get_array_element_class_with_bindings(
                    ann,
                    self.type_bindings.as_ref(),
                    Some(&self.module_ctx.shape_registry),
                    Some(&self.module_ctx.union_registry),
                ) {
                    self.local_array_elem_classes
                        .insert(name.clone(), elem_class);
                }
            }
            // Track string type
            if types::is_string_type_with_bindings(ann, self.type_bindings.as_ref()) {
                self.local_string_vars.insert(name.clone());
            }
            types::resolve_type_annotation_with_unions(
                ann,
                &self.module_ctx.class_names,
                self.type_bindings.as_ref(),
                &self.module_ctx.non_i32_union_wasm_types,
            )
            .map_err(|e| self.locate(e, decl.span.start))?
        } else if let Some(init) = &decl.init {
            // Infer closure sig from arrow initializer
            if let Expression::ArrowFunctionExpression(arrow) = init
                && let Some(sig) = self.infer_arrow_sig(arrow)
            {
                self.local_closure_sigs.insert(name.clone(), sig);
            }
            // Infer closure sig from function call that returns a closure
            if let Expression::CallExpression(call) = init
                && let Expression::Identifier(ident) = &call.callee
                && let Some(sig) = self
                    .module_ctx
                    .func_return_closure_sigs
                    .get(ident.name.as_str())
            {
                self.local_closure_sigs.insert(name.clone(), sig.clone());
            }
            // Infer string from string literal initializer
            if matches!(init, Expression::StringLiteral(_)) {
                self.local_string_vars.insert(name.clone());
            }
            // Infer array element type from a non-empty array literal initializer
            if let Expression::ArrayExpression(arr) = init
                && let Some(first) = arr.elements.first()
            {
                match first {
                    ArrayExpressionElement::SpreadElement(s) => {
                        if let Some(elem_ty) = self.resolve_expr_array_elem(&s.argument) {
                            self.local_array_elem_types.insert(name.clone(), elem_ty);
                            if let Some(class_name) =
                                self.resolve_expr_array_elem_class(&s.argument)
                            {
                                self.local_array_elem_classes
                                    .insert(name.clone(), class_name);
                            }
                        }
                    }
                    _ => {
                        if let Some(expr) = first.as_expression()
                            && let Ok((first_ty, first_class)) = self.infer_init_type(expr)
                        {
                            self.local_array_elem_types.insert(name.clone(), first_ty);
                            if let Some(class_name) = first_class {
                                self.local_array_elem_classes
                                    .insert(name.clone(), class_name);
                            }
                        }
                    }
                }
            }
            // Infer array element type from an array-returning call expression
            // (Array.of, Array.from, .slice/.filter/.map/...). The
            // `resolve_expr_array_elem` path already knows the rules — if it
            // can see an element type through the call, propagate it to the
            // new variable.
            if matches!(init, Expression::CallExpression(_))
                && let Some(elem_ty) = self.resolve_expr_array_elem(init)
            {
                self.local_array_elem_types.insert(name.clone(), elem_ty);
                if let Some(class_name) = self.resolve_expr_array_elem_class(init) {
                    self.local_array_elem_classes
                        .insert(name.clone(), class_name);
                }
            }
            let (inferred_ty, inferred_class) = self
                .infer_init_type(init)
                .map_err(|e| self.locate(e, decl.span.start))?;
            if let Some(class_name) = inferred_class {
                self.local_class_types.insert(name.clone(), class_name);
            }
            inferred_ty
        } else {
            return Err(self.locate(
                CompileError::type_err(format!(
                    "variable '{name}' requires a type annotation or initializer"
                )),
                decl.span.start,
            ));
        };

        // Check if this variable needs boxing (captured by closure AND mutated)
        if self.boxed_vars.contains(&name) {
            // Boxed: local holds a pointer into arena memory
            self.boxed_var_types.insert(name.clone(), ty);
            let ptr_idx = self.declare_local(&name, WasmType::I32);
            let arena_idx = self
                .module_ctx
                .arena_ptr_global
                .ok_or_else(|| CompileError::codegen("arena not initialized"))?;
            let size = if ty == WasmType::F64 { 8u32 } else { 4u32 };

            // ptr = arena_ptr; arena_ptr += size
            self.push(Instruction::GlobalGet(arena_idx));
            self.push(Instruction::LocalSet(ptr_idx));
            self.push(Instruction::GlobalGet(arena_idx));
            self.push(Instruction::I32Const(size as i32));
            self.push(Instruction::I32Add);
            self.push(Instruction::GlobalSet(arena_idx));

            // Store initial value (or zero) through pointer
            self.push(Instruction::LocalGet(ptr_idx));
            if let Some(init) = &decl.init {
                let init_ty = self.emit_expr(init)?;
                if init_ty != ty {
                    return Err(CompileError::type_err(format!(
                        "cannot initialize {ty:?} variable '{name}' with {init_ty:?}"
                    )));
                }
            } else {
                // Zero-initialize: WASM locals default to zero, boxed vars should too
                match ty {
                    WasmType::F64 => self.push(Instruction::F64Const(0.0f64)),
                    _ => self.push(Instruction::I32Const(0)),
                }
            }
            match ty {
                WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg {
                    offset: 0,
                    align: 3,
                    memory_index: 0,
                })),
                _ => self.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                })),
            }

            return Ok(());
        }

        let idx = self.declare_local(&name, ty);

        // Emit initializer if present
        if let Some(init) = &decl.init {
            // Array literals get the annotation's element type threaded in so
            // empty `[]` can be typed purely from the declaration. When the
            // annotation targets a tuple shape, route to the tuple emitter
            // instead — tuples are stored as synthetic-class pointers, not
            // Array<T>.
            let init_ty = match init {
                Expression::ArrayExpression(arr) => {
                    let target_class = self.local_class_types.get(&name).cloned();
                    if let Some(target) = target_class.as_deref()
                        && self.is_tuple_shape(target)
                    {
                        let (ty, _) = self.emit_tuple_literal(arr, target)?;
                        ty
                    } else if annotated_array_elem_ty.is_some() {
                        // `Array<[T, U]>` / `Array<Shape>`: thread the
                        // element class so inner literal forms route to
                        // `emit_tuple_literal` / `emit_object_literal`.
                        let elem_class =
                            self.local_array_elem_classes.get(&name).cloned();
                        self.emit_array_literal_with_class(
                            arr,
                            annotated_array_elem_ty,
                            elem_class.as_deref(),
                        )?
                    } else {
                        self.emit_array_literal(arr, None)?
                    }
                }
                Expression::ObjectExpression(obj) => {
                    let expected = self.local_class_types.get(&name).cloned();
                    let (ty, resolved) = self.emit_object_literal(obj, expected.as_deref())?;
                    // Populate tracking for the fingerprint-fallback path where
                    // no annotation pre-seeded local_class_types.
                    self.local_class_types
                        .entry(name.clone())
                        .or_insert(resolved);
                    ty
                }
                _ => {
                    let target_class = self.local_class_types.get(&name).cloned();
                    self.emit_expr_coerced(init, target_class.as_deref())?
                }
            };
            if init_ty != ty {
                return Err(CompileError::type_err(format!(
                    "cannot initialize {ty:?} variable '{name}' with {init_ty:?}"
                )));
            }
            self.push(Instruction::LocalSet(idx));
        }

        Ok(())
    }

    /// `const { x, y } = entity;` → desugar to field loads from the class instance.
    fn emit_object_destructuring(
        &mut self,
        obj_pat: &ObjectPattern<'a>,
        decl: &VariableDeclarator<'a>,
    ) -> Result<(), CompileError> {
        let init = decl
            .init
            .as_ref()
            .ok_or_else(|| CompileError::codegen("destructuring requires an initializer"))?;

        // Resolve the class name of the initializer. ObjectExpression is
        // special-cased so the literal emits exactly once; everything else
        // routes through `resolve_expr_class` (identifier, `this`, member
        // access on a class field, free/method call returning a class, ...).
        let obj_local = self.alloc_local(WasmType::I32);
        let class_name = match init {
            Expression::ObjectExpression(obj) => {
                let (_, name) = self.emit_object_literal(obj, None)?;
                self.push(Instruction::LocalSet(obj_local));
                name
            }
            _ => {
                let name = self.resolve_expr_class(init).map_err(|_| {
                    CompileError::codegen(
                        "object destructuring requires a class instance — annotate the source \
                         variable with its type, or assign to a typed local first",
                    )
                })?;
                self.emit_expr(init)?;
                self.push(Instruction::LocalSet(obj_local));
                name
            }
        };

        let layout = self
            .module_ctx
            .class_registry
            .get(&class_name)
            .ok_or_else(|| CompileError::codegen(format!("unknown class '{class_name}'")))?
            .clone();

        // For each property in the pattern, load the field
        for prop in &obj_pat.properties {
            let field_name = match &prop.key {
                PropertyKey::StaticIdentifier(ident) => ident.name.as_str(),
                _ => return Err(CompileError::unsupported("computed destructuring key")),
            };

            let &(offset, field_ty) = layout.field_map.get(field_name).ok_or_else(|| {
                CompileError::codegen(format!("class '{class_name}' has no field '{field_name}'"))
            })?;

            // Get the local variable name (may differ from field name in non-shorthand)
            let var_name = match &prop.value {
                BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
                _ => {
                    return Err(CompileError::unsupported(
                        "nested destructuring in object pattern — not yet supported (Phase E)",
                    ));
                }
            };

            // Declare local and load the field value
            let local_idx = self.declare_local(&var_name, field_ty);

            // Propagate class-type / string tracking from field metadata to the
            // new local so further destructuring or method calls on it work.
            if let Some(field_class) = layout.field_class_types.get(field_name) {
                self.local_class_types
                    .insert(var_name.clone(), field_class.clone());
            }
            if layout.field_string_types.contains(field_name) {
                self.local_string_vars.insert(var_name.clone());
            }

            self.push(Instruction::LocalGet(obj_local));
            match field_ty {
                WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg {
                    offset: offset as u64,
                    align: 3,
                    memory_index: 0,
                })),
                WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: offset as u64,
                    align: 2,
                    memory_index: 0,
                })),
                _ => return Err(CompileError::codegen("void field in destructuring")),
            }
            self.push(Instruction::LocalSet(local_idx));
        }

        if obj_pat.rest.is_some() {
            return Err(CompileError::unsupported(
                "rest element in object destructuring",
            ));
        }

        Ok(())
    }

    /// `const [a, b] = t;` where `t` is a tuple → per-slot field loads.
    /// Pattern arity must match tuple arity (with holes allowed for the
    /// "skip this slot" form `const [, b] = t`). Rest elements `...r` are
    /// rejected — tuples have a fixed shape.
    fn emit_tuple_destructuring(
        &mut self,
        arr_pat: &ArrayPattern<'a>,
        init: &Expression<'a>,
        class_name: &str,
    ) -> Result<(), CompileError> {
        if arr_pat.rest.is_some() {
            return Err(CompileError::unsupported(
                "rest element in tuple destructuring — tuples have fixed arity",
            ));
        }
        let layout = self
            .module_ctx
            .class_registry
            .get(class_name)
            .ok_or_else(|| CompileError::codegen(format!("tuple '{class_name}' not registered")))?
            .clone();
        if arr_pat.elements.len() > layout.fields.len() {
            return Err(CompileError::type_err(format!(
                "tuple destructuring pattern has {} element(s), tuple type '{class_name}' has {}",
                arr_pat.elements.len(),
                layout.fields.len()
            )));
        }

        // Evaluate the source once into a temp; all field loads read from it.
        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(init)?;
        self.push(Instruction::LocalSet(src_local));

        for (i, element) in arr_pat.elements.iter().enumerate() {
            let Some(binding) = element else {
                continue; // hole: const [, b] = t
            };
            let var_name = match binding {
                BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
                _ => {
                    return Err(CompileError::unsupported(
                        "nested destructuring in tuple pattern — not yet supported",
                    ));
                }
            };
            let (field_name, offset, slot_ty) = layout.fields[i].clone();
            let local_idx = self.declare_local(&var_name, slot_ty);
            if let Some(cn) = layout.field_class_types.get(&field_name) {
                self.local_class_types.insert(var_name.clone(), cn.clone());
            }
            if layout.field_string_types.contains(&field_name) {
                self.local_string_vars.insert(var_name.clone());
            }
            self.push(Instruction::LocalGet(src_local));
            match slot_ty {
                WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg {
                    offset: offset as u64,
                    align: 3,
                    memory_index: 0,
                })),
                WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: offset as u64,
                    align: 2,
                    memory_index: 0,
                })),
                _ => return Err(CompileError::codegen("void tuple slot")),
            }
            self.push(Instruction::LocalSet(local_idx));
        }

        Ok(())
    }

    /// `const [first, second] = arr;` → desugar to indexed loads from the
    /// array. When the source is a tuple value (identifier / call / field /
    /// etc.) each position maps to the tuple's `_N` field instead — so
    /// heterogeneous slot types work.
    fn emit_array_destructuring(
        &mut self,
        arr_pat: &ArrayPattern<'a>,
        decl: &VariableDeclarator<'a>,
    ) -> Result<(), CompileError> {
        let init = decl
            .init
            .as_ref()
            .ok_or_else(|| CompileError::codegen("destructuring requires an initializer"))?;

        // Tuple destructuring: when the source resolves to a tuple shape,
        // walk per-slot layout (`_0`, `_1`, …) instead of the uniform
        // `Array<T>` stride. Pick this branch before the Array<T> path so
        // `const [a, b] = t` works even when `t` is also an Array-like
        // identifier (it wouldn't be, but the ordering keeps the error
        // message clean).
        if let Ok(class_name) = self.resolve_expr_class(init)
            && let Some(&i) = self.module_ctx.shape_registry.by_name.get(&class_name)
            && self.module_ctx.shape_registry.shapes[i].is_tuple
        {
            return self.emit_tuple_destructuring(arr_pat, init, &class_name);
        }

        // Resolve the array element type
        let elem_ty = match init {
            Expression::Identifier(ident) => {
                let name = ident.name.as_str();
                self.local_array_elem_types
                    .get(name)
                    .copied()
                    .ok_or_else(|| {
                        CompileError::codegen(format!(
                            "cannot destructure '{name}' — not a known array"
                        ))
                    })?
            }
            _ => {
                return Err(CompileError::unsupported(
                    "array destructuring only supported on Array<T> variables",
                ));
            }
        };

        let elem_size: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };

        // Also get element class if applicable
        let elem_class = match init {
            Expression::Identifier(ident) => self
                .local_array_elem_classes
                .get(ident.name.as_str())
                .cloned(),
            _ => None,
        };

        // Evaluate the source array once, store in a temp local
        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(init)?;
        self.push(Instruction::LocalSet(arr_local));

        // For each element in the pattern, load arr[i]
        for (i, element) in arr_pat.elements.iter().enumerate() {
            let binding = match element {
                Some(pat) => pat,
                None => continue, // hole: const [, second] = arr
            };

            let var_name = match binding {
                BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
                _ => return Err(CompileError::unsupported("nested destructuring")),
            };

            let local_idx = self.declare_local(&var_name, elem_ty);

            // Track class type for destructured elements
            if let Some(ref class_name) = elem_class {
                self.local_class_types
                    .insert(var_name.clone(), class_name.clone());
            }

            // Compute element address: arr + 8 + i * elem_size
            self.push(Instruction::LocalGet(arr_local));
            self.push(Instruction::I32Const(8)); // ARRAY_HEADER_SIZE
            self.push(Instruction::I32Add);
            self.push(Instruction::I32Const(i as i32 * elem_size));
            self.push(Instruction::I32Add);

            // Load element (no bounds check — destructuring is a compile-time pattern)
            match elem_ty {
                WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 3,
                    memory_index: 0,
                })),
                WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                })),
                _ => unreachable!(),
            }
            self.push(Instruction::LocalSet(local_idx));
        }

        if let Some(rest) = &arr_pat.rest {
            // `const [a, b, ...rest] = src` — allocate a fresh array holding
            // source[prefix_count..]. `rest` stays an Array<T> with the same
            // element type (and class, if any).
            let rest_name = match &rest.argument {
                BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
                _ => {
                    return Err(CompileError::unsupported(
                        "rest element in array destructuring must bind a plain identifier",
                    ));
                }
            };
            let prefix_count = arr_pat.elements.len() as i32;

            // rest_len_signed = src.length - prefix_count
            let rest_len_signed = self.alloc_local(WasmType::I32);
            self.push(Instruction::LocalGet(arr_local));
            self.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
            self.push(Instruction::I32Const(prefix_count));
            self.push(Instruction::I32Sub);
            self.push(Instruction::LocalSet(rest_len_signed));

            // rest_len = max(rest_len_signed, 0) via select
            let rest_len = self.alloc_local(WasmType::I32);
            self.push(Instruction::LocalGet(rest_len_signed));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::LocalGet(rest_len_signed));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32GeS);
            self.push(Instruction::Select);
            self.push(Instruction::LocalSet(rest_len));

            // Alloc new array: size = 8 + rest_len * elem_size
            self.push(Instruction::I32Const(8));
            self.push(Instruction::LocalGet(rest_len));
            self.push(Instruction::I32Const(elem_size));
            self.push(Instruction::I32Mul);
            self.push(Instruction::I32Add);
            let rest_ptr = self.emit_arena_alloc_to_local(true)?;

            // Store length and capacity (both = rest_len)
            self.push(Instruction::LocalGet(rest_ptr));
            self.push(Instruction::LocalGet(rest_len));
            self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
            self.push(Instruction::LocalGet(rest_ptr));
            self.push(Instruction::LocalGet(rest_len));
            self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 4,
                align: 2,
                memory_index: 0,
            }));

            // memory.copy(rest_ptr + 8, arr_local + 8 + prefix_count*elem_size, rest_len*elem_size)
            self.push(Instruction::LocalGet(rest_ptr));
            self.push(Instruction::I32Const(8));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalGet(arr_local));
            self.push(Instruction::I32Const(8 + prefix_count * elem_size));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalGet(rest_len));
            self.push(Instruction::I32Const(elem_size));
            self.push(Instruction::I32Mul);
            self.push(Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });

            // Declare `rest` local and bind the new pointer; track as an array.
            let rest_local = self.declare_local(&rest_name, WasmType::I32);
            self.push(Instruction::LocalGet(rest_ptr));
            self.push(Instruction::LocalSet(rest_local));
            self.local_array_elem_types
                .insert(rest_name.clone(), elem_ty);
            if let Some(ref class_name) = elem_class {
                self.local_array_elem_classes
                    .insert(rest_name, class_name.clone());
            }
        }

        Ok(())
    }

    fn emit_return(&mut self, ret: &ReturnStatement<'a>) -> Result<(), CompileError> {
        if let Some(arg) = &ret.argument {
            match arg {
                Expression::ObjectExpression(obj) => {
                    let expected = self.return_class.clone();
                    self.emit_object_literal(obj, expected.as_deref())?;
                }
                Expression::ArrayExpression(arr) => {
                    // Tuple-typed return routes the literal to the tuple
                    // emitter; otherwise falls back to the regular array path.
                    let target = self.return_class.clone();
                    if let Some(t) = target.as_deref()
                        && self.is_tuple_shape(t)
                    {
                        self.emit_tuple_literal(arr, t)?;
                    } else {
                        self.emit_array_literal(arr, None)?;
                    }
                }
                _ => {
                    let target = self.return_class.clone();
                    self.emit_expr_coerced(arg, target.as_deref())?;
                }
            }
        }
        self.push(Instruction::Return);
        Ok(())
    }

    fn emit_if(&mut self, if_stmt: &IfStatement<'a>) -> Result<(), CompileError> {
        // Recognize narrowing facts BEFORE emitting the test expression.
        // The recognizer inspects AST, not bytecode, so the order doesn't
        // affect correctness — but reading the fact list before any
        // codegen makes the lifecycle easier to follow. Sub-phase 4 stub
        // returns empty; Sub-phase 5 fills it.
        let (positive_facts, negative_facts) = self.recognize_narrowing_facts(&if_stmt.test);

        self.emit_expr(&if_stmt.test)?;
        self.push(Instruction::If(BlockType::Empty));
        self.block_depth += 1;

        self.enter_refinement_scope();
        for fact in positive_facts {
            self.refine_local(&fact.local_name, fact.refined);
        }
        self.emit_statement(&if_stmt.consequent)?;
        self.leave_refinement_scope();

        if let Some(alt) = &if_stmt.alternate {
            self.push(Instruction::Else);
            self.enter_refinement_scope();
            for fact in negative_facts {
                self.refine_local(&fact.local_name, fact.refined);
            }
            self.emit_statement(alt)?;
            self.leave_refinement_scope();
        }

        self.push(Instruction::End);
        self.block_depth -= 1;
        Ok(())
    }

    fn emit_while(&mut self, while_stmt: &WhileStatement<'a>) -> Result<(), CompileError> {
        // block $break
        //   loop $continue
        //     <cond> i32.eqz br_if $break
        //     <body>
        //     br $continue
        //   end
        // end
        self.push(Instruction::Block(BlockType::Empty));
        self.block_depth += 1;
        let break_depth = self.block_depth; // outer block = break target
        self.push(Instruction::Loop(BlockType::Empty));
        self.block_depth += 1;
        let continue_depth = self.block_depth; // loop = continue target
        self.loop_stack.push(LoopLabels {
            break_depth,
            continue_depth,
        });

        // Condition
        self.emit_expr(&while_stmt.test)?;
        self.push(Instruction::I32Eqz);
        self.push(Instruction::BrIf(1)); // break out of block

        // Body
        self.emit_statement(&while_stmt.body)?;

        // Loop back
        self.push(Instruction::Br(0)); // continue to loop start

        self.loop_stack.pop();
        self.push(Instruction::End); // end loop
        self.block_depth -= 1;
        self.push(Instruction::End); // end block
        self.block_depth -= 1;
        Ok(())
    }

    fn emit_do_while(&mut self, do_while: &DoWhileStatement<'a>) -> Result<(), CompileError> {
        // block $break
        //   loop $loop
        //     <body>
        //     <cond>
        //     br_if $loop   (continue if true)
        //   end
        // end
        self.push(Instruction::Block(BlockType::Empty));
        self.block_depth += 1;
        let break_depth = self.block_depth; // outer block = break target
        self.push(Instruction::Loop(BlockType::Empty));
        self.block_depth += 1;
        let continue_depth = self.block_depth; // loop = continue target
        self.loop_stack.push(LoopLabels {
            break_depth,
            continue_depth,
        });

        // Body first (executes at least once)
        self.emit_statement(&do_while.body)?;

        // Condition — loop back if true
        self.emit_expr(&do_while.test)?;
        self.push(Instruction::BrIf(0)); // br to loop start

        self.loop_stack.pop();
        self.push(Instruction::End); // end loop
        self.block_depth -= 1;
        self.push(Instruction::End); // end block
        self.block_depth -= 1;
        Ok(())
    }

    fn emit_for(&mut self, for_stmt: &ForStatement<'a>) -> Result<(), CompileError> {
        // <init>
        // block $break
        //   loop $loop_top
        //     <cond> i32.eqz br_if $break
        //     block $continue_target
        //       <body>
        //     end
        //     <update>
        //     br $loop_top
        //   end
        // end

        // Init
        if let Some(init) = &for_stmt.init {
            match init {
                ForStatementInit::VariableDeclaration(var_decl) => {
                    for declarator in &var_decl.declarations {
                        self.emit_var_declarator(declarator)?;
                    }
                }
                _ => {
                    let init_expr = init.to_expression();
                    let ty = self.emit_expr(init_expr)?;
                    if ty != WasmType::Void {
                        self.push(Instruction::Drop);
                    }
                }
            }
        }

        self.push(Instruction::Block(BlockType::Empty));
        self.block_depth += 1;
        let break_depth = self.block_depth; // outer block = break target
        self.push(Instruction::Loop(BlockType::Empty));
        self.block_depth += 1;

        // Condition
        if let Some(test) = &for_stmt.test {
            self.emit_expr(test)?;
            self.push(Instruction::I32Eqz);
            self.push(Instruction::BrIf(1)); // break
        }

        // Inner block for continue target
        self.push(Instruction::Block(BlockType::Empty));
        self.block_depth += 1;

        let continue_depth = self.block_depth;
        self.loop_stack.push(LoopLabels {
            break_depth,
            continue_depth,
        });

        // Body
        self.emit_statement(&for_stmt.body)?;

        self.loop_stack.pop();
        self.push(Instruction::End); // end continue_target block
        self.block_depth -= 1;

        // Update
        if let Some(update) = &for_stmt.update {
            let ty = self.emit_expr(update)?;
            if ty != WasmType::Void {
                self.push(Instruction::Drop);
            }
        }

        // Loop back
        self.push(Instruction::Br(0));

        self.push(Instruction::End); // end loop
        self.block_depth -= 1;
        self.push(Instruction::End); // end block
        self.block_depth -= 1;
        Ok(())
    }

    fn emit_for_of(&mut self, for_of: &ForOfStatement<'a>) -> Result<(), CompileError> {
        // Desugar: for (const elem of arr) { body }
        // →  let __arr = arr; for (let __i = 0; __i < __arr.length; __i++) { const elem = __arr[__i]; body }

        use super::expr::ARRAY_HEADER_SIZE;

        // Get the binding name
        let elem_name = match &for_of.left {
            ForStatementLeft::VariableDeclaration(var_decl) => {
                if let Some(decl) = var_decl.declarations.first() {
                    match &decl.id {
                        BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
                        _ => return Err(CompileError::unsupported("destructured for..of binding")),
                    }
                } else {
                    return Err(CompileError::codegen("empty for..of binding"));
                }
            }
            _ => {
                return Err(CompileError::unsupported(
                    "for..of requires a variable declaration",
                ));
            }
        };

        // Resolve array element type from the right-hand expression
        let elem_ty = self.resolve_expr_array_elem(&for_of.right).ok_or_else(|| {
            CompileError::codegen("for..of requires an Array<T> — cannot resolve element type")
        })?;
        let elem_class = self.resolve_expr_array_elem_class(&for_of.right);
        let elem_size: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => {
                return Err(CompileError::type_err(
                    "invalid array element type for for..of",
                ));
            }
        };

        // Evaluate array, save to local
        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&for_of.right)?;
        self.push(Instruction::LocalSet(arr_local));

        // Load length
        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(len_local));

        // Loop counter
        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        // Declare element local
        let elem_local = self.declare_local(&elem_name, elem_ty);
        if let Some(class_name) = &elem_class {
            self.local_class_types
                .insert(elem_name.clone(), class_name.clone());
        }

        // Track as const if declared with const
        if let ForStatementLeft::VariableDeclaration(var_decl) = &for_of.left
            && var_decl.kind == VariableDeclarationKind::Const
        {
            self.const_locals.insert(elem_name.clone());
        }

        // block $break
        //   loop $loop
        //     if i >= len: br $break
        //     elem = arr[i]
        //     block $continue
        //       body
        //     end
        //     i++
        //     br $loop
        //   end
        // end

        self.push(Instruction::Block(BlockType::Empty));
        self.block_depth += 1;
        let break_depth = self.block_depth; // outer block = break target
        self.push(Instruction::Loop(BlockType::Empty));
        self.block_depth += 1;

        // if i >= len, break
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeU);
        self.push(Instruction::BrIf(1));

        // Load element: arr + HEADER + i * elem_size
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(elem_size));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg {
                offset: 0,
                align: 3,
                memory_index: 0,
            })),
            WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            })),
            _ => unreachable!(),
        }
        self.push(Instruction::LocalSet(elem_local));

        // Continue target block (for break/continue)
        self.push(Instruction::Block(BlockType::Empty));
        self.block_depth += 1;

        let continue_depth = self.block_depth;
        self.loop_stack.push(LoopLabels {
            break_depth,
            continue_depth,
        });

        // Body
        self.emit_statement(&for_of.body)?;

        self.loop_stack.pop();
        self.push(Instruction::End); // end continue block
        self.block_depth -= 1;

        // i++
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Br(0)); // loop back

        self.push(Instruction::End); // end loop
        self.block_depth -= 1;
        self.push(Instruction::End); // end block
        self.block_depth -= 1;

        Ok(())
    }

    fn emit_switch(&mut self, switch: &SwitchStatement<'a>) -> Result<(), CompileError> {
        // Recognize narrowing facts BEFORE emitting code. The recognizer
        // walks the AST only and never mutates the function's emit state,
        // so reading facts up front keeps the case-by-case lifecycle
        // (enter scope → install positive → emit body → leave scope)
        // straightforward.
        let cases: Vec<&SwitchCase<'a>> = switch.cases.iter().collect();
        let narrowing = self.recognize_switch_facts(&switch.discriminant, &cases);

        // Evaluate discriminant once
        let disc_ty = self.emit_expr(&switch.discriminant)?;
        let disc_local = self.alloc_local(disc_ty);
        self.push(Instruction::LocalSet(disc_local));

        // Outer block for break statements
        self.push(Instruction::Block(BlockType::Empty));
        self.block_depth += 1;
        let switch_depth = self.block_depth;

        // Push a fake loop entry so `break` inside switch works
        // (break in switch = br to the outer block)
        self.loop_stack.push(LoopLabels {
            break_depth: switch_depth,
            continue_depth: switch_depth,
        });

        let mut default_idx: Option<usize> = None;

        for (i, case) in cases.iter().enumerate() {
            if case.test.is_none() {
                default_idx = Some(i);
                continue;
            }

            // Compare discriminant with case test
            self.push(Instruction::LocalGet(disc_local));
            if let Some(test) = &case.test {
                self.emit_expr(test)?;
            }
            match disc_ty {
                WasmType::I32 => self.push(Instruction::I32Eq),
                WasmType::F64 => self.push(Instruction::F64Eq),
                _ => {
                    return Err(CompileError::type_err(
                        "switch discriminant must be i32 or f64",
                    ));
                }
            }

            self.push(Instruction::If(BlockType::Empty));

            // Case body — install per-case positive refinement so
            // `sh.kind === 'circle'` narrows `sh` inside this branch.
            self.enter_refinement_scope();
            if let Some(facts) = narrowing.case_facts.get(i) {
                for fact in facts {
                    self.refine_local(&fact.local_name, fact.refined.clone());
                }
            }
            for stmt in &case.consequent {
                match stmt {
                    Statement::BreakStatement(_) => {
                        // break → br to switch end block
                        let relative = self.block_depth - switch_depth + 1;
                        self.push(Instruction::Br(relative));
                    }
                    _ => self.emit_statement(stmt)?,
                }
            }
            self.leave_refinement_scope();

            self.push(Instruction::End); // end if
        }

        // Default case — refinement is the cumulative negative: original
        // active member set minus every shape / literal handled above.
        if let Some(idx) = default_idx {
            self.enter_refinement_scope();
            for fact in &narrowing.default_facts {
                self.refine_local(&fact.local_name, fact.refined.clone());
            }
            for stmt in &cases[idx].consequent {
                match stmt {
                    Statement::BreakStatement(_) => {
                        let relative = self.block_depth - switch_depth;
                        self.push(Instruction::Br(relative));
                    }
                    _ => self.emit_statement(stmt)?,
                }
            }
            self.leave_refinement_scope();
        }

        self.loop_stack.pop();
        self.push(Instruction::End); // end switch block
        self.block_depth -= 1;

        Ok(())
    }

    fn emit_break(&mut self) -> Result<(), CompileError> {
        let labels = self
            .loop_stack
            .last()
            .ok_or_else(|| CompileError::codegen("break outside of loop"))?;
        // br to the outer block (break target)
        // break_depth points at the outer block's depth level
        let relative = self.block_depth - labels.break_depth;
        self.push(Instruction::Br(relative));
        Ok(())
    }

    fn emit_continue(&mut self) -> Result<(), CompileError> {
        let labels = self
            .loop_stack
            .last()
            .ok_or_else(|| CompileError::codegen("continue outside of loop"))?;
        // In a for loop, continue jumps to the continue_target block end,
        // which falls through to the update expression
        let relative = self.block_depth - labels.continue_depth;
        self.push(Instruction::Br(relative));
        Ok(())
    }
}
