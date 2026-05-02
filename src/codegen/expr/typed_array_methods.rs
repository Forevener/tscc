//! Typed-array instance methods — sub-phase 3 of the typed-arrays plan.
//!
//! Covers the immutable + mutable surface (no HOFs, those land in sub-phase 4):
//! `at`, `indexOf`, `lastIndexOf`, `includes`, `slice`, `subarray`, `fill`,
//! `set`, `reverse`, `sort` (numeric default), `copyWithin`, `join`.
//!
//! Every method follows the same shape:
//!   1. Evaluate args (in JS arg-order, before mutation).
//!   2. Evaluate the receiver into `arr_local`.
//!   3. Load `len` and `buf_ptr` once into locals.
//!   4. Element access uses `buf_ptr + i * stride` with the descriptor's
//!      load_op / store_op — sub-phase 5's `Uint8Array` lands automatically
//!      since the descriptor already encodes its byte stride and load form.
//!
//! `slice` returns a self-owned copy (mutation isolated). `subarray` returns
//! a view: 8-byte header alloc, body pointed at the receiver's body. The view
//! distinction is the load-bearing semantic difference between the two and
//! is covered by tests in `tests/it/typed_arrays.rs`.

use oxc_ast::ast::*;
use wasm_encoder::{BlockType, Instruction, MemArg};

use crate::codegen::func::FuncContext;
use crate::codegen::typed_arrays::{
    TYPED_ARRAY_BUF_PTR_OFFSET, TYPED_ARRAY_HEADER_SIZE, TYPED_ARRAY_LEN_OFFSET,
    TypedArrayDescriptor,
};
use crate::error::CompileError;
use crate::types::WasmType;

use super::ARRAY_HEADER_SIZE;

pub(super) fn len_memarg() -> MemArg {
    MemArg {
        offset: TYPED_ARRAY_LEN_OFFSET as u64,
        align: 2,
        memory_index: 0,
    }
}

pub(super) fn buf_ptr_memarg() -> MemArg {
    MemArg {
        offset: TYPED_ARRAY_BUF_PTR_OFFSET as u64,
        align: 2,
        memory_index: 0,
    }
}

/// Receiver bindings shared by every typed-array method emitter. Built once
/// up front so each method body is a tight emit and the layout-tax `buf_ptr`
/// load happens at the head of the function rather than inside the loop.
pub(super) struct Receiver {
    pub(super) arr_local: u32,
    pub(super) len_local: u32,
    pub(super) buf_ptr_local: u32,
}

impl<'a> FuncContext<'a> {
    /// Evaluate `expr` (a typed-array receiver), set its pointer / length /
    /// buf_ptr into fresh locals, and return their indices. After this runs,
    /// the loop body in each method just needs to add `i * stride` to
    /// `buf_ptr_local` for an element address.
    pub(super) fn emit_typed_array_receiver(
        &mut self,
        expr: &Expression<'a>,
    ) -> Result<Receiver, CompileError> {
        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(expr)?;
        self.push(Instruction::LocalSet(arr_local));

        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(len_memarg()));
        self.push(Instruction::LocalSet(len_local));

