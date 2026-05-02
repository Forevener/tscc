//! `Object.<static>` lowerings: `keys`, `values`, `entries`.
//!
//! All three operate on shape-typed objects whose layout is known at compile
//! time. Field names lower to static-string literals, value loads to
//! per-field memory reads at the recorded offset; the synthesized result is
//! a regular `Array<T>` (or `Array<[string, T]>` for `entries`).
//!
//! Per `roadmap.md`, `values` / `entries` require fields that share a
//! `WasmType` — heterogeneous shapes are rejected with a diagnostic instead
//! of forcing a runtime tag+payload box. `entries` additionally requires the
//! tuple shape `[string, T]` to already exist in the registry; users get
//! that for free by annotating the receiver
//! (`const e: [string, number][] = Object.entries(p)`).

use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::classes::ClassLayout;
use crate::codegen::coerce::{emit_field_load, emit_field_store};
use crate::codegen::func::FuncContext;
use crate::codegen::shapes::tuple_fingerprint_of;
use crate::error::CompileError;
use crate::types::{BoundType, WasmType};

use super::ARRAY_HEADER_SIZE;

impl<'a> FuncContext<'a> {
    /// Dispatch `Object.<static>(arg)` calls. Returns `Some` if the callee
    /// matches a known method; an unknown method on `Object` is an error
    /// rather than a fall-through (we don't want `Object.foo` to silently
    /// drift into the generic free-function lookup).
    pub(crate) fn try_emit_object_static_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        let (obj_name, method_name) = match &call.callee {
            Expression::StaticMemberExpression(member) => {
                let obj = match &member.object {
                    Expression::Identifier(ident) => ident.name.as_str(),
                    _ => return Ok(None),
                };
                (obj, member.property.name.as_str())
            }
            _ => return Ok(None),
        };
        if obj_name != "Object" {
            return Ok(None);
        }
        match method_name {
            "keys" => {
                self.emit_object_keys(call)?;
                Ok(Some(WasmType::I32))
            }
            "values" => {
                self.emit_object_values(call)?;
                Ok(Some(WasmType::I32))
            }
            "entries" => {
                self.emit_object_entries(call)?;
                Ok(Some(WasmType::I32))
            }
            _ => Err(CompileError::unsupported(format!(
                "Object.{method_name} is not supported (Object statics: keys, values, entries)"
            ))),
        }
    }

    /// `Object.keys(p)` — emit a fresh `Array<string>` populated with the
    /// argument's field names in declaration order. The argument expression
    /// is evaluated for its side effects (and dropped), since keys are
    /// purely a compile-time view of the layout.
    fn emit_object_keys(&mut self, call: &CallExpression<'a>) -> Result<(), CompileError> {
        self.expect_args(call, 1, "Object.keys")?;
        let layout = self.resolve_object_arg_layout(call, "Object.keys")?;

        let name_offsets: Vec<u32> = layout
            .fields
            .iter()
            .map(|(name, _, _)| self.module_ctx.alloc_static_string(name))
            .collect();

        // Evaluate the argument for side effects, then discard. Object.keys
        // doesn't read any fields, but the user's expression may.
        self.emit_expr(call.arguments[0].to_expression())?;
        self.push(Instruction::Drop);

        let count = layout.fields.len() as i32;
        let ptr_local = emit_array_header_alloc(self, count, 4)?;

        for (i, off) in name_offsets.iter().enumerate() {
            self.push(Instruction::LocalGet(ptr_local));
            self.push(Instruction::I32Const(*off as i32));
            self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: ARRAY_HEADER_SIZE as u64 + (i as u64) * 4,
                align: 2,
                memory_index: 0,
            }));
        }

        self.push(Instruction::LocalGet(ptr_local));
        Ok(())
    }

    /// `Object.values(p)` — emit a fresh `Array<T>` whose elements are the
    /// argument's field values in declaration order. Rejects shapes whose
    /// fields don't share a `BoundType` (the typed-subset stance — no
    /// runtime tag+payload boxing for primitive unions).
    fn emit_object_values(&mut self, call: &CallExpression<'a>) -> Result<(), CompileError> {
        self.expect_args(call, 1, "Object.values")?;
        let layout = self.resolve_object_arg_layout(call, "Object.values")?;
        // BoundType homogeneity implies WasmType homogeneity — and the
        // wasm side is what drives load/store widths and array stride.
        let _ = homogeneous_value_bound_type(&layout, "Object.values")?;
        let elem_ty = layout.fields[0].2;
        let esize = wasm_ty_size(elem_ty);

        // Evaluate the receiver once and stash its pointer — each value load
        // re-reads from the same instance, so a temp keeps codegen
        // straightforward (and avoids re-evaluating side-effecting arguments).
        self.emit_expr(call.arguments[0].to_expression())?;
        let recv_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalSet(recv_local));

        let count = layout.fields.len() as i32;
        let ptr_local = emit_array_header_alloc(self, count, esize)?;

        for (i, (_, field_offset, field_ty)) in layout.fields.iter().enumerate() {
            self.push(Instruction::LocalGet(ptr_local));
            self.push(Instruction::LocalGet(recv_local));
            emit_field_load(self, *field_offset, *field_ty);
            let arr_offset = ARRAY_HEADER_SIZE as u64 + (i as u64) * (esize as u64);
            emit_field_store(self, arr_offset as u32, *field_ty);
        }

        self.push(Instruction::LocalGet(ptr_local));
        Ok(())
    }

    /// `Object.entries(p)` — emit a fresh `Array<[string, T]>`. Each entry is
    /// a freshly arena-allocated tuple instance; the array stores tuple
    /// pointers (i32). Requires the tuple shape `[string, T]` to be
    /// pre-registered (typically by an annotation on the receiving variable
    /// or chain — `const e: [string, number][] = Object.entries(p)`).
    fn emit_object_entries(&mut self, call: &CallExpression<'a>) -> Result<(), CompileError> {
        self.expect_args(call, 1, "Object.entries")?;
        let layout = self.resolve_object_arg_layout(call, "Object.entries")?;
        let value_bt = homogeneous_value_bound_type(&layout, "Object.entries")?;

        // Resolve tuple [string, T] in the registry. We need its synthesized
        // class layout to know the slot offsets — different `T`s land at
        // different positions because `[string, f64]` aligns the f64 to 8.
        let tuple_class_name = lookup_string_value_tuple_class(self, &value_bt).ok_or_else(|| {
            self.locate(
                CompileError::type_err(format!(
                    "Object.entries requires the tuple shape [string, {pretty}] to be registered — \
                     annotate the receiver, e.g. `const e: [string, {pretty}][] = Object.entries(p)`",
                    pretty = pretty_bound_type(&value_bt),
                )),
                call.span.start,
            )
        })?;
        let tuple_layout = self
            .module_ctx
            .class_registry
            .get(&tuple_class_name)
            .ok_or_else(|| {
                CompileError::codegen(format!(
                    "tuple shape '{tuple_class_name}' is in the shape registry but has no class layout"
                ))
            })?
            .clone();
        let (key_offset, _) = *tuple_layout.field_map.get("_0").ok_or_else(|| {
            CompileError::codegen("entries tuple missing slot _0")
        })?;
        let (val_offset, _) = *tuple_layout.field_map.get("_1").ok_or_else(|| {
            CompileError::codegen("entries tuple missing slot _1")
        })?;
        let tuple_size = tuple_layout.size as i32;

        let name_offsets: Vec<u32> = layout
            .fields
            .iter()
            .map(|(name, _, _)| self.module_ctx.alloc_static_string(name))
            .collect();

        self.emit_expr(call.arguments[0].to_expression())?;
        let recv_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalSet(recv_local));

        let count = layout.fields.len() as i32;
        let arr_local = emit_array_header_alloc(self, count, 4)?;

        for (i, (_, field_offset, field_ty)) in layout.fields.iter().enumerate() {
            // Allocate tuple instance. emit_arena_alloc_to_local creates a
            // fresh local; we use it for the per-iteration tuple pointer.
            self.push(Instruction::I32Const(tuple_size));
            let tuple_ptr_local = self.emit_arena_alloc_to_local(true)?;

            // tuple._0 = key
            self.push(Instruction::LocalGet(tuple_ptr_local));
            self.push(Instruction::I32Const(name_offsets[i] as i32));
            emit_field_store(self, key_offset, WasmType::I32);

            // tuple._1 = recv.<field>
            self.push(Instruction::LocalGet(tuple_ptr_local));
            self.push(Instruction::LocalGet(recv_local));
            emit_field_load(self, *field_offset, *field_ty);
            emit_field_store(self, val_offset, *field_ty);

            // arr[i] = tuple_ptr
            self.push(Instruction::LocalGet(arr_local));
            self.push(Instruction::LocalGet(tuple_ptr_local));
            self.push(Instruction::I32Store(wasm_encoder::MemArg {
                offset: ARRAY_HEADER_SIZE as u64 + (i as u64) * 4,
                align: 2,
                memory_index: 0,
            }));
        }

        self.push(Instruction::LocalGet(arr_local));
        Ok(())
    }

    /// Resolve the argument expression of an `Object.<m>(arg)` call to its
    /// shape-typed class layout. Anything that isn't a registered class
    /// (typed array, generic Map/Set, primitive) is rejected here — those
    /// don't have a fixed compile-time field layout that `keys` / `values`
    /// / `entries` could project from.
    fn resolve_object_arg_layout(
        &self,
        call: &CallExpression<'a>,
        api: &str,
    ) -> Result<ClassLayout, CompileError> {
        let expr = call.arguments[0].to_expression();
        let class_name = self.resolve_expr_class(expr).map_err(|_| {
            self.locate(
                CompileError::type_err(format!(
                    "{api} requires a shape-typed argument — \
                     could not resolve a compile-time shape for the expression"
                )),
                call.span.start,
            )
        })?;
        if crate::codegen::typed_arrays::descriptor_for(&class_name).is_some() {
            return Err(self.locate(
                CompileError::type_err(format!(
                    "{api} on a typed array is not supported"
                )),
                call.span.start,
            ));
        }
        if self.module_ctx.hash_table_info.contains_key(&class_name) {
            return Err(self.locate(
                CompileError::type_err(format!(
                    "{api} on Map/Set is not supported — use the receiver's own .keys()/.values()/.entries()"
                )),
                call.span.start,
            ));
        }
        let layout = self.module_ctx.class_registry.get(&class_name).ok_or_else(|| {
            self.locate(
                CompileError::codegen(format!(
                    "{api}: class '{class_name}' is not in the registry"
                )),
                call.span.start,
            )
        })?;
        if layout.fields.is_empty() {
            return Err(self.locate(
                CompileError::type_err(format!(
                    "{api}: type '{class_name}' has no enumerable fields"
                )),
                call.span.start,
            ));
        }
        Ok(layout.clone())
    }
}

