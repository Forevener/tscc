use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

use super::{
    ARRAY_HEADER_SIZE, elem_size, emit_alloc_array, emit_arr_length, emit_elem_addr,
    emit_elem_load, emit_elem_store, eval_arrow_body, extract_arrow, extract_arrow_params,
    restore_arrow_scope, setup_arrow_scope,
};

impl<'a> FuncContext<'a> {
    /// arr.sort() / arr.sort((a, b) => ...) — in-place bottom-up iterative
    /// merge sort. Without a comparator, elements are compared by numeric
    /// natural order (ascending) — this diverges from ES (which stringifies
    /// and lexicographically compares) but matches what typed-subset users
    /// expect and avoids the cost of a ToString per comparison.
    pub(super) fn emit_array_sort(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: Option<&Expression<'a>>,
    ) -> Result<(), CompileError> {
        let comparator = Self::extract_sort_comparator(callback, "sort")?;

        // Evaluate array pointer, load length.
        let arr_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(arr_local));
        let len_local = self.alloc_local(WasmType::I32);
        emit_arr_length(self, arr_local);
        self.push(Instruction::LocalSet(len_local));

        self.emit_merge_sort_in_place(
            arr_local,
            len_local,
            elem_ty,
            elem_class,
            comparator.as_ref().map(|(p, a)| (p.as_slice(), *a)),
        )
    }

    /// arr.toSorted() / arr.toSorted((a, b) => ...) — ES2023 immutable sort.
    /// Clones the source array into a fresh arena allocation and sorts the
    /// clone, returning it. The source is untouched. Comparator semantics
    /// match `sort` (optional; numeric natural-order default).
    pub(crate) fn emit_array_to_sorted(
        &mut self,
        arr_expr: &Expression<'a>,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        callback: Option<&Expression<'a>>,
    ) -> Result<(), CompileError> {
        let comparator = Self::extract_sort_comparator(callback, "toSorted")?;
        let esize = elem_size(elem_ty)?;

        // Evaluate source pointer, load length.
        let src_local = self.alloc_local(WasmType::I32);
        self.emit_expr(arr_expr)?;
        self.push(Instruction::LocalSet(src_local));
        let len_local = self.alloc_local(WasmType::I32);
        emit_arr_length(self, src_local);
        self.push(Instruction::LocalSet(len_local));

        // Allocate the result array (header written by helper with length=0),
        // then set length=len and copy source body into it.
        let new_ptr = emit_alloc_array(self, len_local, elem_ty)?;
        self.push(Instruction::LocalGet(new_ptr));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        // memory.copy(new+HEADER, src+HEADER, len*esize)
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

        // Sort the clone in place, then return it.
        self.emit_merge_sort_in_place(
            new_ptr,
            len_local,
            elem_ty,
            elem_class,
            comparator.as_ref().map(|(p, a)| (p.as_slice(), *a)),
        )?;
        self.push(Instruction::LocalGet(new_ptr));
        Ok(())
    }

    /// Pull the arrow + param names out of an optional comparator callback.
    /// Returns `None` when the caller passed no argument (the default
    /// numeric-order comparator is emitted inline by `emit_merge_sort_in_place`).
    fn extract_sort_comparator<'b>(
        callback: Option<&'b Expression<'a>>,
        method: &str,
    ) -> Result<Option<(Vec<String>, &'b ArrowFunctionExpression<'a>)>, CompileError> {
        let Some(cb) = callback else {
            return Ok(None);
        };
        let arrow = extract_arrow(cb)?;
        let params = extract_arrow_params(arrow)?;
        if params.len() != 2 {
            return Err(CompileError::codegen(format!(
                "{method} comparator must have exactly 2 parameters"
            )));
        }
        Ok(Some((params, arrow)))
    }

    /// Bottom-up iterative merge sort over `arr_local[0..len_local)`.
    /// Assumes the caller has already loaded the array pointer and length into
    /// locals. Allocates a same-sized temp buffer via the arena. After the final
    /// pass, the sorted data lives in `arr_local`.
    fn emit_merge_sort_in_place(
        &mut self,
        arr_local: u32,
        len_local: u32,
        elem_ty: WasmType,
        elem_class: Option<&str>,
        comparator: Option<(&[String], &ArrowFunctionExpression<'a>)>,
    ) -> Result<(), CompileError> {
        let esize = elem_size(elem_ty)?;

        // Allocate temp buffer via arena (same capacity as arr)
        let tmp_local = emit_alloc_array(self, len_local, elem_ty)?;

        // Merge sort locals
        let width_local = self.alloc_local(WasmType::I32);
        let i_local = self.alloc_local(WasmType::I32);
        let mid_local = self.alloc_local(WasmType::I32);
        let right_local = self.alloc_local(WasmType::I32);
        let l_local = self.alloc_local(WasmType::I32);
        let r_local = self.alloc_local(WasmType::I32);
        let k_local = self.alloc_local(WasmType::I32);
        let a_local = self.alloc_local(elem_ty);
        let b_local = self.alloc_local(elem_ty);
        let copy_idx = self.alloc_local(WasmType::I32);

        // width = 1
        self.push(Instruction::I32Const(1));
        self.push(Instruction::LocalSet(width_local));

        // === Outer loop: while width < len ===
        self.push(Instruction::Block(wasm_encoder::BlockType::Empty)); // $outer_break (depth 0 from block = br(1) from loop)
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty)); // $outer_loop

        // if width >= len, break
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break outer

        // i = 0
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        // === Inner loop: while i < len ===
        self.push(Instruction::Block(wasm_encoder::BlockType::Empty)); // $inner_break
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty)); // $inner_loop

        // if i >= len, break inner
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break inner

        // mid = min(i + width, len)
        // Emit: i + width
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Add);
        // Emit: min(i+width, len) using select: a, b, a < b => if true pick a else b
        self.push(Instruction::LocalGet(len_local));
        // Stack: [i+width, len]. We need: a, b, cond => select picks a if cond!=0
        // We want min(i+width, len): pick i+width if i+width < len, else pick len
        // select(a, b, cond): returns a if cond != 0, b otherwise
        // So: select(i+width, len, i+width < len)
        // But stack is [i+width, len] right now — need to dup for comparison.
        // Easier to use locals:
        self.push(Instruction::LocalSet(mid_local)); // mid_local = len (temporarily)
        // Recompute i+width and do comparison
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Add);
        // Stack: [i+width]
        self.push(Instruction::LocalGet(mid_local)); // [i+width, len]
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Add); // [i+width, len, i+width]
        self.push(Instruction::LocalGet(mid_local)); // [i+width, len, i+width, len]
        self.push(Instruction::I32LtS); // [i+width, len, i+width < len]
        self.push(Instruction::Select); // min(i+width, len)
        self.push(Instruction::LocalSet(mid_local));

        // right = min(i + 2*width, len)
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Const(2));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        // Stack: [i+2w]
        self.push(Instruction::LocalGet(len_local)); // [i+2w, len]
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Const(2));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add); // [i+2w, len, i+2w]
        self.push(Instruction::LocalGet(len_local)); // [i+2w, len, i+2w, len]
        self.push(Instruction::I32LtS); // [i+2w, len, i+2w < len]
        self.push(Instruction::Select); // min(i+2w, len)
        self.push(Instruction::LocalSet(right_local));

        // l = i (which is left)
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalSet(l_local));

        // r = mid
        self.push(Instruction::LocalGet(mid_local));
        self.push(Instruction::LocalSet(r_local));

        // k = i (which is left)
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalSet(k_local));

        // === Merge loop: while l < mid && r < right ===
        self.push(Instruction::Block(wasm_encoder::BlockType::Empty)); // $merge_break
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty)); // $merge_loop

        // if l >= mid, break merge
        self.push(Instruction::LocalGet(l_local));
        self.push(Instruction::LocalGet(mid_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break merge

        // if r >= right, break merge
        self.push(Instruction::LocalGet(r_local));
        self.push(Instruction::LocalGet(right_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break merge

        // Load arr[l] into a_local
        emit_elem_addr(self, arr_local, l_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(a_local));

        // Load arr[r] into b_local
        emit_elem_addr(self, arr_local, r_local, esize);
        emit_elem_load(self, elem_ty);
        self.push(Instruction::LocalSet(b_local));

        // Evaluate compare(a, b). With a user-supplied comparator we inline its
        // arrow body; without one we emit `(a > b) - (a < b)` so the result is
        // -1 / 0 / +1 and the existing `<= 0 → take left` branch is correct.
        let cmp_ty = match comparator {
            Some((params, arrow)) => {
                let scope = setup_arrow_scope(
                    self,
                    params,
                    &[(a_local, elem_ty), (b_local, elem_ty)],
                    &[
                        elem_class.map(|s| s.to_string()),
                        elem_class.map(|s| s.to_string()),
                    ],
                );
                let ty = eval_arrow_body(self, arrow)?;
                restore_arrow_scope(self, scope);
                ty
            }
            None => {
                self.push(Instruction::LocalGet(a_local));
                self.push(Instruction::LocalGet(b_local));
                match elem_ty {
                    WasmType::I32 => self.push(Instruction::I32GtS),
                    WasmType::F64 => self.push(Instruction::F64Gt),
                    _ => {
                        return Err(CompileError::type_err(
                            "sort elements must be i32 or f64 for default comparator",
                        ));
                    }
                }
                self.push(Instruction::LocalGet(a_local));
                self.push(Instruction::LocalGet(b_local));
                match elem_ty {
                    WasmType::I32 => self.push(Instruction::I32LtS),
                    WasmType::F64 => self.push(Instruction::F64Lt),
                    _ => unreachable!(),
                }
                self.push(Instruction::I32Sub);
                WasmType::I32
            }
        };

        // if compare(a, b) <= 0: copy arr[l] to tmp[k], l++
        // else: copy arr[r] to tmp[k], r++
        match cmp_ty {
            WasmType::I32 => {
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32LeS);
            }
            WasmType::F64 => {
                self.push(Instruction::F64Const(0.0f64));
                self.push(Instruction::F64Le);
            }
            _ => {
                return Err(CompileError::type_err(
                    "sort comparator must return i32 or f64",
                ));
            }
        }

        // if-else: comparator <= 0 means take from left
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));

        // tmp[k] = a_local (arr[l])
        emit_elem_addr(self, tmp_local, k_local, esize);
        self.push(Instruction::LocalGet(a_local));
        emit_elem_store(self, elem_ty);
        // l++
        self.push(Instruction::LocalGet(l_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(l_local));

        self.push(Instruction::Else);

        // tmp[k] = b_local (arr[r])
        emit_elem_addr(self, tmp_local, k_local, esize);
        self.push(Instruction::LocalGet(b_local));
        emit_elem_store(self, elem_ty);
        // r++
        self.push(Instruction::LocalGet(r_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(r_local));

        self.push(Instruction::End); // end if-else

        // k++
        self.push(Instruction::LocalGet(k_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(k_local));

        self.push(Instruction::Br(0)); // continue merge loop
        self.push(Instruction::End); // end merge loop
        self.push(Instruction::End); // end merge block

        // === Copy remaining left elements: while l < mid ===
        self.push(Instruction::Block(wasm_encoder::BlockType::Empty)); // $left_break
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty)); // $left_loop

        self.push(Instruction::LocalGet(l_local));
        self.push(Instruction::LocalGet(mid_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break left

        // tmp[k] = arr[l]
        emit_elem_addr(self, tmp_local, k_local, esize);
        emit_elem_addr(self, arr_local, l_local, esize);
        emit_elem_load(self, elem_ty);
        emit_elem_store(self, elem_ty);

        // l++
        self.push(Instruction::LocalGet(l_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(l_local));
        // k++
        self.push(Instruction::LocalGet(k_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(k_local));

        self.push(Instruction::Br(0)); // continue left loop
        self.push(Instruction::End); // end left loop
        self.push(Instruction::End); // end left block

        // === Copy remaining right elements: while r < right ===
        self.push(Instruction::Block(wasm_encoder::BlockType::Empty)); // $right_break
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty)); // $right_loop

        self.push(Instruction::LocalGet(r_local));
        self.push(Instruction::LocalGet(right_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break right

        // tmp[k] = arr[r]
        emit_elem_addr(self, tmp_local, k_local, esize);
        emit_elem_addr(self, arr_local, r_local, esize);
        emit_elem_load(self, elem_ty);
        emit_elem_store(self, elem_ty);

        // r++
        self.push(Instruction::LocalGet(r_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(r_local));
        // k++
        self.push(Instruction::LocalGet(k_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(k_local));

        self.push(Instruction::Br(0)); // continue right loop
        self.push(Instruction::End); // end right loop
        self.push(Instruction::End); // end right block

        // i += 2 * width
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Const(2));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));

        self.push(Instruction::Br(0)); // continue inner loop
        self.push(Instruction::End); // end inner loop
        self.push(Instruction::End); // end inner block

        // === Copy tmp data back to arr: for copy_idx = 0; copy_idx < len; copy_idx++ ===
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(copy_idx));

        self.push(Instruction::Block(wasm_encoder::BlockType::Empty)); // $copy_break
        self.push(Instruction::Loop(wasm_encoder::BlockType::Empty)); // $copy_loop

        self.push(Instruction::LocalGet(copy_idx));
        self.push(Instruction::LocalGet(len_local));
        self.push(Instruction::I32GeS);
        self.push(Instruction::BrIf(1)); // break copy

        // arr[copy_idx] = tmp[copy_idx]
        emit_elem_addr(self, arr_local, copy_idx, esize);
        emit_elem_addr(self, tmp_local, copy_idx, esize);
        emit_elem_load(self, elem_ty);
        emit_elem_store(self, elem_ty);

        // copy_idx++
        self.push(Instruction::LocalGet(copy_idx));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(copy_idx));

        self.push(Instruction::Br(0)); // continue copy loop
        self.push(Instruction::End); // end copy loop
        self.push(Instruction::End); // end copy block

        // width *= 2
        self.push(Instruction::LocalGet(width_local));
        self.push(Instruction::I32Const(2));
        self.push(Instruction::I32Mul);
        self.push(Instruction::LocalSet(width_local));

        self.push(Instruction::Br(0)); // continue outer loop
        self.push(Instruction::End); // end outer loop
        self.push(Instruction::End); // end outer block

        Ok(())
    }
}