        let buf_ptr_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(buf_ptr_memarg()));
        self.push(Instruction::LocalSet(buf_ptr_local));

        Ok(Receiver {
            arr_local,
            len_local,
            buf_ptr_local,
        })
    }

    /// Push the address `buf_ptr + idx * stride` onto the stack. Stride 1
    /// (Uint8Array) elides the multiply.
    pub(super) fn emit_typed_array_elem_addr(
        &mut self,
        buf_ptr_local: u32,
        idx_local: u32,
        byte_stride: u32,
    ) {
        self.push(Instruction::LocalGet(buf_ptr_local));
        self.push(Instruction::LocalGet(idx_local));
        if byte_stride != 1 {
            self.push(Instruction::I32Const(byte_stride as i32));
            self.push(Instruction::I32Mul);
        }
        self.push(Instruction::I32Add);
    }

    /// Allocate a fresh self-owned typed array sized to `len_local`, write the
    /// header, and return the pointer local. Body is uninitialized — caller
    /// must fill it.
    pub(super) fn emit_alloc_self_owned_typed(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        len_local: u32,
    ) -> Result<u32, CompileError> {
        // total = HEADER + len * stride.
        self.push(Instruction::I32Const(TYPED_ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(desc.byte_stride as i32));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        // len at +0, buf_ptr = self + HEADER at +4.
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Store(len_memarg()));
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(TYPED_ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Store(buf_ptr_memarg()));

        Ok(ptr_local)
    }

    /// Coerce a value-on-stack to the typed-array element type. i32→f64
    /// promotes; other mismatches are a type error. Mirrors the construction
    /// path's coerce so methods accepting element values (`fill`, `with`-like
    /// shapes) behave identically to the constructor.
    pub(super) fn coerce_to_typed_array_elem(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        actual: WasmType,
        method: &str,
    ) -> Result<(), CompileError> {
        if actual == desc.elem_wasm_ty {
            return Ok(());
        }
        if desc.elem_wasm_ty == WasmType::F64 && actual == WasmType::I32 {
            self.push(Instruction::F64ConvertI32S);
            return Ok(());
        }
        Err(CompileError::type_err(format!(
            "{}.{method}: value has type {actual:?}, expected {:?}",
            desc.name, desc.elem_wasm_ty
        )))
    }

    /// Try to dispatch a typed-array instance method call. Routed before
    /// `try_emit_array_method_call` so the typed-array name doesn't fall
    /// through to the generic `Array<T>` path (which would then fail since
    /// the receiver isn't a tracked `Array<T>` local).
    pub(crate) fn try_emit_typed_array_method_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        let member = match &call.callee {
            Expression::StaticMemberExpression(m) => m,
            _ => return Ok(None),
        };
        let Some(desc) = self.resolve_expr_typed_array(&member.object) else {
            return Ok(None);
        };
        let method = member.property.name.as_str();

        match method {
            "at" => {
                self.expect_args(call, 1, &format!("{}.at", desc.name))?;
                self.emit_typed_array_at(desc, &member.object, call.arguments[0].to_expression())?;
                Ok(Some(desc.elem_wasm_ty))
            }
            "indexOf" => {
                if !matches!(call.arguments.len(), 1 | 2) {
                    return Err(CompileError::codegen(format!(
                        "{}.indexOf expects 1 or 2 arguments",
                        desc.name
                    )));
                }
                let from = call.arguments.get(1).map(|a| a.to_expression());
                self.emit_typed_array_index_of(
                    desc,
                    &member.object,
                    call.arguments[0].to_expression(),
                    false,
                    from,
                )?;
                Ok(Some(WasmType::I32))
            }
            "lastIndexOf" => {
                if !matches!(call.arguments.len(), 1 | 2) {
                    return Err(CompileError::codegen(format!(
                        "{}.lastIndexOf expects 1 or 2 arguments",
                        desc.name
                    )));
                }
                let from = call.arguments.get(1).map(|a| a.to_expression());
                self.emit_typed_array_index_of(
                    desc,
                    &member.object,
                    call.arguments[0].to_expression(),
                    true,
                    from,
                )?;
                Ok(Some(WasmType::I32))
            }
            "includes" => {
                if !matches!(call.arguments.len(), 1 | 2) {
                    return Err(CompileError::codegen(format!(
                        "{}.includes expects 1 or 2 arguments",
                        desc.name
                    )));
                }
                let from = call.arguments.get(1).map(|a| a.to_expression());
                self.emit_typed_array_index_of(
                    desc,
                    &member.object,
                    call.arguments[0].to_expression(),
                    false,
                    from,
                )?;
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32GeS);
                Ok(Some(WasmType::I32))
            }
            "slice" => {
                if call.arguments.len() > 2 {
                    return Err(CompileError::codegen(format!(
                        "{}.slice expects 0-2 arguments",
                        desc.name
                    )));
                }
                self.emit_typed_array_slice(desc, &member.object, call)?;
                Ok(Some(WasmType::I32))
            }
            "subarray" => {
                if call.arguments.len() > 2 {
                    return Err(CompileError::codegen(format!(
                        "{}.subarray expects 0-2 arguments",
                        desc.name
                    )));
                }
                self.emit_typed_array_subarray(desc, &member.object, call)?;
                Ok(Some(WasmType::I32))
            }
            "fill" => {
                if !matches!(call.arguments.len(), 1..=3) {
                    return Err(CompileError::codegen(format!(
                        "{}.fill expects 1-3 arguments",
                        desc.name
                    )));
                }
                self.emit_typed_array_fill(desc, &member.object, call)?;
                Ok(Some(WasmType::I32))
            }
            "set" => {
                if !matches!(call.arguments.len(), 1 | 2) {
                    return Err(CompileError::codegen(format!(
                        "{}.set expects 1 or 2 arguments (src, offset?)",
                        desc.name
                    )));
                }
                self.emit_typed_array_set(desc, &member.object, call)?;
                Ok(Some(WasmType::Void))
            }
            "reverse" => {
                self.expect_args(call, 0, &format!("{}.reverse", desc.name))?;
                self.emit_typed_array_reverse(desc, &member.object)?;
                Ok(Some(WasmType::I32))
            }
            "sort" => {
                if call.arguments.len() > 1 {
                    return Err(CompileError::codegen(format!(
                        "{}.sort expects 0 or 1 arguments",
                        desc.name
                    )));
                }
                let cmp = call.arguments.first().map(|a| a.to_expression());
                self.emit_typed_array_sort(desc, &member.object, cmp)?;
                Ok(Some(WasmType::I32))
            }
            "forEach" => {
                self.expect_args(call, 1, &format!("{}.forEach", desc.name))?;
                self.emit_typed_array_for_each(
                    desc,
                    &member.object,
                    call.arguments[0].to_expression(),
                )?;
                Ok(Some(WasmType::Void))
            }
            "some" | "every" => {
                self.expect_args(call, 1, &format!("{}.{method}", desc.name))?;
                let all = method == "every";
                self.emit_typed_array_some_every(
                    desc,
                    &member.object,
                    call.arguments[0].to_expression(),
                    all,
                )?;
                Ok(Some(WasmType::I32))
            }
            "find" | "findIndex" | "findLast" | "findLastIndex" => {
                self.expect_args(call, 1, &format!("{}.{method}", desc.name))?;
                let reverse = method.starts_with("findLast");
                let return_index = method.ends_with("Index");
                let result_ty = self.emit_typed_array_find(
                    desc,
                    &member.object,
                    call.arguments[0].to_expression(),
                    reverse,
                    return_index,
                )?;
                Ok(Some(result_ty))
            }
            "map" => {
                self.expect_args(call, 1, &format!("{}.map", desc.name))?;
                self.emit_typed_array_map(
                    desc,
                    &member.object,
                    call.arguments[0].to_expression(),
                )?;
                Ok(Some(WasmType::I32))
            }
            "filter" => {
                self.expect_args(call, 1, &format!("{}.filter", desc.name))?;
                self.emit_typed_array_filter(
                    desc,
                    &member.object,
                    call.arguments[0].to_expression(),
                )?;
                Ok(Some(WasmType::I32))
            }
            "reduce" | "reduceRight" => {
                if call.arguments.len() != 2 {
                    return Err(CompileError::codegen(format!(
                        "{}.{method} expects 2 arguments (callback, initialValue)",
                        desc.name
                    )));
                }
                let reverse = method == "reduceRight";
                let result_ty = self.emit_typed_array_reduce(
                    desc,
                    &member.object,
                    call.arguments[0].to_expression(),
                    call.arguments[1].to_expression(),
                    reverse,
                )?;
                Ok(Some(result_ty))
            }
            "copyWithin" => {
                if !matches!(call.arguments.len(), 2 | 3) {
                    return Err(CompileError::codegen(format!(
                        "{}.copyWithin expects 2 or 3 arguments (target, start, end?)",
                        desc.name
                    )));
                }
                self.emit_typed_array_copy_within(desc, &member.object, call)?;
                Ok(Some(WasmType::I32))
            }
            "join" => {
                if !matches!(call.arguments.len(), 0 | 1) {
                    return Err(CompileError::codegen(format!(
                        "{}.join expects 0 or 1 arguments",
                        desc.name
                    )));
                }
                self.emit_typed_array_join(desc, &member.object, call)?;
                Ok(Some(WasmType::I32))
            }
            _ => Err(CompileError::codegen(format!(
                "{} has no method '{method}' — supported: at, indexOf, lastIndexOf, includes, slice, subarray, fill, set, reverse, sort, copyWithin, join, forEach, map, filter, reduce, reduceRight, find, findIndex, findLast, findLastIndex, some, every",
                desc.name
            ))),
        }
    }
}