fn wasm_ty_size(ty: WasmType) -> i32 {
    match ty {
        WasmType::F64 => 8,
        WasmType::I32 => 4,
        WasmType::Void => 4,
    }
}

fn pretty_bound_type(bt: &BoundType) -> String {
    match bt {
        BoundType::I32 => "i32".to_string(),
        BoundType::F64 => "f64".to_string(),
        BoundType::Bool => "boolean".to_string(),
        BoundType::Str => "string".to_string(),
        BoundType::Class(name) => name.clone(),
        BoundType::Union { name, .. } => name.clone(),
        BoundType::Never => "never".to_string(),
    }
}

/// Per-field semantic type from layout markers. `field_string_types` /
/// `field_class_types` are populated at registration; bool collapses to i32
/// because the layout doesn't track a `field_bool_types` set.
fn field_bound_type(layout: &ClassLayout, name: &str, wasm_ty: WasmType) -> BoundType {
    if layout.field_string_types.contains(name) {
        return BoundType::Str;
    }
    if let Some(cn) = layout.field_class_types.get(name) {
        return BoundType::Class(cn.clone());
    }
    match wasm_ty {
        WasmType::F64 => BoundType::F64,
        _ => BoundType::I32,
    }
}

/// Reject heterogeneous shapes (`{a: number, b: string}`) for `values` /
/// `entries`. The roadmap explicitly defers mixed primitive unions; same
/// stance applies here. Stricter than WasmType equality — a shape with
/// `{a: string, b: number}` (both i32-wide if `b` is also i32) still gets
/// rejected because the fingerprint of `Array<string | i32>` would need a
/// runtime tag.
fn homogeneous_value_bound_type(
    layout: &ClassLayout,
    api: &str,
) -> Result<BoundType, CompileError> {
    let first = field_bound_type(layout, &layout.fields[0].0, layout.fields[0].2);
    for (name, _, ty) in &layout.fields {
        let bt = field_bound_type(layout, name, *ty);
        if bt != first {
            return Err(CompileError::type_err(format!(
                "{api}: type '{}' has fields with mixed types \
                 (first field is {}, but field '{name}' is {}) — \
                 the typed-subset stance requires all fields to share a type",
                layout.name,
                pretty_bound_type(&first),
                pretty_bound_type(&bt),
            )));
        }
    }
    Ok(first)
}

