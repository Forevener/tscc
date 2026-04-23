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
    BUCKET_EMPTY, BUCKET_OCCUPIED, EMPTY_LINK, HashTableInfo, hash_helper_for, load_i32,
    load_typed, store_i32,
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
