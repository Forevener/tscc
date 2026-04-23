//! Per-monomorphization method dispatch for compiler-owned `Map<K, V>`.
//!
//! Map methods are emitted inline at each call site rather than registered as
//! real WASM functions — this lets us specialize hashing + key equality per
//! (K, V) without paying the call overhead. The dispatcher returns `Ok(None)`
//! when the call's receiver is not a Map, so upstream dispatchers keep their
//! normal fall-through behavior.
//!
//! ## Probing & insertion order
//!
//! Open-addressing linear probing: `slot = (hash(k) + i) & (capacity - 1)`
//! where `capacity` is always a power of two. A bucket's `state` byte is
//! `EMPTY` / `OCCUPIED` / `TOMBSTONE`; a probe walking a key stops as soon as
//! it hits an `EMPTY` slot (definite miss). Tombstones are skipped on find
//! and reused on insert, so delete+reinsert never wastes a slot.
//!
//! Iteration order matches JS spec (insertion order). Each occupied bucket
//! stores `prev_insert` / `next_insert` pointers forming a doubly-linked list
//! threaded through the bucket array; `head_idx` / `tail_idx` on the header
//! mark its ends. `forEach` walks the chain from `head`. `set` appends a
//! fresh entry to the tail; `delete` unlinks in place. Rebuild-on-grow (C.4)
//! walks the old chain to preserve order into the new array.
//!
//! ## Load factor & growth
//!
//! `set` checks `size * 4 >= capacity * 3` **before** probing for an
//! insertion slot, so the probe always runs on a ≥25%-empty array and always
//! terminates. When the check trips, `emit_map_rebuild` doubles capacity,
//! walks the old insertion chain, and re-inserts each entry into fresh
//! buckets — tombstones get collected out and the chain is rewoven with new
//! indices.

use oxc_ast::ast::*;
use wasm_encoder::{BlockType, Instruction, MemArg};

use super::hash_table::{ArrowArity, extract_foreach_params};
use crate::codegen::func::FuncContext;
use crate::codegen::hash_table::{
    BUCKET_OCCUPIED, BUCKET_TOMBSTONE, EMPTY_LINK, HashTableInfo, load_i32, load_typed,
    store_i32, store_typed,
};
use crate::error::CompileError;
use crate::types::{BoundType, WasmType};