/// Allocate `[len][cap][N * esize]` and write the header. Returns the local
/// holding the fresh pointer. Mirrors `emit_array_of`'s preamble — extracted
/// because the three Object statics share it.
fn emit_array_header_alloc(
    ctx: &mut FuncContext<'_>,
    count: i32,
    esize: i32,
) -> Result<u32, CompileError> {
    let total = ARRAY_HEADER_SIZE as i32 + count * esize;
    ctx.push(Instruction::I32Const(total));
    let ptr_local = ctx.emit_arena_alloc_to_local(true)?;

    ctx.push(Instruction::LocalGet(ptr_local));
    ctx.push(Instruction::I32Const(count));
    ctx.push(Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    ctx.push(Instruction::LocalGet(ptr_local));
    ctx.push(Instruction::I32Const(count));
    ctx.push(Instruction::I32Store(wasm_encoder::MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    Ok(ptr_local)
}

/// Look up the synthetic class name for the tuple `[string, <value_bt>]`.
/// Returns `None` if no such tuple shape was registered during the
/// pre-codegen walk — i.e. the user hasn't written a `[string, T][]`
/// annotation anywhere in their program.
fn lookup_string_value_tuple_class(
    ctx: &FuncContext<'_>,
    value_bt: &BoundType,
) -> Option<String> {
    let elements = vec![BoundType::Str, value_bt.clone()];
    let fp = tuple_fingerprint_of(&elements);
    ctx.module_ctx
        .shape_registry
        .get_by_fingerprint(&fp)
        .map(|s| s.name.clone())
}