// =====================================================================
// at / indexOf / lastIndexOf / includes
// =====================================================================

impl<'a> FuncContext<'a> {
    /// `ta.at(i)` — negative-index lookup with trap on out-of-range, matching
    /// `Array.at` semantics (and the bounds-check posture used elsewhere).
    fn emit_typed_array_at(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        idx_expr: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let idx_local = self.alloc_local(WasmType::I32);
        let idx_ty = self.emit_expr(idx_expr)?;
        if idx_ty == WasmType::F64 {
            self.push(Instruction::I32TruncF64S);
        }
        self.push(Instruction::LocalSet(idx_local));

        let recv = self.emit_typed_array_receiver(ta_expr)?;

        // if idx < 0: idx += len.
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32LtS);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(idx_local));
        self.push(Instruction::End);

        // Bounds: idx < 0 || idx >= len → trap.
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32LtS);
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::I32Or);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::Unreachable);
        self.push(Instruction::End);

        self.emit_typed_array_elem_addr(recv.buf_ptr_local, idx_local, desc.byte_stride);
        self.push(desc.load_inst(0));
        Ok(())
    }

    /// `ta.indexOf(x[, fromIndex])` / `ta.lastIndexOf(...)` — linear scan.
    /// Matches `Array.indexOf` semantics, including negative-fromIndex
    /// normalisation and forward-vs-reverse direction.
    fn emit_typed_array_index_of(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        needle_expr: &Expression<'a>,
        reverse: bool,
        from_idx: Option<&Expression<'a>>,
    ) -> Result<(), CompileError> {
        // Needle first (JS arg order), with i32→f64 promotion to match
        // descriptor's element type.
        let needle_local = self.alloc_local(desc.elem_wasm_ty);
        let needle_ty = self.emit_expr(needle_expr)?;
        self.coerce_to_typed_array_elem(desc, needle_ty, "indexOf")?;
        self.push(Instruction::LocalSet(needle_local));

        let from_local = if let Some(from) = from_idx {
            let local = self.alloc_local(WasmType::I32);
            let ty = self.emit_expr(from)?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
            self.push(Instruction::LocalSet(local));
            Some(local)
        } else {
            None
        };

        let recv = self.emit_typed_array_receiver(ta_expr)?;

        let i_local = self.alloc_local(WasmType::I32);
        let result_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(-1));
        self.push(Instruction::LocalSet(result_local));

        // Choose starting index. Same shape as `Array.indexOf`'s clamp logic.
        if let Some(from) = from_local {
            self.push(Instruction::LocalGet(from));
            self.push(Instruction::LocalSet(i_local));
            // Negative fromIndex: i += len.
            self.push(Instruction::LocalGet(i_local));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(BlockType::Empty));
            self.push(Instruction::LocalGet(i_local));
            self.push(Instruction::LocalGet(recv.len_local));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(i_local));
            self.push(Instruction::End);
            if reverse {
                // Forward overflow clamps via the loop bound; reverse needs
                // to clamp i > len-1 down to len-1 to avoid skipping past.
                self.push(Instruction::LocalGet(i_local));
                self.push(Instruction::LocalGet(recv.len_local));
                self.push(Instruction::I32GeS);
                self.push(Instruction::If(BlockType::Empty));
                self.push(Instruction::LocalGet(recv.len_local));
                self.push(Instruction::I32Const(1));
                self.push(Instruction::I32Sub);
                self.push(Instruction::LocalSet(i_local));
                self.push(Instruction::End);
            } else {
                // Forward: still-negative clamps to 0.
                self.push(Instruction::LocalGet(i_local));
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32LtS);
                self.push(Instruction::If(BlockType::Empty));
                self.push(Instruction::I32Const(0));
                self.push(Instruction::LocalSet(i_local));
                self.push(Instruction::End);
            }
        } else if reverse {
            self.push(Instruction::LocalGet(recv.len_local));
            self.push(Instruction::I32Const(1));
            self.push(Instruction::I32Sub);
            self.push(Instruction::LocalSet(i_local));
        } else {
            self.push(Instruction::I32Const(0));
            self.push(Instruction::LocalSet(i_local));
        }

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));

        self.push(Instruction::LocalGet(i_local));
        if reverse {
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
        } else {
            self.push(Instruction::LocalGet(recv.len_local));
            self.push(Instruction::I32GeS);
        }
        self.push(Instruction::BrIf(1));

        // Load ta[i] via descriptor's load op.
        self.emit_typed_array_elem_addr(recv.buf_ptr_local, i_local, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(Instruction::LocalGet(needle_local));
        match desc.elem_wasm_ty {
            WasmType::F64 => self.push(Instruction::F64Eq),
            _ => self.push(Instruction::I32Eq),
        }
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalSet(result_local));
        self.push(Instruction::Br(2));
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(if reverse { -1 } else { 1 }));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End);
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(result_local));
        Ok(())
    }
}

// =====================================================================
// slice (copy) / subarray (view)
// =====================================================================

