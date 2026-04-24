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

use crate::codegen::func::FuncContext;
use crate::codegen::hash_table::{HashTableInfo, load_i32};
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
                self.emit_hash_table_has(&member.object, &class_name, arg)?;
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
                self.emit_hash_table_insert(&member.object, &class_name, k_arg, Some(v_arg))?;
                Ok(Some(WasmType::Void))
            }
            "delete" => {
                self.expect_args(call, 1, "Map.delete")?;
                let arg = call.arguments[0].to_expression();
                self.emit_hash_table_delete(&member.object, &class_name, arg)?;
                Ok(Some(WasmType::I32))
            }
            "forEach" => {
                self.expect_args(call, 1, "Map.forEach")?;
                let arg = call.arguments[0].to_expression();
                self.emit_hash_table_foreach(&member.object, &class_name, arg)?;
                Ok(Some(WasmType::Void))
            }
            other => Err(CompileError::codegen(format!(
                "Map has no method '{other}' — supported: clear, has, get, set, delete, forEach"
            ))),
        }
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

}

