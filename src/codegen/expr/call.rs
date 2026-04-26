use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

/// Best-effort compile-time integer extraction for validating constant
/// arguments (e.g. `toString(16)`'s radix). Returns `None` when the expression
/// is non-literal or non-integer; callers should defer validation to the
/// runtime helper for those cases.
fn const_int_arg(expr: &Expression<'_>) -> Option<i64> {
    match expr {
        Expression::NumericLiteral(lit) if lit.value.fract() == 0.0 => Some(lit.value as i64),
        Expression::UnaryExpression(u) if matches!(u.operator, UnaryOperator::UnaryNegation) => {
            const_int_arg(&u.argument).map(|v| -v)
        }
        Expression::ParenthesizedExpression(p) => const_int_arg(&p.expression),
        _ => None,
    }
}

impl<'a> FuncContext<'a> {
    pub(crate) fn emit_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        // Handle super(args) — call parent constructor
        if matches!(&call.callee, Expression::Super(_)) {
            return self.emit_super_constructor_call(call);
        }

        // Handle super.method(args) — static dispatch to parent method
        if let Expression::StaticMemberExpression(member) = &call.callee
            && matches!(&member.object, Expression::Super(_))
        {
            return self.emit_super_method_call(member.property.name.as_str(), call);
        }

        // Check for Number.<static> calls
        if let Some(result) = self.try_emit_number_call(call)? {
            return Ok(result);
        }

        // Check for Array.<static> calls (Array.isArray)
        if let Some(result) = self.try_emit_array_static_call(call)? {
            return Ok(result);
        }

        // Check for Math.* member calls first
        if let Some(result) = self.try_emit_math_call(call)? {
            return Ok(result);
        }

        // Check for array builtin calls (filter, map, forEach, reduce, sort)
        if let Some(result) = self.try_emit_array_builtin(call)? {
            return Ok(result);
        }

        // Check for String.<static> calls (fromCharCode, fromCodePoint)
        if let Some(result) = self.try_emit_string_static_call(call)? {
            return Ok(result);
        }

        // Check for string method calls (str.indexOf, str.slice, etc.)
        if let Some(result) = self.try_emit_string_method_call(call)? {
            return Ok(result);
        }

        // Check for array method calls (arr.push, etc.)
        if let Some(result) = self.try_emit_array_method_call(call)? {
            return Ok(result);
        }

        // Check for Map<K, V> method calls (m.clear, m.get, ...). Must run
        // before the generic class-method dispatch — Map's layout is
        // synthesized and has no registered methods to fall through to.
        if let Some(result) = self.try_emit_map_method_call(call)? {
            return Ok(result);
        }

        // Same story for Set<T> — synthesized layout, no registered methods.
        if let Some(result) = self.try_emit_set_method_call(call)? {
            return Ok(result);
        }

        // Check for number instance method calls (x.toString(), x.toFixed())
        if let Some(result) = self.try_emit_number_instance_call(call)? {
            return Ok(result);
        }

        // Check for obj.method(args) calls
        if let Some(result) = self.try_emit_method_call(call)? {
            return Ok(result);
        }

        let callee_name_raw = match &call.callee {
            Expression::Identifier(ident) => ident.name.as_str(),
            _ => return Err(CompileError::unsupported("non-identifier callee")),
        };

        // Generic function instantiation. Two cases:
        //   - explicit args (`identity<i32>(x)`) — mangle per the type args.
        //   - inferred args (`identity(x)`) — look up the pre-computed mangled
        //     name the pre-codegen collector stashed per call-site span.
        let mangled_call;
        let callee_name: &str = if let Some(type_args) = call.type_arguments.as_ref() {
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
            mangled_call = format!("{callee_name_raw}${}", tokens.join("$"));
            if self.module_ctx.get_func(&mangled_call).is_some() {
                &mangled_call
            } else {
                callee_name_raw
            }
        } else if let Some(stashed) = self.module_ctx.inferred_fn_calls.get(&call.span.start)
            && self.module_ctx.get_func(stashed).is_some()
        {
            mangled_call = stashed.clone();
            &mangled_call
        } else {
            callee_name_raw
        };

        // Type cast: f64(x) -> f64.convert_i32_s, i32(x) -> i32.trunc_f64_s
        if callee_name == "f64" && call.arguments.len() == 1 {
            let arg_ty = self.emit_expr(call.arguments[0].to_expression())?;
            if arg_ty == WasmType::I32 {
                self.push(Instruction::F64ConvertI32S);
            }
            return Ok(WasmType::F64);
        }
        if callee_name == "i32" && call.arguments.len() == 1 {
            let arg_ty = self.emit_expr(call.arguments[0].to_expression())?;
            if arg_ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
            return Ok(WasmType::I32);
        }