/// Side products of `start`/`end` argument resolution: clamped ints in
/// locals + the count between them. Both `slice` and `subarray` need this
/// shape, so it lives in one place.
struct SliceBounds {
    start_local: u32,
    count_local: u32,
}

impl<'a> FuncContext<'a> {
    /// Resolve `start` and `end` arguments per ES `slice`/`subarray` rules:
    /// truncate f64→i32, normalize negatives by adding len, clamp to [0, len],
    /// then `count = max(0, end - start)`. Returns `(start, count)` locals.
    fn emit_slice_bounds(
        &mut self,
        len_local: u32,
        call: &CallExpression<'a>,
    ) -> Result<SliceBounds, CompileError> {
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

        // Normalize negatives: bound < 0 ⇒ bound += len.
        for &bound in &[start_local, end_local] {
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(BlockType::Empty));
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(bound));
            self.push(Instruction::End);
        }
        // Clamp to [0, len].
        for &bound in &[start_local, end_local] {
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(BlockType::Empty));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::LocalSet(bound));
            self.push(Instruction::End);
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::I32GtS);
            self.push(Instruction::If(BlockType::Empty));
            self.push(Instruction::LocalGet(len_local));
            self.push(Instruction::LocalSet(bound));
            self.push(Instruction::End);
        }
        // end = max(end, start) — empty result when end < start.
        self.push(Instruction::LocalGet(end_local));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32LtS);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::LocalSet(end_local));
        self.push(Instruction::End);

        let count_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(end_local));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(count_local));

        Ok(SliceBounds {
            start_local,
            count_local,
        })
    }

    /// `ta.slice(start?, end?)` — fresh self-owned typed array, `memory.copy`
    /// from `receiver.buf_ptr + start*stride`. Mutation through the result
    /// does NOT propagate to the receiver — that's `subarray`'s job.
    fn emit_typed_array_slice(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        let recv = self.emit_typed_array_receiver(ta_expr)?;
        let bounds = self.emit_slice_bounds(recv.len_local, call)?;

        let new_ptr = self.emit_alloc_self_owned_typed(desc, bounds.count_local)?;

        // memory.copy(dst = new + HEADER, src = buf_ptr + start*stride,
        //             n = count * stride).
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::I32Const(TYPED_ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.emit_typed_array_elem_addr(
            recv.buf_ptr_local,
            bounds.start_local,
            desc.byte_stride,
        );
        self.push(Instruction::LocalGet(bounds.count_local));
        if desc.byte_stride != 1 {
            self.push(Instruction::I32Const(desc.byte_stride as i32));
            self.push(Instruction::I32Mul);
        }
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        self.push(Instruction::LocalGet(new_ptr));
        Ok(())
    }

    /// `ta.subarray(start?, end?)` — view with shared body. Allocates an
    /// 8-byte header only; `len = count`, `buf_ptr = receiver.buf_ptr +
    /// start*stride`. Mutation through the view propagates to the receiver
    /// (and any other views over the same body) — this is the load-bearing
    /// distinction from `slice`.
    fn emit_typed_array_subarray(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        let recv = self.emit_typed_array_receiver(ta_expr)?;
        let bounds = self.emit_slice_bounds(recv.len_local, call)?;

        // Header-only allocation: 8 bytes, no body.
        self.push(Instruction::I32Const(TYPED_ARRAY_HEADER_SIZE as i32));
        let view_ptr = self.emit_arena_alloc_to_local(true)?;

        // len = count.
        self.push(Instruction::LocalGet(view_ptr));
        self.push(Instruction::LocalGet(bounds.count_local));
        self.push(Instruction::I32Store(len_memarg()));

        // buf_ptr = receiver.buf_ptr + start * stride. Reuses the address
        // helper so stride-1 path (Uint8Array) elides the multiply.
        self.push(Instruction::LocalGet(view_ptr));
        self.emit_typed_array_elem_addr(
            recv.buf_ptr_local,
            bounds.start_local,
            desc.byte_stride,
        );
        self.push(Instruction::I32Store(buf_ptr_memarg()));

        self.push(Instruction::LocalGet(view_ptr));
        Ok(())
    }
}

// =====================================================================
// fill
// =====================================================================

impl<'a> FuncContext<'a> {
    /// `ta.fill(value, start?, end?)` — in-place. Two strategies:
    ///   - i32-stride 1 with constant value byte-replicated: emit `memory.fill`
    ///     directly.
    ///   - everything else: element-wise loop with descriptor's store op.
    ///
    /// Returns the receiver pointer to allow chaining.
    fn emit_typed_array_fill(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        // Eval value first per JS arg-order, with elem-type coercion.
        let val_local = self.alloc_local(desc.elem_wasm_ty);
        let val_ty = self.emit_expr(call.arguments[0].to_expression())?;
        self.coerce_to_typed_array_elem(desc, val_ty, "fill")?;
        self.push(Instruction::LocalSet(val_local));

        let recv = self.emit_typed_array_receiver(ta_expr)?;

        // start / end with normalisation + clamp to [0, len], end clamped to
        // start (so `count = end - start ≥ 0`).
        let mut bounds_call_args: Vec<&Expression<'a>> = Vec::new();
        for arg in call.arguments.iter().skip(1) {
            bounds_call_args.push(arg.to_expression());
        }
        let start_local = self.alloc_local(WasmType::I32);
        let end_local = self.alloc_local(WasmType::I32);
        if !bounds_call_args.is_empty() {
            let ty = self.emit_expr(bounds_call_args[0])?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
        } else {
            self.push(Instruction::I32Const(0));
        }
        self.push(Instruction::LocalSet(start_local));
        if bounds_call_args.len() == 2 {
            let ty = self.emit_expr(bounds_call_args[1])?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
        } else {
            self.push(Instruction::LocalGet(recv.len_local));
        }
        self.push(Instruction::LocalSet(end_local));

        // Normalize / clamp shape lifted from `slice` (kept inline so the
        // local indices stay scoped to this method).
        for &bound in &[start_local, end_local] {
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(BlockType::Empty));
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::LocalGet(recv.len_local));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(bound));
            self.push(Instruction::End);
        }
        for &bound in &[start_local, end_local] {
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(BlockType::Empty));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::LocalSet(bound));
            self.push(Instruction::End);
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::LocalGet(recv.len_local));
            self.push(Instruction::I32GtS);
            self.push(Instruction::If(BlockType::Empty));
            self.push(Instruction::LocalGet(recv.len_local));
            self.push(Instruction::LocalSet(bound));
            self.push(Instruction::End);
        }
        // end = max(end, start).
        self.push(Instruction::LocalGet(end_local));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32LtS);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::LocalSet(end_local));
        self.push(Instruction::End);

        // Element-wise loop. The Uint8Array memory.fill fast-path is a nice
        // future optimisation but isn't load-bearing for v1; the loop is small
        // enough that Cranelift unrolls it for short bursts.
        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(end_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        self.emit_typed_array_elem_addr(recv.buf_ptr_local, i_local, desc.byte_stride);
        self.push(Instruction::LocalGet(val_local));
        self.push(desc.store_inst(0));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(recv.arr_local));
        Ok(())
    }
}

