use oxc_ast::ast::*;
use wasm_encoder::{Instruction, ValType};

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

use super::ARRAY_HEADER_SIZE;

impl<'a> FuncContext<'a> {
    // ---- Phase 3: Classes ----

    pub(crate) fn emit_new(
        &mut self,
        new_expr: &NewExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        let class_name = match &new_expr.callee {
            Expression::Identifier(ident) => ident.name.as_str(),
            _ => return Err(CompileError::unsupported("non-identifier new target")),
        };

        // Handle new Array<T>(capacity)
        if class_name == "Array" {
            return self.emit_new_array(new_expr);
        }

        let layout = self
            .module_ctx
            .class_registry
            .get(class_name)
            .ok_or_else(|| CompileError::codegen(format!("unknown class '{class_name}'")))?;
        let size = layout.size;

        // Allocate object via arena
        self.push(Instruction::I32Const(size as i32));
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        // Write vtable pointer at offset 0 for polymorphic classes
        if layout.is_polymorphic && !layout.vtable_methods.is_empty() {
            self.push(Instruction::LocalGet(ptr_local));
            self.push(Instruction::I32Const(layout.vtable_offset as i32));
            self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
        }

        // Call constructor if it exists
        let ctor_key = format!("{class_name}.constructor");
        if let Some(&(func_idx, _)) = self.module_ctx.method_map.get(&ctor_key) {
            // Push this pointer
            self.push(Instruction::LocalGet(ptr_local));
            // Push constructor arguments
            for arg in &new_expr.arguments {
                self.emit_expr(arg.to_expression())?;
            }
            self.push(Instruction::Call(func_idx));
            self.push(Instruction::Drop); // constructor returns this, but we already have it
        }

        // Return pointer
        self.push(Instruction::LocalGet(ptr_local));
        Ok(WasmType::I32)
    }