impl<'a> FuncContext<'a> {
    /// Entry point invoked from `emit_call`. Peeks at the call's callee; if
    /// it's `<mapExpr>.<method>(...)` and the receiver resolves to a known
    /// Map monomorphization, emits the method inline and returns its type.
    pub(crate) fn try_emit_map_method_call(
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
            Some(info) if info.value_ty.is_some() => {}
            _ => return Ok(None),
        }
        let method_name = member.property.name.as_str();
        match method_name {
            "clear" => {
                self.expect_args(call, 0, "Map.clear")?;
                self.emit_hash_table_clear(&member.object, &class_name)?;
                Ok(Some(WasmType::Void))
            }
            "has" => {
                self.expect_args(call, 1, "Map.has")?;
                let arg = call.arguments[0].to_expression();
                self.emit_map_has(&member.object, &class_name, arg)?;
                Ok(Some(WasmType::I32))
            }
            "get" => {
                self.expect_args(call, 1, "Map.get")?;
                let arg = call.arguments[0].to_expression();
                let ret = self.emit_map_get(&member.object, &class_name, arg)?;
                Ok(Some(ret))
            }
            "set" => {
                self.expect_args(call, 2, "Map.set")?;
                let k_arg = call.arguments[0].to_expression();
                let v_arg = call.arguments[1].to_expression();
                self.emit_map_set(&member.object, &class_name, k_arg, v_arg)?;
                Ok(Some(WasmType::Void))
            }
            "delete" => {
                self.expect_args(call, 1, "Map.delete")?;
                let arg = call.arguments[0].to_expression();
                self.emit_map_delete(&member.object, &class_name, arg)?;
                Ok(Some(WasmType::I32))
            }
            "forEach" => {
                self.expect_args(call, 1, "Map.forEach")?;
                let arg = call.arguments[0].to_expression();
                self.emit_map_foreach(&member.object, &class_name, arg)?;
                Ok(Some(WasmType::Void))
            }
            other => Err(CompileError::codegen(format!(
                "Map has no method '{other}' — supported: clear, has, get, set, delete, forEach"
            ))),
        }
    }

    /// `m.has(k)` — returns `1` on hit, `0` on miss. Probes linearly until an
    /// EMPTY slot is seen (definite miss) or an OCCUPIED slot matches the
    /// key. Tombstones are skipped.
    fn emit_map_has(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        key_arg: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let info = self.map_info(class_name);
        let ctx = self.begin_hash_table_find(receiver, class_name, key_arg, &info, "Map key")?;
        // Leaves 1 (hit) / 0 (miss) on the stack.
        self.push(Instruction::LocalGet(ctx.found_local));
        Ok(())
    }

    /// `m.get(k)` — returns the stored value on hit, or the zero value of V
    /// on miss (`0` for i32/bool/pointers, `0.0` for f64). Call `.has(k)`
    /// first when disambiguation matters — tscc has no optional type to model
    /// "absent" directly.
    fn emit_map_get(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        key_arg: &Expression<'a>,
    ) -> Result<WasmType, CompileError> {
        let info = self.map_info(class_name);
        let value_wasm = info.expect_value_ty().wasm_ty();
        let ctx = self.begin_hash_table_find(receiver, class_name, key_arg, &info, "Map key")?;

        // `found` is 1 on hit and `slot` points to the matching bucket.
        // Branch on it: if hit, load value; otherwise push zero of V type.
        self.push(Instruction::LocalGet(ctx.found_local));
        let result_block_ty = match value_wasm {
            WasmType::F64 => BlockType::Result(wasm_encoder::ValType::F64),
            _ => BlockType::Result(wasm_encoder::ValType::I32),
        };
        self.push(Instruction::If(result_block_ty));
        // Load: addr = buckets + slot * bucket_size; value = *(addr + value_offset)
        self.emit_bucket_addr(ctx.buckets_local, ctx.slot_local, info.bucket.total_size);
        match info.expect_value_ty() {
            BoundType::F64 => self.push(Instruction::F64Load(MemArg {
                offset: info.bucket.value_offset.expect("map bucket has value slot") as u64,
                align: 3,
                memory_index: 0,
            })),
            _ => self.push(load_i32(info.bucket.value_offset.expect("map bucket has value slot"))),
        }
        self.push(Instruction::Else);
        // Zero value of V as miss sentinel.
        match value_wasm {
            WasmType::F64 => self.push(Instruction::F64Const(0.0)),
            _ => self.push(Instruction::I32Const(0)),
        }
        self.push(Instruction::End);
        Ok(value_wasm)
    }

    /// `m.set(k, v)` — insert or overwrite. Triggers a 2× rebuild before
    /// probing if the load factor would exceed 75% with one more entry.
    fn emit_map_set(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        key_arg: &Expression<'a>,
        value_arg: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let info = self.map_info(class_name);
        let size_off = self.hash_table_field_offset(class_name, "size");
        let cap_off = self.hash_table_field_offset(class_name, "capacity");
        let buckets_off = self.hash_table_field_offset(class_name, "buckets_ptr");
        let head_off = self.hash_table_field_offset(class_name, "head_idx");
        let tail_off = self.hash_table_field_offset(class_name, "tail_idx");

        // Evaluate receiver, key, value into locals up front — this pins the
        // map pointer for the duration of the set, even if evaluating the
        // value expression itself allocates into the arena (which could move
        // the bucket array's neighbor but NOT the buckets' own base pointer;
        // the header stores it by index). Still cheapest to cache once.
        let this_local = self.alloc_local(WasmType::I32);
        self.emit_expr(receiver)?;
        self.push(Instruction::LocalSet(this_local));

        let key_local = self.alloc_local(info.slot_ty.wasm_ty());
        let ty = self.emit_expr(key_arg)?;
        self.check_slot_type(&info, ty, "Map key")?;
        self.push(Instruction::LocalSet(key_local));

        let value_local = self.alloc_local(info.expect_value_ty().wasm_ty());
        let vty = self.emit_expr(value_arg)?;
        self.check_value_type(&info, vty)?;
        self.push(Instruction::LocalSet(value_local));

        // Load-factor check: if size * 4 >= capacity * 3, grow first. The
        // rebuild rewrites buckets_ptr + capacity on `this_local`, so we
        // re-read both below.
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
        self.emit_map_rebuild(this_local, class_name, &info)?;
        self.push(Instruction::End);

        // Probe. Tracks first tombstone for insert reuse; if an OCCUPIED
        // bucket matches the key, overwrite its value and finish.
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
        self.emit_hash_for_local(key_local, &info.slot_ty);
        self.push(Instruction::LocalGet(mask_local));
        self.push(Instruction::I32And);
        self.push(Instruction::LocalSet(slot_local));

        let first_tomb = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::LocalSet(first_tomb));

        // is_update = 1 when we overwrote an existing key; 0 means we landed
        // on an EMPTY/TOMBSTONE slot and should link a fresh entry in.
        let is_update = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(is_update));

        // Probe loop — exits via br to the surrounding block.
        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        let state_local = self.alloc_local(WasmType::I32);
        // Load state
        self.emit_bucket_addr(buckets_local, slot_local, info.bucket.total_size);
        self.push(Instruction::I32Load8U(MemArg {
            offset: info.bucket.state_offset as u64,
            align: 0,
            memory_index: 0,
        }));
        self.push(Instruction::LocalTee(state_local));
        // EMPTY → break (insertion target is first_tomb if set, else this slot)
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
        // OCCUPIED → compare keys; if match, overwrite and exit
        self.emit_slot_equals_stored(buckets_local, slot_local, key_local, &info);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::LocalSet(is_update));
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

        // After probe: `slot_local` is either the hit slot (is_update=1) or
        // the first EMPTY slot (is_update=0). Prefer `first_tomb` over the
        // EMPTY slot for insert to keep probe chains short.
        let insert_slot = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(is_update));
        self.push(Instruction::If(BlockType::Result(wasm_encoder::ValType::I32)));
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::Else);
        self.push(Instruction::LocalGet(first_tomb));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Ne);
        self.push(Instruction::If(BlockType::Result(wasm_encoder::ValType::I32)));
        self.push(Instruction::LocalGet(first_tomb));
        self.push(Instruction::Else);
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::End);
        self.push(Instruction::End);
        self.push(Instruction::LocalSet(insert_slot));

        // Compute the target bucket address once and reuse it.
        let target_addr = self.alloc_local(WasmType::I32);
        self.emit_bucket_addr(buckets_local, insert_slot, info.bucket.total_size);
        self.push(Instruction::LocalSet(target_addr));

        // Always write the value (overwrite or fresh insert).
        self.push(Instruction::LocalGet(target_addr));
        self.push(Instruction::LocalGet(value_local));
        self.push(store_typed(info.expect_value_ty(), info.bucket.value_offset.expect("map bucket has value slot")));

        // Insert-only path: state → OCCUPIED, write key, link into chain, bump size.
        self.push(Instruction::LocalGet(is_update));
        self.push(Instruction::I32Eqz);
        self.push(Instruction::If(BlockType::Empty));
        // state = OCCUPIED
        self.push(Instruction::LocalGet(target_addr));
        self.push(Instruction::I32Const(BUCKET_OCCUPIED));
        self.push(Instruction::I32Store8(MemArg {
            offset: info.bucket.state_offset as u64,
            align: 0,
            memory_index: 0,
        }));
        // key = k
        self.push(Instruction::LocalGet(target_addr));
        self.push(Instruction::LocalGet(key_local));
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

        self.push(Instruction::End); // end insert-only if

        Ok(())
    }

    /// `m.delete(k)` — probe; on hit set state=TOMBSTONE, unlink from the
    /// insertion chain, decrement size, return `1`. Miss returns `0`.
    fn emit_map_delete(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        key_arg: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let info = self.map_info(class_name);
        let size_off = self.hash_table_field_offset(class_name, "size");
        let head_off = self.hash_table_field_offset(class_name, "head_idx");
        let tail_off = self.hash_table_field_offset(class_name, "tail_idx");

        let ctx = self.begin_hash_table_find(receiver, class_name, key_arg, &info, "Map key")?;

        // if found: unlink + tombstone + decrement. Leaves `found` on stack.
        self.push(Instruction::LocalGet(ctx.found_local));
        self.push(Instruction::If(BlockType::Empty));

        let target_addr = self.alloc_local(WasmType::I32);
        self.emit_bucket_addr(ctx.buckets_local, ctx.slot_local, info.bucket.total_size);
        self.push(Instruction::LocalSet(target_addr));

        // Read prev/next before stomping state — they are the only fields we
        // need to preserve; the key slot may hold a stale pointer after, but
        // that's fine because state=TOMBSTONE gates every future read.
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
        self.emit_bucket_addr(ctx.buckets_local, prev_idx, info.bucket.total_size);
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
        self.emit_bucket_addr(ctx.buckets_local, next_idx, info.bucket.total_size);
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

        // Leave `found` on the stack as the return value.
        self.push(Instruction::LocalGet(ctx.found_local));
        Ok(())
    }

    /// `m.forEach((v, k) => { ... })` — walk the insertion chain from head
    /// via each bucket's `next_insert` pointer, binding the 1 or 2 arrow
    /// params to `(value, key)` per iteration. Mirrors `arr.forEach`'s arrow
    /// scope management so captured outer vars resolve correctly.
    fn emit_map_foreach(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        callback: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let info = self.map_info(class_name);
        let buckets_off = self.hash_table_field_offset(class_name, "buckets_ptr");
        let head_off = self.hash_table_field_offset(class_name, "head_idx");

        let (arrow, params) = extract_foreach_params(callback, ArrowArity::OneOrTwo, "Map")?;

        // Cache receiver, buckets_ptr into locals.
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

        // Per-iteration locals: current value + key. The arrow body reads them
        // through the arrow-scope bindings below.
        let value_local = self.alloc_local(info.expect_value_ty().wasm_ty());
        let key_local = self.alloc_local(info.slot_ty.wasm_ty());

        // The iteration contract matches JS — arrow params are (value, key).
        // If the arrow only takes one param, bind it to value. Two params
        // bind (value, key). Saved scope is restored after the loop.
        let saved = self.push_hash_table_arrow_scope(
            &params,
            &[(value_local, info.expect_value_ty()), (key_local, &info.slot_ty)],
        );

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        // if slot == -1: break
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Eq);
        self.push(Instruction::BrIf(1));

        // Load value, key, next from bucket BEFORE calling the body — the
        // callback could in principle mutate the map, but that's not
        // supported semantics and we don't guard against it. Reading fields
        // up front keeps the memory accesses contiguous.
        let addr_local = self.alloc_local(WasmType::I32);
        self.emit_bucket_addr(buckets_local, slot_local, info.bucket.total_size);
        self.push(Instruction::LocalTee(addr_local));
        self.push(load_typed(info.expect_value_ty(), info.bucket.value_offset.expect("map bucket has value slot")));
        self.push(Instruction::LocalSet(value_local));
        self.push(Instruction::LocalGet(addr_local));
        self.push(load_typed(&info.slot_ty, info.bucket.slot_offset));
        self.push(Instruction::LocalSet(key_local));

        let next_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(addr_local));
        self.push(load_i32(info.bucket.next_offset));
        self.push(Instruction::LocalSet(next_local));

        // Evaluate arrow body; drop any return value.
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

        self.pop_hash_table_arrow_scope(saved);
        Ok(())
    }

    /// 2× capacity rebuild. Called from inside `set()` when the load factor
    /// would exceed 75% with one more entry. Walks the old insertion chain
    /// (head → next) into freshly-allocated buckets, preserving order and
    /// collecting tombstones out as a side effect.
    fn emit_map_rebuild(
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

        // old_buckets / old_capacity / old_head
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

        // Allocate new bucket array: new_cap * bucket_size (zero-init via arena bump).
        self.push(Instruction::LocalGet(new_cap));
        self.push(Instruction::I32Const(bucket_size));
        self.push(Instruction::I32Mul);
        let new_buckets = self.emit_arena_alloc_to_local(true)?;

        // Snapshot old head (first entry to re-insert), then clear header's
        // chain + point it at the new array.
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

        // Walk old chain, re-inserting into new array while maintaining order.
        // Re-using insertion-link append gives us the source-order chain for free.
        let old_addr = self.alloc_local(WasmType::I32);
        let hash_slot = self.alloc_local(WasmType::I32);
        let new_addr = self.alloc_local(WasmType::I32);
        let key_local = self.alloc_local(info.slot_ty.wasm_ty());
        let value_local = self.alloc_local(info.expect_value_ty().wasm_ty());

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        // if old_slot == -1: break
        self.push(Instruction::LocalGet(old_slot));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Eq);
        self.push(Instruction::BrIf(1));

        // addr = old_buckets + old_slot * bucket_size
        self.emit_bucket_addr(old_buckets, old_slot, info.bucket.total_size);
        self.push(Instruction::LocalSet(old_addr));

        // Load key + value.
        self.push(Instruction::LocalGet(old_addr));
        self.push(load_typed(&info.slot_ty, info.bucket.slot_offset));
        self.push(Instruction::LocalSet(key_local));
        self.push(Instruction::LocalGet(old_addr));
        self.push(load_typed(info.expect_value_ty(), info.bucket.value_offset.expect("map bucket has value slot")));
        self.push(Instruction::LocalSet(value_local));

        // Probe in the new array: no duplicates and no tombstones, so we
        // only need to scan for the first EMPTY slot.
        self.emit_hash_for_local(key_local, &info.slot_ty);
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
        // advance
        self.push(Instruction::LocalGet(hash_slot));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(new_mask));
        self.push(Instruction::I32And);
        self.push(Instruction::LocalSet(hash_slot));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);

        // Write to new bucket at hash_slot.
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
        self.push(Instruction::LocalGet(key_local));
        self.push(store_typed(&info.slot_ty, info.bucket.slot_offset));
        self.push(Instruction::LocalGet(new_addr));
        self.push(Instruction::LocalGet(value_local));
        self.push(store_typed(info.expect_value_ty(), info.bucket.value_offset.expect("map bucket has value slot")));

        // Link into chain: new_prev = header.tail, new_next = -1.
        // If header.tail == -1: header.head = hash_slot. Else: tail_bucket.next = hash_slot.
        // Then header.tail = hash_slot.
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

    /// Emit a call to a runtime helper by name, splicing if it's L_splice and
    /// falling back to `Call(idx)` otherwise. Args must already be on the
    /// stack in the helper's parameter order. The result (if any) replaces
    /// them on the stack — same convention either path.
    pub(crate) fn emit_helper_invocation(&mut self, name: &str) {
        if let Some(pf) = crate::codegen::precompiled::find_inline(name) {
            let reg_borrow = self.module_ctx.helper_registration.borrow();
            let reg = reg_borrow.as_ref().unwrap_or_else(|| {
                panic!(
                    "helper_registration unset — register_string_helpers must run \
                     before method codegen splices inline helper '{name}'"
                )
            });
            let plan = crate::codegen::precompiled::RewritePlan {
                func_index_map: &reg.func_index_map,
                type_index_map: &reg.type_index_map,
                global_index_map: &reg.global_index_map,
                helper_table_index: reg.helper_table_index,
            };
            crate::codegen::splice::splice_inline_call(self, pf, &plan)
                .unwrap_or_else(|e| panic!("splicing '{name}' failed: {e:?}"));
            return;
        }
        let (func_idx, _) = self
            .module_ctx
            .get_func(name)
            .unwrap_or_else(|| panic!("helper '{name}' not registered"));
        self.push(Instruction::Call(func_idx));
    }

    /// Copy of `HashTableInfo` — dispatcher wants a long-lived view while
    /// emitting method bodies that need a borrowed `&mut self`. Cheap (two
    /// enums + a small BucketLayout struct).
    fn map_info(&self, class_name: &str) -> HashTableInfo {
        self.module_ctx
            .hash_table_info
            .get(class_name)
            .expect("caller verified map membership")
            .clone()
    }

    /// Like `check_key_type`, but for the value slot.
    fn check_value_type(&mut self, info: &HashTableInfo, ty: WasmType) -> Result<(), CompileError> {
        self.coerce_numeric(info.expect_value_ty().wasm_ty(), ty, "Map value")
    }

}

