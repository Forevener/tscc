//! Per-monomorphization method dispatch for compiler-owned `Set<T>`.
//!
//! Set methods are emitted inline at each call site, mirroring the Map path:
//! per-(T) specialization avoids a call boundary and lets us pick the right
//! hash + equality helpers without generic dispatch overhead. Returns
//! `Ok(None)` when the call's receiver isn't a Set so upstream dispatchers
//! keep their normal fall-through behavior.
//!
//! Shares semantics with `expr/map.rs` (same probing, same insertion-chain
//! bookkeeping, same rebuild-on-grow policy). The differences from Map:
//!
//! - Bucket has no value slot — `add(v)` writes only the element.
//! - `forEach((v) => ...)` takes exactly one parameter.
//! - Methods use element-centric names: `add` instead of `set`, a single
//!   argument throughout.

use oxc_ast::ast::*;
use wasm_encoder::{BlockType, Instruction, MemArg};

use crate::codegen::array_builtins::extract_arrow;
use crate::codegen::func::FuncContext;
use crate::codegen::hash_table::{
    BUCKET_EMPTY, BUCKET_OCCUPIED, BUCKET_TOMBSTONE, EMPTY_LINK, load_i32, load_typed, store_i32,
    store_typed,
};
use crate::codegen::set_builtins::SetInfo;
use crate::error::CompileError;
use crate::types::{BoundType, ClosureSig, WasmType};