        // Global isNaN(x): NaN !== NaN, so `x != x` (F64Ne with self).
        // Only f64 can be NaN in our model; i32 is always "not NaN".
        if callee_name == "isNaN" && call.arguments.len() == 1 {
            let ty = self.emit_expr(call.arguments[0].to_expression())?;
            match ty {
                WasmType::F64 => {
                    let tmp = self.alloc_local(WasmType::F64);
                    self.push(Instruction::LocalTee(tmp));
                    self.push(Instruction::LocalGet(tmp));
                    self.push(Instruction::F64Ne);
                }
                WasmType::I32 => {
                    self.push(Instruction::Drop);
                    self.push(Instruction::I32Const(0));
                }
                _ => return Err(CompileError::type_err("isNaN requires a numeric argument")),
            }
            return Ok(WasmType::I32);
        }
        // Global isFinite(x): (x - x) == 0 iff x is finite. finite-finite=0,
        // NaN-NaN=NaN, ±Inf-±Inf=NaN. F64Eq with 0.0 yields 1 for finite, 0
        // otherwise. i32 values are always finite.
        if callee_name == "isFinite" && call.arguments.len() == 1 {
            let ty = self.emit_expr(call.arguments[0].to_expression())?;
            match ty {
                WasmType::F64 => {
                    let tmp = self.alloc_local(WasmType::F64);
                    self.push(Instruction::LocalTee(tmp));
                    self.push(Instruction::LocalGet(tmp));
                    self.push(Instruction::F64Sub);
                    self.push(Instruction::F64Const(0.0));
                    self.push(Instruction::F64Eq);
                }
                WasmType::I32 => {
                    self.push(Instruction::Drop);
                    self.push(Instruction::I32Const(1));
                }
                _ => {
                    return Err(CompileError::type_err(
                        "isFinite requires a numeric argument",
                    ));
                }
            }
            return Ok(WasmType::I32);
        }

        // parseInt(s) -> i32
        if callee_name == "parseInt" && call.arguments.len() == 1 {
            self.emit_expr(call.arguments[0].to_expression())?;
            let (func_idx, _) = self.module_ctx.get_func("__str_parseInt").unwrap();
            self.push(Instruction::Call(func_idx));
            return Ok(WasmType::I32);
        }
        // parseFloat(s) -> f64
        if callee_name == "parseFloat" && call.arguments.len() == 1 {
            self.emit_expr(call.arguments[0].to_expression())?;
            let (func_idx, _) = self.module_ctx.get_func("__str_parseFloat").unwrap();
            self.push(Instruction::Call(func_idx));
            return Ok(WasmType::F64);
        }

        // Memory intrinsics: load_f64(offset), load_i32(offset), store_f64(offset, val), store_i32(offset, val)
        if let Some(result) = self.try_emit_memory_intrinsic(callee_name, call)? {
            return Ok(result);
        }

        // Static data allocation: __static_alloc(size) -> i32 offset (compile-time constant)
        if callee_name == "__static_alloc" && call.arguments.len() == 1 {
            return self.emit_static_alloc(call);
        }

        // Check if callee is a closure variable
        if let Some(sig) = self.local_closure_sigs.get(callee_name).cloned() {
            return self.emit_closure_call(callee_name, &sig, call);
        }

        // Look up function
        let (func_idx, ret_ty) = self.module_ctx.get_func(callee_name).ok_or_else(|| {
            self.locate(
                CompileError::codegen(format!("undefined function '{callee_name}'")),
                call.span.start,
            )
        })?;

        // Emit arguments, threading callee parameter class names into
        // ObjectExpression and tuple-typed ArrayExpression arguments.
        let param_classes = self.module_ctx.fn_param_classes.get(callee_name).cloned();
        for (i, arg) in call.arguments.iter().enumerate() {
            let expr = arg.to_expression();
            let expected = param_classes
                .as_ref()
                .and_then(|pc| pc.get(i))
                .and_then(|c| c.as_deref());
            match (expr, expected) {
                (Expression::ObjectExpression(obj), Some(expected_name)) => {
                    self.emit_object_literal(obj, Some(expected_name))?;
                }
                (Expression::ArrayExpression(arr), Some(expected_name))
                    if self.is_tuple_shape(expected_name) =>
                {
                    self.emit_tuple_literal(arr, expected_name)?;
                }
                _ => {
                    self.emit_expr_coerced(expr, expected)?;
                }
            }
        }

