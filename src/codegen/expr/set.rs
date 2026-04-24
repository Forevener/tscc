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

use crate::codegen::func::FuncContext;
use crate::codegen::hash_table::{
    BUCKET_OCCUPIED, BUCKET_TOMBSTONE, EMPTY_LINK, HashTableInfo, load_i32, load_typed,
    store_i32, store_typed,
};
use crate::error::CompileError;
use crate::types::WasmType;

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
        match self.module_ctx.hash_table_info.get(&class_name) {
            Some(info) if info.value_ty.is_none() => {}
            _ => return Ok(None),
        }
        let method_name = member.property.name.as_str();
        match method_name {
            "clear" => {
                self.expect_args(call, 0, "Set.clear")?;
                self.emit_hash_table_clear(&member.object, &class_name)?;
                Ok(Some(WasmType::Void))
            }
            "has" => {
                self.expect_args(call, 1, "Set.has")?;
                let arg = call.arguments[0].to_expression();
                self.emit_hash_table_has(&member.object, &class_name, arg)?;
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
                self.emit_hash_table_delete(&member.object, &class_name, arg)?;
                Ok(Some(WasmType::I32))
            }
            "forEach" => {
                self.expect_args(call, 1, "Set.forEach")?;
                let arg = call.arguments[0].to_expression();
                self.emit_hash_table_foreach(&member.object, &class_name, arg)?;
                Ok(Some(WasmType::Void))
            }
            other => Err(CompileError::codegen(format!(
                "Set has no method '{other}' — supported: clear, has, add, delete, forEach"
            ))),
        }
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
        let size_off = self.hash_table_field_offset(class_name, "size");
        let cap_off = self.hash_table_field_offset(class_name, "capacity");
        let buckets_off = self.hash_table_field_offset(class_name, "buckets_ptr");
        let head_off = self.hash_table_field_offset(class_name, "head_idx");
        let tail_off = self.hash_table_field_offset(class_name, "tail_idx");

        let this_local = self.alloc_local(WasmType::I32);
        self.emit_expr(receiver)?;
        self.push(Instruction::LocalSet(this_local));

        let elem_local = self.alloc_local(info.slot_ty.wasm_ty());
        let ty = self.emit_expr(elem_arg)?;
        self.check_slot_type(&info, ty, "Set element")?;
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
        self.emit_hash_for_local(elem_local, &info.slot_ty);
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
        self.emit_bucket_addr(buckets_local, slot_local, info.bucket.total_size);
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
        self.emit_slot_equals_stored(buckets_local, slot_local, elem_local, &info);
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
        self.emit_bucket_addr(buckets_local, insert_slot, info.bucket.total_size);
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
        self.push(store_typed(&info.slot_ty, info.bucket.slot_offset));
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
        self.emit_bucket_addr(buckets_local, old_tail, info.bucket.total_size);
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

    /// `s.forEach((v) => { ... })` — walk the insertion chain from head via
    /// each bucket's `next_insert` pointer, binding the arrow's single param
    /// to the element per iteration.
    /// 2× capacity rebuild. Called from `add()` when the load factor would
    /// exceed 75% with one more entry. Walks the old insertion chain into
    /// freshly-allocated buckets, preserving order and collecting tombstones
    /// out.
    fn emit_set_rebuild(
        &mut self,
        this_local: u32,
        class_name: &str,
        info: &HashTableInfo,
    ) -> Result<(), CompileError> {
        let buckets_off = self.hash_table_field_offset(class_name, "buckets_ptr");
        let cap_off = self.hash_table_field_offset(class_name, "capacity");
        let head_off = self.hash_table_field_offset(class_name, "head_idx");
        let tail_off = self.hash_table_field_offset(class_name, "tail_idx");
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
        let elem_local = self.alloc_local(info.slot_ty.wasm_ty());

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.push(Instruction::LocalGet(old_slot));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Eq);
        self.push(Instruction::BrIf(1));

        self.emit_bucket_addr(old_buckets, old_slot, info.bucket.total_size);
        self.push(Instruction::LocalSet(old_addr));

        self.push(Instruction::LocalGet(old_addr));
        self.push(load_typed(&info.slot_ty, info.bucket.slot_offset));
        self.push(Instruction::LocalSet(elem_local));

        // Probe in the new array: no duplicates, no tombstones, so just find
        // the first EMPTY slot.
        self.emit_hash_for_local(elem_local, &info.slot_ty);
        self.push(Instruction::LocalGet(new_mask));
        self.push(Instruction::I32And);
        self.push(Instruction::LocalSet(hash_slot));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        self.emit_bucket_addr(new_buckets, hash_slot, info.bucket.total_size);
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

        self.emit_bucket_addr(new_buckets, hash_slot, info.bucket.total_size);
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
        self.push(store_typed(&info.slot_ty, info.bucket.slot_offset));

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
        self.emit_bucket_addr(new_buckets, prev_tail, info.bucket.total_size);
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

    fn set_info(&self, class_name: &str) -> HashTableInfo {
        self.module_ctx
            .hash_table_info
            .get(class_name)
            .expect("caller verified set membership")
            .clone()
    }

}
