use oxc_ast::ast::*;
use wasm_encoder::{Instruction, ValType};

use crate::codegen::func::{FuncContext, Refinement, peel_parens};
use crate::codegen::unions::UnionMember;
use crate::error::CompileError;
use crate::types::WasmType;

use super::ARRAY_HEADER_SIZE;
use super::member::{SharedMethodIssue, resolve_shared_method_in_members};

impl<'a> FuncContext<'a> {
    // ---- Phase 3: Classes ----

    pub(crate) fn emit_new(
        &mut self,
        new_expr: &NewExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        let base_name = match &new_expr.callee {
            Expression::Identifier(ident) => ident.name.as_str(),
            _ => return Err(CompileError::unsupported("non-identifier new target")),
        };

        // Handle new Array<T>(capacity)
        if base_name == "Array" {
            return self.emit_new_array(new_expr);
        }

        // Handle new Map<K, V>() — compiler-owned, per-monomorphization layout.
        if base_name == crate::codegen::map_builtins::MAP_BASE {
            return self.emit_new_map(new_expr);
        }

        // Handle new Set<T>() — compiler-owned, per-monomorphization layout.
        if base_name == crate::codegen::set_builtins::SET_BASE {
            return self.emit_new_set(new_expr);
        }

        // Typed arrays — Int32Array / Float64Array / Uint8Array. Routed here
        // (before the generic class lookup) because their `ClassLayout` is
        // synthetic — methodless, fieldless — and the generic path would
        // happily allocate an 8-byte zeroed pseudo-instance instead of a
        // proper typed-array header + body.
        if let Some(desc) = crate::codegen::typed_arrays::descriptor_for(base_name) {
            return self.emit_new_typed_array(desc, new_expr);
        }

        // Generic instantiation: `new Box<i32>(...)` → look up the mangled
        // monomorphization. Type args may themselves reference the enclosing
        // function's type parameters, so we thread through `type_bindings`.
        let mangled_owned;
        let class_name: &str = if let Some(type_args) = new_expr.type_arguments.as_ref() {
            let mut tokens = Vec::with_capacity(type_args.params.len());
            for p in &type_args.params {
                let bt = crate::codegen::generics::resolve_bound_type(
                    p,
                    &self.module_ctx.class_names,
                    self.type_bindings.as_ref(),
                    &self.module_ctx.non_i32_union_wasm_types,
                )?;
                tokens.push(bt.mangle_token());
            }
            mangled_owned = format!("{base_name}${}", tokens.join("$"));
            &mangled_owned
        } else {
            base_name
        };

        let layout = self
            .module_ctx
            .class_registry
            .get(class_name)
            .ok_or_else(|| CompileError::codegen(format!("unknown class '{class_name}'")))?;
        let size = layout.size;

        // Allocate object via arena
        self.push(Instruction::I32Const(size as i32));
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        // Write vtable pointer at offset 0 for polymorphic classes. Stored
        // unconditionally — even methodless polymorphic classes need a
        // unique runtime tag for Phase 2 `instanceof` narrowing, and the
        // vtable allocator now guarantees each polymorphic class has its
        // own 4-byte region (see `module.rs` vtable construction).
        if layout.is_polymorphic {
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

    /// Emit `new Map<K, V>()` — arena-allocate the 5-field header and an
    /// initial bucket array of size `INITIAL_CAPACITY`, then initialize the
    /// header. State bytes in the bucket array default to `BUCKET_EMPTY` (0)
    /// via the arena's bump-over-fresh-memory property, so no explicit
    /// memory.fill is needed here. `head_idx`/`tail_idx` are set to `-1` so
    /// the first `set()` can detect an empty list.
    pub(crate) fn emit_new_map(
        &mut self,
        new_expr: &NewExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        use crate::codegen::hash_table::INITIAL_CAPACITY;
        use crate::codegen::map_builtins;

        if !new_expr.arguments.is_empty() {
            return Err(CompileError::codegen(
                "new Map<K, V>() takes no arguments",
            ));
        }
        let type_args = new_expr.type_arguments.as_ref().ok_or_else(|| {
            CompileError::type_err("new Map requires explicit <K, V> type arguments")
        })?;
        if type_args.params.len() != map_builtins::MAP_ARITY {
            return Err(CompileError::type_err(format!(
                "Map<K, V> expects 2 type arguments, got {}",
                type_args.params.len()
            )));
        }
        let key_ty = crate::codegen::generics::resolve_bound_type(
            &type_args.params[0],
            &self.module_ctx.class_names,
            self.type_bindings.as_ref(),
            &self.module_ctx.non_i32_union_wasm_types,
        )?;
        let value_ty = crate::codegen::generics::resolve_bound_type(
            &type_args.params[1],
            &self.module_ctx.class_names,
            self.type_bindings.as_ref(),
            &self.module_ctx.non_i32_union_wasm_types,
        )?;
        let mangled = map_builtins::mangle_map_name(&key_ty, &value_ty);
        let info = self.module_ctx.hash_table_info.get(&mangled).ok_or_else(|| {
            CompileError::codegen(format!("map instantiation '{mangled}' not registered"))
        })?;
        let header_size = self
            .module_ctx
            .class_registry
            .get(&mangled)
            .expect("map layout registered alongside hash_table_info")
            .size;
        let bucket_size = info.bucket.total_size;

        // Allocate header.
        self.push(Instruction::I32Const(header_size as i32));
        let header_ptr = self.emit_arena_alloc_to_local(true)?;

        // Allocate bucket array.
        self.push(Instruction::I32Const(
            (INITIAL_CAPACITY * bucket_size) as i32,
        ));
        let buckets_ptr = self.emit_arena_alloc_to_local(true)?;

        let buckets_off = self.field_offset(&mangled, "buckets_ptr");
        let capacity_off = self.field_offset(&mangled, "capacity");
        let head_off = self.field_offset(&mangled, "head_idx");
        let tail_off = self.field_offset(&mangled, "tail_idx");

        // header.buckets_ptr = buckets_ptr
        self.push(Instruction::LocalGet(header_ptr));
        self.push(Instruction::LocalGet(buckets_ptr));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: buckets_off as u64,
            align: 2,
            memory_index: 0,
        }));