// =====================================================================
// set
// =====================================================================

impl<'a> FuncContext<'a> {
    /// `ta.set(src, offset?)` — copy `src` into `ta` starting at `offset`
    /// (default 0). Three source shapes:
    ///   - same-kind typed array: `memory.copy` from `src.buf_ptr`.
    ///   - cross-kind typed array: element-wise widen/narrow loop with
    ///     descriptor coercion.
    ///   - `Array<T>` (matching elem_ty): `memory.copy` from `src + ARRAY_HEADER`.
    ///
    /// Bounds: trap if `offset < 0` or `offset + src.length > ta.length`.
    fn emit_typed_array_set(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        let src_expr = call.arguments[0].to_expression();
        let offset_local = self.alloc_local(WasmType::I32);
        if call.arguments.len() == 2 {
            let ty = self.emit_expr(call.arguments[1].to_expression())?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
        } else {
            self.push(Instruction::I32Const(0));
        }
        self.push(Instruction::LocalSet(offset_local));

        let recv = self.emit_typed_array_receiver(ta_expr)?;

        // Three branches, picked by the source's resolvable shape.
        if let Some(src_desc) = self.resolve_expr_typed_array(src_expr) {
            self.emit_set_from_typed_array(desc, src_desc, src_expr, &recv, offset_local)
        } else if let Some(src_elem) = self.resolve_expr_array_elem(src_expr) {
            self.emit_set_from_array(desc, src_elem, src_expr, &recv, offset_local)
        } else {
            Err(CompileError::type_err(format!(
                "{}.set: source must be an Array<T> or another typed array",
                desc.name
            )))
        }
    }

    fn emit_set_bounds_check(&mut self, offset_local: u32, src_len_local: u32, ta_len_local: u32) {
        // offset < 0 || offset + src_len > ta_len → trap.
        self.push(Instruction::LocalGet(offset_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32LtS);
        self.push(Instruction::LocalGet(offset_local));
        self.push(Instruction::LocalGet(src_len_local));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(ta_len_local));
        self.push(Instruction::I32GtS);
        self.push(Instruction::I32Or);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::Unreachable);
        self.push(Instruction::End);
    }

    fn emit_set_from_typed_array(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        src_desc: &'static TypedArrayDescriptor,
        src_expr: &Expression<'a>,
        recv: &Receiver,
        offset_local: u32,
    ) -> Result<(), CompileError> {
        // Eval src into locals.
        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(src_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(src_local));
        self.push(Instruction::I32Load(len_memarg()));
        self.push(Instruction::LocalSet(src_len_local));

        let src_buf_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(src_local));
        self.push(Instruction::I32Load(buf_ptr_memarg()));
        self.push(Instruction::LocalSet(src_buf_local));

        self.emit_set_bounds_check(offset_local, src_len_local, recv.len_local);

        if src_desc.byte_stride == desc.byte_stride
            && src_desc.elem_wasm_ty == desc.elem_wasm_ty
        {
            // Same kind: bulk memory.copy. Even view sources work because
            // we read src_buf_local which already accounts for sub-views.
            // dst = recv.buf_ptr + offset * stride.
            self.emit_typed_array_elem_addr(recv.buf_ptr_local, offset_local, desc.byte_stride);
            self.push(Instruction::LocalGet(src_buf_local));
            self.push(Instruction::LocalGet(src_len_local));
            if desc.byte_stride != 1 {
                self.push(Instruction::I32Const(desc.byte_stride as i32));
                self.push(Instruction::I32Mul);
            }
            self.push(Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });
        } else {
            // Cross-kind: element-wise widen/narrow. Read each src element
            // with src_desc.load_op (zero-extends for u8 → i32), coerce to
            // dest's wasm type, then store with desc.store_op (truncates
            // for u8). Cross-stride copy can't use memory.copy because
            // element widths differ.
            self.emit_set_cross_kind_loop(desc, src_desc, src_buf_local, src_len_local, recv, offset_local)?;
        }
        Ok(())
    }

    fn emit_set_cross_kind_loop(
        &mut self,
        dest: &'static TypedArrayDescriptor,
        src: &'static TypedArrayDescriptor,
        src_buf_local: u32,
        src_len_local: u32,
        recv: &Receiver,
        offset_local: u32,
    ) -> Result<(), CompileError> {
        // for (i = 0; i < src_len; i++) ta[offset + i] = src[i].
        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(src_len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        // dst addr = recv.buf_ptr + (offset + i) * stride.
        let dst_idx_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(offset_local));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(dst_idx_local));
        self.emit_typed_array_elem_addr(recv.buf_ptr_local, dst_idx_local, dest.byte_stride);

        // Load src[i] via src_desc, coerce to dest type.
        self.emit_typed_array_elem_addr(src_buf_local, i_local, src.byte_stride);
        self.push(src.load_inst(0));
        self.coerce_to_typed_array_elem(dest, src.elem_wasm_ty, "set")?;
        self.push(dest.store_inst(0));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);
        Ok(())
    }

    fn emit_set_from_array(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        src_elem: WasmType,
        src_expr: &Expression<'a>,
        recv: &Receiver,
        offset_local: u32,
    ) -> Result<(), CompileError> {
        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(src_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let src_len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(src_local));
        self.push(Instruction::I32Load(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(src_len_local));

        self.emit_set_bounds_check(offset_local, src_len_local, recv.len_local);

        // Element widths align ⇒ memory.copy. Otherwise loop with conversion.
        let src_stride: u32 = match src_elem {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            _ => {
                return Err(CompileError::type_err(format!(
                    "{}.set: Array<T> source must have i32 or f64 elements",
                    desc.name
                )));
            }
        };
        if src_elem == desc.elem_wasm_ty && src_stride == desc.byte_stride {
            // dst = recv.buf_ptr + offset * stride.
            self.emit_typed_array_elem_addr(recv.buf_ptr_local, offset_local, desc.byte_stride);
            // src body = src_local + ARRAY_HEADER.
            self.push(Instruction::LocalGet(src_local));
            self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalGet(src_len_local));
            if desc.byte_stride != 1 {
                self.push(Instruction::I32Const(desc.byte_stride as i32));
                self.push(Instruction::I32Mul);
            }
            self.push(Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });
            return Ok(());
        }

        // Mismatched widths — element-wise loop with coercion.
        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(src_len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        let dst_idx_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(offset_local));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(dst_idx_local));
        self.emit_typed_array_elem_addr(recv.buf_ptr_local, dst_idx_local, desc.byte_stride);

        // src[i] address: src_local + ARRAY_HEADER + i*src_stride.
        self.push(Instruction::LocalGet(src_local));
        self.push(Instruction::I32Const(ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(src_stride as i32));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        match src_elem {
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
        self.coerce_to_typed_array_elem(desc, src_elem, "set")?;
        self.push(desc.store_inst(0));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);
        Ok(())
    }
}

