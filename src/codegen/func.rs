use std::collections::{HashMap, HashSet};

use oxc_ast::ast::*;
use oxc_span::GetSpan;
use wasm_encoder::{BlockType, Function, Instruction, ValType};

use crate::error::{self, CompileError};
use crate::types::{self, ClosureSig, WasmType};

use super::module::ModuleContext;

pub struct FuncContext<'a> {
    pub module_ctx: &'a ModuleContext,
    pub locals: HashMap<String, (u32, WasmType)>,
    pub instructions: Vec<Instruction<'static>>,
    local_types: Vec<ValType>,
    param_count: u32,
    #[allow(dead_code)]
    return_type: WasmType,
    /// Stack of (break_depth, continue_depth) for loops
    loop_stack: Vec<LoopLabels>,
    /// Current WASM block nesting depth
    block_depth: u32,
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
}

struct LoopLabels {
    break_depth: u32,
    continue_depth: u32,
}

impl<'a> FuncContext<'a> {
    pub fn new(module_ctx: &'a ModuleContext, params: &[(String, WasmType)], return_type: WasmType, source: &'a str) -> Self {
        let mut locals = HashMap::new();
        for (i, (name, ty)) in params.iter().enumerate() {
            locals.insert(name.clone(), (i as u32, *ty));
        }
        FuncContext {
            module_ctx,
            locals,
            instructions: Vec::new(),
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
        }
    }

    pub fn push(&mut self, inst: Instruction<'static>) {
        self.instructions.push(inst);
    }

