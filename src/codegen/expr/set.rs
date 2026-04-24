//! Per-monomorphization method dispatch for compiler-owned `Set<T>`.
//!
//! Set methods are emitted inline at each call site, mirroring the Map path:
//! per-(T) specialization avoids a call boundary and lets us pick the right
//! hash + equality helpers without generic dispatch overhead. Returns
//! `Ok(None)` when the call's receiver isn't a Set so upstream dispatchers
//! keep their normal fall-through behavior.
//!
//! All method bodies are Map/Set-shared and live in `hash_table.rs`; the
//! kind split (Set has no value slot, `forEach` takes exactly one param,
//! `add` replaces `set`) falls out of `info.value_ty.is_some()` plus the
//! per-method dispatcher wiring here. This file is only the dispatcher.

use oxc_ast::ast::*;

use crate::codegen::func::FuncContext;
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
                self.emit_hash_table_insert(&member.object, &class_name, arg, None)?;
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

}