// =====================================================================
// reverse
// =====================================================================

impl<'a> FuncContext<'a> {
    /// `ta.reverse()` — in-place. Two-pointer swap loop using descriptor
    /// load/store ops. Returns the receiver pointer to allow chaining.
    fn emit_typed_array_reverse(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let recv = self.emit_typed_array_receiver(ta_expr)?;

        let lo = self.alloc_local(WasmType::I32);
        let hi = self.alloc_local(WasmType::I32);
        let tmp_a = self.alloc_local(desc.elem_wasm_ty);
        let tmp_b = self.alloc_local(desc.elem_wasm_ty);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(lo));
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(hi));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));

        // lo >= hi → done. Note: signed compare so a length-0 input (hi = -1)
        // also exits cleanly without entering the body.
        self.push(Instruction::LocalGet(lo));
        self.push(Instruction::LocalGet(hi));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        self.emit_typed_array_elem_addr(recv.buf_ptr_local, lo, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(Instruction::LocalSet(tmp_a));
        self.emit_typed_array_elem_addr(recv.buf_ptr_local, hi, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(Instruction::LocalSet(tmp_b));

        self.emit_typed_array_elem_addr(recv.buf_ptr_local, lo, desc.byte_stride);
        self.push(Instruction::LocalGet(tmp_b));
        self.push(desc.store_inst(0));
        self.emit_typed_array_elem_addr(recv.buf_ptr_local, hi, desc.byte_stride);
        self.push(Instruction::LocalGet(tmp_a));
        self.push(desc.store_inst(0));

        self.push(Instruction::LocalGet(lo));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(lo));
        self.push(Instruction::LocalGet(hi));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(hi));
        self.push(Instruction::Br(0));

        self.push(Instruction::End);
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(recv.arr_local));
        Ok(())
    }
}

// =====================================================================
// sort (default comparator)
// =====================================================================

impl<'a> FuncContext<'a> {
    /// `ta.sort([cmp])` — bottom-up iterative merge sort. Without a comparator
    /// the order is the descriptor's natural numeric order (signed for i32,
    /// IEEE for f64). With a comparator we inline its arrow body once per
    /// merge comparison and treat the result `<= 0` as "take left", matching
    /// the `Array<T>.sort` convention.
    fn emit_typed_array_sort(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        cmp: Option<&Expression<'a>>,
    ) -> Result<(), CompileError> {
        use crate::codegen::array_builtins::{
            extract_arrow, restore_arrow_scope, setup_arrow_scope,
        };

        // Resolve the comparator arrow up front (if any) so we know the
        // body's return shape before we start emitting the merge loop.
        let cmp_arrow = match cmp {
            Some(expr) => Some(extract_arrow(expr)?),
            None => None,
        };
        let cmp_params = match cmp_arrow {
            Some(arrow) => {
                let mut names = Vec::with_capacity(2);
                for p in &arrow.params.items {
                    match &p.pattern {
                        BindingPattern::BindingIdentifier(id) => {
                            names.push(id.name.as_str().to_string())
                        }
                        _ => {
                            return Err(CompileError::unsupported(format!(
                                "{}.sort comparator parameters must be simple identifiers",
                                desc.name
                            )));
                        }
                    }
                }
                if names.len() != 2 {
                    return Err(CompileError::codegen(format!(
                        "{}.sort comparator must take exactly 2 parameters",
                        desc.name
                    )));
                }
                names
            }
            None => Vec::new(),
        };

        let recv = self.emit_typed_array_receiver(ta_expr)?;

        // Allocate temp buffer for the merge passes — same shape as the
        // receiver but always self-owned (no body sharing). Body is
        // overwritten on each pass, so we only need the buf_ptr.
        let tmp_ptr = self.emit_alloc_self_owned_typed(desc, recv.len_local)?;
        let tmp_buf_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(tmp_ptr));
        self.push(Instruction::I32Load(buf_ptr_memarg()));
        self.push(Instruction::LocalSet(tmp_buf_local));

