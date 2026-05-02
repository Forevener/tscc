//! Typed-array codegen — `Int32Array` / `Float64Array` / `Uint8Array`.
//!
//! Sub-phase 2 lands here: construction (`new T(n | [...] | src)`,
//! `T.of(...)`, `T.from(src[, mapFn])`), indexed read/write, the three
//! properties (`length`, `byteLength`, static `BYTES_PER_ELEMENT`), and the
//! `for..of` case. Everything routes through a `TypedArrayDescriptor` so
//! sub-phase 5's `Uint8Array` lands without revisiting these sites — only
//! the descriptor's `load_op` / `store_op` / `byte_stride` change.
//!
//! Layout reminder: `[len: u32 @ +0][buf_ptr: u32 @ +4]`. Self-owned typed
//! arrays follow the header with the body inline (`buf_ptr = self + 8`);
//! `subarray` views (sub-phase 3) share the parent's body. Indexed access
//! always loads `buf_ptr` first, so the same emit path works for both.
//!
//! This file deliberately does NOT depend on `Array<T>`'s codegen — the
//! header shape is similar but the semantics (no growth, view-capable) are
//! different enough that sharing the existing `Array<T>` helpers would
//! couple the two paths into a tangle. The cost is small: each method here
//! is a tight emit, not a deep helper chain.

use oxc_ast::ast::*;
use wasm_encoder::{BlockType, Instruction, MemArg};

use crate::codegen::func::FuncContext;
use crate::codegen::typed_arrays::{
    TYPED_ARRAY_BUF_PTR_OFFSET, TYPED_ARRAY_HEADER_SIZE, TYPED_ARRAY_LEN_OFFSET,
    TypedArrayDescriptor, descriptor_for,
};
use crate::error::CompileError;
use crate::types::WasmType;

/// `MemArg` for the header `len` load (4-byte aligned, offset 0).
fn len_memarg() -> MemArg {
    MemArg {
        offset: TYPED_ARRAY_LEN_OFFSET as u64,
        align: 2,
        memory_index: 0,
    }
}

/// `MemArg` for the header `buf_ptr` load/store (4-byte aligned, offset 4).
fn buf_ptr_memarg() -> MemArg {
    MemArg {
        offset: TYPED_ARRAY_BUF_PTR_OFFSET as u64,
        align: 2,
        memory_index: 0,
    }
}