        // header.capacity = INITIAL_CAPACITY
        self.push(Instruction::LocalGet(header_ptr));
        self.push(Instruction::I32Const(INITIAL_CAPACITY as i32));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: capacity_off as u64,
            align: 2,
            memory_index: 0,
        }));

        // header.head_idx = -1, header.tail_idx = -1
        self.push(Instruction::LocalGet(header_ptr));
        self.push(Instruction::I32Const(crate::codegen::hash_table::EMPTY_LINK));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: head_off as u64,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(header_ptr));
        self.push(Instruction::I32Const(crate::codegen::hash_table::EMPTY_LINK));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: tail_off as u64,
            align: 2,
            memory_index: 0,
        }));

        // size defaults to 0 from arena zero-init.

        self.push(Instruction::LocalGet(header_ptr));
        Ok(WasmType::I32)
    }

    /// Emit `new Set<T>()` — arena-allocate the 5-field header and an initial
    /// bucket array of size `INITIAL_CAPACITY`, then initialize the header.
    /// State bytes in the bucket array default to `BUCKET_EMPTY` (0) via the
    /// arena's bump-over-fresh-memory property. `head_idx`/`tail_idx` are set
    /// to `-1` so the first `add()` can detect an empty list.
    pub(crate) fn emit_new_set(
        &mut self,
        new_expr: &NewExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        use crate::codegen::hash_table::INITIAL_CAPACITY;
        use crate::codegen::set_builtins;

        if !new_expr.arguments.is_empty() {
            return Err(CompileError::codegen(
                "new Set<T>() takes no arguments",
            ));
        }
        let type_args = new_expr.type_arguments.as_ref().ok_or_else(|| {
            CompileError::type_err("new Set requires explicit <T> type argument")
        })?;
        if type_args.params.len() != set_builtins::SET_ARITY {
            return Err(CompileError::type_err(format!(
                "Set<T> expects 1 type argument, got {}",
                type_args.params.len()
            )));
        }
        let elem_ty = crate::codegen::generics::resolve_bound_type(
            &type_args.params[0],
            &self.module_ctx.class_names,
            self.type_bindings.as_ref(),
            &self.module_ctx.non_i32_union_wasm_types,
        )?;
        let mangled = set_builtins::mangle_set_name(&elem_ty);
        let info = self.module_ctx.hash_table_info.get(&mangled).ok_or_else(|| {
            CompileError::codegen(format!("set instantiation '{mangled}' not registered"))
        })?;
        let header_size = self
            .module_ctx
            .class_registry
            .get(&mangled)
            .expect("set layout registered alongside hash_table_info")
            .size;
        let bucket_size = info.bucket.total_size;

        // Allocate header.
        self.push(Instruction::I32Const(header_size as i32));
        let header_ptr = self.emit_arena_alloc_to_local(true)?;

        // Allocate bucket array.
        self.push(Instruction::I32Const(
            (INITIAL_CAPACITY * bucket_size) as i32,
        ));
        let buckets_ptr = self.emit_arena_alloc_to_local(true)?;

        let buckets_off = self.field_offset(&mangled, "buckets_ptr");
        let capacity_off = self.field_offset(&mangled, "capacity");
        let head_off = self.field_offset(&mangled, "head_idx");
        let tail_off = self.field_offset(&mangled, "tail_idx");

        self.push(Instruction::LocalGet(header_ptr));
        self.push(Instruction::LocalGet(buckets_ptr));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: buckets_off as u64,
            align: 2,
            memory_index: 0,
        }));

        self.push(Instruction::LocalGet(header_ptr));
        self.push(Instruction::I32Const(INITIAL_CAPACITY as i32));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: capacity_off as u64,
            align: 2,
            memory_index: 0,
        }));

        self.push(Instruction::LocalGet(header_ptr));
        self.push(Instruction::I32Const(crate::codegen::hash_table::EMPTY_LINK));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: head_off as u64,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(header_ptr));
        self.push(Instruction::I32Const(crate::codegen::hash_table::EMPTY_LINK));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: tail_off as u64,
            align: 2,
            memory_index: 0,
        }));

        self.push(Instruction::LocalGet(header_ptr));
        Ok(WasmType::I32)
    }

    /// Field offset lookup shared between Map codegen paths. Panics if the
    /// class or field is missing — callers stage the check earlier.
    fn field_offset(&self, class_name: &str, field_name: &str) -> u32 {
        self.module_ctx
            .class_registry
            .get(class_name)
            .and_then(|l| l.field_map.get(field_name).map(|(off, _)| *off))
            .unwrap_or_else(|| {
                panic!("class '{class_name}' has no field '{field_name}'");
            })
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

    /// Emit an array literal `[a, b, c]` — arena-allocates header + elements in
    /// a single bump, then stores each element inline. Element type is taken
    /// from `expected_elem_ty` when supplied (e.g. the declared annotation on
    /// the target variable), otherwise inferred from the first element. Empty
    /// literals without context raise a type error rather than silently picking
    /// a default.
    pub(crate) fn emit_array_literal(
        &mut self,
        arr_expr: &ArrayExpression<'a>,
        expected_elem_ty: Option<WasmType>,
    ) -> Result<WasmType, CompileError> {
        self.emit_array_literal_with_class(arr_expr, expected_elem_ty, None)
    }

    /// Variant of `emit_array_literal` that also carries the expected class
    /// name of each element — used so `Array<[i32, i32]>` literals route
    /// their inner `[a, b]` ArrayExpressions through `emit_tuple_literal`
    /// (and `Array<Shape>` routes inner ObjectExpressions through
    /// `emit_object_literal`). When `expected_elem_class` is `None` the
    /// behavior is identical to `emit_array_literal`.
    pub(crate) fn emit_array_literal_with_class(
        &mut self,
        arr_expr: &ArrayExpression<'a>,
        expected_elem_ty: Option<WasmType>,
        expected_elem_class: Option<&str>,
    ) -> Result<WasmType, CompileError> {
        // Reject holes; spreads go through a runtime-length path.
        let mut has_spread = false;
        for el in &arr_expr.elements {
            match el {
                ArrayExpressionElement::SpreadElement(_) => has_spread = true,
                ArrayExpressionElement::Elision(_) => {
                    return Err(CompileError::unsupported("hole in array literal"));
                }
                _ => {}
            }
        }

        // Determine element type from context, first inline element, or first spread source.
        let elem_ty = if let Some(ty) = expected_elem_ty {
            ty
        } else if arr_expr.elements.is_empty() {
            return Err(CompileError::type_err(
                "cannot infer element type of empty array literal — add a type annotation: `let x: number[] = []`",
            ));
        } else {
            let first = &arr_expr.elements[0];
            match first {
                ArrayExpressionElement::SpreadElement(s) => self
                    .resolve_expr_array_elem(&s.argument)
                    .ok_or_else(|| CompileError::type_err(
                        "cannot infer element type from spread source — add a type annotation on the target variable",
                    ))?,
                _ => {
                    let expr = first.as_expression().ok_or_else(|| {
                        CompileError::codegen("unsupported first array literal element")
                    })?;
                    let (ty, _class) = self.infer_init_type(expr)?;
                    ty
                }
            }
        };

        let elem_size: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => {
                return Err(CompileError::type_err(
                    "Array element type must be i32 or f64",
                ));
            }
        };

        if !has_spread {
            return self.emit_array_literal_fixed(
                arr_expr,
                elem_ty,
                elem_size,
                expected_elem_class,
            );
        }
        self.emit_array_literal_with_spread(arr_expr, elem_ty, elem_size)
    }

    /// Fast path for spread-free array literals: element count is known at
    /// compile time, so we allocate once and emit inline stores.
    fn emit_array_literal_fixed(
        &mut self,
        arr_expr: &ArrayExpression<'a>,
        elem_ty: WasmType,
        elem_size: i32,
        expected_elem_class: Option<&str>,
    ) -> Result<WasmType, CompileError> {
        let elem_count = arr_expr.elements.len() as i32;
        let total = ARRAY_HEADER_SIZE as i32 + elem_count * elem_size;
        self.push(Instruction::I32Const(total));
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(elem_count));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(elem_count));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        for (i, el) in arr_expr.elements.iter().enumerate() {
            let expr = el.as_expression().ok_or_else(|| {
                CompileError::codegen("unsupported array literal element kind")
            })?;
            self.push(Instruction::LocalGet(ptr_local));
            let ty = self.emit_element_with_class(expr, expected_elem_class)?;
            if ty != elem_ty {
                if elem_ty == WasmType::F64 && ty == WasmType::I32 {
                    self.push(Instruction::F64ConvertI32S);
                } else {
                    return Err(CompileError::type_err(format!(
                        "array literal element {i} has type {ty:?}, expected {elem_ty:?}"
                    )));
                }
            }
            let offset = (ARRAY_HEADER_SIZE as i32 + (i as i32) * elem_size) as u64;
            match elem_ty {
                WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg {
                    offset,
                    align: 3,
                    memory_index: 0,
                })),
                WasmType::I32 => self.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset,
                    align: 2,
                    memory_index: 0,
                })),
                _ => unreachable!(),
            }
        }

        self.push(Instruction::LocalGet(ptr_local));
        Ok(WasmType::I32)
    }

    /// Emit a single array element, routing nested literal forms through
    /// the shape-aware emitters when the element class is a registered
    /// tuple or object shape. Falls back to `emit_expr` otherwise.
    fn emit_element_with_class(
        &mut self,
        expr: &Expression<'a>,
        expected_elem_class: Option<&str>,
    ) -> Result<WasmType, CompileError> {
        match (expr, expected_elem_class) {
            (Expression::ArrayExpression(inner), Some(cn)) if self.is_tuple_shape(cn) => {
                let (ty, _) = self.emit_tuple_literal(inner, cn)?;
                Ok(ty)
            }
            (Expression::ObjectExpression(inner), Some(cn))
                if self
                    .module_ctx
                    .shape_registry
                    .by_name
                    .contains_key(cn) =>
            {
                let (ty, _) = self.emit_object_literal(inner, Some(cn))?;
                Ok(ty)
            }
            _ => self.emit_expr(expr),
        }
    }

    /// Runtime-length path for array literals containing `...spread` elements.
    /// Strategy:
    ///   1. Evaluate every inline element and every spread source upfront into
    ///      locals (JS evaluation order, and protects against arena moves).
    ///   2. Sum spread lengths + inline count into `total_len` at runtime.
    ///   3. Allocate header + total_len * elem_size; write header.
    ///   4. Walk elements again, copying each inline value or memory.copy'ing
    ///      each spread source into the output at the running write cursor.
    fn emit_array_literal_with_spread(
        &mut self,
        arr_expr: &ArrayExpression<'a>,
        elem_ty: WasmType,
        elem_size: i32,
    ) -> Result<WasmType, CompileError> {
        // Pre-evaluate elements into locals, tagging what each slot holds.
        enum Piece {
            Inline(u32), // local holds the value
            Spread { ptr: u32, len: u32 }, // locals hold source pointer and its length
        }
        let mut pieces: Vec<Piece> = Vec::with_capacity(arr_expr.elements.len());

        for (i, el) in arr_expr.elements.iter().enumerate() {
            match el {
                ArrayExpressionElement::SpreadElement(s) => {
                    // Validate spread source element type matches.
                    let src_elem = self.resolve_expr_array_elem(&s.argument).ok_or_else(|| {
                        CompileError::type_err(format!(
                            "spread source at literal position {i} is not a known array"
                        ))
                    })?;
                    if src_elem != elem_ty {
                        return Err(CompileError::type_err(format!(
                            "spread source element type {src_elem:?} does not match array literal element type {elem_ty:?}"
                        )));
                    }
                    // Evaluate once, save pointer.
                    let ptr = self.alloc_local(WasmType::I32);
                    self.emit_expr(&s.argument)?;
                    self.push(Instruction::LocalSet(ptr));
                    // Load .length
                    let len = self.alloc_local(WasmType::I32);
                    self.push(Instruction::LocalGet(ptr));
                    self.push(Instruction::I32Load(wasm_encoder::MemArg {
                        offset: 0,
                        align: 2,
                        memory_index: 0,
                    }));
                    self.push(Instruction::LocalSet(len));
                    pieces.push(Piece::Spread { ptr, len });
                }
                _ => {
                    let expr = el.as_expression().ok_or_else(|| {
                        CompileError::codegen("unsupported array literal element kind")
                    })?;
                    let ty = self.emit_expr(expr)?;
                    if ty != elem_ty {
                        if elem_ty == WasmType::F64 && ty == WasmType::I32 {
                            self.push(Instruction::F64ConvertI32S);
                        } else {
                            return Err(CompileError::type_err(format!(
                                "array literal element {i} has type {ty:?}, expected {elem_ty:?}"
                            )));
                        }
                    }
                    let val = self.alloc_local(elem_ty);
                    self.push(Instruction::LocalSet(val));
                    pieces.push(Piece::Inline(val));
                }
            }
        }

        // total_len = inline_count + sum(spread.len)
        let inline_count: i32 = pieces
            .iter()
            .filter(|p| matches!(p, Piece::Inline(_)))
            .count() as i32;
        let total_len = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(inline_count));
        self.push(Instruction::LocalSet(total_len));
        for p in &pieces {
            if let Piece::Spread { len, .. } = p {
                self.push(Instruction::LocalGet(total_len));
                self.push(Instruction::LocalGet(*len));
                self.push(Instruction::I32Add);
                self.push(Instruction::LocalSet(total_len));
            }
        }

        // Allocate: HEADER + total_len * elem_size
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(total_len));
        self.push(Instruction::I32Const(elem_size));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        // Header: length = total_len, capacity = total_len
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::LocalGet(total_len));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::LocalGet(total_len));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // write_off: byte offset from ptr + HEADER at which the next element lands.
        let write_off = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(write_off));

        for piece in &pieces {
            match piece {
                Piece::Inline(val) => {
                    // Store val at ptr + HEADER + write_off; write_off += elem_size.
                    self.push(Instruction::LocalGet(ptr_local));
                    self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
                    self.push(Instruction::I32Add);
                    self.push(Instruction::LocalGet(write_off));
                    self.push(Instruction::I32Add);
                    self.push(Instruction::LocalGet(*val));
                    match elem_ty {
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
                    self.push(Instruction::LocalGet(write_off));
                    self.push(Instruction::I32Const(elem_size));
                    self.push(Instruction::I32Add);
                    self.push(Instruction::LocalSet(write_off));
                }
                Piece::Spread { ptr, len } => {
                    // memory.copy(ptr + HEADER + write_off, src + HEADER, len * elem_size)
                    self.push(Instruction::LocalGet(ptr_local));
                    self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
                    self.push(Instruction::I32Add);
                    self.push(Instruction::LocalGet(write_off));
                    self.push(Instruction::I32Add);
                    self.push(Instruction::LocalGet(*ptr));
                    self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
                    self.push(Instruction::I32Add);
                    self.push(Instruction::LocalGet(*len));
                    self.push(Instruction::I32Const(elem_size));
                    self.push(Instruction::I32Mul);
                    self.push(Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                    // write_off += len * elem_size
                    self.push(Instruction::LocalGet(write_off));
                    self.push(Instruction::LocalGet(*len));
                    self.push(Instruction::I32Const(elem_size));
                    self.push(Instruction::I32Mul);
                    self.push(Instruction::I32Add);
                    self.push(Instruction::LocalSet(write_off));
                }
            }
        }

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
            return crate::types::resolve_ts_type_full(
                first,
                &self.module_ctx.class_names,
                self.type_bindings.as_ref(),
                Some(&self.module_ctx.non_i32_union_wasm_types),
            );
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

        // Phase 2 sub-phase 3 — union receiver. If the resolved name names
        // a union (i.e. the local's static type is `type U = A | B | …`),
        // dispatch via the vtable when every variant carries the method at
        // the same slot with matching parameter / return WasmTypes. The
        // polymorphism gate (sub-phase 1) guarantees every class member
        // already has a vtable pointer at offset 0; the shared-slot
        // constraint here is the method-side mirror of the Phase 1
        // shared-field rule. `Refinement::Class(c)` is invisible at this
        // point because `current_class_of` collapsed it to `c` — so only
        // the unrefined / `Subunion(_)` / `Never` cases reach this arm.
        if let Some(union) = self
            .module_ctx
            .union_registry
            .get_by_name(&class_name)
            .cloned()
        {
            return self
                .emit_union_method_call(call, member, &class_name, &union.members, method_name)
                .map(Some);
        }

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

        // Pull the method signature once so we can thread expected-type hints
        // into `ObjectExpression` arguments on both dispatch paths.
        let param_classes: Option<Vec<Option<String>>> = layout
            .and_then(|l| l.methods.get(method_name))
            .map(|sig| sig.param_classes.clone());

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
            for (i, arg) in call.arguments.iter().enumerate() {
                let expr = arg.to_expression();
                let expected = param_classes
                    .as_ref()
                    .and_then(|pc| pc.get(i))
                    .and_then(|c| c.as_deref());
                match (expr, expected) {
                    (Expression::ObjectExpression(obj), Some(en)) => {
                        self.emit_object_literal(obj, Some(en))?;
                    }
                    (Expression::ArrayExpression(arr), Some(en)) if self.is_tuple_shape(en) => {
                        self.emit_tuple_literal(arr, en)?;
                    }
                    _ => {
                        self.emit_expr_coerced(expr, expected)?;
                    }
                }
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
            for (i, arg) in call.arguments.iter().enumerate() {
                let expr = arg.to_expression();
                let expected = param_classes
                    .as_ref()
                    .and_then(|pc| pc.get(i))
                    .and_then(|c| c.as_deref());
                match (expr, expected) {
                    (Expression::ObjectExpression(obj), Some(en)) => {
                        self.emit_object_literal(obj, Some(en))?;
                    }
                    (Expression::ArrayExpression(arr), Some(en)) if self.is_tuple_shape(en) => {
                        self.emit_tuple_literal(arr, en)?;
                    }
                    _ => {
                        self.emit_expr_coerced(expr, expected)?;
                    }
                }
            }
            self.push(Instruction::Call(func_idx));

            Ok(Some(ret_ty))
        }
    }

    /// Phase 2 sub-phase 3 — emit a method call on a union receiver. The
    /// receiver's static type is `union_name`; `union_members` carries the
    /// declared member set. If the receiver is a refined identifier we
    /// substitute its `Subunion(_)` member set and reject `Never`. The
    /// shared-method helper either yields a `(slot, sig)` pair or names
    /// the failure mode; the latter is mapped to a narrow-first
    /// diagnostic that distinguishes missing / mis-slotted / mis-signed.
    fn emit_union_method_call(
        &mut self,
        call: &CallExpression<'a>,
        member: &StaticMemberExpression<'a>,
        union_name: &str,
        union_members: &[UnionMember],
        method_name: &str,
    ) -> Result<WasmType, CompileError> {
        // Apply per-receiver refinement (mirror of `emit_member_access`):
        // a `Subunion` narrows the candidate member list; `Never` is
        // unreachable; `Class(_)` was already collapsed to a class name
        // by `current_class_of`, so it never reaches this branch.
        let receiver_refinement = if let Expression::Identifier(ident) = peel_parens(&member.object)
        {
            self.current_refinement_of(ident.name.as_str()).cloned()
        } else {
            None
        };
        let (effective_members, refined): (Vec<UnionMember>, bool) = match receiver_refinement {
            Some(Refinement::Never) => {
                return Err(CompileError::type_err(format!(
                    "value of union '{union_name}' is unreachable here — every \
                     variant has been ruled out by prior narrowing"
                )));
            }
            Some(Refinement::Subunion(members)) => (members, true),
            Some(Refinement::Class(_)) | None => (union_members.to_vec(), false),
        };
        let suffix = if refined { " (after refinement)" } else { "" };
        let (slot, sig) = resolve_shared_method_in_members(self, &effective_members, method_name)
            .map_err(|issue| match issue {
                SharedMethodIssue::MissingOnVariant(v) => CompileError::type_err(format!(
                    "method '{method_name}' is not declared on every variant of union \
                     '{union_name}'{suffix} (variant '{v}' lacks it) — narrow the value with \
                     `if (x instanceof …)` before calling variant-specific methods"
                )),
                SharedMethodIssue::ShapeHasNoMethods(v) => CompileError::type_err(format!(
                    "method '{method_name}' cannot be called on union '{union_name}'{suffix} \
                     because shape variant '{v}' has no methods (only classes carry a vtable). \
                     Narrow the value with `if (x instanceof <Class>)` so the receiver is a \
                     concrete class before calling '{method_name}'"
                )),
                SharedMethodIssue::SlotMismatch => CompileError::type_err(format!(
                    "method '{method_name}' on union '{union_name}'{suffix} is declared \
                     independently per variant — no common base owns the method, so \
                     dispatch slots differ. Add a common base class that declares \
                     '{method_name}', or narrow with `if (x instanceof …)` first"
                )),
                SharedMethodIssue::SignatureMismatch => CompileError::type_err(format!(
                    "method '{method_name}' on union '{union_name}'{suffix} has different \
                     parameter or return types across variants — narrow with \
                     `if (x instanceof …)` first"
                )),
            })?;

        // Polymorphic dispatch via call_indirect. Standard idiom: load
        // vtable pointer at offset 0, index by slot, call_indirect with a
        // synthesized type for `(this, params...) -> ret`.
        if let Some(recv_local) = self.method_receiver_override {
            self.push(Instruction::LocalGet(recv_local));
        } else {
            self.emit_expr(&member.object)?;
        }
        let this_tmp = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalTee(this_tmp));

        // Emit args. We thread `param_classes` from the resolved
        // representative `MethodSig` — for shared methods inherited from a
        // common ancestor (the typical Phase 2 case) every variant points
        // at the same record, so this is the right hint.
        for (i, arg) in call.arguments.iter().enumerate() {
            let expr = arg.to_expression();
            let expected = sig.param_classes.get(i).and_then(|c| c.as_deref());
            match (expr, expected) {
                (Expression::ObjectExpression(obj), Some(en)) => {
                    self.emit_object_literal(obj, Some(en))?;
                }
                (Expression::ArrayExpression(arr), Some(en)) if self.is_tuple_shape(en) => {
                    self.emit_tuple_literal(arr, en)?;
                }
                _ => {
                    self.emit_expr_coerced(expr, expected)?;
                }
            }
        }

        // Load vtable pointer from this (offset 0), then table index at
        // slot offset.
        self.push(Instruction::LocalGet(this_tmp));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: (slot as u64) * 4,
            align: 2,
            memory_index: 0,
        }));

        // Synthesize the call_indirect type signature.
        let mut param_types = vec![ValType::I32]; // this
        for (_pname, pty) in &sig.params {
            if let Some(vt) = pty.to_val_type() {
                param_types.push(vt);
            }
        }
        let result_types = crate::codegen::wasm_types::wasm_results(sig.return_type);
        let type_idx = self
            .module_ctx
            .get_or_add_type_sig(param_types, result_types);
        self.push(Instruction::CallIndirect {
            type_index: type_idx,
            table_index: 0,
        });

        Ok(sig.return_type)
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
                // `current_class_of` consults the refinement env first and
                // falls back to the declared class name — so inside a
                // narrowed branch, `sh` resolves to its refined variant and
                // all downstream consumers (coerce, member access, method
                // dispatch) pick up the refinement automatically.
                if let Some(class_name) = self.current_class_of(name) {
                    return Ok(class_name.to_string());
                }
                Err(CompileError::codegen(format!(
                    "cannot resolve class type of variable '{name}'"
                )))
            }
            Expression::ThisExpression(_) => self
                .this_class
                .clone()
                .ok_or_else(|| CompileError::codegen("`this` used outside of a method")),
            // new ClassName(...) → class is ClassName (or the mangled
            // monomorphization when type arguments are present).
            Expression::NewExpression(new_expr) => {
                if let Expression::Identifier(ident) = &new_expr.callee {
                    let base = ident.name.as_str();
                    if let Some(type_args) = new_expr.type_arguments.as_ref() {
                        let mut tokens = Vec::with_capacity(type_args.params.len());
                        for p in &type_args.params {
                            let bt = crate::codegen::generics::resolve_bound_type(
                                p,
                                &self.module_ctx.class_names,
                                self.type_bindings.as_ref(),
                                &self.module_ctx.non_i32_union_wasm_types,
                            )?;
                            tokens.push(bt.mangle_token());
                        }
                        let mangled = format!("{base}${}", tokens.join("$"));
                        if self.module_ctx.class_names.contains(&mangled) {
                            return Ok(mangled);
                        }
                    }
                    if self.module_ctx.class_names.contains(base) {
                        return Ok(base.to_string());
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
                        // Typed-array methods that return the same kind as
                        // the receiver — `slice` / `subarray` / `map` /
                        // `filter` / `fill` / `reverse` / `sort` /
                        // `copyWithin`. Without this, chained HOF calls
                        // (e.g. `ta.filter(...).map(...)`) lose their
                        // typed-array kind and fall through to the
                        // generic call dispatch.
                        if crate::codegen::typed_arrays::descriptor_for(&obj_class).is_some()
                            && matches!(
                                method_name,
                                "slice"
                                    | "subarray"
                                    | "map"
                                    | "filter"
                                    | "fill"
                                    | "reverse"
                                    | "sort"
                                    | "copyWithin"
                            )
                        {
                            return Ok(obj_class);
                        }
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
                    if let Some(class_name) = self.module_ctx.fn_return_classes.get(name) {
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
            // tuple[N] or arr[i] → resolve the slot / element class.
            Expression::ComputedMemberExpression(member) => {
                // Tuple: `t[0]` where t's class is a registered tuple shape
                // and the index is a literal.
                if let Ok(obj_class) = self.resolve_expr_class(&member.object)
                    && let Some(&shape_idx) =
                        self.module_ctx.shape_registry.by_name.get(&obj_class)
                    && self.module_ctx.shape_registry.shapes[shape_idx].is_tuple
                {
                    let layout = self
                        .module_ctx
                        .class_registry
                        .get(&obj_class)
                        .ok_or_else(|| {
                            CompileError::codegen(format!("tuple '{obj_class}' not registered"))
                        })?;
                    let index = tuple_index_from_literal(&member.expression).ok_or_else(|| {
                        CompileError::type_err(format!(
                            "tuple '{obj_class}' requires a literal numeric index"
                        ))
                    })?;
                    if index >= layout.fields.len() {
                        return Err(CompileError::type_err(format!(
                            "tuple index {index} out of bounds for '{obj_class}'"
                        )));
                    }
                    let slot_name = &layout.fields[index].0;
                    if let Some(cn) = layout.field_class_types.get(slot_name) {
                        return Ok(cn.clone());
                    }
                    return Err(CompileError::codegen(format!(
                        "tuple '{obj_class}' slot {index} is not a class"
                    )));
                }
                // Array of class-typed elements: `arr[i]` → element class.
                if let Expression::Identifier(ident) = &member.object
                    && let Some(cn) = self.local_array_elem_classes.get(ident.name.as_str())
                {
                    return Ok(cn.clone());
                }
                Err(CompileError::codegen(
                    "cannot resolve class type of computed member access",
                ))
            }
            // (expr as ClassName) → target class
            Expression::TSAsExpression(as_expr) => {
                if let Some(class_name) = crate::types::get_class_type_name_from_ts_type(
                    &as_expr.type_annotation,
                    Some(&self.module_ctx.shape_registry),
                    Some(&self.module_ctx.union_registry),
                ) && self.module_ctx.class_names.contains(&class_name)
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

/// Extract a non-negative integer literal from a tuple-index expression.
/// Duplicated from `expr::member` because class-resolution runs in a `&self`
/// path that pre-dates emit-time index extraction. Keep the two in sync.
fn tuple_index_from_literal(expr: &Expression<'_>) -> Option<usize> {
    match expr {
        Expression::ParenthesizedExpression(p) => tuple_index_from_literal(&p.expression),
        Expression::NumericLiteral(lit) => {
            let v = lit.value;
            if v.fract() != 0.0 || v < 0.0 {
                return None;
            }
            Some(v as usize)
        }
        _ => None,
    }
}