        let width_local = self.alloc_local(WasmType::I32);
        let i_local = self.alloc_local(WasmType::I32);
        let mid_local = self.alloc_local(WasmType::I32);
        let right_local = self.alloc_local(WasmType::I32);
        let l_local = self.alloc_local(WasmType::I32);
        let r_local = self.alloc_local(WasmType::I32);
        let k_local = self.alloc_local(WasmType::I32);
        let a_local = self.alloc_local(desc.elem_wasm_ty);
        let b_local = self.alloc_local(desc.elem_wasm_ty);
        let copy_idx = self.alloc_local(WasmType::I32);

        self.push(Instruction::I32Const(1));
        self.push(Instruction::LocalSet(width_local));

        // Outer: while width < len.
        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        // Inner: while i < len.
        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        // mid = min(i + width, len). select(a, b, cond): a if cond != 0.
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::I32LtS);
        self.push(Instruction::Select);
        self.push(Instruction::LocalSet(mid_local));

        // right = min(i + 2w, len).
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Const(2));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Const(2));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::I32LtS);
        self.push(Instruction::Select);
        self.push(Instruction::LocalSet(right_local));

        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalSet(l_local));
        self.push(Instruction::LocalGet(mid_local));
        self.push(Instruction::LocalSet(r_local));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalSet(k_local));

        // Merge: while l < mid && r < right.
        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(l_local));
        self.push(Instruction::LocalGet(mid_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));
        self.push(Instruction::LocalGet(r_local));
        self.push(Instruction::LocalGet(right_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        self.emit_typed_array_elem_addr(recv.buf_ptr_local, l_local, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(Instruction::LocalSet(a_local));
        self.emit_typed_array_elem_addr(recv.buf_ptr_local, r_local, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(Instruction::LocalSet(b_local));

        // Comparison decision. Without a comparator we just emit
        // `a <= b` per the descriptor's element type. With a comparator we
        // inline the arrow body bound to (a, b) and test `result <= 0`.
        match cmp_arrow {
            None => {
                self.push(Instruction::LocalGet(a_local));
                self.push(Instruction::LocalGet(b_local));
                match desc.elem_wasm_ty {
                    WasmType::F64 => self.push(Instruction::F64Le),
                    _ => self.push(Instruction::I32LeS),
                }
            }
            Some(arrow) => {
                let scope = setup_arrow_scope(
                    self,
                    &cmp_params,
                    &[(a_local, desc.elem_wasm_ty), (b_local, desc.elem_wasm_ty)],
                    &[None, None],
                );
                let body_ty = crate::codegen::array_builtins::eval_arrow_body(self, arrow)?;
                restore_arrow_scope(self, scope);
                match body_ty {
                    WasmType::I32 => {
                        self.push(Instruction::I32Const(0));
                        self.push(Instruction::I32LeS);
                    }
                    WasmType::F64 => {
                        self.push(Instruction::F64Const(0.0));
                        self.push(Instruction::F64Le);
                    }
                    _ => {
                        return Err(CompileError::type_err(format!(
                            "{}.sort comparator must return i32 or f64, got {body_ty:?}",
                            desc.name
                        )));
                    }
                }
            }
        }
        self.push(Instruction::If(BlockType::Empty));

        self.emit_typed_array_elem_addr(tmp_buf_local, k_local, desc.byte_stride);
        self.push(Instruction::LocalGet(a_local));
        self.push(desc.store_inst(0));
        self.push(Instruction::LocalGet(l_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(l_local));

        self.push(Instruction::Else);

        self.emit_typed_array_elem_addr(tmp_buf_local, k_local, desc.byte_stride);
        self.push(Instruction::LocalGet(b_local));
        self.push(desc.store_inst(0));
        self.push(Instruction::LocalGet(r_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(r_local));

        self.push(Instruction::End);

        self.push(Instruction::LocalGet(k_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(k_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End); // end merge loop
        self.push(Instruction::End); // end merge block

        // Drain left.
        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(l_local));
        self.push(Instruction::LocalGet(mid_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));
        self.emit_typed_array_elem_addr(tmp_buf_local, k_local, desc.byte_stride);
        self.emit_typed_array_elem_addr(recv.buf_ptr_local, l_local, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(desc.store_inst(0));
        self.push(Instruction::LocalGet(l_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(l_local));
        self.push(Instruction::LocalGet(k_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(k_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);

        // Drain right.
        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(r_local));
        self.push(Instruction::LocalGet(right_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));
        self.emit_typed_array_elem_addr(tmp_buf_local, k_local, desc.byte_stride);
        self.emit_typed_array_elem_addr(recv.buf_ptr_local, r_local, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(desc.store_inst(0));
        self.push(Instruction::LocalGet(r_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(r_local));
        self.push(Instruction::LocalGet(k_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(k_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);

        // i += 2 * width.
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Const(2));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Br(0));
        self.push(Instruction::End); // inner loop
        self.push(Instruction::End); // inner block

        // Copy tmp body back into receiver body via memory.copy. tmp and recv
        // bodies are both sized to len * stride.
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(copy_idx));
        // memory.copy(recv.buf_ptr, tmp.buf_ptr, len * stride).
        self.push(Instruction::LocalGet(recv.buf_ptr_local));
        self.push(Instruction::LocalGet(tmp_buf_local));
        self.push(Instruction::LocalGet(recv.len_local));
        if desc.byte_stride != 1 {
            self.push(Instruction::I32Const(desc.byte_stride as i32));
            self.push(Instruction::I32Mul);
        }
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        // copy_idx is unused (the per-element copy path was replaced with
        // memory.copy above) — silence unused warnings by reading it once.
        self.push(Instruction::LocalGet(copy_idx));
        self.push(Instruction::Drop);

        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Const(2));
        self.push(Instruction::I32Mul);
        self.push(Instruction::LocalSet(width_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End); // outer loop
        self.push(Instruction::End); // outer block

        self.push(Instruction::LocalGet(recv.arr_local));
        Ok(())
    }
}

// =====================================================================
// copyWithin
// =====================================================================

impl<'a> FuncContext<'a> {
    /// `ta.copyWithin(target, start, end?)` — in-place range copy. Wasm's
    /// `memory.copy` handles overlap correctly (per the spec: equivalent to
    /// reading the entire source into a temporary), so the same one
    /// `memory.copy` works regardless of whether target is before, after,
    /// or overlapping start.
    fn emit_typed_array_copy_within(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        let recv = self.emit_typed_array_receiver(ta_expr)?;

        let target_local = self.alloc_local(WasmType::I32);
        let start_local = self.alloc_local(WasmType::I32);
        let end_local = self.alloc_local(WasmType::I32);

        let ty = self.emit_expr(call.arguments[0].to_expression())?;
        if ty == WasmType::F64 {
            self.push(Instruction::I32TruncF64S);
        }
        self.push(Instruction::LocalSet(target_local));

        let ty = self.emit_expr(call.arguments[1].to_expression())?;
        if ty == WasmType::F64 {
            self.push(Instruction::I32TruncF64S);
        }
        self.push(Instruction::LocalSet(start_local));

        if call.arguments.len() == 3 {
            let ty = self.emit_expr(call.arguments[2].to_expression())?;
            if ty == WasmType::F64 {
                self.push(Instruction::I32TruncF64S);
            }
        } else {
            self.push(Instruction::LocalGet(recv.len_local));
        }
        self.push(Instruction::LocalSet(end_local));

        // Normalize negatives.
        for &bound in &[target_local, start_local, end_local] {
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(BlockType::Empty));
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::LocalGet(recv.len_local));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(bound));
            self.push(Instruction::End);
        }
        // Clamp to [0, len].
        for &bound in &[target_local, start_local, end_local] {
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::I32LtS);
            self.push(Instruction::If(BlockType::Empty));
            self.push(Instruction::I32Const(0));
            self.push(Instruction::LocalSet(bound));
            self.push(Instruction::End);
            self.push(Instruction::LocalGet(bound));
            self.push(Instruction::LocalGet(recv.len_local));
            self.push(Instruction::I32GtS);
            self.push(Instruction::If(BlockType::Empty));
            self.push(Instruction::LocalGet(recv.len_local));
            self.push(Instruction::LocalSet(bound));
            self.push(Instruction::End);
        }

        // count = min(end - start, len - target).
        let count_local = self.alloc_local(WasmType::I32);
        let avail_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(end_local));
        self.push(Instruction::LocalGet(start_local));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(count_local));
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::LocalGet(target_local));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(avail_local));
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::LocalGet(avail_local));
        self.push(Instruction::I32GtS);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::LocalGet(avail_local));
        self.push(Instruction::LocalSet(count_local));
        self.push(Instruction::End);

        // if count > 0: memory.copy(buf_ptr + target*s, buf_ptr + start*s,
        //                            count*s).
        self.push(Instruction::LocalGet(count_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32GtS);
        self.push(Instruction::If(BlockType::Empty));
        self.emit_typed_array_elem_addr(recv.buf_ptr_local, target_local, desc.byte_stride);
        self.emit_typed_array_elem_addr(recv.buf_ptr_local, start_local, desc.byte_stride);
        self.push(Instruction::LocalGet(count_local));
        if desc.byte_stride != 1 {
            self.push(Instruction::I32Const(desc.byte_stride as i32));
            self.push(Instruction::I32Mul);
        }
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        self.push(Instruction::End);

        self.push(Instruction::LocalGet(recv.arr_local));
        Ok(())
    }
}