impl<'a> FuncContext<'a> {
    /// Resolve an expression to a typed-array descriptor when its class type
    /// names one of the registered typed-array variants. Returns `None` for
    /// any other expression — including expressions whose class is not yet
    /// inferable. Receiver-shaped queries (indexed access, `for..of`) call
    /// this; if it comes back `None`, they fall through to the regular
    /// `Array<T>` path.
    pub(crate) fn resolve_expr_typed_array(
        &self,
        expr: &Expression<'a>,
    ) -> Option<&'static TypedArrayDescriptor> {
        let class_name = self.resolve_expr_class(expr).ok()?;
        descriptor_for(&class_name)
    }

    /// `new Int32Array(arg)` / `new Float64Array(arg)` / `new Uint8Array(arg)`.
    /// Dispatches on `arg` shape: numeric literal / number variable → length
    /// form (zero-fill); array literal → literal-init; `Array<T>` or another
    /// typed array → memory.copy. Returns the typed-array pointer (i32).
    pub(crate) fn emit_new_typed_array(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        new_expr: &NewExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        if new_expr.arguments.len() != 1 {
            return Err(CompileError::codegen(format!(
                "new {}(...) requires exactly 1 argument: a length, an array literal, an Array<T>, or another typed array",
                desc.name
            )));
        }
        let arg = new_expr.arguments[0].to_expression();

        // Array-literal form: `new Int32Array([1, 2, 3])`. Stride-aware store
        // of each element; element count is known at compile time, so we
        // allocate header + n*stride in one bump.
        if let Expression::ArrayExpression(arr) = arg {
            return self.emit_typed_array_from_literal(desc, arr);
        }

        // Source-shape form: `new Int32Array(src)` where src is `Array<T>` or
        // another typed array. Resolved before the length form so a typed
        // local doesn't accidentally match the length path.
        if let Some(src_desc) = self.resolve_expr_typed_array(arg) {
            return self.emit_typed_array_from_typed_array(desc, src_desc, arg);
        }
        if let Some(src_elem_ty) = self.resolve_expr_array_elem(arg) {
            return self.emit_typed_array_from_array(desc, src_elem_ty, arg);
        }

        // Length form: `new Int32Array(n)`. Argument must be i32 (length); a
        // float length is a programming error in our typed subset.
        self.emit_typed_array_from_length(desc, arg)
    }

    /// Allocate a self-owned typed array of length `n` and zero-fill the body.
    /// The argument must be i32 — typed-array length is integer-shaped.
    fn emit_typed_array_from_length(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        len_expr: &Expression<'a>,
    ) -> Result<WasmType, CompileError> {
        let len_local = self.alloc_local(WasmType::I32);
        let len_ty = self.emit_expr(len_expr)?;
        if len_ty != WasmType::I32 {
            return Err(CompileError::type_err(format!(
                "new {}(n) length must be i32, got {len_ty:?}",
                desc.name
            )));
        }
        self.push(Instruction::LocalSet(len_local));

        // Allocate header + body in one bump: HEADER + len * stride.
        let ptr_local = self.emit_alloc_self_owned(desc, len_local)?;

        // Zero-fill the body. memory.fill(dst = ptr + HEADER, val = 0,
        // n = len * stride). Skip when len is statically zero (saves three
        // wasm ops in the common `new T(0)` corner case).
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(TYPED_ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(desc.byte_stride as i32));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryFill(0));

        self.push(Instruction::LocalGet(ptr_local));
        Ok(WasmType::I32)
    }

    /// Allocate a self-owned typed array sized exactly to the literal's
    /// element count, then emit per-element stores. Element count is known
    /// at compile time — no length local needed.
    fn emit_typed_array_from_literal(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        arr: &ArrayExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        // Reject holes / spread; spread is a runtime-length form we don't
        // need yet (and Array<T>'s spread path doesn't compose cleanly with
        // typed-array stride dispatch).
        for el in &arr.elements {
            match el {
                ArrayExpressionElement::SpreadElement(_) => {
                    return Err(CompileError::unsupported(format!(
                        "spread in `new {}([...])` is not supported yet — pass an existing typed array or Array<T> instead",
                        desc.name
                    )));
                }
                ArrayExpressionElement::Elision(_) => {
                    return Err(CompileError::unsupported(format!(
                        "hole in `new {}([...])` literal",
                        desc.name
                    )));
                }
                _ => {}
            }
        }

        let count = arr.elements.len() as u32;
        let total = TYPED_ARRAY_HEADER_SIZE + count * desc.byte_stride;
        self.push(Instruction::I32Const(total as i32));
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        // Header: len = count, buf_ptr = self + 8. Both writes use the
        // header-shaped MemArgs (i32 align=2).
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(count as i32));
        self.push(Instruction::I32Store(len_memarg()));
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(TYPED_ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Store(buf_ptr_memarg()));

        // Element stores. Body is inline at ptr + HEADER, so the descriptor's
        // store_op uses an immediate offset of (HEADER + i*stride).
        for (i, el) in arr.elements.iter().enumerate() {
            let expr = el.as_expression().ok_or_else(|| {
                CompileError::codegen(format!(
                    "unsupported element kind in `new {}([...])` literal",
                    desc.name
                ))
            })?;
            self.push(Instruction::LocalGet(ptr_local));
            let elem_ty = self.emit_expr(expr)?;
            self.coerce_value_to_typed_array_elem(desc, elem_ty, i)?;
            let offset = TYPED_ARRAY_HEADER_SIZE as u64 + (i as u64) * (desc.byte_stride as u64);
            self.push(desc.store_inst(offset));
        }

        self.push(Instruction::LocalGet(ptr_local));
        Ok(WasmType::I32)
    }

    /// `new Int32Array(src)` where `src` is an `Array<i32>`. Allocates a
    /// fresh typed array sized to `src.length` and `memory.copy`s the
    /// elements. Cross-stride conversion (e.g. `new Float64Array(int32_arr)`)
    /// would need an element-wise widen loop; for v1 we require matching
    /// element widths and document the constraint via the type-mismatch
    /// error.
    fn emit_typed_array_from_array(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        src_elem_ty: WasmType,
        src_expr: &Expression<'a>,
    ) -> Result<WasmType, CompileError> {
        if src_elem_ty != desc.elem_wasm_ty {
            return Err(CompileError::type_err(format!(
                "new {}(src) requires src element type {:?}, got {:?} — cross-width conversion is not supported in v1; map explicitly first",
                desc.name, desc.elem_wasm_ty, src_elem_ty
            )));
        }

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(src_expr)?;
        self.push(Instruction::LocalSet(src_local));

        // src is an `Array<T>`: header is [len][cap], body starts at +8.
        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(src_local));
        self.push(Instruction::I32Load(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        self.push(Instruction::LocalSet(len_local));

        let ptr_local = self.emit_alloc_self_owned(desc, len_local)?;

        // memory.copy(dst = ptr + HEADER, src = src_ptr + ARRAY_HEADER, n = len * stride).
        // Both src and dst headers are 8 bytes, so the constants line up.
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(TYPED_ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(src_local));
        self.push(Instruction::I32Const(crate::codegen::expr::ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(desc.byte_stride as i32));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        self.push(Instruction::LocalGet(ptr_local));
        Ok(WasmType::I32)
    }

    /// `new Int32Array(src)` where `src` is another typed array. Reads
    /// `src.buf_ptr` (offset 4) so view sources work transparently — the
    /// new array always ends up self-owned, regardless of whether `src`
    /// was a view or self-owned.
    fn emit_typed_array_from_typed_array(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        src_desc: &'static TypedArrayDescriptor,
        src_expr: &Expression<'a>,
    ) -> Result<WasmType, CompileError> {
        if src_desc.byte_stride != desc.byte_stride {
            return Err(CompileError::type_err(format!(
                "new {}(src) requires src to be the same kind (got {}); cross-kind copy is deferred to sub-phase 3 (`set` / `subarray`)",
                desc.name, src_desc.name
            )));
        }

        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(src_expr)?;
        self.push(Instruction::LocalSet(src_local));

        let len_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(src_local));
        self.push(Instruction::I32Load(len_memarg()));
        self.push(Instruction::LocalSet(len_local));

        let ptr_local = self.emit_alloc_self_owned(desc, len_local)?;

        // memory.copy(dst = ptr + HEADER, src = src.buf_ptr, n = len * stride).
        // Reading buf_ptr (not src + HEADER) means views copy through correctly.
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(TYPED_ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(src_local));
        self.push(Instruction::I32Load(buf_ptr_memarg()));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Const(desc.byte_stride as i32));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        self.push(Instruction::LocalGet(ptr_local));
        Ok(WasmType::I32)
    }

    /// Shared sub-routine: allocate `HEADER + len * stride` bytes via the
    /// arena, write `len` and `buf_ptr = self + HEADER` into the header,
    /// return the pointer local. Caller is responsible for filling the body
    /// (zero-init on length, store-each on literal, memory.copy on source).
    fn emit_alloc_self_owned(
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

        // Header: len at +0, buf_ptr = ptr + HEADER at +4.
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

    /// Coerce a value-on-stack to the typed-array's element type. i32→f64
    /// promotes (matches the `Array<T>` literal behavior); other mismatches
    /// are a type error. Used by `new T([...])` and `T.of(...)`.
    fn coerce_value_to_typed_array_elem(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        actual: WasmType,
        idx: usize,
    ) -> Result<(), CompileError> {
        if actual == desc.elem_wasm_ty {
            return Ok(());
        }
        if desc.elem_wasm_ty == WasmType::F64 && actual == WasmType::I32 {
            self.push(Instruction::F64ConvertI32S);
            return Ok(());
        }
        Err(CompileError::type_err(format!(
            "{} element {idx} has type {actual:?}, expected {:?}",
            desc.name, desc.elem_wasm_ty
        )))
    }

    /// `Int32Array.of(...args)` — build a self-owned typed array containing
    /// the argument list. Same shape as the literal-init path but the
    /// element exprs come from the argument list, not the literal.
    pub(crate) fn emit_typed_array_static_of(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        let count = call.arguments.len() as u32;
        let total = TYPED_ARRAY_HEADER_SIZE + count * desc.byte_stride;
        self.push(Instruction::I32Const(total as i32));
        let ptr_local = self.emit_arena_alloc_to_local(true)?;

        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(count as i32));
        self.push(Instruction::I32Store(len_memarg()));
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::LocalGet(ptr_local));
        self.push(Instruction::I32Const(TYPED_ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::I32Store(buf_ptr_memarg()));

        for (i, arg) in call.arguments.iter().enumerate() {
            self.push(Instruction::LocalGet(ptr_local));
            let elem_ty = self.emit_expr(arg.to_expression())?;
            self.coerce_value_to_typed_array_elem(desc, elem_ty, i)?;
            let offset = TYPED_ARRAY_HEADER_SIZE as u64 + (i as u64) * (desc.byte_stride as u64);
            self.push(desc.store_inst(offset));
        }

        self.push(Instruction::LocalGet(ptr_local));
        Ok(())
    }

    /// `Int32Array.from(src)` — same shape as `new Int32Array(src)`.
    /// `Int32Array.from(src, mapFn)` runs the arrow body once per element
    /// and stores the result. The arrow's body type must match the
    /// descriptor's `elem_wasm_ty` (with i32→f64 promotion allowed).
    pub(crate) fn emit_typed_array_static_from(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        call: &CallExpression<'a>,
    ) -> Result<(), CompileError> {
        if call.arguments.is_empty() || call.arguments.len() > 2 {
            return Err(CompileError::codegen(format!(
                "{}.from expects 1 or 2 arguments: {}.from(src) or {}.from(src, mapFn)",
                desc.name, desc.name, desc.name
            )));
        }
        let src_expr = call.arguments[0].to_expression();

        // No mapFn: equivalent to `new T(src)`. Reuse the same dispatch logic
        // so all source shapes (Array<T>, typed array, literal) compose.
        if call.arguments.len() == 1 {
            if let Expression::ArrayExpression(arr) = src_expr {
                self.emit_typed_array_from_literal(desc, arr)?;
            } else if let Some(src_desc) = self.resolve_expr_typed_array(src_expr) {
                self.emit_typed_array_from_typed_array(desc, src_desc, src_expr)?;
            } else if let Some(src_elem) = self.resolve_expr_array_elem(src_expr) {
                self.emit_typed_array_from_array(desc, src_elem, src_expr)?;
            } else {
                return Err(CompileError::type_err(format!(
                    "{}.from(src) requires src to be an Array<T> or another typed array",
                    desc.name
                )));
            }
            return Ok(());
        }

        // With mapFn — element type must come from the arrow body, not the
        // source. Tolerate either array or typed-array sources.
        let map_fn = call.arguments[1].to_expression();
        self.emit_typed_array_from_with_map(desc, src_expr, map_fn)
    }

    /// `T.from(src, mapFn)` — element-by-element loop over `src`, store each
    /// `mapFn(value, index)` into the freshly-allocated typed array.
    fn emit_typed_array_from_with_map(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        src_expr: &Expression<'a>,
        map_fn: &Expression<'a>,
    ) -> Result<(), CompileError> {
        use crate::codegen::array_builtins::{
            eval_arrow_body, extract_arrow, restore_arrow_scope, setup_arrow_scope,
        };

        let arrow = extract_arrow(map_fn)?;
        let mut params: Vec<String> = Vec::new();
        for p in &arrow.params.items {
            match &p.pattern {
                BindingPattern::BindingIdentifier(id) => params.push(id.name.as_str().to_string()),
                _ => {
                    return Err(CompileError::unsupported(format!(
                        "{}.from(src, fn): mapFn parameter must be a simple identifier",
                        desc.name
                    )));
                }
            }
        }
        if params.is_empty() || params.len() > 2 {
            return Err(CompileError::codegen(format!(
                "{}.from(src, fn): mapFn must take 1 or 2 parameters (value, index)",
                desc.name
            )));
        }

        // Resolve the source's element shape so we know how to load each
        // element to feed into the arrow body. Both Array<T> and typed
        // arrays are accepted; the per-source emit differs in header size
        // and load opcode.
        let (src_load_setup, src_elem_ty) =
            self.eval_typed_array_from_source(desc, src_expr)?;
        // src_load_setup tells us how to materialise element addresses inside
        // the loop — we hold src ptr / len / body base in locals.

        let SourceBinding {
            len_local,
            body_base_local,
            elem_load_inst,
        } = src_load_setup;

        // Allocate the destination using the source length.
        let dst_ptr = self.emit_alloc_self_owned(desc, len_local)?;

        // Loop counter.
        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));

        // if i >= len: break
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1));

        // value = src[i] — load into a local of the source element type so
        // the arrow scope can bind it.
        let value_local = self.alloc_local(src_elem_ty);
        self.push(Instruction::LocalGet(body_base_local));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(src_load_setup_stride(src_elem_ty)));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(elem_load_inst);
        self.push(Instruction::LocalSet(value_local));

        // Bind arrow params: value, index.
        let mut param_locals: Vec<(u32, WasmType)> = vec![(value_local, src_elem_ty)];
        let mut param_classes: Vec<Option<String>> = vec![None];
        if params.len() == 2 {
            param_locals.push((i_local, WasmType::I32));
            param_classes.push(None);
        }
        let scope = setup_arrow_scope(self, &params, &param_locals, &param_classes);

        // Pre-compute store address: dst + HEADER + i * stride. Then evaluate
        // the arrow body (which leaves the result on the stack), then store.
        self.push(Instruction::LocalGet(dst_ptr));
        self.push(Instruction::I32Const(TYPED_ARRAY_HEADER_SIZE as i32));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(desc.byte_stride as i32));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);

        let body_ty = eval_arrow_body(self, arrow)?;
        self.coerce_value_to_typed_array_elem(desc, body_ty, 0)?;
        self.push(desc.store_inst(0));

        restore_arrow_scope(self, scope);

        // i++
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));
        self.push(Instruction::Br(0));

        self.push(Instruction::End); // loop
        self.push(Instruction::End); // block

        self.push(Instruction::LocalGet(dst_ptr));
        Ok(())
    }

    /// Evaluate a `T.from`-style source into locals: pointer to body base,
    /// length, and the per-element load instruction. Source may be an
    /// `Array<T>` (body at src + ARRAY_HEADER) or a typed array (body at
    /// src.buf_ptr).
    fn eval_typed_array_from_source(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        src_expr: &Expression<'a>,
    ) -> Result<(SourceBinding, WasmType), CompileError> {
        if let Some(src_desc) = self.resolve_expr_typed_array(src_expr) {
            // Typed-array source.
            let src_local = self.alloc_local(WasmType::I32);
            self.emit_expr(src_expr)?;
            self.push(Instruction::LocalSet(src_local));

            let len_local = self.alloc_local(WasmType::I32);
            self.push(Instruction::LocalGet(src_local));
            self.push(Instruction::I32Load(len_memarg()));
            self.push(Instruction::LocalSet(len_local));

            let body_base = self.alloc_local(WasmType::I32);
            self.push(Instruction::LocalGet(src_local));
            self.push(Instruction::I32Load(buf_ptr_memarg()));
            self.push(Instruction::LocalSet(body_base));

            let binding = SourceBinding {
                len_local,
                body_base_local: body_base,
                elem_load_inst: src_desc.load_inst(0),
            };
            return Ok((binding, src_desc.elem_wasm_ty));
        }

        if let Some(src_elem) = self.resolve_expr_array_elem(src_expr) {
            let src_local = self.alloc_local(WasmType::I32);
            self.emit_expr(src_expr)?;
            self.push(Instruction::LocalSet(src_local));

            let len_local = self.alloc_local(WasmType::I32);
            self.push(Instruction::LocalGet(src_local));
            self.push(Instruction::I32Load(MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
            self.push(Instruction::LocalSet(len_local));

            // For Array<T> the body lives at src + ARRAY_HEADER (8). Compute
            // it once into a local so the loop body just adds i*stride.
            let body_base = self.alloc_local(WasmType::I32);
            self.push(Instruction::LocalGet(src_local));
            self.push(Instruction::I32Const(crate::codegen::expr::ARRAY_HEADER_SIZE as i32));
            self.push(Instruction::I32Add);
            self.push(Instruction::LocalSet(body_base));

            let load_inst = match src_elem {
                WasmType::F64 => Instruction::F64Load(MemArg {
                    offset: 0,
                    align: 3,
                    memory_index: 0,
                }),
                WasmType::I32 => Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }),
                _ => {
                    return Err(CompileError::type_err(format!(
                        "{}.from(src, fn): src element type {src_elem:?} is not supported",
                        desc.name
                    )));
                }
            };

            return Ok((
                SourceBinding {
                    len_local,
                    body_base_local: body_base,
                    elem_load_inst: load_inst,
                },
                src_elem,
            ));
        }

        Err(CompileError::type_err(format!(
            "{}.from(src, fn): src must be an Array<T> or another typed array",
            desc.name
        )))
    }
}

