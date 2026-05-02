//! Shared inline emitters for compiler-owned `Map<K, V>` / `Set<T>` methods.
//!
//! Both modules run the same open-addressing hash table. This file owns the
//! kind-agnostic primitives — bucket addressing, hashing, slot-equality, the
//! type-check gate, numeric coercion, forEach-arrow scope bookkeeping — so
//! per-kind modules don't each carry a copy. Callers pass the full
//! `HashTableInfo`; the emitters branch on `info.slot_ty` / `info.bucket`
//! as needed.

use oxc_ast::ast::{ArrowFunctionExpression, BindingPattern, Expression};
use wasm_encoder::{BlockType, Instruction, MemArg};

use crate::codegen::array_builtins::extract_arrow;
use crate::codegen::func::FuncContext;
use crate::codegen::hash_table::{
    BUCKET_EMPTY, BUCKET_OCCUPIED, BUCKET_TOMBSTONE, EMPTY_LINK, HashTableInfo, hash_helper_for,
    load_i32, load_typed, store_i32, store_typed,
};
use crate::error::CompileError;
use crate::types::{BoundType, ClosureSig, WasmType};

/// Shape of a hashtable `forEach` callback's parameter list. `Set.forEach`
/// binds exactly one param (the element); `Map.forEach` binds one or two
/// (value, optional key). Carried as a parameter into `extract_foreach_params`
/// and (in Phase C) the shared foreach emitter.
pub(super) enum ArrowArity {
    /// Exactly 1 param (Set.forEach).
    One,
    /// 1 or 2 params (Map.forEach — key is optional).
    OneOrTwo,
}

/// Which bucket field `keys()` / `values()` should materialize.
/// `Key` reads `slot_offset` and is valid for both Map and Set.
/// `Value` reads `value_offset` and is Map-only — passing it for a Set
/// receiver is a programmer error and the emitter panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HashTableColumn {
    Key,
    Value,
}

