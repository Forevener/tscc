//! Shared inline emitters for compiler-owned `Map<K, V>` / `Set<T>` methods.
//!
//! Both modules run the same open-addressing hash table. This file owns the
//! kind-agnostic primitives — bucket addressing, hashing, slot-equality, the
//! type-check gate, numeric coercion, forEach-arrow scope bookkeeping — so
//! per-kind modules don't each carry a copy. Callers pass the full
//! `HashTableInfo`; the emitters branch on `info.slot_ty` / `info.bucket`
//! as needed.

use oxc_ast::ast::{ArrowFunctionExpression, BindingPattern, Expression};
use wasm_encoder::Instruction;

use crate::codegen::array_builtins::extract_arrow;
use crate::codegen::func::FuncContext;
use crate::codegen::hash_table::{HashTableInfo, hash_helper_for, load_typed};
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