/// Holds the per-source pieces an `T.from(src, fn)` loop body needs after the
/// source has been evaluated and split into header / body locals. Built once
/// up front so the loop emit is a tight sequence with no source re-evaluation.
struct SourceBinding {
    len_local: u32,
    body_base_local: u32,
    elem_load_inst: Instruction<'static>,
}

impl<'a> FuncContext<'a> {
    /// Typed-array instance property dispatch — `ta.length` / `ta.byteLength`.
    /// `length` is `i32.load @ +0`; `byteLength` multiplies by `byte_stride`
    /// (constant-fold for stride 1). Anything else is an error — typed arrays
    /// have a deliberately small property surface.
    pub(crate) fn emit_typed_array_property(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        member: &StaticMemberExpression<'a>,
        field_name: &str,
    ) -> Result<WasmType, CompileError> {
        match field_name {
            "length" => {
                self.emit_expr(&member.object)?;
                self.push(Instruction::I32Load(len_memarg()));
                Ok(WasmType::I32)
            }
            "byteLength" => {
                self.emit_expr(&member.object)?;
                self.push(Instruction::I32Load(len_memarg()));
                if desc.byte_stride != 1 {
                    self.push(Instruction::I32Const(desc.byte_stride as i32));
                    self.push(Instruction::I32Mul);
                }
                Ok(WasmType::I32)
            }
            _ => Err(CompileError::codegen(format!(
                "{} has no property '{field_name}' — supported: length, byteLength",
                desc.name
            ))),
        }
    }

