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

use std::collections::HashSet;

use wasm_encoder::Instruction;

use crate::codegen::func::{FuncContext, Refinement, peel_parens};
use crate::codegen::shapes::TagValue;
use crate::codegen::unions::UnionMember;
use crate::error::CompileError;
use crate::types::{NEVER_CLASS_NAME, WasmType};

impl<'a> FuncContext<'a> {
    /// Emit a structural coercion from `source_class` to `target_class`
    /// on the value currently at the top of the stack. See module docs
    /// for semantics. When the source local has a refinement narrower
    /// than its declared type, callers pass it via `source_refinement`
    /// so the union arms can validate against the refined member set
    /// rather than the original union's full membership.
    pub(crate) fn emit_shape_coerce(
        &mut self,
        source_class: &str,
        target_class: &str,
        source_refinement: Option<&Refinement>,
    ) -> Result<(), CompileError> {
        if source_class == target_class {
            return Ok(());
        }
        // `Refinement::Never` means the source is unreachable at this
        // program point — assigning it to anything is vacuously type-safe
        // (no value can actually flow). The wasm value on the stack is
        // never observed because no execution reaches this site in a
        // sound program.
        if matches!(source_refinement, Some(Refinement::Never)) {
            return Ok(());
        }
        // `Never → T`: a value of type `never` is uninhabited, so reading
        // from a `: never` slot is unreachable. Vacuously type-safe to
        // assign anywhere. The matching `T → Never` direction is the
        // exhaustiveness gate handled below.
        if source_class == NEVER_CLASS_NAME {
            return Ok(());
        }
        // `T → Never`: the load-bearing exhaustiveness check. A non-Never
        // source is assignable to a `: never` slot only if its current
        // refinement has reduced it to the empty set (handled by the
        // `Refinement::Never` early return above). Anything else is a
        // compile error whose message lists the still-possible variants —
        // that diagnostic is the entire point of the `: never` pattern.
        if target_class == NEVER_CLASS_NAME {
            return Err(self.never_assignment_error(source_class, source_refinement));
        }
        // Union targets: zero-copy when source is a known member (variant
        // pointer or another subset union). Phase 1 union members are all
        // i32-representable, so the runtime value passes through unchanged;
        // we only need to type-check membership here.
        if let Some(target_union) =
            self.module_ctx.union_registry.get_by_name(target_class)
        {
            // Sub-phase 1.5.1: a refined source replaces the source-side
            // member set — even when the declared type is a wider union,
            // the refined sub-union may still be assignable to the target.
            if let Some(Refinement::Subunion(members)) = source_refinement {
                let target_set: HashSet<String> = target_union
                    .members
                    .iter()
                    .map(|m| m.canonical())
                    .collect();
                if members.iter().all(|m| target_set.contains(&m.canonical())) {
                    return Ok(());
                }
                return Err(CompileError::type_err(format!(
                    "cannot assign refined sub-union of '{source_class}' to \
                     '{target_class}': source has members not in target"
                )));
            }
            // Source is itself a union — require its member set to be a
            // subset of the target's (union → union widening).
            if let Some(source_union) =
                self.module_ctx.union_registry.get_by_name(source_class)
            {
                if source_union.is_subset_of(target_union) {
                    return Ok(());
                }
                return Err(CompileError::type_err(format!(
                    "cannot assign union '{source_class}' to union '{target_class}': \
                     source has members not in target"
                )));
            }
            // Source is a variant — require it to be one of the union's
            // members. Members are matched by canonical name, which for
            // shape members is the registered shape / class name.
            if target_union.contains(source_class) {
                return Ok(());
            }
            return Err(CompileError::type_err(format!(
                "cannot assign '{source_class}' to union '{target_class}': \
                 '{source_class}' is not a member of the union"
            )));
        }
        // Source is a union, target is a variant: rejected unless
        // refinement has narrowed it to a single class — in which case
        // `source_class` would have been resolved to the refined class
        // by the caller (via `resolve_expr_class` consulting
        // `current_class_of`) and we'd have hit the `==` short-circuit at
        // the top of this function.
        if self
            .module_ctx
            .union_registry
            .get_by_name(source_class)
            .is_some()
        {
            return Err(CompileError::type_err(format!(
                "cannot assign union '{source_class}' to variant '{target_class}' \
                 without narrowing — guard with `if (x.kind === '...')` before the \
                 assignment"
            )));
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
        // shape and union targets care about coercion. Real classes keep
        // nominal semantics (inheritance covers upcasts).
        let is_shape_target = self
            .module_ctx
            .shape_registry
            .by_name
            .contains_key(target);
        let is_union_target = self
            .module_ctx
            .union_registry
            .get_by_name(target)
            .is_some();
        let is_never_target = target == NEVER_CLASS_NAME;
        if !is_shape_target && !is_union_target && !is_never_target {
            return self.emit_expr(expr);
        }
        let source_class = self.resolve_expr_class(expr).ok();
        // Sub-phase 1.5.1: capture the source local's refinement (if
        // any) before emitting, so the coerce check sees the refined
        // sub-union or `Never` rather than only the declared type. Only
        // identifiers carry refinement; deeper expressions fall back to
        // the declared type.
        let source_refinement = match peel_parens(expr) {
            oxc_ast::ast::Expression::Identifier(ident) => {
                self.current_refinement_of(ident.name.as_str()).cloned()
            }
            _ => None,
        };
        let ty = self.emit_expr(expr)?;
        if let Some(src) = source_class {
            self.emit_shape_coerce(&src, target, source_refinement.as_ref())?;
        } else if is_never_target {
            // A primitive / un-classed source flowing into a `: never` slot
            // is always wrong — `Refinement::Never` only attaches to
            // identifiers, so there is no path that would have made this
            // assignment legal. Surface the diagnostic without a source
            // class name (the helper renders a fallback message in that
            // case).
            return Err(self.never_assignment_error("<expression>", source_refinement.as_ref()));
        }
        Ok(ty)
    }

    /// Construct the missing-variants diagnostic used by `T → never`
    /// rejections. `source_refinement` narrows the message to the variants
    /// still possible at the assignment site; with no refinement the
    /// declared union is enumerated in full. For a non-union source the
    /// message degenerates to the source's name — this is the
    /// "called assertNever with a value of type X" case.
    fn never_assignment_error(
        &self,
        source_class: &str,
        source_refinement: Option<&Refinement>,
    ) -> CompileError {
        let remaining: Vec<String> = match source_refinement {
            Some(Refinement::Class(c)) => vec![c.clone()],
            Some(Refinement::Subunion(members)) => {
                members.iter().map(member_display_name).collect()
            }
            // `Never` is the success path — handled in `emit_shape_coerce`
            // before this helper runs. Reached here means the caller
            // constructed the diagnostic for a primitive source with no
            // identifier-level refinement.
            Some(Refinement::Never) | None => match self
                .module_ctx
                .union_registry
                .get_by_name(source_class)
            {
                Some(layout) => layout.members.iter().map(member_display_name).collect(),
                None => vec![source_class.to_string()],
            },
        };
        let list = remaining.join(", ");
        CompileError::type_err(format!(
            "cannot assign '{source_class}' to 'never': value is still inhabited by \
             [{list}] at this program point — handle every variant before reaching the \
             `never` slot to make the switch exhaustive"
        ))
    }
}

/// Render a union member for diagnostic display. Shape members use the
/// shape's source name; literal members render in source-faithful form
/// (`'red'`, `1`, `true`) so users see exactly the discriminator they
/// missed rather than the fingerprint-safe `s_red` / `i_1` encoding.
fn member_display_name(m: &UnionMember) -> String {
    match m {
        UnionMember::Shape(n) => n.clone(),
        UnionMember::Literal(tv) => match tv {
            TagValue::Str(s) => format!("'{s}'"),
            TagValue::I32(n) => n.to_string(),
            TagValue::F64(n) => n.to_string(),
            TagValue::Bool(b) => b.to_string(),
        },
    }
}

pub(crate) fn emit_field_load(ctx: &mut FuncContext<'_>, offset: u32, ty: WasmType) {
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

pub(crate) fn emit_field_store(ctx: &mut FuncContext<'_>, offset: u32, ty: WasmType) {
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
