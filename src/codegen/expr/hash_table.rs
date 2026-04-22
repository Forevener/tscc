//! Shared inline emitters for compiler-owned `Map<K, V>` / `Set<T>` methods.
//!
//! Both modules run the same open-addressing hash table. This file owns the
//! kind-agnostic primitives — bucket addressing, hashing, slot-equality, the
//! type-check gate, and numeric coercion — so per-kind modules don't each
//! carry a copy. Callers pass the full `HashTableInfo`; the emitters branch
//! on `info.slot_ty` / `info.bucket` as needed.

use wasm_encoder::Instruction;

use crate::codegen::func::FuncContext;
use crate::codegen::hash_table::{HashTableInfo, hash_helper_for, load_typed};
use crate::error::CompileError;
use crate::types::{BoundType, WasmType};

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
}
