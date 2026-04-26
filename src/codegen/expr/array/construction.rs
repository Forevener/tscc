use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

use super::super::ARRAY_HEADER_SIZE;

impl<'a> FuncContext<'a> {
    /// Emit `arr.slice(start?, end?)` — allocates a new array and copies the
    /// selected range via memory.copy. Negative indices are normalized by
    /// adding len; both ends are clamped to [0, len].
    pub(crate) fn emit_array_slice(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };
        let arena_idx = self
            .module_ctx
            .arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(arr_local));

        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(len_local));

        let start_local = self.alloc_local(WasmType::I32);
        let end_local = self.alloc_local(WasmType::I32);
        if !call.arguments.is_empty() {
            let ty = self.emit_expr(call.arguments[0].to_expression())?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
        } else {
            self.push(Instruction::I32Const(0));
        }
        self.push(Instruction::LocalSet(start_local));
        if call.arguments.len() == 2 {
            let ty = self.emit_expr(call.arguments[1].to_expression())?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
        } else {
            self.push(Instruction::LocalGet(len_local));
        }
        self.push(Instruction::LocalSet(end_local));

        // Normalize + clamp (same pattern as fill)
        for &bound in &[start_local, end_local] {
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(wasm_encoder::BlockType::Empty));
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(bound));
            self.push(Instruction::End);
        }
        // Clamp to [0, len]
        let clamp_to_len = |fc: &mut FuncContext<'a>, bound: u32| {
            // if bound < 0: bound = 0
            fc.push(Instruction::LocalGet(bound));
            fc.push(Instruction::I32Const(0));
            fc.push(Instruction::I32LtS);
            fc.push(Instruction::If(wasm_encoder::BlockType::Empty));
            fc.push(Instruction::I32Const(0));
            fc.push(Instruction::LocalSet(bound));
            fc.push(Instruction::End);
            // if bound > len: bound = len
            fc.push(Instruction::LocalGet(bound));
            fc.push(Instruction::LocalGet(len_local));
            fc.push(Instruction::I32GtS);
            fc.push(Instruction::If(wasm_encoder::BlockType::Empty));
            fc.push(Instruction::LocalGet(len_local));
            fc.push(Instruction::LocalSet(bound));
            fc.push(Instruction::End);
        };
        clamp_to_len(self, start_local);
        clamp_to_len(self, end_local);
        // if end < start: end = start
        self.push(Instruction::LocalGet(end_local));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32LtS);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::LocalSet(end_local));
        self.push(Instruction::End);

        // count = end - start
        let count_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(end_local));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(count_local));

        // Allocate new array via arena (header + count * esize)
        let new_ptr = self.alloc_local(WasmType::I32);
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::LocalSet(new_ptr));
        // bump arena by header + count*esize
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Add);
        self.push(Instruction::GlobalSet(arena_idx));

        // Write header: length=count, capacity=count
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // memory.copy(dst=new_ptr+HEADER, src=arr+HEADER+start*esize, n=count*esize)
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        self.push(Instruction::LocalGet(new_ptr));
        Ok(())
    }

    /// Emit `arr.concat(other)` — new array = this + other. Only the
    /// single-argument, same-element-type form is supported (richer overloads
    /// can be layered via the closure builtins in a later pass).
    /// `arr.concat(b, c, ...)` — variadic concat. Each argument must be an
    /// array of the same element type. Allocates once for the total length and
    /// memcpys each source in order. Single-argument calls are the common case
    /// but the variadic form mirrors the ES spec's overload (ignoring the
    /// non-array-arg form, which doesn't fit the typed subset).
    pub(crate) fn emit_array_concat(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };
        let arena_idx = self
            .module_ctx
            .arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

        // Collect (ptr_local, len_local) pairs for the receiver and each arg.
        let mut sources: Vec<(u32, u32)> = Vec::with_capacity(call.arguments.len() + 1);
        let push_source = |fc: &mut FuncContext<'a>,
                               expr: &Expression<'a>|
         -> Result<(u32, u32), CompileError> {
            let ptr = fc.alloc_local(WasmType::I32);
            fc.emit_expr(expr)?;
            fc.push(Instruction::LocalSet(ptr));
            let len = fc.alloc_local(WasmType::I32);
            fc.push(Instruction::LocalGet(ptr));
            fc.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
            fc.push(Instruction::LocalSet(len));
            Ok((ptr, len))
        };
        sources.push(push_source(self, arr_expr)?);
        for arg in &call.arguments {
            sources.push(push_source(self, arg.to_expression())?);
        }

        // total_len = sum of all lengths.
        let total_len = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(sources[0].1));
        for (_, len) in sources.iter().skip(1) {
            self.push(Instruction::LocalGet(*len));
            self.push(Instruction::I32Add);
        }
        self.push(Instruction::LocalSet(total_len));

        // Allocate new array.
        let new_ptr = self.alloc_local(WasmType::I32);
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::LocalSet(new_ptr));
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(total_len));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Add);
        self.push(Instruction::GlobalSet(arena_idx));

        // Header: length = capacity = total_len.
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(total_len));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(total_len));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // Running byte offset within the new array's body, held in a local so
        // each copy step can advance it by the current source's byte length.
        let offset_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(offset_local));

        for (ptr, len) in &sources {
            // memory.copy(new_ptr + HEADER + offset, ptr + HEADER, len * esize)
            self.push(Instruction::LocalGet(new_ptr));
            self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalGet(offset_local));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalGet(*ptr));
            self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalGet(*len));
            self.push(Instruction::I32Const(esize));
            self.push(Instruction::I32Mul);
            self.push(Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });
            // offset += len * esize
            self.push(Instruction::LocalGet(offset_local));
            self.push(Instruction::LocalGet(*len));
            self.push(Instruction::I32Const(esize));
            self.push(Instruction::I32Mul);
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(offset_local));
        }

        self.push(Instruction::LocalGet(new_ptr));
        Ok(())
    }

    /// Emit `arr.join(sep?)` — stringifies each element (i32 via __str_from_i32,
    /// f64 via __str_from_f64, string elements pass through) and concatenates
    /// with `sep` (default ",") between them.
    pub(crate) fn emit_array_join(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };

        // Select element-stringifier helper
        let to_str_helper: &str = match elem_ty {
            WasmType::I32 => "__str_from_i32",
            WasmType::F64 => "__str_from_f64",
            _ => unreachable!(),
        };
        let to_str_idx = self
            .module_ctx
            .get_func(to_str_helper)
            .ok_or_else(|| {
                CompileError::codegen(format!(
                    "Array.join requires {to_str_helper} — ensure string runtime is registered"
                ))
            })?
            .0;
        let concat_idx = self
            .module_ctx
            .get_func("__str_concat")
            .ok_or_else(|| CompileError::codegen("Array.join requires __str_concat"))?
            .0;

        // sep: evaluate once
        let sep_local = self.alloc_local(WasmType::I32);
        if call.arguments.is_empty() {
            // Default separator "," — intern once
            let offset = self.module_ctx.alloc_static_string(",");
            self.push(Instruction::I32Const(offset as i32));
        } else {
            self.emit_expr(call.arguments[0].to_expression())?;
        }
        self.push(Instruction::LocalSet(sep_local));

        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(arr_local));
        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(len_local));

        // result = "" (empty interned string)
        let empty_off = self.module_ctx.alloc_static_string("");
        let result_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(empty_off as i32));
        self.push(Instruction::LocalSet(result_local));

        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        // If i > 0, prepend sep: result = concat(result, sep)
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32GtS);
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::LocalGet(result_local));
        self.push(Instruction::LocalGet(sep_local));
        self.push(Instruction::Call(concat_idx));
        self.push(Instruction::LocalSet(result_local));
        self.push(Instruction::End);

        // Load arr[i], stringify, concat
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Load(wasm_encoder::MemArg {
                offset: 0,
                align: 3,
                memory_index: 0,
            })),
            _ => self.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            })),
        }
        self.push(Instruction::Call(to_str_idx));
        // concat result with element string
        let elem_str = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalSet(elem_str));
        self.push(Instruction::LocalGet(result_local));
        self.push(Instruction::LocalGet(elem_str));
        self.push(Instruction::Call(concat_idx));
        self.push(Instruction::LocalSet(result_local));

        // i++
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End);
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(result_local));
        Ok(())
    }

    /// `Array.of<T>(...items)` — construct a new array containing the argument
    /// list in order. Element type is taken from the explicit `<T>` when given,
    /// otherwise inferred from the first argument (same rule as an array
    /// literal). The empty `Array.of()` without `<T>` is a type error.
    pub(crate) fn emit_array_of(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        // Resolve element type: explicit <T> wins, else infer from arg 0.
        let elem_ty = if let Some(type_args) = call.type_arguments.as_ref()
            && let Some(first) = type_args.params.first()
        {
            crate::types::resolve_ts_type_full(
                first,
                &self.module_ctx.class_names,
                self.type_bindings.as_ref(),
                Some(&self.module_ctx.non_i32_union_wasm_types),
            )?
        } else if let Some(first) = call.arguments.first() {
            let (ty, _) = self.infer_init_type(first.to_expression())?;
            ty
        } else {
            return Err(CompileError::type_err(
                "Array.of() requires at least one argument or an explicit type: Array.of<T>()",
            ));
        };
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => {
                return Err(CompileError::type_err(
                    "Array.of element type must be i32 or f64",
                ));
            }
        };

        let count = call.arguments.len() as i32;
        let total = ARRAY_HEADER_SIZE as i32 + count * esize;
        self.push(Instruction::I32Const(total));
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        // length = count
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(count));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        // capacity = count
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(count));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        for (i, arg) in call.arguments.iter().enumerate() {
            self.push(Instruction::LocalGet(ptr_local));
            let ty = self.emit_expr(arg.to_expression())?;
            if ty != elem_ty {
                if elem_ty == WasmType::F64 && ty == WasmType::I32 {
                    self.push(Instruction::F64ConvertI32S);
                } else {
                    return Err(CompileError::type_err(format!(
                        "Array.of argument {i} has type {ty:?}, expected {elem_ty:?}"
                    )));
                }
            }
            let offset = (ARRAY_HEADER_SIZE as i32 + (i as i32) * esize) as u64;
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
        Ok(())
    }

    /// `Array.from(src)` — shallow clone of an existing array (same shape as
    /// `src.slice()`). `Array.from(src, mapFn)` — same shape as `src.map(fn)`.
    /// `Array.from({length: n}, mapFn)` — sequence-generation form, recognized
    /// as a narrow object-literal pattern (exactly one `length` property); the
    /// map function is required so the element type can be inferred from its
    /// return, and each invocation sees `value = 0` since the typed subset has
    /// no `undefined`. When general object literals arrive, this recognizer
    /// still fires only on the exact shape — richer literals fall through to
    /// the array-source path and error appropriately.
    pub(crate) fn emit_array_from(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        if call.arguments.is_empty() || call.arguments.len() > 2 {
            return Err(CompileError::codegen(
                "Array.from expects 1 or 2 arguments: Array.from(src) or Array.from(src, mapFn)",
            ));
        }
        let src_expr = call.arguments[0].to_expression();

        if let Some(len_expr) = super::extract_length_only_object(src_expr) {
            let map_fn = call.arguments.get(1).map(|a| a.to_expression()).ok_or_else(|| {
                CompileError::codegen(
                    "Array.from({length: n}) requires a mapping function as the second argument — without it the element type can't be inferred in the typed subset",
                )
            })?;
            return self.emit_array_from_length(call, len_expr, map_fn);
        }

        let src_elem = self.resolve_expr_array_elem(src_expr).ok_or_else(|| {
            CompileError::type_err(
                "Array.from source must be an array or a `{length: n}` object literal",
            )
        })?;
        let src_class = self.resolve_expr_array_elem_class(src_expr);

        if call.arguments.len() == 1 {
            self.emit_array_from_copy(src_expr, src_elem)
        } else {
            let map_fn = call.arguments[1].to_expression();
            self.emit_array_from_map(src_expr, src_elem, src_class.as_deref(), map_fn)
        }
    }

    /// `Array.from({length: n}, mapFn)` — allocate an array of length n,
    /// invoke mapFn(0, i) for each i in [0, n), write results. The `value`
    /// argument is always 0 (the typed subset has no `undefined`); idiomatic
    /// code writes `(_, i) => …` and ignores it.
    fn emit_array_from_length(
        &mut self,
        call: &CallExpression<'a>,
        len_expr: &Expression<'a>,
        map_fn: &Expression<'a>,
    ) -> Result<(), CompileError> {
        use crate::codegen::array_builtins::{eval_arrow_body, extract_arrow};

        let arrow = extract_arrow(map_fn)?;
        let mut params: Vec<String> = Vec::new();
        for p in &arrow.params.items {
            match &p.pattern {
                BindingPattern::BindingIdentifier(id) => params.push(id.name.as_str().to_string()),
                _ => {
                    return Err(CompileError::unsupported(
                        "Array.from({length}, fn): mapFn parameter must be a simple identifier",
                    ));
                }
            }
        }
        if params.is_empty() || params.len() > 2 {
            return Err(CompileError::codegen(
                "Array.from({length}, fn): mapFn must take 1 or 2 parameters (value, index)",
            ));
        }

        // Element type resolution: explicit `Array.from<T>(...)` wins, else
        // infer from the arrow body with value-param defaulted to i32.
        let elem_ty = if let Some(type_args) = call.type_arguments.as_ref()
            && let Some(first) = type_args.params.first()
        {
            crate::types::resolve_ts_type_full(
                first,
                &self.module_ctx.class_names,
                self.type_bindings.as_ref(),
                Some(&self.module_ctx.non_i32_union_wasm_types),
            )?
        } else {
            self.infer_arrow_result_type(arrow, &params, WasmType::I32, None)?
        };
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => {
                return Err(CompileError::type_err(
                    "Array.from({length}, fn): element type must be i32 or f64",
                ));
            }
        };

        // Evaluate length into a local (i32).
        let len_local = self.alloc_local(WasmType::I32);
        let len_ty = self.emit_expr(len_expr)?;
        if len_ty == WasmType::F64 {
            self.push(Instruction::I32TruncSatF64S);
        }
        self.push(Instruction::LocalSet(len_local));

        // Allocate result: header + len * esize. Capacity = len.
        let arena_idx = self
            .module_ctx
            .arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

        let result_ptr = self.alloc_local(WasmType::I32);
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::LocalSet(result_ptr));
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Add);
        self.push(Instruction::GlobalSet(arena_idx));

        // length = len
        self.push(Instruction::LocalGet(result_ptr));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        // capacity = len
        self.push(Instruction::LocalGet(result_ptr));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // value_local = 0 (placeholder for undefined)
        let value_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(value_local));

        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

        // if i >= len, break
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        // Bind arrow params: value -> value_local (i32=0), index -> i_local
        let mut param_locals: Vec<(u32, WasmType)> = vec![(value_local, WasmType::I32)];
        let mut param_classes: Vec<Option<String>> = vec![None];
        if params.len() >= 2 {
            param_locals.push((i_local, WasmType::I32));
            param_classes.push(None);
        }
        let scope = crate::codegen::array_builtins::setup_arrow_scope(
            self,
            &params,
            &param_locals,
            &param_classes,
        );

        // Pre-compute destination address: result_ptr + HEADER + i*esize
        // so we can write the arrow's result without threading it through a
        // temp local. Interleaved with arrow evaluation: push addr first,
        // then push arrow body, then store.
        self.push(Instruction::LocalGet(result_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);

        let body_ty = eval_arrow_body(self, arrow)?;
        if body_ty != elem_ty {
            if elem_ty == WasmType::F64 && body_ty == WasmType::I32 {
                self.push(Instruction::F64ConvertI32S);
            } else {
                return Err(CompileError::type_err(format!(
                    "Array.from({{length}}, fn): mapFn returns {body_ty:?}, expected {elem_ty:?}"
                )));
            }
        }

        // Store element
        match elem_ty {
            WasmType::F64 => self.push(Instruction::F64Store(wasm_encoder::MemArg {
                offset: 0,
                align: 3,
                memory_index: 0,
            })),
            WasmType::I32 => self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            })),
            _ => unreachable!(),
        }

        crate::codegen::array_builtins::restore_arrow_scope(self, scope);

        // i++
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End); // loop
        self.push(Instruction::End); // block

        self.push(Instruction::LocalGet(result_ptr));
        Ok(())
    }

    /// Shallow clone of `src` via a single memory.copy of header + elements.
    fn emit_array_from_copy(
        &mut self,
        src_expr: &Expression<'a>,
        elem_ty: WasmType,
    ) -> Result<(), CompileError> {
        let esize: i32 = match elem_ty {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => return Err(CompileError::type_err("invalid array element type")),
        };
        let arena_idx = self
            .module_ctx
            .arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(src_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(src_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(len_local));

        // Allocate header + len * esize; capacity = len.
        let new_ptr = self.alloc_local(WasmType::I32);
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::LocalSet(new_ptr));
        self.push(Instruction::GlobalGet(arena_idx));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Add);
        self.push(Instruction::GlobalSet(arena_idx));

        // Write header: length=len, capacity=len.
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        }));

        // memory.copy(dst=new_ptr+HEADER, src=src_ptr+HEADER, n=len*esize)
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(src_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        self.push(Instruction::LocalGet(new_ptr));
        Ok(())
    }

    /// `Array.from(src, mapFn)` form — same shape as `src.map(mapFn)`, so we
    /// just delegate. The dispatcher already validated `src` is an array and
    /// resolved the element type.
    fn emit_array_from_map(
        &mut self,
        src_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        map_fn: &Expression<'a>,
    ) -> Result<(), CompileError> {
        self.emit_array_map(src_expr, elem_ty, elem_class, map_fn)?;
        Ok(())
    }
}
