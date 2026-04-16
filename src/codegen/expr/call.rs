use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

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

        // Check for String.fromCharCode(code)
        if let Expression::StaticMemberExpression(member) = &call.callee
            && let Expression::Identifier(obj) = &member.object
            && obj.name.as_str() == "String"
            && member.property.name.as_str() == "fromCharCode"
            && call.arguments.len() == 1
        {
            self.emit_expr(call.arguments[0].to_expression())?;
            let (func_idx, _) = self.module_ctx.get_func("__str_fromCharCode").unwrap();
            self.push(Instruction::Call(func_idx));
            return Ok(WasmType::I32);
        }

        // Check for string method calls (str.indexOf, str.slice, etc.)
        if let Some(result) = self.try_emit_string_method_call(call)? {
            return Ok(result);
        }

        // Check for array method calls (arr.push, etc.)
        if let Some(result) = self.try_emit_array_method_call(call)? {
            return Ok(result);
        }

        // Check for obj.method(args) calls
        if let Some(result) = self.try_emit_method_call(call)? {
            return Ok(result);
        }

        let callee_name = match &call.callee {
            Expression::Identifier(ident) => ident.name.as_str(),
            _ => return Err(CompileError::unsupported("non-identifier callee")),
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

        // Emit arguments
        for arg in &call.arguments {
            self.emit_expr(arg.to_expression())?;
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
            _ => Err(CompileError::unsupported(format!(
                "Array.{method_name} is not supported"
            ))),
        }
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
                self.push(Instruction::F64Const(0.5f64.into()));
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