        self.push(Instruction::Call(func_idx));
        Ok(ret_ty)
    }

    pub(crate) fn try_emit_memory_intrinsic(
        &mut self,
        name: &str,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        match name {
            "load_f64" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen(
                        "load_f64 expects 1 argument (offset)",
                    ));
                }
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 3, // 2^3 = 8 byte alignment
                    memory_index: 0,
                }));
                Ok(Some(WasmType::F64))
            }
            "load_i32" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen(
                        "load_i32 expects 1 argument (offset)",
                    ));
                }
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2, // 2^2 = 4 byte alignment
                    memory_index: 0,
                }));
                Ok(Some(WasmType::I32))
            }
            "store_f64" => {
                if call.arguments.len() != 2 {
                    return Err(CompileError::codegen(
                        "store_f64 expects 2 arguments (offset, value)",
                    ));
                }
                self.emit_expr(call.arguments[0].to_expression())?;
                self.emit_expr(call.arguments[1].to_expression())?;
                self.push(Instruction::F64Store(wasm_encoder::MemArg {
                    offset: 0,
                    align: 3,
                    memory_index: 0,
                }));
                Ok(Some(WasmType::Void))
            }
            "store_i32" => {
                if call.arguments.len() != 2 {
                    return Err(CompileError::codegen(
                        "store_i32 expects 2 arguments (offset, value)",
                    ));
                }
                self.emit_expr(call.arguments[0].to_expression())?;
                self.emit_expr(call.arguments[1].to_expression())?;
                self.push(Instruction::I32Store(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                Ok(Some(WasmType::Void))
            }
            _ => Ok(None),
        }
    }

    /// Dispatch Number.<static> calls. Returns Some if recognized, None otherwise.
    ///
    /// Semantics per ECMA-262 §21.1.2:
    /// - `Number.isNaN(x)`: true only when x is the NaN value. Our f64 inputs
    ///   compare via `x != x`; i32 inputs are never NaN.
    /// - `Number.isFinite(x)`: true when x is finite. Unlike global `isFinite`,
    ///   it does not coerce — but in our typed world the inputs are already
    ///   numeric, so it behaves the same.
    /// - `Number.isInteger(x)`: finite AND `x == floor(x)`. i32 values are
    ///   always integers.
    /// - `Number.isSafeInteger(x)`: integer AND |x| ≤ 2^53 − 1.
    // Compile-time Array.isArray: typed subset resolves this statically since
    // arrays are arena pointers with no runtime tag.
    pub(crate) fn try_emit_array_static_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        let (obj_name, method_name) = match &call.callee {
            Expression::StaticMemberExpression(member) => {
                let obj = match &member.object {
                    Expression::Identifier(ident) => ident.name.as_str(),
                    _ => return Ok(None),
                };
                (obj, member.property.name.as_str())
            }
            _ => return Ok(None),
        };
        if obj_name != "Array" {
            return Ok(None);
        }
        match method_name {
            "isArray" => {
                self.expect_args(call, 1, "Array.isArray")?;
                let is_arr = self.expr_is_array(call.arguments[0].to_expression());
                self.push(Instruction::I32Const(if is_arr { 1 } else { 0 }));
                Ok(Some(WasmType::I32))
            }
            "of" => {
                self.emit_array_of(call)?;
                Ok(Some(WasmType::I32))
            }
            "from" => {
                self.emit_array_from(call)?;
                Ok(Some(WasmType::I32))
            }
            _ => Err(CompileError::unsupported(format!(
                "Array.{method_name} is not supported"
            ))),
        }
    }

    /// Dispatch `String.<static>(...)` calls. Returns `Some` if recognized.
    ///
    /// - `String.fromCharCode(...codes)` — variadic; each code is stored as a
    ///   single byte (low 8 bits). tscc strings are UTF-8 byte sequences, so
    ///   codes above 0xFF are truncated — matching existing behavior for
    ///   1-arg calls, just lifted to N args.
    /// - `String.fromCodePoint(...cps)` — variadic; each code point is
    ///   UTF-8-encoded (1-4 bytes) via `__utf8_encode_cp`. Allocates the
    ///   worst-case `N*4 + 4` bytes and rewinds the arena by the unused tail
    ///   after encoding — single allocation, no waste.
    pub(crate) fn try_emit_string_static_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        let (obj_name, method_name) = match &call.callee {
            Expression::StaticMemberExpression(member) => {
                let obj = match &member.object {
                    Expression::Identifier(ident) => ident.name.as_str(),
                    _ => return Ok(None),
                };
                (obj, member.property.name.as_str())
            }
            _ => return Ok(None),
        };
        if obj_name != "String" {
            return Ok(None);
        }
        match method_name {
            "fromCharCode" => {
                self.emit_string_from_char_code(call)?;
                Ok(Some(WasmType::I32))
            }
            "fromCodePoint" => {
                self.emit_string_from_code_point(call)?;
                Ok(Some(WasmType::I32))
            }
            _ => Ok(None),
        }
    }

    /// Emit `String.fromCharCode(...codes)` inline — allocate 4+N bytes,
    /// write length=N, then store each code as a single byte. Empty-arg form
    /// returns the deduplicated empty-string static.
    fn emit_string_from_char_code(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        let n = call.arguments.len() as i32;
        if n == 0 {
            let offset = self.module_ctx.alloc_static_string("");
            self.push(Instruction::I32Const(offset as i32));
            return Ok(());
        }

        let total = 4 + n;
        self.push(Instruction::I32Const(total));
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        // length header
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(n));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));

        // bytes
        for (i, arg) in call.arguments.iter().enumerate() {
            self.push(Instruction::LocalGet(ptr_local));
            self.emit_expr(arg.to_expression())?;
            self.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: (4 + i as u64),
                align: 0,
                memory_index: 0,
            }));
        }

        self.push(Instruction::LocalGet(ptr_local));
        Ok(())
    }

    /// Emit `String.fromCodePoint(...cps)` inline — allocate `4 + N*4` bytes
    /// (worst case), encode each code point via `__utf8_encode_cp`, then
    /// write the length header and rewind the arena to the actual end. The
    /// helper traps on code points outside [0, 0x10FFFF].
    fn emit_string_from_code_point(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        let n = call.arguments.len() as i32;
        if n == 0 {
            let offset = self.module_ctx.alloc_static_string("");
            self.push(Instruction::I32Const(offset as i32));
            return Ok(());
        }

        // Evaluate each argument once into an i32 local to preserve JS
        // left-to-right evaluation order before any allocation / encoding
        // side effects.
        let mut arg_locals = Vec::with_capacity(call.arguments.len());
        for arg in &call.arguments {
            let local = self.alloc_local(WasmType::I32);
            self.emit_expr(arg.to_expression())?;
            self.push(Instruction::LocalSet(local));
            arg_locals.push(local);
        }

        let arena_idx = self
            .module_ctx
            .arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;

        // Worst-case alloc: 4 (header) + N * 4 bytes.
        self.push(Instruction::I32Const(4 + n * 4));
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        // cursor = ptr + 4
        let cursor = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(4));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(cursor));

        let (encode_idx, _) = self.module_ctx.get_func("__utf8_encode_cp").unwrap();
        for cp_local in &arg_locals {
            // cursor += __utf8_encode_cp(cursor, cp)
            self.push(Instruction::LocalGet(cursor));
            self.push(Instruction::LocalGet(cursor));
            self.push(Instruction::LocalGet(*cp_local));
            self.push(Instruction::Call(encode_idx));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(cursor));
        }

        // length header = cursor - (ptr + 4)
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::LocalGet(cursor));
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Sub);
        self.push(Instruction::I32Const(4));
        self.push(Instruction::I32Sub);
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));

        // Rewind arena to the end of the actually-encoded bytes. Safe because
        // we control the entire sequence — nothing else allocated in between.
        self.push(Instruction::LocalGet(cursor));
        self.push(Instruction::GlobalSet(arena_idx));

        self.push(Instruction::LocalGet(ptr_local));
        Ok(())
    }

    pub(crate) fn expr_is_array(&self, expr: &Expression<'a>) -> bool {
        match expr {
            Expression::ArrayExpression(_) => true,
            Expression::Identifier(ident) => self
                .local_array_elem_types
                .contains_key(ident.name.as_str()),
            Expression::NewExpression(new) => {
                if let Expression::Identifier(id) = &new.callee {
                    id.name.as_str() == "Array"
                } else {
                    false
                }
            }
            Expression::ParenthesizedExpression(paren) => self.expr_is_array(&paren.expression),
            _ => false,
        }
    }

    pub(crate) fn try_emit_number_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        let (obj_name, method_name) = match &call.callee {
            Expression::StaticMemberExpression(member) => {
                let obj = match &member.object {
                    Expression::Identifier(ident) => ident.name.as_str(),
                    _ => return Ok(None),
                };
                (obj, member.property.name.as_str())
            }
            _ => return Ok(None),
        };
        if obj_name != "Number" {
            return Ok(None);
        }

        self.expect_args(call, 1, &format!("Number.{method_name}"))?;
        let ty = self.emit_expr(call.arguments[0].to_expression())?;

        match method_name {
            "isNaN" => {
                match ty {
                    WasmType::F64 => {
                        let tmp = self.alloc_local(WasmType::F64);
                        self.push(Instruction::LocalTee(tmp));
                        self.push(Instruction::LocalGet(tmp));
                        self.push(Instruction::F64Ne);
                    }
                    WasmType::I32 => {
                        self.push(Instruction::Drop);
                        self.push(Instruction::I32Const(0));
                    }
                    _ => {
                        return Err(CompileError::type_err(
                            "Number.isNaN requires a numeric argument",
                        ));
                    }
                }
                Ok(Some(WasmType::I32))
            }
            "isFinite" => {
                match ty {
                    WasmType::F64 => {
                        let tmp = self.alloc_local(WasmType::F64);
                        self.push(Instruction::LocalTee(tmp));
                        self.push(Instruction::LocalGet(tmp));
                        self.push(Instruction::F64Sub);
                        self.push(Instruction::F64Const(0.0));
                        self.push(Instruction::F64Eq);
                    }
                    WasmType::I32 => {
                        self.push(Instruction::Drop);
                        self.push(Instruction::I32Const(1));
                    }
                    _ => {
                        return Err(CompileError::type_err(
                            "Number.isFinite requires a numeric argument",
                        ));
                    }
                }
                Ok(Some(WasmType::I32))
            }
            "isInteger" => {
                match ty {
                    WasmType::I32 => {
                        self.push(Instruction::Drop);
                        self.push(Instruction::I32Const(1));
                    }
                    WasmType::F64 => {
                        // finite(x) && x == floor(x)
                        // Compute (x - x == 0) && (x == trunc(x)). Use trunc so
                        // ±Inf maps to ±Inf (not itself after floor for Inf),
                        // and the finite guard is a separate check.
                        let x = self.alloc_local(WasmType::F64);
                        self.push(Instruction::LocalSet(x));
                        // finite check
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::F64Sub);
                        self.push(Instruction::F64Const(0.0));
                        self.push(Instruction::F64Eq);
                        // integer check: x == trunc(x)
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::F64Trunc);
                        self.push(Instruction::F64Eq);
                        self.push(Instruction::I32And);
                    }
                    _ => {
                        return Err(CompileError::type_err(
                            "Number.isInteger requires a numeric argument",
                        ));
                    }
                }
                Ok(Some(WasmType::I32))
            }
            "isSafeInteger" => {
                match ty {
                    WasmType::I32 => {
                        // All i32 fit in ±(2^53 − 1), so just `true`.
                        self.push(Instruction::Drop);
                        self.push(Instruction::I32Const(1));
                    }
                    WasmType::F64 => {
                        let x = self.alloc_local(WasmType::F64);
                        self.push(Instruction::LocalSet(x));
                        // finite(x)
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::F64Sub);
                        self.push(Instruction::F64Const(0.0));
                        self.push(Instruction::F64Eq);
                        // x == trunc(x)
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::F64Trunc);
                        self.push(Instruction::F64Eq);
                        self.push(Instruction::I32And);
                        // abs(x) <= 2^53 − 1
                        self.push(Instruction::LocalGet(x));
                        self.push(Instruction::F64Abs);
                        self.push(Instruction::F64Const(9_007_199_254_740_991.0));
                        self.push(Instruction::F64Le);
                        self.push(Instruction::I32And);
                    }
                    _ => {
                        return Err(CompileError::type_err(
                            "Number.isSafeInteger requires a numeric argument",
                        ));
                    }
                }
                Ok(Some(WasmType::I32))
            }
            // Number.parseInt / Number.parseFloat — ES6 aliases for the global
            // parseInt/parseFloat. The arg is a string (i32 pointer); reuse the
            // existing __str_parseInt / __str_parseFloat helpers.
            "parseInt" | "parseFloat" => {
                if ty != WasmType::I32 {
                    return Err(CompileError::type_err(format!(
                        "Number.{method_name} requires a string argument"
                    )));
                }
                let helper = if method_name == "parseInt" {
                    "__str_parseInt"
                } else {
                    "__str_parseFloat"
                };
                let (func_idx, _) = self.module_ctx.get_func(helper).ok_or_else(|| {
                    CompileError::codegen(format!("{helper} helper not registered"))
                })?;
                self.push(Instruction::Call(func_idx));
                let ret = if method_name == "parseInt" {
                    WasmType::I32
                } else {
                    WasmType::F64
                };
                Ok(Some(ret))
            }
            _ => Err(CompileError::codegen(format!(
                "Number.{method_name} is not a supported builtin"
            ))),
        }
    }

    /// Number instance method calls: x.toString(), x.toFixed(digits).
    /// Returns Some if recognized as a number method, None otherwise.
    pub(crate) fn try_emit_number_instance_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        let member = match &call.callee {
            Expression::StaticMemberExpression(m) => m,
            _ => return Ok(None),
        };

        let method = member.property.name.as_str();
        if !matches!(
            method,
            "toString" | "toFixed" | "toPrecision" | "toExponential"
        ) {
            return Ok(None);
        }

        // Don't intercept string/array/class method calls
        if self.resolve_expr_is_string(&member.object) {
            return Ok(None);
        }
        if self.resolve_expr_array_elem(&member.object).is_some() {
            return Ok(None);
        }
        if self.resolve_expr_class(&member.object).is_ok() {
            return Ok(None);
        }

        match method {
            "toString" => {
                if call.arguments.len() > 1 {
                    return Err(CompileError::codegen(
                        "Number.prototype.toString() takes 0 or 1 arguments (radix)",
                    ));
                }
                // Literal-radix validation: compile-time error for out-of-range
                // constants so users catch typos (e.g. `.toString(1)`) without
                // a runtime silent-fallback. Non-literal radices skip this and
                // rely on the helper's runtime check.
                if let Some(arg0) = call.arguments.first()
                    && let Some(r) = const_int_arg(arg0.to_expression())
                    && !(2..=36).contains(&r)
                {
                    return Err(CompileError::codegen(format!(
                        "toString() radix must be between 2 and 36, got {r}"
                    )));
                }
                let ty = self.emit_expr(&member.object)?;
                if call.arguments.is_empty() {
                    match ty {
                        WasmType::I32 => {
                            let (func_idx, _) =
                                self.module_ctx.get_func("__str_from_i32").unwrap();
                            self.push(Instruction::Call(func_idx));
                        }
                        WasmType::F64 => {
                            let (func_idx, _) =
                                self.module_ctx.get_func("__str_from_f64").unwrap();
                            self.push(Instruction::Call(func_idx));
                        }
                        _ => {
                            return Err(CompileError::type_err(
                                "toString() requires a numeric receiver",
                            ));
                        }
                    }
                } else {
                    // Widen i32 receivers to f64 so a single helper handles
                    // both — i32 → f64 is lossless and saves authoring a
                    // separate `__str_from_i32_radix`.
                    match ty {
                        WasmType::I32 => self.push(Instruction::F64ConvertI32S),
                        WasmType::F64 => {}
                        _ => {
                            return Err(CompileError::type_err(
                                "toString() requires a numeric receiver",
                            ));
                        }
                    }
                    let radix_ty =
                        self.emit_expr(call.arguments[0].to_expression())?;
                    if radix_ty == WasmType::F64 {
                        self.push(Instruction::I32TruncSatF64S);
                    } else if radix_ty != WasmType::I32 {
                        return Err(CompileError::type_err(
                            "toString() radix must be a number",
                        ));
                    }
                    let (func_idx, _) = self
                        .module_ctx
                        .get_func("__str_from_f64_radix")
                        .ok_or_else(|| {
                            CompileError::codegen("__str_from_f64_radix not registered")
                        })?;
                    self.push(Instruction::Call(func_idx));
                }
                Ok(Some(WasmType::I32))
            }
            "toFixed" => {
                // ES § 21.1.3.3: fractionDigits defaults to 0.
                if call.arguments.len() > 1 {
                    return Err(CompileError::codegen(
                        "toFixed() expects 0 or 1 arguments (digits)",
                    ));
                }
                let ty = self.emit_expr(&member.object)?;
                if ty == WasmType::I32 {
                    self.push(Instruction::F64ConvertI32S);
                }
                if call.arguments.is_empty() {
                    self.push(Instruction::I32Const(0));
                } else {
                    self.emit_expr(call.arguments[0].to_expression())?;
                }
                let (func_idx, _) = self
                    .module_ctx
                    .get_func("__str_toFixed")
                    .ok_or_else(|| CompileError::codegen("__str_toFixed not registered"))?;
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            "toPrecision" => {
                // ES § 21.1.3.5: if precision is undefined, return ToString(x).
                // We route the no-arg form straight to __str_from_f64.
                if call.arguments.len() > 1 {
                    return Err(CompileError::codegen(
                        "toPrecision() expects 0 or 1 arguments (precision)",
                    ));
                }
                let ty = self.emit_expr(&member.object)?;
                if ty == WasmType::I32 {
                    self.push(Instruction::F64ConvertI32S);
                }
                if call.arguments.is_empty() {
                    let (func_idx, _) = self
                        .module_ctx
                        .get_func("__str_from_f64")
                        .ok_or_else(|| {
                            CompileError::codegen("__str_from_f64 not registered")
                        })?;
                    self.push(Instruction::Call(func_idx));
                } else {
                    self.emit_expr(call.arguments[0].to_expression())?;
                    let (func_idx, _) = self
                        .module_ctx
                        .get_func("__str_toPrecision")
                        .ok_or_else(|| {
                            CompileError::codegen("__str_toPrecision not registered")
                        })?;
                    self.push(Instruction::Call(func_idx));
                }
                Ok(Some(WasmType::I32))
            }
            "toExponential" => {
                // ES § 21.1.3.4: if fractionDigits is undefined, pick the shortest
                // round-trippable mantissa. We signal that to the helper by
                // passing a negative sentinel for `digits`.
                if call.arguments.len() > 1 {
                    return Err(CompileError::codegen(
                        "toExponential() expects 0 or 1 arguments (digits)",
                    ));
                }
                let ty = self.emit_expr(&member.object)?;
                if ty == WasmType::I32 {
                    self.push(Instruction::F64ConvertI32S);
                }
                if call.arguments.is_empty() {
                    self.push(Instruction::I32Const(-1));
                } else {
                    self.emit_expr(call.arguments[0].to_expression())?;
                }
                let (func_idx, _) = self
                    .module_ctx
                    .get_func("__str_toExponential")
                    .ok_or_else(|| CompileError::codegen("__str_toExponential not registered"))?;
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::I32))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn try_emit_math_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        let (obj_name, method_name) = match &call.callee {
            Expression::StaticMemberExpression(member) => {
                let obj = match &member.object {
                    Expression::Identifier(ident) => ident.name.as_str(),
                    _ => return Ok(None),
                };
                (obj, member.property.name.as_str())
            }
            _ => return Ok(None),
        };

        if obj_name != "Math" {
            return Ok(None);
        }

        match method_name {
            // Single-argument math functions -> WASM f64 instructions
            "sqrt" => {
                self.expect_args(call, 1, "Math.sqrt")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Sqrt);
                Ok(Some(WasmType::F64))
            }
            "abs" => {
                self.expect_args(call, 1, "Math.abs")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Abs);
                Ok(Some(WasmType::F64))
            }
            "ceil" => {
                self.expect_args(call, 1, "Math.ceil")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Ceil);
                Ok(Some(WasmType::F64))
            }
            "floor" => {
                self.expect_args(call, 1, "Math.floor")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Floor);
                Ok(Some(WasmType::F64))
            }
            "trunc" => {
                self.expect_args(call, 1, "Math.trunc")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Trunc);
                Ok(Some(WasmType::F64))
            }
            "nearest" => {
                self.expect_args(call, 1, "Math.nearest")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Nearest);
                Ok(Some(WasmType::F64))
            }
            // Math.round(x) rounds half toward +Infinity per JS spec.
            // Lowered to floor(x + 0.5) — does NOT use F64Nearest (half-to-even).
            "round" => {
                self.expect_args(call, 1, "Math.round")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F64Const(0.5f64));
                self.push(Instruction::F64Add);
                self.push(Instruction::F64Floor);
                Ok(Some(WasmType::F64))
            }
            // Two-argument math functions -> WASM f64 instructions
            "min" => {
                self.expect_args(call, 2, "Math.min")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.emit_expr(call.arguments[1].to_expression())?;
                self.push(Instruction::F64Min);
                Ok(Some(WasmType::F64))
            }
            "max" => {
                self.expect_args(call, 2, "Math.max")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.emit_expr(call.arguments[1].to_expression())?;
                self.push(Instruction::F64Max);
                Ok(Some(WasmType::F64))
            }
            // copysign
            "copysign" => {
                self.expect_args(call, 2, "Math.copysign")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.emit_expr(call.arguments[1].to_expression())?;
                self.push(Instruction::F64Copysign);
                Ok(Some(WasmType::F64))
            }
            // Math.sign(x): preserves ±0 and NaN; returns ±1 otherwise.
            // Evaluates x once via a local to stay correct with side-effecting args.
            "sign" => {
                self.expect_args(call, 1, "Math.sign")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                let l = self.alloc_local(WasmType::F64);
                self.push(Instruction::LocalSet(l));
                // val1 = x (used when x is ±0 or NaN)
                self.push(Instruction::LocalGet(l));
                // val2 = copysign(1.0, x)
                self.push(Instruction::F64Const(1.0));
                self.push(Instruction::LocalGet(l));
                self.push(Instruction::F64Copysign);
                // cond = (x == 0.0) | (x != x)
                self.push(Instruction::LocalGet(l));
                self.push(Instruction::F64Const(0.0));
                self.push(Instruction::F64Eq);
                self.push(Instruction::LocalGet(l));
                self.push(Instruction::LocalGet(l));
                self.push(Instruction::F64Ne);
                self.push(Instruction::I32Or);
                self.push(Instruction::Select);
                Ok(Some(WasmType::F64))
            }
            // Math.hypot(x, y): naive sqrt(x*x + y*y).
            // Note: overflows when x*x or y*y exceed f64 range. Adequate for
            // typical game-space coordinates; if you need libm's scaled
            // algorithm, call __tscc_hypot directly under --math=libm.
            "hypot" => {
                self.expect_args(call, 2, "Math.hypot")?;
                let lx = self.alloc_local(WasmType::F64);
                let ly = self.alloc_local(WasmType::F64);
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::LocalSet(lx));
                self.emit_expr(call.arguments[1].to_expression())?;
                self.push(Instruction::LocalSet(ly));
                self.push(Instruction::LocalGet(lx));
                self.push(Instruction::LocalGet(lx));
                self.push(Instruction::F64Mul);
                self.push(Instruction::LocalGet(ly));
                self.push(Instruction::LocalGet(ly));
                self.push(Instruction::F64Mul);
                self.push(Instruction::F64Add);
                self.push(Instruction::F64Sqrt);
                Ok(Some(WasmType::F64))
            }
            // Math.fround(x): round x to the nearest 32-bit float. Emitted as
            // an f64→f32→f64 round-trip, which is bit-exact to the ECMA-262
            // definition of fround on any platform (IEEE-754 round-nearest).
            "fround" => {
                self.expect_args(call, 1, "Math.fround")?;
                self.emit_expr(call.arguments[0].to_expression())?;
                self.push(Instruction::F32DemoteF64);
                self.push(Instruction::F64PromoteF32);
                Ok(Some(WasmType::F64))
            }
            // Math.clz32(x): number of leading zero bits in the 32-bit int
            // representation of x. ECMA coerces x via ToUint32; we require an
            // i32 input in our typed world and emit `i32.clz` directly.
            "clz32" => {
                self.expect_args(call, 1, "Math.clz32")?;
                let ty = self.emit_expr(call.arguments[0].to_expression())?;
                match ty {
                    WasmType::I32 => {}
                    WasmType::F64 => self.push(Instruction::I32TruncSatF64S),
                    _ => {
                        return Err(CompileError::type_err(
                            "Math.clz32 requires a numeric argument",
                        ));
                    }
                }
                self.push(Instruction::I32Clz);
                Ok(Some(WasmType::I32))
            }
            // Math.imul(a, b): C-style 32-bit multiply. Operands are truncated
            // to i32 (if f64), then multiplied with wraparound (i32.mul).
            "imul" => {
                self.expect_args(call, 2, "Math.imul")?;
                for arg in &call.arguments {
                    let ty = self.emit_expr(arg.to_expression())?;
                    match ty {
                        WasmType::I32 => {}
                        WasmType::F64 => self.push(Instruction::I32TruncSatF64S),
                        _ => {
                            return Err(CompileError::type_err(
                                "Math.imul requires numeric arguments",
                            ));
                        }
                    }
                }
                self.push(Instruction::I32Mul);
                Ok(Some(WasmType::I32))
            }
            // Math.random() — call the lazily-emitted PCG32 step function.
            // Embedder controls the seed via the exported `__rng_state` global.
            "random" => {
                self.expect_args(call, 0, "Math.random")?;
                let (func_idx, _) = self
                    .module_ctx
                    .get_func(crate::codegen::math_builtins::RNG_NEXT_FUNC)
                    .expect("__rng_next not registered — scanner missed Math.random()");
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::F64))
            }
            // Transcendentals (sin, cos, log, exp, pow, ...): lower to host
            // imports declared in Pass 1a (see codegen/module.rs). The scanner
            // ensures only referenced ones are imported, so get_func will
            // succeed here for any transcendental that reached codegen.
            other if crate::codegen::math_builtins::is_transcendental(other) => {
                let arity = crate::codegen::math_builtins::MATH_TRANSCENDENTALS
                    .iter()
                    .find(|(n, _)| *n == other)
                    .map(|(_, a)| *a as usize)
                    .unwrap();
                self.expect_args(call, arity, &format!("Math.{other}"))?;
                for arg in &call.arguments {
                    self.emit_expr(arg.to_expression())?;
                }
                let import_name = crate::codegen::math_builtins::import_name(other);
                let (func_idx, _) = self.module_ctx.get_func(&import_name).unwrap();
                self.push(Instruction::Call(func_idx));
                Ok(Some(WasmType::F64))
            }
            _ => Err(CompileError::unsupported(format!(
                "Math.{method_name} is not a supported builtin"
            ))),
        }
    }

    pub(crate) fn expect_args(
        &self,
        call: &CallExpression,
        expected: usize,
        name: &str,
    ) -> Result<(), CompileError> {
        if call.arguments.len() != expected {
            return Err(CompileError::codegen(format!(
                "{name} expects {expected} argument(s), got {}",
                call.arguments.len()
            )));
        }
        Ok(())
    }

    pub(crate) fn emit_static_alloc(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        // __static_alloc(size) -> i32 constant (compile-time offset)
        let size = match &call.arguments[0].to_expression() {
            Expression::NumericLiteral(lit) => lit.value as u32,
            _ => {
                return Err(CompileError::codegen(
                    "__static_alloc size must be a numeric literal",
                ));
            }
        };
        let offset = self.module_ctx.alloc_static(size);
        self.push(Instruction::I32Const(offset as i32));
        Ok(WasmType::I32)
    }
}
