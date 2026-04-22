//! Structural coercion between shape types (Phase C.1 of
//! `plan-object-literals-tuples.md`).
//!
//! When a value of one shape is assigned to a slot declared with a different
//! shape, the two synthetic class layouts may or may not line up. Examples:
//!
//! - `let p: Point = getPoint3D()` where `Point3D extends Point` — C.2
//!   guarantees prefix-compatible layout in the common case, so a pointer
//!   reassignment is sound. Zero-copy.
//! - Shapes that *happen* to share prefix offsets (first-declaration-wins
//!   layout, field types aligned the same way) — also zero-copy.
//! - Shapes declared in orders that leave the target's fields at different
//!   offsets than the source's — need a field-pick copy into a fresh
//!   target-sized allocation.
//!
//! This module implements the check + emit. It never fires on literals
//! (`emit_object_literal` already writes the target layout directly) or on
//! real classes (nominal equality + `extends`-based upcast already handle
//! those). It's only reachable when both sides resolve to a registered
//! shape name and the names differ.
//!
//! Preconditions at each call site: a pointer of type `source_class` is on
//! top of the WASM stack. Postcondition: a pointer of type `target_class`
//! is on top of the WASM stack (which may be the same pointer, for the
//! zero-copy path, or a freshly allocated copy).

use wasm_encoder::Instruction;

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

impl<'a> FuncContext<'a> {
    /// Emit a structural coercion from `source_class` to `target_class` on the
    /// value currently at the top of the stack. See module docs for semantics.
    pub(crate) fn emit_shape_coerce(
        &mut self,
        source_class: &str,
        target_class: &str,
    ) -> Result<(), CompileError> {
        if source_class == target_class {
            return Ok(());
        }
        // Only structurally coerce when the target is a registered shape.
        // Real classes keep nominal semantics (inheritance covers upcasts).
        if !self
            .module_ctx
            .shape_registry
            .by_name
            .contains_key(target_class)
        {
            return Ok(());
        }

        let source_layout = self
            .module_ctx
            .class_registry
            .get(source_class)
            .ok_or_else(|| {
                CompileError::codegen(format!(
                    "structural coerce: source class '{source_class}' not registered"
                ))
            })?
            .clone();
        let target_layout = self
            .module_ctx
            .class_registry
            .get(target_class)
            .ok_or_else(|| {
                CompileError::codegen(format!(
                    "structural coerce: target shape '{target_class}' not registered"
                ))
            })?
            .clone();

        // Collect (src_offset, tgt_offset, wasm_ty) per target field and
        // detect layout-equivalence. Any missing field or type mismatch is a
        // compile error.
        let mut moves: Vec<(u32, u32, WasmType)> = Vec::with_capacity(target_layout.fields.len());
        let mut layout_equivalent = true;
        for (fname, tgt_offset, tgt_ty) in &target_layout.fields {
            let &(src_offset, src_ty) = source_layout.field_map.get(fname).ok_or_else(|| {
                CompileError::type_err(format!(
                    "cannot assign '{source_class}' to '{target_class}': field '{fname}' is \
                     missing from the source type"
                ))
            })?;
            if src_ty != *tgt_ty {
                return Err(CompileError::type_err(format!(
                    "cannot assign '{source_class}' to '{target_class}': field '{fname}' has \
                     type {src_ty:?} in source but {tgt_ty:?} in target"
                )));
            }
            if src_offset != *tgt_offset {
                layout_equivalent = false;
            }
            moves.push((src_offset, *tgt_offset, *tgt_ty));
        }

        if layout_equivalent {
            // Prefix-compatible — source pointer is already usable as target.
            return Ok(());
        }

        // Field-pick copy. Stash source pointer, allocate target-sized block,
        // move each field, leave new pointer on stack.
        let src_local = self.alloc_local(WasmType::I32);
        self.push(Instruction::LocalSet(src_local));

        self.push(Instruction::I32Const(target_layout.size as i32));
        let dst_local = self.emit_arena_alloc_to_local(true)?;

        for (src_offset, tgt_offset, ty) in moves {
            self.push(Instruction::LocalGet(dst_local));
            self.push(Instruction::LocalGet(src_local));
            emit_field_load(self, src_offset, ty);
            emit_field_store(self, tgt_offset, ty);
        }

        self.push(Instruction::LocalGet(dst_local));
        Ok(())
    }

    /// Convenience wrapper: emit `expr` and then structurally coerce the
    /// result to `target_class` if both sides are resolvable shape classes.
    /// Falls through with the plain `emit_expr` result when `target_class` is
    /// `None`, when `expr` doesn't resolve to a known class, or when the
    /// target isn't a shape — preserving today's behavior for non-shape paths.
    ///
    /// Object-literal expressions are handled by the caller (via
    /// `emit_object_literal` directly), because the literal's fingerprint
    /// path writes the target layout in one step and never needs coercion.
    pub(crate) fn emit_expr_coerced(
        &mut self,
        expr: &oxc_ast::ast::Expression<'a>,
        target_class: Option<&str>,
    ) -> Result<WasmType, CompileError> {
        let Some(target) = target_class else {
            return self.emit_expr(expr);
        };
        // Cheap opt-out before paying for source-class resolution: only
        // shape targets can receive structural coercion.
        let is_shape_target = self
            .module_ctx
            .shape_registry
            .by_name
            .contains_key(target);
        if !is_shape_target {
            return self.emit_expr(expr);
        }
        let source_class = self.resolve_expr_class(expr).ok();
        let ty = self.emit_expr(expr)?;
        if let Some(src) = source_class {
            self.emit_shape_coerce(&src, target)?;
        }
        Ok(ty)
    }
}

fn emit_field_load(ctx: &mut FuncContext<'_>, offset: u32, ty: WasmType) {
    match ty {
        WasmType::F64 => ctx.push(Instruction::F64Load(wasm_encoder::MemArg {
            offset: offset as u64,
            align: 3,
            memory_index: 0,
        })),
        WasmType::I32 => ctx.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: offset as u64,
            align: 2,
            memory_index: 0,
        })),
        WasmType::Void => {}
    }
}

fn emit_field_store(ctx: &mut FuncContext<'_>, offset: u32, ty: WasmType) {
    match ty {
        WasmType::F64 => ctx.push(Instruction::F64Store(wasm_encoder::MemArg {
            offset: offset as u64,
            align: 3,
            memory_index: 0,
        })),
        WasmType::I32 => ctx.push(Instruction::I32Store(wasm_encoder::MemArg {
            offset: offset as u64,
            align: 2,
            memory_index: 0,
        })),
        WasmType::Void => {}
    }
}