    /// Typed-array static property dispatch — `Int32Array.BYTES_PER_ELEMENT`.
    /// Returns `Ok(Some(ty))` when the property is recognized so the caller
    /// can route around the regular instance dispatch; `Ok(None)` for any
    /// other property (defers to the regular member-access machinery, which
    /// will error since the typed-array layout has no real fields).
    pub(crate) fn try_emit_typed_array_static_property(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        property_name: &str,
    ) -> Result<Option<WasmType>, CompileError> {
        match property_name {
            "BYTES_PER_ELEMENT" => {
                self.push(Instruction::I32Const(desc.bytes_per_element as i32));
                Ok(Some(WasmType::I32))
            }
            _ => Ok(None),
        }
    }

    /// Emit `ta[i]` — bounds-checked typed-array element read. Loads
    /// `buf_ptr` from offset 4, computes `buf_ptr + i*stride`, dispatches
    /// the descriptor's `load_op` against it. The extra `buf_ptr` load is
    /// the layout-tax discussed in the plan; Cranelift hoists it for loops
    /// (GVN + LICM) so the inner cost matches a flat layout.
    pub(crate) fn emit_typed_array_indexed_read(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        member: &ComputedMemberExpression<'a>,
    ) -> Result<WasmType, CompileError> {
        let ta_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&member.object)?;
        self.push(Instruction::LocalSet(ta_local));