// =====================================================================
// join
// =====================================================================

impl<'a> FuncContext<'a> {
    /// `ta.join(sep?)` — stringifies each element via `__str_from_*` and
    /// concatenates with the separator (default `,`). Mirrors `Array.join`'s
    /// shape; `Uint8Array` reuses the i32 stringifier since reads zero-extend
    /// to i32 anyway.
    fn emit_typed_array_join(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        ta_expr: &Expression<'a>,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        let to_str_helper: &str = match desc.elem_wasm_ty {
            WasmType::F64 => "__str_from_f64",
            _ => "__str_from_i32",
        };
        let to_str_idx = self
            .module_ctx
            .get_func(to_str_helper)
            .ok_or_else(|| {
                CompileError::codegen(format!(
                    "{}.join requires {to_str_helper} — ensure string runtime is registered",
                    desc.name
                ))
            })?
            .0;
        let concat_idx = self
            .module_ctx
            .get_func("__str_concat")
            .ok_or_else(|| CompileError::codegen("typed-array join requires __str_concat"))?
            .0;

        let sep_local = self.alloc_local(WasmType::I32);
        if call.arguments.is_empty() {
            let offset = self.module_ctx.alloc_static_string(",");
            self.push(Instruction::I32Const(offset as i32));
        } else {
            self.emit_expr(call.arguments[0].to_expression())?;
        }
        self.push(Instruction::LocalSet(sep_local));

        let recv = self.emit_typed_array_receiver(ta_expr)?;

        let empty_off = self.module_ctx.alloc_static_string("");
        let result_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(empty_off as i32));
        self.push(Instruction::LocalSet(result_local));

        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(recv.len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        // For i > 0, prepend separator.
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(0));
        self.push(Instruction::I32GtS);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::LocalGet(result_local));
        self.push(Instruction::LocalGet(sep_local));
        self.push(Instruction::Call(concat_idx));
        self.push(Instruction::LocalSet(result_local));
        self.push(Instruction::End);

        self.emit_typed_array_elem_addr(recv.buf_ptr_local, i_local, desc.byte_stride);
        self.push(desc.load_inst(0));
        self.push(Instruction::Call(to_str_idx));
        let elem_str = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalSet(elem_str));
        self.push(Instruction::LocalGet(result_local));
        self.push(Instruction::LocalGet(elem_str));
        self.push(Instruction::Call(concat_idx));
        self.push(Instruction::LocalSet(result_local));

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
}