    /// Record a source location for the next instruction to be emitted.
    pub fn mark_loc(&mut self, source_offset: u32) {
        self.source_map.push((self.instructions.len(), source_offset));
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
        let arena_idx = self.module_ctx.arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;
        let ptr_local = self.alloc_local(WasmType::I32);

        if let Some(alloc_idx) = self.module_ctx.arena_alloc_func {
            // size is already on stack
            if !size_on_stack {
                return Err(CompileError::codegen("emit_arena_alloc_to_local requires size on stack"));
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
                return Err(CompileError::codegen("emit_arena_alloc_to_local requires size on stack"));
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
        let local_groups: Vec<(u32, ValType)> = self.local_types.iter().map(|vt| (1, *vt)).collect();
        let mut func = Function::new(local_groups);

        // Track byte offset of each instruction within the function body
        let mut inst_byte_offsets: Vec<u32> = Vec::with_capacity(self.instructions.len());
        for inst in &self.instructions {
            inst_byte_offsets.push(func.byte_len() as u32);
            func.instruction(inst);
        }
        func.instruction(&Instruction::End);

        // Convert source_map from instruction indices to byte offsets
        let byte_source_map: Vec<(u32, u32)> = self.source_map.iter()
            .filter_map(|&(inst_idx, src_offset)| {
                inst_byte_offsets.get(inst_idx).map(|&byte_off| (byte_off, src_offset))
            })
            .collect();

        (func, byte_source_map)
    }

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

    fn emit_var_declaration(&mut self, var_decl: &VariableDeclaration<'a>) -> Result<(), CompileError> {
        for declarator in &var_decl.declarations {
            self.emit_var_declarator(declarator)?;
            // Track const for immutability enforcement
            if var_decl.kind == VariableDeclarationKind::Const
                && let BindingPattern::BindingIdentifier(ident) = &declarator.id {
                    self.const_locals.insert(ident.name.as_str().to_string());
                }
        }
        Ok(())
    }

    pub fn emit_var_declarator(&mut self, decl: &VariableDeclarator<'a>) -> Result<(), CompileError> {
        match &decl.id {
            BindingPattern::BindingIdentifier(_) => self.emit_simple_var_declarator(decl),
            BindingPattern::ObjectPattern(obj_pat) => self.emit_object_destructuring(obj_pat, decl),
            BindingPattern::ArrayPattern(arr_pat) => self.emit_array_destructuring(arr_pat, decl),
            _ => Err(CompileError::unsupported("assignment pattern with default value in destructuring")),
        }
    }

    fn emit_simple_var_declarator(&mut self, decl: &VariableDeclarator<'a>) -> Result<(), CompileError> {
        let name = match &decl.id {
            BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
            _ => unreachable!(),
        };

        // Resolve type from annotation (on VariableDeclarator, not BindingPattern)
        let ty = if let Some(ann) = &decl.type_annotation {
            // Track closure signature from function type annotation
            if let Some(sig) = types::get_closure_sig(ann, &self.module_ctx.class_names) {
                self.local_closure_sigs.insert(name.clone(), sig);
            }
            // Track class type for property access resolution
            if let Some(class_name) = types::get_class_type_name(ann) {
                self.local_class_types.insert(name.clone(), class_name);
            }
            // Track array element type
            if let Some(elem_ty) = types::get_array_element_type(ann, &self.module_ctx.class_names) {
                self.local_array_elem_types.insert(name.clone(), elem_ty);
                // Track array element class if applicable
                if let Some(elem_class) = types::get_array_element_class(ann) {
                    self.local_array_elem_classes.insert(name.clone(), elem_class);
                }
            }
            // Track string type
            if types::is_string_type(ann) {
                self.local_string_vars.insert(name.clone());
            }
            types::resolve_type_annotation_with_classes(ann, &self.module_ctx.class_names)
                .map_err(|e| self.locate(e, decl.span.start))?
        } else if let Some(init) = &decl.init {
            // Infer closure sig from arrow initializer
            if let Expression::ArrowFunctionExpression(arrow) = init
                && let Some(sig) = self.infer_arrow_sig(arrow) {
                    self.local_closure_sigs.insert(name.clone(), sig);
                }
            // Infer closure sig from function call that returns a closure
            if let Expression::CallExpression(call) = init
                && let Expression::Identifier(ident) = &call.callee
                    && let Some(sig) = self.module_ctx.func_return_closure_sigs.get(ident.name.as_str()) {
                        self.local_closure_sigs.insert(name.clone(), sig.clone());
                    }
            // Infer string from string literal initializer
            if matches!(init, Expression::StringLiteral(_)) {
                self.local_string_vars.insert(name.clone());
            }
            let (inferred_ty, inferred_class) = self.infer_init_type(init)
                .map_err(|e| self.locate(e, decl.span.start))?;
            if let Some(class_name) = inferred_class {
                self.local_class_types.insert(name.clone(), class_name);
            }
            inferred_ty
        } else {
            return Err(self.locate(
                CompileError::type_err(format!("variable '{name}' requires a type annotation or initializer")),
                decl.span.start,
            ));
        };

        // Check if this variable needs boxing (captured by closure AND mutated)
        if self.boxed_vars.contains(&name) {
            // Boxed: local holds a pointer into arena memory
            self.boxed_var_types.insert(name.clone(), ty);
            let ptr_idx = self.declare_local(&name, WasmType::I32);
            let arena_idx = self.module_ctx.arena_ptr_global
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
                    offset: 0, align: 3, memory_index: 0,
                })),
                _ => self.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset: 0, align: 2, memory_index: 0,
                })),
            }

            return Ok(());
        }

        let idx = self.declare_local(&name, ty);

        // Emit initializer if present
        if let Some(init) = &decl.init {
            let init_ty = self.emit_expr(init)?;
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
        let init = decl.init.as_ref()
            .ok_or_else(|| CompileError::codegen("destructuring requires an initializer"))?;

        // Resolve the class type of the initializer
        let class_name = match init {
            Expression::Identifier(ident) => {
                let name = ident.name.as_str();
                self.local_class_types.get(name).cloned()
                    .ok_or_else(|| CompileError::codegen(format!(
                        "cannot destructure '{name}' — not a known class instance"
                    )))?
            }
            Expression::ThisExpression(_) => {
                self.this_class.clone()
                    .ok_or_else(|| CompileError::codegen("`this` used outside of a method"))?
            }
            _ => return Err(CompileError::unsupported(
                "object destructuring only supported on class instances"
            )),
        };

        let layout = self.module_ctx.class_registry.get(&class_name)
            .ok_or_else(|| CompileError::codegen(format!("unknown class '{class_name}'")))?
            .clone();

        // Evaluate the source object once, store in a temp local
        let obj_local = self.alloc_local(WasmType::I32);
        self.emit_expr(init)?;
        self.push(Instruction::LocalSet(obj_local));

        // For each property in the pattern, load the field
        for prop in &obj_pat.properties {
            let field_name = match &prop.key {
                PropertyKey::StaticIdentifier(ident) => ident.name.as_str(),
                _ => return Err(CompileError::unsupported("computed destructuring key")),
            };

            let &(offset, field_ty) = layout.field_map.get(field_name)
                .ok_or_else(|| CompileError::codegen(format!(
                    "class '{class_name}' has no field '{field_name}'"
                )))?;

            // Get the local variable name (may differ from field name in non-shorthand)
            let var_name = match &prop.value {
                BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
                _ => return Err(CompileError::unsupported("nested destructuring")),
            };

            // Declare local and load the field value
            let local_idx = self.declare_local(&var_name, field_ty);

            // Propagate class-type / string tracking from field metadata to the
            // new local so further destructuring or method calls on it work.
            if let Some(field_class) = layout.field_class_types.get(field_name) {
                self.local_class_types.insert(var_name.clone(), field_class.clone());
            }
            if layout.field_string_types.contains(field_name) {
                self.local_string_vars.insert(var_name.clone());
            }

            self.push(Instruction::LocalGet(obj_local));
            match field_ty {
                WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg {
                    offset: offset as u64, align: 3, memory_index: 0,
                })),
                WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: offset as u64, align: 2, memory_index: 0,
                })),
                _ => return Err(CompileError::codegen("void field in destructuring")),
            }
            self.push(Instruction::LocalSet(local_idx));
        }

        if obj_pat.rest.is_some() {
            return Err(CompileError::unsupported("rest element in object destructuring"));
        }

        Ok(())
    }

    /// `const [first, second] = arr;` → desugar to indexed loads from the array.
    fn emit_array_destructuring(
        &mut self,
        arr_pat: &ArrayPattern<'a>,
        decl: &VariableDeclarator<'a>,
    ) -> Result<(), CompileError> {
        let init = decl.init.as_ref()
            .ok_or_else(|| CompileError::codegen("destructuring requires an initializer"))?;

        // Resolve the array element type
        let elem_ty = match init {
            Expression::Identifier(ident) => {
                let name = ident.name.as_str();
                self.local_array_elem_types.get(name).copied()
                    .ok_or_else(|| CompileError::codegen(format!(
                        "cannot destructure '{name}' — not a known array"
                    )))?
            }
            _ => return Err(CompileError::unsupported(
                "array destructuring only supported on Array<T> variables"
            )),
        };

        let elem_size: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };

        // Also get element class if applicable
        let elem_class = match init {
            Expression::Identifier(ident) => {
                self.local_array_elem_classes.get(ident.name.as_str()).cloned()
            }
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
                self.local_class_types.insert(var_name.clone(), class_name.clone());
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
                    offset: 0, align: 3, memory_index: 0,
                })),
                WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: 0, align: 2, memory_index: 0,
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
                _ => return Err(CompileError::unsupported(
                    "rest element in array destructuring must bind a plain identifier",
                )),
            };
            let prefix_count = arr_pat.elements.len() as i32;

            // rest_len_signed = src.length - prefix_count
            let rest_len_signed = self.alloc_local(WasmType::I32);
            self.push(Instruction::LocalGet(arr_local));
            self.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0, align: 2, memory_index: 0,
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
                offset: 0, align: 2, memory_index: 0,
            }));
            self.push(Instruction::LocalGet(rest_ptr));
            self.push(Instruction::LocalGet(rest_len));
            self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 4, align: 2, memory_index: 0,
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
            self.push(Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

            // Declare `rest` local and bind the new pointer; track as an array.
            let rest_local = self.declare_local(&rest_name, WasmType::I32);
            self.push(Instruction::LocalGet(rest_ptr));
            self.push(Instruction::LocalSet(rest_local));
            self.local_array_elem_types.insert(rest_name.clone(), elem_ty);
            if let Some(ref class_name) = elem_class {
                self.local_array_elem_classes.insert(rest_name, class_name.clone());
            }
        }

        Ok(())
    }

    fn emit_return(&mut self, ret: &ReturnStatement<'a>) -> Result<(), CompileError> {
        if let Some(arg) = &ret.argument {
            self.emit_expr(arg)?;
        }
        self.push(Instruction::Return);
        Ok(())
    }

    fn emit_if(&mut self, if_stmt: &IfStatement<'a>) -> Result<(), CompileError> {
        self.emit_expr(&if_stmt.test)?;
        self.push(Instruction::If(BlockType::Empty));
        self.block_depth += 1;

        self.emit_statement(&if_stmt.consequent)?;

        if let Some(alt) = &if_stmt.alternate {
            self.push(Instruction::Else);
            self.emit_statement(alt)?;
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
        self.loop_stack.push(LoopLabels { break_depth, continue_depth });

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
        self.loop_stack.push(LoopLabels { break_depth, continue_depth });

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
        self.loop_stack.push(LoopLabels { break_depth, continue_depth });

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
            _ => return Err(CompileError::unsupported("for..of requires a variable declaration")),
        };

        // Resolve array element type from the right-hand expression
        let elem_ty = self.resolve_expr_array_elem(&for_of.right)
            .ok_or_else(|| CompileError::codegen(
                "for..of requires an Array<T> — cannot resolve element type"
            ))?;
        let elem_class = self.resolve_expr_array_elem_class(&for_of.right);
        let elem_size: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type for for..of")),
        };

        // Evaluate array, save to local
        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&for_of.right)?;
        self.push(Instruction::LocalSet(arr_local));

        // Load length
        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 }));
        self.push(Instruction::LocalSet(len_local));

        // Loop counter
        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        // Declare element local
        let elem_local = self.declare_local(&elem_name, elem_ty);
        if let Some(class_name) = &elem_class {
            self.local_class_types.insert(elem_name.clone(), class_name.clone());
        }

        // Track as const if declared with const
        if let ForStatementLeft::VariableDeclaration(var_decl) = &for_of.left
            && var_decl.kind == VariableDeclarationKind::Const {
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
            WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg { offset: 0, align: 3, memory_index: 0 })),
            WasmType::I32 => self.push(Instruction::I32Load(wasm_encoder::MemArg { offset: 0, align: 2, memory_index: 0 })),
            _ => unreachable!(),
        }
        self.push(Instruction::LocalSet(elem_local));

        // Continue target block (for break/continue)
        self.push(Instruction::Block(BlockType::Empty));
        self.block_depth += 1;

        let continue_depth = self.block_depth;
        self.loop_stack.push(LoopLabels { break_depth, continue_depth });

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
        // Evaluate discriminant once
        let disc_ty = self.emit_expr(&switch.discriminant)?;
        let disc_local = self.alloc_local(disc_ty);
        self.push(Instruction::LocalSet(disc_local));

        // Simpler approach: if/else chain
        // For each case, compare and branch
        let cases: Vec<_> = switch.cases.iter().collect();

        // Outer block for break statements
        self.push(Instruction::Block(BlockType::Empty));
        self.block_depth += 1;
        let switch_depth = self.block_depth;

        // Push a fake loop entry so `break` inside switch works
        // (break in switch = br to the outer block)
        self.loop_stack.push(LoopLabels { break_depth: switch_depth, continue_depth: switch_depth });

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
                _ => return Err(CompileError::type_err("switch discriminant must be i32 or f64")),
            }

            self.push(Instruction::If(BlockType::Empty));

            // Case body
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

            self.push(Instruction::End); // end if
        }

        // Default case
        if let Some(idx) = default_idx {
            for stmt in &cases[idx].consequent {
                match stmt {
                    Statement::BreakStatement(_) => {
                        let relative = self.block_depth - switch_depth;
                        self.push(Instruction::Br(relative));
                    }
                    _ => self.emit_statement(stmt)?,
                }
            }
        }

        self.loop_stack.pop();
        self.push(Instruction::End); // end switch block
        self.block_depth -= 1;

        Ok(())
    }

    fn emit_break(&mut self) -> Result<(), CompileError> {
        let labels = self.loop_stack.last()
            .ok_or_else(|| CompileError::codegen("break outside of loop"))?;
        // br to the outer block (break target)
        // break_depth points at the outer block's depth level
        let relative = self.block_depth - labels.break_depth;
        self.push(Instruction::Br(relative));
        Ok(())
    }

    fn emit_continue(&mut self) -> Result<(), CompileError> {
        let labels = self.loop_stack.last()
            .ok_or_else(|| CompileError::codegen("continue outside of loop"))?;
        // In a for loop, continue jumps to the continue_target block end,
        // which falls through to the update expression
        let relative = self.block_depth - labels.continue_depth;
        self.push(Instruction::Br(relative));
        Ok(())
    }

    /// Infer the type of an expression without emitting WASM code.
    /// Used for type inference when no annotation is provided.
    pub fn infer_init_type(&self, expr: &Expression<'a>) -> Result<(WasmType, Option<String>), CompileError> {
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
                "cannot infer type from null — add a type annotation"
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
                    Err(CompileError::type_err(format!("cannot infer type from undefined variable '{name}'")))
                }
            }
            Expression::NewExpression(new_expr) => {
                if let Expression::Identifier(ident) = &new_expr.callee {
                    let class_name = ident.name.as_str();
                    if class_name == "Array" {
                        // Array<T> → i32, but we need the element type from type params
                        // For now, require annotation for arrays
                        return Err(CompileError::type_err(
                            "Array variables require a type annotation: Array<T>"
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
                    if name == "f64" { return Ok((WasmType::F64, None)); }
                    if name == "i32" { return Ok((WasmType::I32, None)); }
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
                        && ident.name.as_str() == "Math" {
                            return Ok((WasmType::F64, None));
                        }
                Err(CompileError::type_err("cannot infer type from this expression — add a type annotation"))
            }
            Expression::BinaryExpression(bin) => {
                // Comparisons always return i32
                match bin.operator {
                    BinaryOperator::LessThan | BinaryOperator::LessEqualThan |
                    BinaryOperator::GreaterThan | BinaryOperator::GreaterEqualThan |
                    BinaryOperator::StrictEquality | BinaryOperator::Equality |
                    BinaryOperator::StrictInequality | BinaryOperator::Inequality => {
                        return Ok((WasmType::I32, None));
                    }
                    _ => {}
                }
                // For arithmetic, infer from left operand
                self.infer_init_type(&bin.left)
            }
            Expression::UnaryExpression(un) => {
                match un.operator {
                    UnaryOperator::LogicalNot | UnaryOperator::BitwiseNot => Ok((WasmType::I32, None)),
                    _ => self.infer_init_type(&un.argument),
                }
            }
            Expression::ParenthesizedExpression(paren) => self.infer_init_type(&paren.expression),
            Expression::ConditionalExpression(cond) => self.infer_init_type(&cond.consequent),
            Expression::StaticMemberExpression(member) => {
                // e.field — try to resolve class and field type
                let class_name = match self.resolve_expr_class(&member.object) {
                    Ok(name) => name,
                    Err(_) => return Err(CompileError::type_err("cannot infer type — add a type annotation")),
                };
                if let Some(layout) = self.module_ctx.class_registry.get(&class_name)
                    && let Some(&(_, field_ty)) = layout.field_map.get(member.property.name.as_str()) {
                        return Ok((field_ty, None));
                    }
                Err(CompileError::type_err("cannot infer type — add a type annotation"))
            }
            // Arrow functions are closure pointers (i32)
            Expression::ArrowFunctionExpression(_) => Ok((WasmType::I32, None)),
            _ => Err(CompileError::type_err("cannot infer type from this expression — add a type annotation")),
        }
    }

    /// Infer a ClosureSig from an ArrowFunctionExpression's parameter annotations and return type.
    pub fn infer_arrow_sig(&self, arrow: &ArrowFunctionExpression<'a>) -> Option<ClosureSig> {
        let mut param_types = Vec::new();
        for param in &arrow.params.items {
            let ty = if let Some(ann) = &param.type_annotation {
                types::resolve_type_annotation_with_classes(ann, &self.module_ctx.class_names).ok()?
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
        Some(ClosureSig { param_types, return_type })
    }
}

// ── Boxing analysis ─────────────────────────────────────────────────
// Identifies variables that need boxing: captured by a closure AND mutated anywhere.

/// Analyze a function body and return the set of variable names that need boxing.
pub fn analyze_boxed_vars(body: &[Statement]) -> HashSet<String> {
    let mut captured = HashSet::new();  // vars referenced inside arrow bodies
    let mut mutated = HashSet::new();   // vars assigned or updated anywhere

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
        Statement::ExpressionStatement(e) => scan_expr_for_boxing(&e.expression, captured, mutated, in_arrow),
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
            if let Some(test) = &f.test { scan_expr_for_boxing(test, captured, mutated, in_arrow); }
            if let Some(update) = &f.update { scan_expr_for_boxing(update, captured, mutated, in_arrow); }
            scan_stmt_for_boxing(&f.body, captured, mutated, in_arrow);
        }
        Statement::ForOfStatement(f) => {
            scan_expr_for_boxing(&f.right, captured, mutated, in_arrow);
            scan_stmt_for_boxing(&f.body, captured, mutated, in_arrow);
        }
        Statement::BlockStatement(b) => {
            for s in &b.body { scan_stmt_for_boxing(s, captured, mutated, in_arrow); }
        }
        Statement::SwitchStatement(s) => {
            scan_expr_for_boxing(&s.discriminant, captured, mutated, in_arrow);
            for case in &s.cases {
                if let Some(test) = &case.test { scan_expr_for_boxing(test, captured, mutated, in_arrow); }
                for s in &case.consequent { scan_stmt_for_boxing(s, captured, mutated, in_arrow); }
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
            let param_names: HashSet<String> = arrow.params.items.iter()
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
        Expression::UnaryExpression(u) => scan_expr_for_boxing(&u.argument, captured, mutated, in_arrow),
        Expression::CallExpression(c) => {
            scan_expr_for_boxing(&c.callee, captured, mutated, in_arrow);
            for arg in &c.arguments { scan_expr_for_boxing(arg.to_expression(), captured, mutated, in_arrow); }
        }
        Expression::StaticMemberExpression(m) => scan_expr_for_boxing(&m.object, captured, mutated, in_arrow),
        Expression::ComputedMemberExpression(m) => {
            scan_expr_for_boxing(&m.object, captured, mutated, in_arrow);
            scan_expr_for_boxing(&m.expression, captured, mutated, in_arrow);
        }
        Expression::ConditionalExpression(c) => {
            scan_expr_for_boxing(&c.test, captured, mutated, in_arrow);
            scan_expr_for_boxing(&c.consequent, captured, mutated, in_arrow);
            scan_expr_for_boxing(&c.alternate, captured, mutated, in_arrow);
        }
        Expression::ParenthesizedExpression(p) => scan_expr_for_boxing(&p.expression, captured, mutated, in_arrow),
        Expression::NewExpression(n) => {
            for arg in &n.arguments { scan_expr_for_boxing(arg.to_expression(), captured, mutated, in_arrow); }
        }
        Expression::TSAsExpression(a) => scan_expr_for_boxing(&a.expression, captured, mutated, in_arrow),
        Expression::ChainExpression(c) => {
            match &c.expression {
                ChainElement::StaticMemberExpression(m) => scan_expr_for_boxing(&m.object, captured, mutated, in_arrow),
                ChainElement::ComputedMemberExpression(m) => {
                    scan_expr_for_boxing(&m.object, captured, mutated, in_arrow);
                    scan_expr_for_boxing(&m.expression, captured, mutated, in_arrow);
                }
                ChainElement::CallExpression(c) => {
                    scan_expr_for_boxing(&c.callee, captured, mutated, in_arrow);
                    for arg in &c.arguments { scan_expr_for_boxing(arg.to_expression(), captured, mutated, in_arrow); }
                }
                _ => {}
            }
        }
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
            for s in &b.body { collect_local_decls(s, out); }
        }
        Statement::IfStatement(i) => {
            collect_local_decls(&i.consequent, out);
            if let Some(alt) = &i.alternate { collect_local_decls(alt, out); }
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