    /// Emit `new Array<T>(capacity)` — arena-allocate array with header + element space.
    /// Layout: [length: i32 (4B)] [capacity: i32 (4B)] [elements...]
    pub(crate) fn emit_new_array(
        &mut self,
        new_expr: &NewExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        if new_expr.arguments.len() != 1 {
            return Err(CompileError::codegen(
                "new Array<T>(capacity) requires exactly 1 argument",
            ));
        }

        // Determine element type from type_parameters on the NewExpression
        let elem_type = self.resolve_new_array_elem_type(new_expr)?;
        let elem_size: u32 = match elem_type {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => {
                return Err(CompileError::type_err(
                    "Array element type must be i32 or f64",
                ));
            }
        };

        // Evaluate capacity argument
        let cap_local = self.alloc_local(WasmType::I32);
        let arg_ty = self.emit_expr(new_expr.arguments[0].to_expression())?;
        if arg_ty != WasmType::I32 {
            return Err(CompileError::type_err("Array capacity must be i32"));
        }
        self.push(Instruction::LocalSet(cap_local));

        // Compute total size: 8 (header) + capacity * elem_size
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(cap_local));
        self.push(Instruction::I32Const(elem_size as i32));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        // Store length = 0 at ptr+0
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));

        // Store capacity at ptr+4
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::LocalGet(cap_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // Return pointer
        self.push(Instruction::LocalGet(ptr_local));
        Ok(WasmType::I32)
    }

    /// Extract the element type from `new Array<T>(...)` type parameters.
    pub(crate) fn resolve_new_array_elem_type(
        &self,
        new_expr: &NewExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        if let Some(type_params) = &new_expr.type_arguments
            && let Some(first) = type_params.params.first()
        {
            return crate::types::resolve_ts_type(first, &self.module_ctx.class_names);
        }
        Err(CompileError::type_err(
            "new Array requires a type parameter: new Array<f64>(n)",
        ))
    }
    pub(crate) fn emit_this(&mut self) -> Result<WasmType, CompileError> {
        if self.this_class.is_none() {
            return Err(CompileError::codegen("`this` used outside of a method"));
        }
        // `this` is always local 0 in methods
        self.push(Instruction::LocalGet(0));
        Ok(WasmType::I32)
    }

    pub(crate) fn try_emit_method_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        let member = match &call.callee {
            Expression::StaticMemberExpression(m) => m,
            _ => return Ok(None),
        };

        let method_name = member.property.name.as_str();

        // Resolve the class of the object
        let class_name = match self.resolve_expr_class(&member.object) {
            Ok(name) => name,
            Err(_) => return Ok(None), // Not a class method call, let it fall through
        };

        // Look up the method — may be inherited from a parent class.
        // Walk up the parent chain checking method_map (which has entries only for declared methods).
        let (func_idx, ret_ty) = {
            let mut found = None;
            let mut cur = class_name.clone();
            loop {
                let key = format!("{cur}.{method_name}");
                if let Some(&v) = self.module_ctx.method_map.get(&key) {
                    found = Some(v);
                    break;
                }
                if let Some(layout) = self.module_ctx.class_registry.get(&cur) {
                    if let Some(ref parent) = layout.parent {
                        cur = parent.clone();
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            found.ok_or_else(|| {
                CompileError::codegen(format!(
                    "class '{class_name}' has no method '{method_name}'"
                ))
            })?
        };

        // Check if this class is polymorphic (uses vtable dispatch)
        let layout = self.module_ctx.class_registry.get(&class_name);
        let is_polymorphic = layout.is_some_and(|l| l.is_polymorphic);

        if is_polymorphic {
            // Vtable dispatch via call_indirect
            let vtable_slot = layout
                .unwrap()
                .vtable_method_map
                .get(method_name)
                .ok_or_else(|| {
                    CompileError::codegen(format!(
                        "method '{method_name}' not in vtable of '{class_name}'"
                    ))
                })?;

            // Emit this pointer, save to temp for vtable lookup. If an optional-call
            // override is set, use the pre-evaluated receiver local instead.
            if let Some(recv_local) = self.method_receiver_override {
                self.push(Instruction::LocalGet(recv_local));
            } else {
                self.emit_expr(&member.object)?;
            }
            let this_tmp = self.alloc_local(WasmType::I32);
            self.push(Instruction::LocalTee(this_tmp));

            // Emit arguments
            for arg in &call.arguments {
                self.emit_expr(arg.to_expression())?;
            }

            // Load vtable pointer from this (offset 0)
            self.push(Instruction::LocalGet(this_tmp));
            self.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));

            // Load table index from vtable at slot offset
            self.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: (*vtable_slot as u64) * 4,
                align: 2,
                memory_index: 0,
            }));

            // Build type signature for call_indirect: (this: i32, params...) -> ret
            // layout.methods includes inherited entries, so we can look up directly
            let method_sig = layout.unwrap().methods.get(method_name).unwrap();

            let mut param_types = vec![ValType::I32]; // this
            for (_pname, pty) in &method_sig.params {
                if let Some(vt) = pty.to_val_type() {
                    param_types.push(vt);
                }
            }
            let result_types = crate::codegen::wasm_types::wasm_results(method_sig.return_type);

            let type_idx = self
                .module_ctx
                .get_or_add_type_sig(param_types, result_types);
            self.push(Instruction::CallIndirect {
                type_index: type_idx,
                table_index: 0,
            });

            Ok(Some(ret_ty))
        } else {
            // Static dispatch (non-polymorphic class)
            if let Some(recv_local) = self.method_receiver_override {
                self.push(Instruction::LocalGet(recv_local));
            } else {
                self.emit_expr(&member.object)?; // this
            }
            for arg in &call.arguments {
                self.emit_expr(arg.to_expression())?;
            }
            self.push(Instruction::Call(func_idx));

            Ok(Some(ret_ty))
        }
    }

    /// Emit `super(args)` — call parent constructor with `this` pointer.
    pub(crate) fn emit_super_constructor_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        let this_class = self
            .this_class
            .as_ref()
            .ok_or_else(|| CompileError::codegen("super() used outside of a method"))?
            .clone();
        let parent = self
            .module_ctx
            .class_registry
            .get(&this_class)
            .and_then(|l| l.parent.clone())
            .ok_or_else(|| {
                CompileError::codegen(format!(
                    "super() used in class '{this_class}' which has no parent"
                ))
            })?;

        let ctor_key = format!("{parent}.constructor");
        if let Some(&(func_idx, _)) = self.module_ctx.method_map.get(&ctor_key) {
            // Parent has an explicit constructor — call it
            self.push(Instruction::LocalGet(0));
            for arg in &call.arguments {
                self.emit_expr(arg.to_expression())?;
            }
            self.push(Instruction::Call(func_idx));
            self.push(Instruction::Drop); // constructor returns this, but we already have it
        } else if !call.arguments.is_empty() {
            return Err(CompileError::codegen(format!(
                "parent class '{parent}' has no constructor, but super() was called with arguments"
            )));
        }
        // else: parent has no constructor and super() has no args — no-op

        Ok(WasmType::Void)
    }

    /// Emit `super.method(args)` — static dispatch to parent's method (bypasses vtable).
    pub(crate) fn emit_super_method_call(
        &mut self,
        method_name: &str,
        call: &CallExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        let this_class = self
            .this_class
            .as_ref()
            .ok_or_else(|| CompileError::codegen("super.method() used outside of a method"))?
            .clone();
        let parent = self
            .module_ctx
            .class_registry
            .get(&this_class)
            .and_then(|l| l.parent.clone())
            .ok_or_else(|| {
                CompileError::codegen(format!(
                    "super.method() used in class '{this_class}' which has no parent"
                ))
            })?;

        // Resolve method — may be on parent or grandparent
        let owner = self
            .module_ctx
            .class_registry
            .resolve_method_owner(&parent, method_name)
            .ok_or_else(|| {
                CompileError::codegen(format!(
                    "parent class '{parent}' has no method '{method_name}'"
                ))
            })?;
        let key = format!("{owner}.{method_name}");
        let &(func_idx, ret_ty) = self.module_ctx.method_map.get(&key).ok_or_else(|| {
            CompileError::codegen(format!(
                "method '{method_name}' not found in parent chain of '{this_class}'"
            ))
        })?;

        // Static dispatch: this + args + Call
        self.push(Instruction::LocalGet(0)); // this
        for arg in &call.arguments {
            self.emit_expr(arg.to_expression())?;
        }
        self.push(Instruction::Call(func_idx));

        Ok(ret_ty)
    }

    /// Resolve which class an expression refers to (for member access / method calls).
    /// Supports: identifiers, `this`, `new ClassName()`, `obj.field` (if field is a class),
    /// `obj.method()` (if method returns a class), and function calls returning classes.
    pub fn resolve_expr_class(&self, expr: &Expression<'a>) -> Result<String, CompileError> {
        match expr {
            Expression::Identifier(ident) => {
                let name = ident.name.as_str();
                if let Some(class_name) = self.local_class_types.get(name) {
                    return Ok(class_name.clone());
                }
                Err(CompileError::codegen(format!(
                    "cannot resolve class type of variable '{name}'"
                )))
            }
            Expression::ThisExpression(_) => self
                .this_class
                .clone()
                .ok_or_else(|| CompileError::codegen("`this` used outside of a method")),
            // new ClassName(...) → class is ClassName
            Expression::NewExpression(new_expr) => {
                if let Expression::Identifier(ident) = &new_expr.callee {
                    let name = ident.name.as_str();
                    if self.module_ctx.class_names.contains(name) {
                        return Ok(name.to_string());
                    }
                }
                Err(CompileError::codegen(
                    "cannot resolve class type of new expression",
                ))
            }
            // obj.field → if the field's type is a class, resolve it
            Expression::StaticMemberExpression(member) => {
                let parent_class = self.resolve_expr_class(&member.object)?;
                let layout = self
                    .module_ctx
                    .class_registry
                    .get(&parent_class)
                    .ok_or_else(|| {
                        CompileError::codegen(format!("unknown class '{parent_class}'"))
                    })?;
                let field_name = member.property.name.as_str();
                if let Some(field_class) = layout.field_class_types.get(field_name) {
                    return Ok(field_class.clone());
                }
                Err(CompileError::codegen(format!(
                    "field '{field_name}' of class '{parent_class}' is not a class instance"
                )))
            }
            // obj.method() → if method returns a class, resolve it
            Expression::CallExpression(call) => {
                if let Expression::StaticMemberExpression(member) = &call.callee {
                    // Try method call: obj.method()
                    if let Ok(obj_class) = self.resolve_expr_class(&member.object) {
                        let method_name = member.property.name.as_str();
                        if let Some(layout) = self.module_ctx.class_registry.get(&obj_class)
                            && let Some(method_sig) = layout.methods.get(method_name)
                            && let Some(ref ret_class) = method_sig.return_class
                        {
                            return Ok(ret_class.clone());
                        }
                    }
                }
                // Try free function call: funcName()
                if let Expression::Identifier(ident) = &call.callee {
                    let name = ident.name.as_str();
                    // Check module-level function return class types
                    if let Some(class_name) = self.module_ctx.var_class_types.get(name) {
                        return Ok(class_name.clone());
                    }
                }
                Err(CompileError::codegen(
                    "cannot resolve class type of call expression",
                ))
            }
            Expression::ParenthesizedExpression(paren) => {
                self.resolve_expr_class(&paren.expression)
            }
            // (expr as ClassName) → target class
            Expression::TSAsExpression(as_expr) => {
                if let Some(class_name) =
                    crate::types::get_class_type_name_from_ts_type(&as_expr.type_annotation)
                    && self.module_ctx.class_names.contains(&class_name)
                {
                    return Ok(class_name);
                }
                Err(CompileError::codegen(
                    "cannot resolve class type of as-expression",
                ))
            }
            _ => Err(CompileError::codegen(
                "cannot resolve class type of expression",
            )),
        }
    }
}