impl<'a> FuncContext<'a> {
    /// Entry point invoked from `emit_call`. If the call is
    /// `<setExpr>.<method>(...)` and the receiver resolves to a known Set
    /// monomorphization, emits the method inline and returns its type.
    pub(crate) fn try_emit_set_method_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        let member = match &call.callee {
            Expression::StaticMemberExpression(m) => m,
            _ => return Ok(None),
        };
        let class_name = match self.resolve_expr_class(&member.object) {
            Ok(name) => name,
            Err(_) => return Ok(None),
        };
        if !self.module_ctx.set_info.contains_key(&class_name) {
            return Ok(None);
        }
        let method_name = member.property.name.as_str();
        match method_name {
            "clear" => {
                self.expect_args(call, 0, "Set.clear")?;
                self.emit_set_clear(&member.object, &class_name)?;
                Ok(Some(WasmType::Void))
            }
            "has" => {
                self.expect_args(call, 1, "Set.has")?;
                let arg = call.arguments[0].to_expression();
                self.emit_set_has(&member.object, &class_name, arg)?;
                Ok(Some(WasmType::I32))
            }
            "add" => {
                self.expect_args(call, 1, "Set.add")?;
                let arg = call.arguments[0].to_expression();
                self.emit_set_add(&member.object, &class_name, arg)?;
                Ok(Some(WasmType::Void))
            }
            "delete" => {
                self.expect_args(call, 1, "Set.delete")?;
                let arg = call.arguments[0].to_expression();
                self.emit_set_delete(&member.object, &class_name, arg)?;
                Ok(Some(WasmType::I32))
            }
            "forEach" => {
                self.expect_args(call, 1, "Set.forEach")?;
                let arg = call.arguments[0].to_expression();
                self.emit_set_foreach(&member.object, &class_name, arg)?;
                Ok(Some(WasmType::Void))
            }
            other => Err(CompileError::codegen(format!(
                "Set has no method '{other}' — supported: clear, has, add, delete, forEach"
            ))),
        }
    }

    /// `s.clear()` — reset size=0, head/tail=-1, and zero every state byte in
    /// the bucket array via a single `memory.fill`. `buckets_ptr` + `capacity`
    /// stay as-is so the set is reusable without re-allocating.
    fn emit_set_clear(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
    ) -> Result<(), CompileError> {
        let info = self.set_info(class_name);
        let size_off = self.set_field_offset_for(class_name, "size");
        let head_off = self.set_field_offset_for(class_name, "head_idx");
        let tail_off = self.set_field_offset_for(class_name, "tail_idx");
        let buckets_off = self.set_field_offset_for(class_name, "buckets_ptr");
        let capacity_off = self.set_field_offset_for(class_name, "capacity");

        let this_local = self.alloc_local(WasmType::I32);
        self.emit_expr(receiver)?;
        self.push(Instruction::LocalSet(this_local));

        // memory.fill(dst=buckets_ptr, val=0, n=capacity * bucket_size).
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(buckets_off));
        self.push(Instruction::I32Const(BUCKET_EMPTY));
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(capacity_off));
        self.push(Instruction::I32Const(info.bucket.total_size as i32));
        self.push(Instruction::I32Mul);
        self.push(Instruction::MemoryFill(0));

        // size = 0
        self.push(Instruction::LocalGet(this_local));
        self.push(Instruction::I32Const(0));
        self.push(store_i32(size_off));

        // head_idx = -1, tail_idx = -1
        self.push(Instruction::LocalGet(this_local));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(store_i32(head_off));
        self.push(Instruction::LocalGet(this_local));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(store_i32(tail_off));

        Ok(())
    }

    /// `s.has(v)` — returns `1` on hit, `0` on miss.
    fn emit_set_has(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        elem_arg: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let info = self.set_info(class_name);
        let ctx = self.begin_set_find(receiver, class_name, elem_arg, &info)?;
        self.push(Instruction::LocalGet(ctx.found_local));
        Ok(())
    }

    /// `s.add(v)` — insert if absent, no-op if already present. Triggers a 2×
    /// rebuild before probing if the load factor would exceed 75% with one
    /// more entry.
    fn emit_set_add(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        elem_arg: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let info = self.set_info(class_name);
        let size_off = self.set_field_offset_for(class_name, "size");
        let cap_off = self.set_field_offset_for(class_name, "capacity");
        let buckets_off = self.set_field_offset_for(class_name, "buckets_ptr");
        let head_off = self.set_field_offset_for(class_name, "head_idx");
        let tail_off = self.set_field_offset_for(class_name, "tail_idx");

        let this_local = self.alloc_local(WasmType::I32);
        self.emit_expr(receiver)?;
        self.push(Instruction::LocalSet(this_local));

        let elem_local = self.alloc_local(info.elem_ty.wasm_ty());
        let ty = self.emit_expr(elem_arg)?;
        self.check_elem_type(&info, ty)?;
        self.push(Instruction::LocalSet(elem_local));

        // Load-factor check — if `size*4 >= cap*3`, grow before probing so the
        // probe always runs on a ≥25%-empty array and always terminates.
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(size_off));
        self.push(Instruction::I32Const(4));
        self.push(Instruction::I32Mul);
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(cap_off));
        self.push(Instruction::I32Const(3));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32GeU);
        self.push(Instruction::If(BlockType::Empty));
        self.emit_set_rebuild(this_local, class_name, &info)?;
        self.push(Instruction::End);

        // Probe. Tracks first tombstone for insert reuse; if an OCCUPIED
        // bucket matches the element, finish without changing anything.
        let buckets_local = self.alloc_local(WasmType::I32);
        let cap_local = self.alloc_local(WasmType::I32);
        let mask_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(buckets_off));
        self.push(Instruction::LocalSet(buckets_local));
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(cap_off));
        self.push(Instruction::LocalTee(cap_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(mask_local));

        let slot_local = self.alloc_local(WasmType::I32);
        self.emit_set_hash_for_local(elem_local, &info.elem_ty);
        self.push(Instruction::LocalGet(mask_local));
        self.push(Instruction::I32And);
        self.push(Instruction::LocalSet(slot_local));

        let first_tomb = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::LocalSet(first_tomb));

        // already_present = 1 when we found a matching OCCUPIED slot; skip
        // the insert path in that case.
        let already_present = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(already_present));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        let state_local = self.alloc_local(WasmType::I32);
        self.emit_set_bucket_addr(buckets_local, slot_local, info.bucket.total_size);
        self.push(Instruction::I32Load8U(MemArg {
            offset: info.bucket.state_offset as u64,
            align: 0,
            memory_index: 0,
        }));
        self.push(Instruction::LocalTee(state_local));
        // EMPTY → break; insertion target is first_tomb if set, else this slot
        self.push(Instruction::I32Eqz);
        self.push(Instruction::BrIf(1));

        // TOMBSTONE → record first_tomb if not yet set, then advance
        self.push(Instruction::LocalGet(state_local));
        self.push(Instruction::I32Const(BUCKET_TOMBSTONE));
        self.push(Instruction::I32Eq);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::LocalGet(first_tomb));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Eq);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::LocalSet(first_tomb));
        self.push(Instruction::End);
        self.push(Instruction::Else);
        // OCCUPIED → compare elements; on match, flag and exit
        self.emit_elem_equals_stored(buckets_local, slot_local, elem_local, &info);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::LocalSet(already_present));
        self.push(Instruction::Br(3)); // exit outer block
        self.push(Instruction::End);
        self.push(Instruction::End);

        // slot = (slot + 1) & mask
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(mask_local));
        self.push(Instruction::I32And);
        self.push(Instruction::LocalSet(slot_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End); // loop
        self.push(Instruction::End); // block

        // If `already_present`, nothing more to do.
        self.push(Instruction::LocalGet(already_present));
        self.push(Instruction::I32Eqz);
        self.push(Instruction::If(BlockType::Empty));

        // Pick insert slot: prefer first tombstone over the terminating EMPTY
        // to keep probe chains short.
        let insert_slot = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(first_tomb));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Ne);
        self.push(Instruction::If(BlockType::Result(wasm_encoder::ValType::I32)));
        self.push(Instruction::LocalGet(first_tomb));
        self.push(Instruction::Else);
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::End);
        self.push(Instruction::LocalSet(insert_slot));

        let target_addr = self.alloc_local(WasmType::I32);
        self.emit_set_bucket_addr(buckets_local, insert_slot, info.bucket.total_size);
        self.push(Instruction::LocalSet(target_addr));

        // state = OCCUPIED
        self.push(Instruction::LocalGet(target_addr));
        self.push(Instruction::I32Const(BUCKET_OCCUPIED));
        self.push(Instruction::I32Store8(MemArg {
            offset: info.bucket.state_offset as u64,
            align: 0,
            memory_index: 0,
        }));
        // elem = v
        self.push(Instruction::LocalGet(target_addr));
        self.push(Instruction::LocalGet(elem_local));
        self.push(store_typed(&info.elem_ty, info.bucket.elem_offset));
        // next_insert = -1 (this becomes the new tail)
        self.push(Instruction::LocalGet(target_addr));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(store_i32(info.bucket.next_offset));
        // prev_insert = old tail_idx
        self.push(Instruction::LocalGet(target_addr));
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(tail_off));
        self.push(store_i32(info.bucket.prev_offset));

        // If old tail != -1: old_tail.next_insert = insert_slot.
        // Else: head_idx = insert_slot (list was empty).
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(tail_off));
        let old_tail = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalTee(old_tail));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Ne);
        self.push(Instruction::If(BlockType::Empty));
        self.emit_set_bucket_addr(buckets_local, old_tail, info.bucket.total_size);
        self.push(Instruction::LocalGet(insert_slot));
        self.push(store_i32(info.bucket.next_offset));
        self.push(Instruction::Else);
        self.push(Instruction::LocalGet(this_local));
        self.push(Instruction::LocalGet(insert_slot));
        self.push(store_i32(head_off));
        self.push(Instruction::End);

        // tail_idx = insert_slot
        self.push(Instruction::LocalGet(this_local));
        self.push(Instruction::LocalGet(insert_slot));
        self.push(store_i32(tail_off));

        // size += 1
        self.push(Instruction::LocalGet(this_local));
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(size_off));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(store_i32(size_off));

        self.push(Instruction::End); // end "if !already_present"

        Ok(())
    }

    /// `s.delete(v)` — probe; on hit set state=TOMBSTONE, unlink from the
    /// insertion chain, decrement size, return `1`. Miss returns `0`.
    fn emit_set_delete(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        elem_arg: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let info = self.set_info(class_name);
        let size_off = self.set_field_offset_for(class_name, "size");
        let head_off = self.set_field_offset_for(class_name, "head_idx");
        let tail_off = self.set_field_offset_for(class_name, "tail_idx");

        let ctx = self.begin_set_find(receiver, class_name, elem_arg, &info)?;

        self.push(Instruction::LocalGet(ctx.found_local));
        self.push(Instruction::If(BlockType::Empty));

        let target_addr = self.alloc_local(WasmType::I32);
        self.emit_set_bucket_addr(ctx.buckets_local, ctx.slot_local, info.bucket.total_size);
        self.push(Instruction::LocalSet(target_addr));

        let prev_idx = self.alloc_local(WasmType::I32);
        let next_idx = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(target_addr));
        self.push(load_i32(info.bucket.prev_offset));
        self.push(Instruction::LocalSet(prev_idx));
        self.push(Instruction::LocalGet(target_addr));
        self.push(load_i32(info.bucket.next_offset));
        self.push(Instruction::LocalSet(next_idx));

        // prev_bucket.next = next_idx (or header.head = next_idx if this was head)
        self.push(Instruction::LocalGet(prev_idx));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Ne);
        self.push(Instruction::If(BlockType::Empty));
        self.emit_set_bucket_addr(ctx.buckets_local, prev_idx, info.bucket.total_size);
        self.push(Instruction::LocalGet(next_idx));
        self.push(store_i32(info.bucket.next_offset));
        self.push(Instruction::Else);
        self.push(Instruction::LocalGet(ctx.this_local));
        self.push(Instruction::LocalGet(next_idx));
        self.push(store_i32(head_off));
        self.push(Instruction::End);

        // next_bucket.prev = prev_idx (or header.tail = prev_idx if this was tail)
        self.push(Instruction::LocalGet(next_idx));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Ne);
        self.push(Instruction::If(BlockType::Empty));
        self.emit_set_bucket_addr(ctx.buckets_local, next_idx, info.bucket.total_size);
        self.push(Instruction::LocalGet(prev_idx));
        self.push(store_i32(info.bucket.prev_offset));
        self.push(Instruction::Else);
        self.push(Instruction::LocalGet(ctx.this_local));
        self.push(Instruction::LocalGet(prev_idx));
        self.push(store_i32(tail_off));
        self.push(Instruction::End);

        // state = TOMBSTONE
        self.push(Instruction::LocalGet(target_addr));
        self.push(Instruction::I32Const(BUCKET_TOMBSTONE));
        self.push(Instruction::I32Store8(MemArg {
            offset: info.bucket.state_offset as u64,
            align: 0,
            memory_index: 0,
        }));

        // size -= 1
        self.push(Instruction::LocalGet(ctx.this_local));
        self.push(Instruction::LocalGet(ctx.this_local));
        self.push(load_i32(size_off));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(store_i32(size_off));

        self.push(Instruction::End); // end "if found"

        self.push(Instruction::LocalGet(ctx.found_local));
        Ok(())
    }

    /// `s.forEach((v) => { ... })` — walk the insertion chain from head via
    /// each bucket's `next_insert` pointer, binding the arrow's single param
    /// to the element per iteration.
    fn emit_set_foreach(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        callback: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let info = self.set_info(class_name);
        let buckets_off = self.set_field_offset_for(class_name, "buckets_ptr");
        let head_off = self.set_field_offset_for(class_name, "head_idx");

        let arrow = extract_arrow(callback)?;
        let params: Vec<String> = arrow
            .params
            .items
            .iter()
            .map(|p| match &p.pattern {
                BindingPattern::BindingIdentifier(ident) => Ok(ident.name.as_str().to_string()),
                _ => Err(CompileError::unsupported(
                    "forEach callback param must be a bare identifier",
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if params.len() != 1 {
            return Err(CompileError::codegen(
                "Set.forEach callback must take exactly 1 parameter: (value)",
            ));
        }

        let this_local = self.alloc_local(WasmType::I32);
        self.emit_expr(receiver)?;
        self.push(Instruction::LocalSet(this_local));

        let buckets_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(buckets_off));
        self.push(Instruction::LocalSet(buckets_local));

        let slot_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(head_off));
        self.push(Instruction::LocalSet(slot_local));

        let elem_local = self.alloc_local(info.elem_ty.wasm_ty());

        let saved = self.push_set_arrow_scope(&params, &[(elem_local, &info.elem_ty)]);

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        // if slot == -1: break
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Eq);
        self.push(Instruction::BrIf(1));

        // Load elem + next from bucket before calling the body.
        let addr_local = self.alloc_local(WasmType::I32);
        self.emit_set_bucket_addr(buckets_local, slot_local, info.bucket.total_size);
        self.push(Instruction::LocalTee(addr_local));
        self.push(load_typed(&info.elem_ty, info.bucket.elem_offset));
        self.push(Instruction::LocalSet(elem_local));

        let next_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(addr_local));
        self.push(load_i32(info.bucket.next_offset));
        self.push(Instruction::LocalSet(next_local));

        let body_ty = crate::codegen::array_builtins::eval_arrow_body(self, arrow)?;
        if body_ty != WasmType::Void {
            self.push(Instruction::Drop);
        }

        // slot = next
        self.push(Instruction::LocalGet(next_local));
        self.push(Instruction::LocalSet(slot_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End); // loop
        self.push(Instruction::End); // block

        self.pop_set_arrow_scope(&params, saved);
        Ok(())
    }

    /// Probe for `has` / `delete`. Evaluates the receiver and element once,
    /// then walks the probe chain until it hits an EMPTY slot (miss) or an
    /// OCCUPIED slot matching the element (hit).
    fn begin_set_find(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        elem_arg: &Expression<'a>,
        info: &SetInfo,
    ) -> Result<SetFindContext, CompileError> {
        let buckets_off = self.set_field_offset_for(class_name, "buckets_ptr");
        let cap_off = self.set_field_offset_for(class_name, "capacity");

        let this_local = self.alloc_local(WasmType::I32);
        self.emit_expr(receiver)?;
        self.push(Instruction::LocalSet(this_local));

        let elem_local = self.alloc_local(info.elem_ty.wasm_ty());
        let ty = self.emit_expr(elem_arg)?;
        self.check_elem_type(info, ty)?;
        self.push(Instruction::LocalSet(elem_local));

        let buckets_local = self.alloc_local(WasmType::I32);
        let mask_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(buckets_off));
        self.push(Instruction::LocalSet(buckets_local));
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(cap_off));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(mask_local));

        let slot_local = self.alloc_local(WasmType::I32);
        self.emit_set_hash_for_local(elem_local, &info.elem_ty);
        self.push(Instruction::LocalGet(mask_local));
        self.push(Instruction::I32And);
        self.push(Instruction::LocalSet(slot_local));

        let found_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(found_local));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        let state_local = self.alloc_local(WasmType::I32);
        self.emit_set_bucket_addr(buckets_local, slot_local, info.bucket.total_size);
        self.push(Instruction::I32Load8U(MemArg {
            offset: info.bucket.state_offset as u64,
            align: 0,
            memory_index: 0,
        }));
        self.push(Instruction::LocalTee(state_local));
        self.push(Instruction::I32Eqz);
        self.push(Instruction::BrIf(1));
        self.push(Instruction::LocalGet(state_local));
        self.push(Instruction::I32Const(BUCKET_OCCUPIED));
        self.push(Instruction::I32Eq);
        self.push(Instruction::If(BlockType::Empty));
        self.emit_elem_equals_stored(buckets_local, slot_local, elem_local, info);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::LocalSet(found_local));
        self.push(Instruction::Br(3));
        self.push(Instruction::End);
        self.push(Instruction::End);

        // slot = (slot + 1) & mask
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(mask_local));
        self.push(Instruction::I32And);
        self.push(Instruction::LocalSet(slot_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End); // loop
        self.push(Instruction::End); // block

        Ok(SetFindContext {
            this_local,
            buckets_local,
            slot_local,
            found_local,
        })
    }

    /// 2× capacity rebuild. Called from `add()` when the load factor would
    /// exceed 75% with one more entry. Walks the old insertion chain into
    /// freshly-allocated buckets, preserving order and collecting tombstones
    /// out.
    fn emit_set_rebuild(
        &mut self,
        this_local: u32,
        class_name: &str,
        info: &SetInfo,
    ) -> Result<(), CompileError> {
        let buckets_off = self.set_field_offset_for(class_name, "buckets_ptr");
        let cap_off = self.set_field_offset_for(class_name, "capacity");
        let head_off = self.set_field_offset_for(class_name, "head_idx");
        let tail_off = self.set_field_offset_for(class_name, "tail_idx");
        let bucket_size = info.bucket.total_size as i32;

        let old_buckets = self.alloc_local(WasmType::I32);
        let new_cap = self.alloc_local(WasmType::I32);
        let new_mask = self.alloc_local(WasmType::I32);
        let old_slot = self.alloc_local(WasmType::I32);

        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(buckets_off));
        self.push(Instruction::LocalSet(old_buckets));

        // new_cap = old_cap * 2
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(cap_off));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Shl);
        self.push(Instruction::LocalTee(new_cap));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(new_mask));

        // Allocate new bucket array.
        self.push(Instruction::LocalGet(new_cap));
        self.push(Instruction::I32Const(bucket_size));
        self.push(Instruction::I32Mul);
        let new_buckets = self.emit_arena_alloc_to_local(true)?;

        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(head_off));
        self.push(Instruction::LocalSet(old_slot));

        self.push(Instruction::LocalGet(this_local));
        self.push(Instruction::LocalGet(new_buckets));
        self.push(store_i32(buckets_off));
        self.push(Instruction::LocalGet(this_local));
        self.push(Instruction::LocalGet(new_cap));
        self.push(store_i32(cap_off));
        self.push(Instruction::LocalGet(this_local));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(store_i32(head_off));
        self.push(Instruction::LocalGet(this_local));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(store_i32(tail_off));

        let old_addr = self.alloc_local(WasmType::I32);
        let hash_slot = self.alloc_local(WasmType::I32);
        let new_addr = self.alloc_local(WasmType::I32);
        let elem_local = self.alloc_local(info.elem_ty.wasm_ty());

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(old_slot));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Eq);
        self.push(Instruction::BrIf(1));

        self.emit_set_bucket_addr(old_buckets, old_slot, info.bucket.total_size);
        self.push(Instruction::LocalSet(old_addr));

        self.push(Instruction::LocalGet(old_addr));
        self.push(load_typed(&info.elem_ty, info.bucket.elem_offset));
        self.push(Instruction::LocalSet(elem_local));

        // Probe in the new array: no duplicates, no tombstones, so just find
        // the first EMPTY slot.
        self.emit_set_hash_for_local(elem_local, &info.elem_ty);
        self.push(Instruction::LocalGet(new_mask));
        self.push(Instruction::I32And);
        self.push(Instruction::LocalSet(hash_slot));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.emit_set_bucket_addr(new_buckets, hash_slot, info.bucket.total_size);
        self.push(Instruction::I32Load8U(MemArg {
            offset: info.bucket.state_offset as u64,
            align: 0,
            memory_index: 0,
        }));
        self.push(Instruction::I32Eqz);
        self.push(Instruction::BrIf(1));
        self.push(Instruction::LocalGet(hash_slot));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(new_mask));
        self.push(Instruction::I32And);
        self.push(Instruction::LocalSet(hash_slot));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);

        self.emit_set_bucket_addr(new_buckets, hash_slot, info.bucket.total_size);
        self.push(Instruction::LocalSet(new_addr));
        self.push(Instruction::LocalGet(new_addr));
        self.push(Instruction::I32Const(BUCKET_OCCUPIED));
        self.push(Instruction::I32Store8(MemArg {
            offset: info.bucket.state_offset as u64,
            align: 0,
            memory_index: 0,
        }));
        self.push(Instruction::LocalGet(new_addr));
        self.push(Instruction::LocalGet(elem_local));
        self.push(store_typed(&info.elem_ty, info.bucket.elem_offset));

        self.push(Instruction::LocalGet(new_addr));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(store_i32(info.bucket.next_offset));
        self.push(Instruction::LocalGet(new_addr));
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(tail_off));
        self.push(store_i32(info.bucket.prev_offset));

        let prev_tail = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(tail_off));
        self.push(Instruction::LocalTee(prev_tail));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Ne);
        self.push(Instruction::If(BlockType::Empty));
        self.emit_set_bucket_addr(new_buckets, prev_tail, info.bucket.total_size);
        self.push(Instruction::LocalGet(hash_slot));
        self.push(store_i32(info.bucket.next_offset));
        self.push(Instruction::Else);
        self.push(Instruction::LocalGet(this_local));
        self.push(Instruction::LocalGet(hash_slot));
        self.push(store_i32(head_off));
        self.push(Instruction::End);
        self.push(Instruction::LocalGet(this_local));
        self.push(Instruction::LocalGet(hash_slot));
        self.push(store_i32(tail_off));

        // old_slot = old_bucket.next_insert
        self.push(Instruction::LocalGet(old_addr));
        self.push(load_i32(info.bucket.next_offset));
        self.push(Instruction::LocalSet(old_slot));
        self.push(Instruction::Br(0));
        self.push(Instruction::End); // loop
        self.push(Instruction::End); // block

        Ok(())
    }

    /// Emit the hash call for an element already held in `elem_local`, pushing
    /// the raw i32 hash onto the stack. Dispatches via the same helpers Map
    /// uses — same bundled precompiled functions.
    fn emit_set_hash_for_local(&mut self, elem_local: u32, elem_ty: &BoundType) {
        self.push(Instruction::LocalGet(elem_local));
        let name = crate::codegen::map_builtins::hash_helper_for(elem_ty);
        self.emit_helper_invocation(name);
    }

    /// Compare the element stored at `buckets[slot]` against `elem_local`,
    /// pushing `1` (equal) or `0`. Assumes the bucket is OCCUPIED.
    fn emit_elem_equals_stored(
        &mut self,
        buckets_local: u32,
        slot_local: u32,
        elem_local: u32,
        info: &SetInfo,
    ) {
        self.emit_set_bucket_addr(buckets_local, slot_local, info.bucket.total_size);
        self.push(load_typed(&info.elem_ty, info.bucket.elem_offset));
        self.push(Instruction::LocalGet(elem_local));
        match &info.elem_ty {
            BoundType::F64 => self.emit_helper_invocation("__key_eq_f64"),
            BoundType::Str => self.emit_helper_invocation("__str_eq"),
            _ => self.push(Instruction::I32Eq),
        }
    }

    /// Push `buckets_ptr + slot * bucket_size` onto the stack.
    fn emit_set_bucket_addr(&mut self, buckets_local: u32, slot_local: u32, bucket_size: u32) {
        self.push(Instruction::LocalGet(buckets_local));
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::I32Const(bucket_size as i32));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
    }

    fn set_info(&self, class_name: &str) -> SetInfo {
        self.module_ctx
            .set_info
            .get(class_name)
            .expect("caller verified set_info membership")
            .clone()
    }

    fn set_field_offset_for(&self, class_name: &str, field_name: &str) -> u32 {
        self.module_ctx
            .class_registry
            .get(class_name)
            .and_then(|l| l.field_map.get(field_name).map(|(off, _)| *off))
            .unwrap_or_else(|| panic!("set class '{class_name}' missing field '{field_name}'"))
    }

    fn check_elem_type(&mut self, info: &SetInfo, ty: WasmType) -> Result<(), CompileError> {
        self.coerce_numeric_set(info.elem_ty.wasm_ty(), ty, "Set element")
    }

    fn coerce_numeric_set(
        &mut self,
        expected: WasmType,
        actual: WasmType,
        slot: &str,
    ) -> Result<(), CompileError> {
        if actual == expected {
            return Ok(());
        }
        if expected == WasmType::F64 && actual == WasmType::I32 {
            self.push(Instruction::F64ConvertI32S);
            return Ok(());
        }
        Err(CompileError::type_err(format!(
            "{slot} type mismatch: expected {expected:?}, got {actual:?}"
        )))
    }

    fn push_set_arrow_scope(
        &mut self,
        param_names: &[String],
        locals: &[(u32, &BoundType)],
    ) -> SetArrowScope {
        let mut saved = SetArrowScope {
            entries: Vec::with_capacity(param_names.len()),
        };
        for (i, name) in param_names.iter().enumerate() {
            let (local_idx, ty) = locals[i];
            saved.entries.push(SetScopeEntry {
                name: name.clone(),
                saved_local: self.locals.get(name).copied(),
                saved_class: self.local_class_types.get(name).cloned(),
                saved_string: self.local_string_vars.contains(name),
                saved_closure_sig: self.local_closure_sigs.get(name).cloned(),
            });
            self.locals.insert(name.clone(), (local_idx, ty.wasm_ty()));
            match ty {
                BoundType::Class(cn) => {
                    self.local_class_types.insert(name.clone(), cn.clone());
                    self.local_string_vars.remove(name);
                }
                BoundType::Str => {
                    self.local_class_types.remove(name);
                    self.local_string_vars.insert(name.clone());
                }
                _ => {
                    self.local_class_types.remove(name);
                    self.local_string_vars.remove(name);
                }
            }
            self.local_closure_sigs.remove(name);
        }
        saved
    }

    fn pop_set_arrow_scope(&mut self, _param_names: &[String], saved: SetArrowScope) {
        for entry in saved.entries.into_iter().rev() {
            match entry.saved_local {
                Some(prev) => {
                    self.locals.insert(entry.name.clone(), prev);
                }
                None => {
                    self.locals.remove(&entry.name);
                }
            }
            match entry.saved_class {
                Some(cn) => {
                    self.local_class_types.insert(entry.name.clone(), cn);
                }
                None => {
                    self.local_class_types.remove(&entry.name);
                }
            }
            if entry.saved_string {
                self.local_string_vars.insert(entry.name.clone());
            } else {
                self.local_string_vars.remove(&entry.name);
            }
            match entry.saved_closure_sig {
                Some(sig) => {
                    self.local_closure_sigs.insert(entry.name, sig);
                }
                None => {
                    self.local_closure_sigs.remove(&entry.name);
                }
            }
        }
    }
}

struct SetFindContext {
    this_local: u32,
    buckets_local: u32,
    slot_local: u32,
    found_local: u32,
}

struct SetArrowScope {
    entries: Vec<SetScopeEntry>,
}

struct SetScopeEntry {
    name: String,
    saved_local: Option<(u32, WasmType)>,
    saved_class: Option<String>,
    saved_string: bool,
    saved_closure_sig: Option<ClosureSig>,
}