        let idx_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&member.expression)?;
        self.push(Instruction::LocalSet(idx_local));

        // Bounds check: trap if idx >= len. Unsigned compare catches negative
        // indices too. Same shape as Array<T>'s bounds check, just inlined
        // here so the typed-array path is self-contained.
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::LocalGet(ta_local));
        self.push(Instruction::I32Load(len_memarg()));
        self.push(Instruction::I32GeU);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::Unreachable);
        self.push(Instruction::End);

        // addr = buf_ptr + i * stride. For stride 1 (Uint8Array) the multiply
        // is a no-op but we still emit it — the wasm validator/optimizer
        // folds `i * 1` cheaply, and gating the const-load on stride keeps
        // the dispatch table simpler.
        self.push(Instruction::LocalGet(ta_local));
        self.push(Instruction::I32Load(buf_ptr_memarg()));
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::I32Const(desc.byte_stride as i32));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(desc.load_inst(0));

        Ok(desc.elem_wasm_ty)
    }

    /// Emit `ta[i] = v` (or `+=` / `-=` / `*=` / `/=`) — bounds-checked
    /// typed-array element write through `buf_ptr`.
    pub(crate) fn emit_typed_array_indexed_write(
        &mut self,
        desc: &'static TypedArrayDescriptor,
        member: &ComputedMemberExpression<'a>,
        value: &Expression<'a>,
        operator: AssignmentOperator,
    ) -> Result<WasmType, CompileError> {
        let ta_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&member.object)?;
        self.push(Instruction::LocalSet(ta_local));

        let idx_local = self.alloc_local(WasmType::I32);
        self.emit_expr(&member.expression)?;
        self.push(Instruction::LocalSet(idx_local));

        // Bounds check.
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::LocalGet(ta_local));
        self.push(Instruction::I32Load(len_memarg()));
        self.push(Instruction::I32GeU);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::Unreachable);
        self.push(Instruction::End);

        // addr = buf_ptr + i * stride; cache so compound ops can read & store
        // through the same address.
        let addr_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(ta_local));
        self.push(Instruction::I32Load(buf_ptr_memarg()));
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::I32Const(desc.byte_stride as i32));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(addr_local));

        self.push(Instruction::LocalGet(addr_local));
        if operator == AssignmentOperator::Assign {
            let value_ty = self.emit_expr(value)?;
            self.coerce_value_to_typed_array_elem(desc, value_ty, 0)?;
        } else {
            // Compound: load through the same address, op, then store.
            self.push(Instruction::LocalGet(addr_local));
            self.push(desc.load_inst(0));
            let value_ty = self.emit_expr(value)?;
            self.coerce_value_to_typed_array_elem(desc, value_ty, 0)?;
            let is_f64 = desc.elem_wasm_ty == WasmType::F64;
            match operator {
                AssignmentOperator::Addition => self.push(if is_f64 {
                    Instruction::F64Add
                } else {
                    Instruction::I32Add
                }),
                AssignmentOperator::Subtraction => self.push(if is_f64 {
                    Instruction::F64Sub
                } else {
                    Instruction::I32Sub
                }),
                AssignmentOperator::Multiplication => self.push(if is_f64 {
                    Instruction::F64Mul
                } else {
                    Instruction::I32Mul
                }),
                AssignmentOperator::Division => self.push(if is_f64 {
                    Instruction::F64Div
                } else {
                    Instruction::I32DivS
                }),
                _ => {
                    return Err(CompileError::unsupported(format!(
                        "compound typed-array element assignment with operator {operator:?}"
                    )));
                }
            }
        }
        self.push(desc.store_inst(0));

        Ok(WasmType::Void)
    }
}

/// Source-element stride in bytes — used to compute `body_base + i * stride`
/// inside the `T.from(src, mapFn)` loop. Source can be any element width
/// independent of the destination (e.g. `Float64Array.from(int32Arr, ...)`),
/// so we can't borrow the destination descriptor's stride.
fn src_load_setup_stride(src_elem: WasmType) -> i32 {
    match src_elem {
        WasmType::F64 => 8,
        _ => 4,
    }
}