impl<'a> FuncContext<'a> {
    /// Push `buckets_ptr + slot * bucket_size` onto the stack.
    pub(super) fn emit_bucket_addr(
        &mut self,
        buckets_local: u32,
        slot_local: u32,
        bucket_size: u32,
    ) {
        self.push(Instruction::LocalGet(buckets_local));
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::I32Const(bucket_size as i32));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
    }

    /// Compute `hash_helper_for(ty)(local)`, leaving the hash on the stack.
    pub(super) fn emit_hash_for_local(&mut self, key_local: u32, key_ty: &BoundType) {
        self.push(Instruction::LocalGet(key_local));
        let name = hash_helper_for(key_ty);
        self.emit_helper_invocation(name);
    }

    /// Compare the key stored at `buckets[slot]` against `key_local`, pushing
    /// `1` (equal) or `0`. Assumes the bucket is OCCUPIED — calling this on
    /// EMPTY/TOMBSTONE slots is undefined because the stored key bytes may be
    /// stale (matters for string keys whose comparison dereferences a pointer).
    pub(super) fn emit_slot_equals_stored(
        &mut self,
        buckets_local: u32,
        slot_local: u32,
        key_local: u32,
        info: &HashTableInfo,
    ) {
        self.emit_bucket_addr(buckets_local, slot_local, info.bucket.total_size);
        self.push(load_typed(&info.slot_ty, info.bucket.slot_offset));
        self.push(Instruction::LocalGet(key_local));
        match &info.slot_ty {
            BoundType::F64 => self.emit_helper_invocation("__key_eq_f64"),
            BoundType::Str => self.emit_helper_invocation("__str_eq"),
            _ => self.push(Instruction::I32Eq),
        }
    }

    /// Gate: the value being inserted/looked-up must match the slot type (with
    /// i32→f64 promotion). `slot` names the argument in errors, e.g.
    /// "Map key" or "Set element".
    pub(super) fn check_slot_type(
        &mut self,
        info: &HashTableInfo,
        ty: WasmType,
        slot: &str,
    ) -> Result<(), CompileError> {
        self.coerce_numeric(info.slot_ty.wasm_ty(), ty, slot)
    }

    /// If `actual` can be losslessly promoted to `expected` (i32 → f64),
    /// emit the conversion; otherwise accept only exact matches. `slot` names
    /// the argument in errors.
    pub(super) fn coerce_numeric(
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

    /// Byte offset of a header field on a compiler-owned `Map<K, V>` or
    /// `Set<T>` header class. Panics if the class or field is absent — the
    /// dispatcher's membership check stages that earlier.
    pub(super) fn hash_table_field_offset(&self, class_name: &str, field_name: &str) -> u32 {
        self.module_ctx
            .class_registry
            .get(class_name)
            .and_then(|l| l.field_map.get(field_name).map(|(off, _)| *off))
            .unwrap_or_else(|| {
                panic!("hash-table class '{class_name}' missing field '{field_name}'")
            })
    }

    /// Cloned copy of the `HashTableInfo` for this class — the shared
    /// emitters need a long-lived owned value while they mutate `self`.
    /// Panics if the class is absent; the dispatcher's membership check
    /// stages that earlier. Cheap (two enums + a small BucketLayout).
    pub(super) fn hash_table_info(&self, class_name: &str) -> HashTableInfo {
        self.module_ctx
            .hash_table_info
            .get(class_name)
            .expect("caller verified hash-table membership")
            .clone()
    }

    /// `m.clear()` / `s.clear()` — reset size=0, head/tail=-1, and zero every
    /// state byte in the bucket array via a single `memory.fill`.
    /// `buckets_ptr` + `capacity` stay as-is so the table is reusable without
    /// re-allocating. Kind-agnostic: Map and Set share the same header layout
    /// so the field offsets and the fill length both come from the shared
    /// `HashTableInfo` / header registry.
    pub(super) fn emit_hash_table_clear(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
    ) -> Result<(), CompileError> {
        let info = self.hash_table_info(class_name);
        let size_off = self.hash_table_field_offset(class_name, "size");
        let head_off = self.hash_table_field_offset(class_name, "head_idx");
        let tail_off = self.hash_table_field_offset(class_name, "tail_idx");
        let buckets_off = self.hash_table_field_offset(class_name, "buckets_ptr");
        let capacity_off = self.hash_table_field_offset(class_name, "capacity");

        // Evaluate receiver once into a local so we can reuse the pointer
        // without re-emitting side-effecting sub-expressions.
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

    /// `m.has(k)` / `s.has(v)` — probe for the slot argument, leave `1` on
    /// the stack for a hit and `0` for a miss. Returns i32. Kind-agnostic:
    /// the only asymmetry (the `"Map key"` vs `"Set element"` type-check
    /// error label) is derived from `info.value_ty.is_some()` via
    /// `hash_table_slot_label`.
    pub(super) fn emit_hash_table_has(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        slot_arg: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let info = self.hash_table_info(class_name);
        let ctx = self.begin_hash_table_find(
            receiver,
            class_name,
            slot_arg,
            &info,
            hash_table_slot_label(&info),
        )?;
        self.push(Instruction::LocalGet(ctx.found_local));
        Ok(())
    }

    /// `m.delete(k)` / `s.delete(v)` — probe for the slot argument; on hit,
    /// unlink the bucket from the insertion chain, set its state to
    /// TOMBSTONE, and decrement size. Leaves `found_local` (1 hit, 0 miss)
    /// on the stack so the caller returns it as `i32`. Kind-agnostic: there
    /// is no value slot to clear on Map delete (state=TOMBSTONE gates every
    /// future read, so stale value bytes are harmless); Set has no value
    /// slot at all. Same body for both.
    pub(super) fn emit_hash_table_delete(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        slot_arg: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let info = self.hash_table_info(class_name);
        let size_off = self.hash_table_field_offset(class_name, "size");
        let head_off = self.hash_table_field_offset(class_name, "head_idx");
        let tail_off = self.hash_table_field_offset(class_name, "tail_idx");

        let ctx = self.begin_hash_table_find(
            receiver,
            class_name,
            slot_arg,
            &info,
            hash_table_slot_label(&info),
        )?;

        // if found: unlink + tombstone + decrement. Leaves `found` on stack.
        self.push(Instruction::LocalGet(ctx.found_local));
        self.push(Instruction::If(BlockType::Empty));

        let target_addr = self.alloc_local(WasmType::I32);
        self.emit_bucket_addr(ctx.buckets_local, ctx.slot_local, info.bucket.total_size);
        self.push(Instruction::LocalSet(target_addr));

        // Read prev/next before stomping state — they are the only fields we
        // need to preserve; the slot value may hold a stale pointer after,
        // but that's fine because state=TOMBSTONE gates every future read.
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

    /// `m.keys()` / `m.values()` / `s.keys()` / `s.values()` — materialize
    /// the insertion chain into a freshly-allocated `Array<X>` whose element
    /// type matches the requested column. `target` selects which bucket
    /// field to copy out:
    /// - `HashTableColumn::Key` reads `slot_offset` (typed `info.slot_ty`).
    /// - `HashTableColumn::Value` reads `value_offset` (typed
    ///   `info.value_ty`). Caller must ensure the receiver is a Map; this
    ///   emitter panics on a Set with `Value` because `value_offset` is
    ///   `None`.
    ///
    /// Capacity is snapped to the current `size` and length is set to the
    /// post-walk index, so a concurrent modification (unsupported semantics
    /// either way) at worst short-circuits with `len < cap`. Order matches
    /// `forEach` because we walk the same chain.
    pub(super) fn emit_hash_table_to_array(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        target: HashTableColumn,
    ) -> Result<(), CompileError> {
        let info = self.hash_table_info(class_name);
        let size_off = self.hash_table_field_offset(class_name, "size");
        let buckets_off = self.hash_table_field_offset(class_name, "buckets_ptr");
        let head_off = self.hash_table_field_offset(class_name, "head_idx");

        let (target_ty, target_offset) = match target {
            HashTableColumn::Key => (info.slot_ty.clone(), info.bucket.slot_offset),
            HashTableColumn::Value => (
                info.value_ty
                    .clone()
                    .expect("value column requested on Set"),
                info.bucket
                    .value_offset
                    .expect("value column requested on Set"),
            ),
        };
        let elem_wasm = target_ty.wasm_ty();
        let esize: i32 = match elem_wasm {
            WasmType::F64 => 8,
            WasmType::I32 => 4,
            WasmType::Void => unreachable!("hash table column is void"),
        };

        // Pin the receiver pointer once.
        let this_local = self.alloc_local(WasmType::I32);
        self.emit_expr(receiver)?;
        self.push(Instruction::LocalSet(this_local));

        // Allocate a result array with capacity = size and length = 0; we'll
        // patch the length to `i` after the walk so partial writes from a
        // concurrent (unsupported) mutator stay self-consistent.
        let size_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(size_off));
        self.push(Instruction::LocalSet(size_local));
        let arr_local = crate::codegen::array_builtins::emit_alloc_array(
            self,
            size_local,
            elem_wasm,
        )?;

        let buckets_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(buckets_off));
        self.push(Instruction::LocalSet(buckets_local));

        let slot_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(head_off));
        self.push(Instruction::LocalSet(slot_local));

        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        let addr_local = self.alloc_local(WasmType::I32);

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        // if slot == EMPTY_LINK: break.
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Eq);
        self.push(Instruction::BrIf(1));

        // addr = buckets + slot * bucket_size
        self.emit_bucket_addr(buckets_local, slot_local, info.bucket.total_size);
        self.push(Instruction::LocalSet(addr_local));

        // arr[i] = bucket.<column>. Compute the destination address inline:
        // arr_local + ARRAY_HEADER + i * esize.
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(
            crate::codegen::expr::ARRAY_HEADER_SIZE as i32,
        ));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(esize));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);

        self.push(Instruction::LocalGet(addr_local));
        self.push(load_typed(&target_ty, target_offset));
        match elem_wasm {
            WasmType::F64 => self.push(Instruction::F64Store(MemArg {
                offset: 0,
                align: 3,
                memory_index: 0,
            })),
            _ => self.push(Instruction::I32Store(MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            })),
        }

        // i += 1
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));

        // slot = bucket.next_insert; continue.
        self.push(Instruction::LocalGet(addr_local));
        self.push(load_i32(info.bucket.next_offset));
        self.push(Instruction::LocalSet(slot_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End); // loop
        self.push(Instruction::End); // block

        // length = i
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Store(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));

        self.push(Instruction::LocalGet(arr_local));
        Ok(())
    }

    /// `m.entries()` / `s.entries()` — materialize the insertion chain into a
    /// freshly-allocated `Array<__Tuple$K$V>` (Map) or `Array<__Tuple$T$T>`
    /// (Set), with each row writing a brand-new tuple instance into the
    /// result. The tuple shapes are pre-registered per Map/Set
    /// instantiation in `module.rs` (see `ensure_tuple_shape`), so the
    /// `pair_class_name` lookup is always populated by the time codegen
    /// runs. Per ES spec, `Set.entries()` returns `Array<[T, T]>` (each pair
    /// is `[v, v]`); Map returns `Array<[K, V]>` in insertion order.
    ///
    /// Capacity / length bookkeeping mirrors `emit_hash_table_to_array`:
    /// snap to current `size`, patch length to the post-walk index. Tuples
    /// are written through the registered `__Tuple$...` synthetic class
    /// layout so field offsets and slot widths follow the same alignment
    /// rules as user-declared tuples — `_0` and `_1` may end up at offsets
    /// other than `0` / `4` when one of K / V is `f64` (8-byte alignment).
    pub(super) fn emit_hash_table_entries(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        pair_class_name: &str,
    ) -> Result<(), CompileError> {
        let info = self.hash_table_info(class_name);
        let size_off = self.hash_table_field_offset(class_name, "size");
        let buckets_off = self.hash_table_field_offset(class_name, "buckets_ptr");
        let head_off = self.hash_table_field_offset(class_name, "head_idx");

        let pair_layout = self
            .module_ctx
            .class_registry
            .get(pair_class_name)
            .unwrap_or_else(|| panic!("entries() pair shape '{pair_class_name}' not registered"))
            .clone();
        let &(k_offset, k_wasm) = pair_layout
            .field_map
            .get("_0")
            .expect("tuple shape has field _0");
        let &(v_offset, v_wasm) = pair_layout
            .field_map
            .get("_1")
            .expect("tuple shape has field _1");
        let pair_size = pair_layout.size;

        // Pin the receiver pointer once so subsequent arena allocations
        // (every iteration allocates a fresh tuple) can't move it.
        let this_local = self.alloc_local(WasmType::I32);
        self.emit_expr(receiver)?;
        self.push(Instruction::LocalSet(this_local));

        // Allocate `Array<i32>` to hold pointer-typed tuple slots. Element
        // width is always 4 bytes — the array carries pair pointers, not
        // inline tuple bytes.
        let size_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(size_off));
        self.push(Instruction::LocalSet(size_local));
        let arr_local = crate::codegen::array_builtins::emit_alloc_array(
            self,
            size_local,
            WasmType::I32,
        )?;

        let buckets_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(buckets_off));
        self.push(Instruction::LocalSet(buckets_local));

        let slot_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(head_off));
        self.push(Instruction::LocalSet(slot_local));

        let i_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(i_local));

        let bucket_addr_local = self.alloc_local(WasmType::I32);

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        // if slot == EMPTY_LINK: break.
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Eq);
        self.push(Instruction::BrIf(1));

        // bucket_addr = buckets + slot * bucket_size.
        self.emit_bucket_addr(buckets_local, slot_local, info.bucket.total_size);
        self.push(Instruction::LocalSet(bucket_addr_local));

        // Allocate a fresh tuple instance: `pair_local = arena_alloc(pair_size)`.
        // `emit_arena_alloc_to_local` returns the local index that holds the
        // result; that local is fixed at compile time (the wasm op runs every
        // loop iteration, but the variable slot is reused).
        self.push(Instruction::I32Const(pair_size as i32));
        let pair_local = self.emit_arena_alloc_to_local(true)?;

        // pair._0 = bucket.slot (key for Map; element for Set).
        self.push(Instruction::LocalGet(pair_local));
        self.push(Instruction::LocalGet(bucket_addr_local));
        self.push(load_typed(&info.slot_ty, info.bucket.slot_offset));
        self.push(emit_field_store_inst(k_wasm, k_offset));

        // pair._1 = bucket.value (Map) | bucket.slot (Set — duplicate of _0
        // per the ES spec for Set.entries()).
        self.push(Instruction::LocalGet(pair_local));
        self.push(Instruction::LocalGet(bucket_addr_local));
        match info.value_ty.as_ref() {
            Some(value_ty) => self.push(load_typed(
                value_ty,
                info.bucket.value_offset.expect("map bucket has value slot"),
            )),
            None => self.push(load_typed(&info.slot_ty, info.bucket.slot_offset)),
        }
        self.push(emit_field_store_inst(v_wasm, v_offset));

        // arr[i] = pair_local. Compute destination as
        // `arr + ARRAY_HEADER + i * 4` since we're storing pointers.
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Const(
            crate::codegen::expr::ARRAY_HEADER_SIZE as i32,
        ));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(4));
        self.push(Instruction::I32Mul);
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(pair_local));
        self.push(Instruction::I32Store(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));

        // i += 1
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalSet(i_local));

        // slot = bucket.next_insert; continue.
        self.push(Instruction::LocalGet(bucket_addr_local));
        self.push(load_i32(info.bucket.next_offset));
        self.push(Instruction::LocalSet(slot_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End); // loop
        self.push(Instruction::End); // block

        // length = i (post-walk count).
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::LocalGet(i_local));
        self.push(Instruction::I32Store(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));

        self.push(Instruction::LocalGet(arr_local));
        Ok(())
    }

    /// `m.forEach((v, k) => ...)` / `s.forEach((v) => ...)` — walk the
    /// insertion chain from `head`, binding the arrow's params to the current
    /// row on each iteration. The callback's return value (if any) is
    /// dropped. Kind split falls out of `info.value_ty.is_some()`: Map
    /// accepts 1 or 2 params and binds `(value, key)`; Set accepts exactly 1
    /// and binds `(element)`.
    pub(super) fn emit_hash_table_foreach(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        callback: &Expression<'a>,
    ) -> Result<(), CompileError> {
        let info = self.hash_table_info(class_name);
        let buckets_off = self.hash_table_field_offset(class_name, "buckets_ptr");
        let head_off = self.hash_table_field_offset(class_name, "head_idx");

        let (arity, kind) = if info.value_ty.is_some() {
            (ArrowArity::OneOrTwo, "Map")
        } else {
            (ArrowArity::One, "Set")
        };
        let (arrow, params) = extract_foreach_params(callback, arity, kind)?;

        // Cache receiver + derived pointers so we can walk the chain without
        // re-emitting the receiver expression each iteration.
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

        // `primary_local` is bound to arrow param 0 — the value for Map
        // (typed as `value_ty`) or the element for Set (typed as `slot_ty`).
        // `secondary_local` is Map-only and binds arrow param 1 (the key,
        // typed as `slot_ty`).
        let primary_ty: &BoundType = info.value_ty.as_ref().unwrap_or(&info.slot_ty);
        let primary_local = self.alloc_local(primary_ty.wasm_ty());
        let secondary_local = info
            .value_ty
            .as_ref()
            .map(|_| self.alloc_local(info.slot_ty.wasm_ty()));

        let mut bindings: Vec<(u32, &BoundType)> = Vec::with_capacity(2);
        bindings.push((primary_local, primary_ty));
        if let Some(sec) = secondary_local {
            bindings.push((sec, &info.slot_ty));
        }
        let saved = self.push_hash_table_arrow_scope(&params, &bindings);

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        // if slot == -1: break
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Eq);
        self.push(Instruction::BrIf(1));

        // Load primary (and secondary, for Map) + next from the bucket up
        // front. Reading once per iteration keeps the callback's view of the
        // row consistent; mutating the table from inside forEach is
        // unsupported semantics, but at least a single iteration stays
        // internally coherent.
        let addr_local = self.alloc_local(WasmType::I32);
        self.emit_bucket_addr(buckets_local, slot_local, info.bucket.total_size);
        self.push(Instruction::LocalTee(addr_local));
        if let Some(value_ty) = info.value_ty.as_ref() {
            self.push(load_typed(
                value_ty,
                info.bucket.value_offset.expect("map bucket has value slot"),
            ));
            self.push(Instruction::LocalSet(primary_local));
            self.push(Instruction::LocalGet(addr_local));
            self.push(load_typed(&info.slot_ty, info.bucket.slot_offset));
            self.push(Instruction::LocalSet(
                secondary_local.expect("map has secondary binding"),
            ));
        } else {
            self.push(load_typed(&info.slot_ty, info.bucket.slot_offset));
            self.push(Instruction::LocalSet(primary_local));
        }

        let next_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(addr_local));
        self.push(load_i32(info.bucket.next_offset));
        self.push(Instruction::LocalSet(next_local));

        let body_ty = crate::codegen::array_builtins::eval_arrow_body(self, arrow)?;
        if body_ty != WasmType::Void {
            self.push(Instruction::Drop);
        }

        // slot = next; continue.
        self.push(Instruction::LocalGet(next_local));
        self.push(Instruction::LocalSet(slot_local));
        self.push(Instruction::Br(0));
        self.push(Instruction::End); // loop
        self.push(Instruction::End); // block

        self.pop_hash_table_arrow_scope(saved);
        Ok(())
    }

    /// 2× capacity rebuild. Called from `set()` / `add()` when the load
    /// factor would exceed 75% with one more entry. Walks the old insertion
    /// chain (head → next) into freshly-allocated buckets, preserving order
    /// and collecting tombstones out as a side effect. Kind split falls out
    /// of `info.value_ty.is_some()`: Map reads+writes a (key, value) pair
    /// per slot, Set reads+writes just the element.
    pub(super) fn emit_hash_table_rebuild(
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

        // Snapshot the old buckets pointer before stomping the header.
        let old_buckets = self.alloc_local(WasmType::I32);
        let new_cap = self.alloc_local(WasmType::I32);
        let new_mask = self.alloc_local(WasmType::I32);
        let old_slot = self.alloc_local(WasmType::I32);

        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(buckets_off));
        self.push(Instruction::LocalSet(old_buckets));

        // new_cap = old_cap * 2; new_mask = new_cap - 1.
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(cap_off));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Shl);
        self.push(Instruction::LocalTee(new_cap));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Sub);
        self.push(Instruction::LocalSet(new_mask));

        // Allocate the new bucket array (arena bump gives zeroed memory, so
        // every bucket starts EMPTY).
        self.push(Instruction::LocalGet(new_cap));
        self.push(Instruction::I32Const(bucket_size));
        self.push(Instruction::I32Mul);
        let new_buckets = self.emit_arena_alloc_to_local(true)?;

        // Snapshot old head (first entry to re-insert), then repoint the
        // header at the new array with an empty chain.
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

        // Walk the old chain, re-inserting into the new array. Because we
        // append in old-chain order we re-materialize insertion order for
        // free, and tombstones drop out because the chain never pointed at
        // them.
        let old_addr = self.alloc_local(WasmType::I32);
        let hash_slot = self.alloc_local(WasmType::I32);
        let new_addr = self.alloc_local(WasmType::I32);
        let slot_local = self.alloc_local(info.slot_ty.wasm_ty());
        let value_local = info
            .value_ty
            .as_ref()
            .map(|v| self.alloc_local(v.wasm_ty()));

        self.push(Instruction::Block(BlockType::Empty));
        self.push(Instruction::Loop(BlockType::Empty));
        // if old_slot == -1: break
        self.push(Instruction::LocalGet(old_slot));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Eq);
        self.push(Instruction::BrIf(1));

        // old_addr = old_buckets + old_slot * bucket_size
        self.emit_bucket_addr(old_buckets, old_slot, info.bucket.total_size);
        self.push(Instruction::LocalSet(old_addr));

        // Load slot (and value, for Map) from the old bucket.
        self.push(Instruction::LocalGet(old_addr));
        self.push(load_typed(&info.slot_ty, info.bucket.slot_offset));
        self.push(Instruction::LocalSet(slot_local));
        if let Some(value_ty) = info.value_ty.as_ref() {
            self.push(Instruction::LocalGet(old_addr));
            self.push(load_typed(
                value_ty,
                info.bucket.value_offset.expect("map bucket has value slot"),
            ));
            self.push(Instruction::LocalSet(
                value_local.expect("map value local allocated"),
            ));
        }

        // Probe in the new array — no duplicates and no tombstones to worry
        // about, so we stop at the first EMPTY slot.
        self.emit_hash_for_local(slot_local, &info.slot_ty);
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
        // advance: hash_slot = (hash_slot + 1) & new_mask
        self.push(Instruction::LocalGet(hash_slot));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::I32Add);
        self.push(Instruction::LocalGet(new_mask));
        self.push(Instruction::I32And);
        self.push(Instruction::LocalSet(hash_slot));
        self.push(Instruction::Br(0));
        self.push(Instruction::End);
        self.push(Instruction::End);

        // Write the entry into the new bucket at hash_slot: state, slot, and
        // (for Map) value.
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
        self.push(Instruction::LocalGet(slot_local));
        self.push(store_typed(&info.slot_ty, info.bucket.slot_offset));
        if let Some(value_ty) = info.value_ty.as_ref() {
            self.push(Instruction::LocalGet(new_addr));
            self.push(Instruction::LocalGet(
                value_local.expect("map value local allocated"),
            ));
            self.push(store_typed(
                value_ty,
                info.bucket.value_offset.expect("map bucket has value slot"),
            ));
        }

        // Link into the insertion chain: new_bucket.next = -1,
        // new_bucket.prev = header.tail; then if header.tail == -1 this is
        // also the new head, else fix tail_bucket.next = hash_slot. Finally
        // header.tail = hash_slot.
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

        // old_slot = old_bucket.next_insert; continue.
        self.push(Instruction::LocalGet(old_addr));
        self.push(load_i32(info.bucket.next_offset));
        self.push(Instruction::LocalSet(old_slot));
        self.push(Instruction::Br(0));
        self.push(Instruction::End); // loop
        self.push(Instruction::End); // block

        Ok(())
    }

    /// `m.set(k, v)` / `s.add(v)` — insert-or-overwrite with chain-linked
    /// insertion order. Triggers a 2× rebuild before probing when the load
    /// factor would exceed 75% with one more entry, so the probe always runs
    /// on a ≥25%-empty array and always terminates. `value_arg` must be
    /// `Some` iff `info.value_ty.is_some()` (Map); `None` for Set.
    ///
    /// The kind split lives at three points:
    /// - Evaluating + type-checking `value_arg` (Map only).
    /// - Writing the value slot on the overwrite/insert path (Map only).
    /// - Nothing else — probe, chain link, tombstone reuse, and insertion
    ///   bookkeeping are identical for both. The `matched` flag unifies
    ///   Map's `is_update` with Set's `already_present`: same semantics
    ///   (probe hit an OCCUPIED slot matching our key), same downstream
    ///   guard (skip state/slot/chain/size++ on match).
    ///
    /// The shared shape computes `insert_slot` and `target_addr`
    /// unconditionally — even on Set's already-present path, where it's
    /// never read. That's a handful of extra pure-arithmetic wasm ops on
    /// the already-present-hot path; in exchange the Map/Set paths share
    /// one suffix instead of two.
    pub(super) fn emit_hash_table_insert(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        key_arg: &Expression<'a>,
        value_arg: Option<&Expression<'a>>,
    ) -> Result<(), CompileError> {
        let info = self.hash_table_info(class_name);
        debug_assert_eq!(info.value_ty.is_some(), value_arg.is_some());

        let size_off = self.hash_table_field_offset(class_name, "size");
        let cap_off = self.hash_table_field_offset(class_name, "capacity");
        let buckets_off = self.hash_table_field_offset(class_name, "buckets_ptr");
        let head_off = self.hash_table_field_offset(class_name, "head_idx");
        let tail_off = self.hash_table_field_offset(class_name, "tail_idx");

        // Evaluate receiver + args into locals up front. Pinning the
        // receiver pointer here means subsequent arena-allocating arg
        // evaluations can't move it; the header stores `buckets_ptr` by
        // pointer so the buckets themselves can't move either.
        let this_local = self.alloc_local(WasmType::I32);
        self.emit_expr(receiver)?;
        self.push(Instruction::LocalSet(this_local));

        let slot_value_local = self.alloc_local(info.slot_ty.wasm_ty());
        let ty = self.emit_expr(key_arg)?;
        self.check_slot_type(&info, ty, hash_table_slot_label(&info))?;
        self.push(Instruction::LocalSet(slot_value_local));

        let value_local = if let Some(value_arg) = value_arg {
            let value_ty = info.value_ty.as_ref().expect("map has value type");
            let local = self.alloc_local(value_ty.wasm_ty());
            let vty = self.emit_expr(value_arg)?;
            self.coerce_numeric(value_ty.wasm_ty(), vty, "Map value")?;
            self.push(Instruction::LocalSet(local));
            Some(local)
        } else {
            None
        };

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
        self.emit_hash_table_rebuild(this_local, class_name, &info)?;
        self.push(Instruction::End);

        // Probe setup: snapshot buckets_ptr, derive mask, compute the
        // initial slot from the hash.
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
        self.emit_hash_for_local(slot_value_local, &info.slot_ty);
        self.push(Instruction::LocalGet(mask_local));
        self.push(Instruction::I32And);
        self.push(Instruction::LocalSet(slot_local));

        let first_tomb = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::LocalSet(first_tomb));

        // `matched` is set to 1 iff the probe hits an OCCUPIED slot whose
        // stored key equals `slot_value_local`. Same role as Map's
        // `is_update` / Set's `already_present` in the per-kind versions.
        let matched = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(matched));

        // Probe loop. Walks slots linearly; an EMPTY terminates
        // (definite miss), a TOMBSTONE remembers first-seen for insert
        // reuse, an OCCUPIED match flags + breaks.
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
        // EMPTY → break.
        self.push(Instruction::I32Eqz);
        self.push(Instruction::BrIf(1));

        self.push(Instruction::LocalGet(state_local));
        self.push(Instruction::I32Const(BUCKET_TOMBSTONE));
        self.push(Instruction::I32Eq);
        self.push(Instruction::If(BlockType::Empty));
        // TOMBSTONE → record first_tomb if unset, then fall through to
        // advance.
        self.push(Instruction::LocalGet(first_tomb));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(Instruction::I32Eq);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::LocalGet(slot_local));
        self.push(Instruction::LocalSet(first_tomb));
        self.push(Instruction::End);
        self.push(Instruction::Else);
        // OCCUPIED → compare the stored slot value; on match, flag and
        // break to the post-probe cleanup.
        self.emit_slot_equals_stored(buckets_local, slot_local, slot_value_local, &info);
        self.push(Instruction::If(BlockType::Empty));
        self.push(Instruction::I32Const(1));
        self.push(Instruction::LocalSet(matched));
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

        // After probe: on match, `slot_local` points at the hit. On miss,
        // `slot_local` points at the terminating EMPTY; prefer
        // `first_tomb` over it to keep probe chains short. (On Set's
        // already-present path these are computed but unused — see the
        // fn-level comment.)
        let insert_slot = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalGet(matched));
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

        let target_addr = self.alloc_local(WasmType::I32);
        self.emit_bucket_addr(buckets_local, insert_slot, info.bucket.total_size);
        self.push(Instruction::LocalSet(target_addr));

        // Map: always write the value — overwrite on update, fresh store
        // on insert. Set has no value slot; skip.
        if let Some(value_ty) = info.value_ty.as_ref() {
            self.push(Instruction::LocalGet(target_addr));
            self.push(Instruction::LocalGet(
                value_local.expect("map value local allocated"),
            ));
            self.push(store_typed(
                value_ty,
                info.bucket.value_offset.expect("map bucket has value slot"),
            ));
        }

        // Insert-only bookkeeping — skipped on match (overwrite leaves
        // state/slot/chain/size untouched).
        self.push(Instruction::LocalGet(matched));
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
        // slot = key/elem
        self.push(Instruction::LocalGet(target_addr));
        self.push(Instruction::LocalGet(slot_value_local));
        self.push(store_typed(&info.slot_ty, info.bucket.slot_offset));
        // next_insert = -1 (this is the new tail)
        self.push(Instruction::LocalGet(target_addr));
        self.push(Instruction::I32Const(EMPTY_LINK));
        self.push(store_i32(info.bucket.next_offset));
        // prev_insert = old tail_idx
        self.push(Instruction::LocalGet(target_addr));
        self.push(Instruction::LocalGet(this_local));
        self.push(load_i32(tail_off));
        self.push(store_i32(info.bucket.prev_offset));

        // If old_tail != -1: old_tail.next_insert = insert_slot.
        // Else: header.head = insert_slot (list was empty).
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

    /// Bind `param_names` (the forEach arrow's formal params) to `locals`
    /// for the duration of the body. Returns a `HashTableArrowScope` that
    /// `pop_hash_table_arrow_scope` consumes to restore any shadowed outer
    /// bindings. Locals are paired to params positionally; extras in `locals`
    /// beyond `param_names.len()` are ignored (Map's 1-param case sees a
    /// two-slot `locals` but only binds `value`).
    pub(super) fn push_hash_table_arrow_scope(
        &mut self,
        param_names: &[String],
        locals: &[(u32, &BoundType)],
    ) -> HashTableArrowScope {
        let mut saved = HashTableArrowScope {
            entries: Vec::with_capacity(param_names.len()),
        };
        for (i, name) in param_names.iter().enumerate() {
            let (local_idx, ty) = locals[i];
            saved.entries.push(HashTableScopeEntry {
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

    /// Reverse of `push_hash_table_arrow_scope`: restore the outer bindings
    /// that the forEach body shadowed.
    pub(super) fn pop_hash_table_arrow_scope(&mut self, saved: HashTableArrowScope) {
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

    /// Probe for `has` / `delete`. Evaluates the receiver and slot argument
    /// (key for Map, element for Set) once, then walks the probe chain until
    /// it hits an EMPTY slot (miss) or an OCCUPIED slot matching the slot
    /// value (hit). Returns the locals that hold the result so callers can
    /// branch on `found_local` and reuse `slot_local`/`buckets_local` without
    /// re-deriving them. `slot_label` ("Map key" / "Set element") names the
    /// argument in the type-check error.
    pub(super) fn begin_hash_table_find(
        &mut self,
        receiver: &Expression<'a>,
        class_name: &str,
        slot_arg: &Expression<'a>,
        info: &HashTableInfo,
        slot_label: &str,
    ) -> Result<HashTableFindContext, CompileError> {
        let buckets_off = self.hash_table_field_offset(class_name, "buckets_ptr");
        let cap_off = self.hash_table_field_offset(class_name, "capacity");

        let this_local = self.alloc_local(WasmType::I32);
        self.emit_expr(receiver)?;
        self.push(Instruction::LocalSet(this_local));

        let slot_value_local = self.alloc_local(info.slot_ty.wasm_ty());
        let ty = self.emit_expr(slot_arg)?;
        self.check_slot_type(info, ty, slot_label)?;
        self.push(Instruction::LocalSet(slot_value_local));

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
        self.emit_hash_for_local(slot_value_local, &info.slot_ty);
        self.push(Instruction::LocalGet(mask_local));
        self.push(Instruction::I32And);
        self.push(Instruction::LocalSet(slot_local));

        let found_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::I32Const(0));
        self.push(Instruction::LocalSet(found_local));

        // Probe.
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
        // EMPTY → miss, break.
        self.push(Instruction::I32Eqz);
        self.push(Instruction::BrIf(1));
        // OCCUPIED → compare; on match, set found and break.
        self.push(Instruction::LocalGet(state_local));
        self.push(Instruction::I32Const(BUCKET_OCCUPIED));
        self.push(Instruction::I32Eq);
        self.push(Instruction::If(BlockType::Empty));
        self.emit_slot_equals_stored(buckets_local, slot_local, slot_value_local, info);
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

        Ok(HashTableFindContext {
            this_local,
            buckets_local,
            slot_local,
            found_local,
        })
    }
}

/// `i32.store` / `f64.store` for a tuple field at `offset`, picked by
/// `WasmType`. Used by `emit_hash_table_entries` to write the K and V
/// columns into a fresh tuple instance — the alignment hint follows the
/// width (`align=2` for i32, `align=3` for f64, matching `mem_align`).
fn emit_field_store_inst(wasm_ty: WasmType, offset: u32) -> Instruction<'static> {
    match wasm_ty {
        WasmType::F64 => Instruction::F64Store(MemArg {
            offset: offset as u64,
            align: 3,
            memory_index: 0,
        }),
        _ => Instruction::I32Store(MemArg {
            offset: offset as u64,
            align: 2,
            memory_index: 0,
        }),
    }
}

/// Error-text label for the slot argument in a Map or Set method
/// (`"Map key"` / `"Set element"`). Picked from `info.value_ty.is_some()`
/// so shared emitters can thread kind-appropriate diagnostics without
/// per-kind shims.
pub(super) fn hash_table_slot_label(info: &HashTableInfo) -> &'static str {
    if info.value_ty.is_some() { "Map key" } else { "Set element" }
}

/// Extract a `forEach` callback's arrow-function AST and validate its param
/// shape against `arity`. `kind` labels the hashtable in error text
/// ("Map" / "Set") so callers don't have to repeat it.
pub(super) fn extract_foreach_params<'b, 'a>(
    callback: &'b Expression<'a>,
    arity: ArrowArity,
    kind: &str,
) -> Result<(&'b ArrowFunctionExpression<'a>, Vec<String>), CompileError> {
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
    let ok = match arity {
        ArrowArity::One => params.len() == 1,
        ArrowArity::OneOrTwo => (1..=2).contains(&params.len()),
    };
    if !ok {
        let expected = match arity {
            ArrowArity::One => "exactly 1 parameter: (value)",
            ArrowArity::OneOrTwo => "1 or 2 parameters: (value) or (value, key)",
        };
        return Err(CompileError::codegen(format!(
            "{kind}.forEach callback must take {expected}"
        )));
    }
    Ok((arrow, params))
}

/// Per-param save for `push_hash_table_arrow_scope` — mirrors the subset of
/// FuncContext state that arrow-binding mutation needs to restore.
/// Locals returned from `begin_hash_table_find` so branches can reuse the
/// probe result without re-deriving them.
pub(super) struct HashTableFindContext {
    pub(super) this_local: u32,
    pub(super) buckets_local: u32,
    pub(super) slot_local: u32,
    pub(super) found_local: u32,
}

pub(super) struct HashTableArrowScope {
    entries: Vec<HashTableScopeEntry>,
}

struct HashTableScopeEntry {
    name: String,
    saved_local: Option<(u32, WasmType)>,
    saved_class: Option<String>,
    saved_string: bool,
    saved_closure_sig: Option<ClosureSig>,
}
