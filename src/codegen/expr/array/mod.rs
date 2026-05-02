mod construction;
mod immutable;
mod mutation;
mod query;

use oxc_ast::ast::*;
use wasm_encoder::Instruction;

use crate::codegen::func::FuncContext;
use crate::error::CompileError;
use crate::types::WasmType;

/// Recognize the `{length: <expr>}` single-property object literal that
/// `Array.from` accepts as a sequence-generation input. Returns the length
/// expression if the shape matches exactly; any additional properties (or a
/// shorthand / spread / getter / computed key) disqualify. Kept narrow on
/// purpose: once general object literals land, other shapes will route
/// through the regular object-expression path and this pattern will keep
/// firing only for the sequence-generation idiom.
pub(super) fn extract_length_only_object<'a, 'b>(expr: &'b Expression<'a>) -> Option<&'b Expression<'a>> {
    match expr {
        Expression::ParenthesizedExpression(p) => extract_length_only_object(&p.expression),
        Expression::ObjectExpression(obj) => {
            if obj.properties.len() != 1 {
                return None;
            }
            let prop = match &obj.properties[0] {
                ObjectPropertyKind::ObjectProperty(p) => p,
                _ => return None,
            };
            if prop.shorthand || prop.method || prop.computed {
                return None;
            }
            let key_ok = match &prop.key {
                PropertyKey::StaticIdentifier(id) => id.name.as_str() == "length",
                PropertyKey::StringLiteral(s) => s.value.as_str() == "length",
                _ => false,
            };
            if !key_ok {
                return None;
            }
            Some(&prop.value)
        }
        _ => None,
    }
}

impl<'a> FuncContext<'a> {
    // ---- Phase 4: Arrays ----

    /// Emit arr.length (load i32 at arr+0)
    pub(crate) fn emit_array_property(
        &mut self,
        member: &StaticMemberExpression<'a>,
        prop: &str,
    ) -> Result<WasmType, CompileError> {
        match prop {
            "length" => {
                self.emit_expr(&member.object)?;
                self.push(Instruction::I32Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                Ok(WasmType::I32)
            }
            _ => Err(CompileError::codegen(format!(
                "Array has no property '{prop}' — supported: length"
            ))),
        }
    }

    /// Try to emit array method calls: arr.push(val)
    pub(crate) fn try_emit_array_method_call(
        &mut self,
        call: &CallExpression<'a>,
    ) -> Result<Option<WasmType>, CompileError> {
        let member = match &call.callee {
            Expression::StaticMemberExpression(m) => m,
            _ => return Ok(None),
        };

        let method_name = member.property.name.as_str();

        // Check if the object is a known array variable
        let elem_ty = match self.resolve_expr_array_elem(&member.object) {
            Some(ty) => ty,
            None => return Ok(None),
        };

        match method_name {
            "push" => {
                if call.arguments.len() != 1 {
                    return Err(CompileError::codegen(
                        "Array.push() expects exactly 1 argument",
                    ));
                }
                self.emit_array_push(&member.object, elem_ty, call.arguments[0].to_expression())?;
                Ok(Some(WasmType::Void))
            }
            "pop" => {
                self.expect_args(call, 0, "Array.pop")?;
                self.emit_array_pop(&member.object, elem_ty)?;
                Ok(Some(elem_ty))
            }
            "indexOf" => {
                if !matches!(call.arguments.len(), 1 | 2) {
                    return Err(CompileError::codegen(
                        "Array.indexOf expects 1 or 2 arguments",
                    ));
                }
                let from = call.arguments.get(1).map(|a| a.to_expression());
                self.emit_array_index_of(
                    &member.object,
                    elem_ty,
                    call.arguments[0].to_expression(),
                    false,
                    from,
                )?;
                Ok(Some(WasmType::I32))
            }
            "lastIndexOf" => {
                if !matches!(call.arguments.len(), 1 | 2) {
                    return Err(CompileError::codegen(
                        "Array.lastIndexOf expects 1 or 2 arguments",
                    ));
                }
                let from = call.arguments.get(1).map(|a| a.to_expression());
                self.emit_array_index_of(
                    &member.object,
                    elem_ty,
                    call.arguments[0].to_expression(),
                    true,
                    from,
                )?;
                Ok(Some(WasmType::I32))
            }
            "includes" => {
                if !matches!(call.arguments.len(), 1 | 2) {
                    return Err(CompileError::codegen(
                        "Array.includes expects 1 or 2 arguments",
                    ));
                }
                let from = call.arguments.get(1).map(|a| a.to_expression());
                self.emit_array_index_of(
                    &member.object,
                    elem_ty,
                    call.arguments[0].to_expression(),
                    false,
                    from,
                )?;
                // Convert index to bool: (idx >= 0)
                self.push(Instruction::I32Const(0));
                self.push(Instruction::I32GeS);
                Ok(Some(WasmType::I32))
            }
            "reverse" => {
                self.expect_args(call, 0, "Array.reverse")?;
                self.emit_array_reverse(&member.object, elem_ty)?;
                Ok(Some(WasmType::I32))
            }
            "toReversed" => {
                self.expect_args(call, 0, "Array.toReversed")?;
                self.emit_array_to_reversed(&member.object, elem_ty)?;
                Ok(Some(WasmType::I32))
            }
            "toSpliced" => {
                if call.arguments.is_empty() {
                    return Err(CompileError::codegen(
                        "Array.toSpliced expects at least 1 argument (start)",
                    ));
                }
                self.emit_array_to_spliced(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "with" => {
                self.expect_args(call, 2, "Array.with")?;
                self.emit_array_with(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "at" => {
                self.expect_args(call, 1, "Array.at")?;
                self.emit_array_at(&member.object, elem_ty, call.arguments[0].to_expression())?;
                Ok(Some(elem_ty))
            }
            "fill" => {
                if !matches!(call.arguments.len(), 1..=3) {
                    return Err(CompileError::codegen("Array.fill expects 1-3 arguments"));
                }
                self.emit_array_fill(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "slice" => {
                if call.arguments.len() > 2 {
                    return Err(CompileError::codegen("Array.slice expects 0-2 arguments"));
                }
                self.emit_array_slice(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "concat" => {
                if call.arguments.is_empty() {
                    return Err(CompileError::codegen(
                        "Array.concat expects at least 1 argument",
                    ));
                }
                self.emit_array_concat(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "join" => {
                if !matches!(call.arguments.len(), 0 | 1) {
                    return Err(CompileError::codegen("Array.join expects 0 or 1 arguments"));
                }
                self.emit_array_join(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "splice" => {
                if call.arguments.is_empty() {
                    return Err(CompileError::codegen(
                        "Array.splice expects at least 1 argument (start)",
                    ));
                }
                self.emit_array_splice(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "shift" => {
                self.expect_args(call, 0, "Array.shift")?;
                self.emit_array_shift(&member.object, elem_ty)?;
                Ok(Some(elem_ty))
            }
            "unshift" => {
                self.emit_array_unshift(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            "copyWithin" => {
                if !matches!(call.arguments.len(), 2 | 3) {
                    return Err(CompileError::codegen(
                        "Array.copyWithin expects 2 or 3 arguments (target, start, end?)",
                    ));
                }
                self.emit_array_copy_within(&member.object, elem_ty, call)?;
                Ok(Some(WasmType::I32))
            }
            _ => Err(CompileError::codegen(format!(
                "Array has no method '{method_name}' — supported: push, pop, shift, unshift, indexOf, lastIndexOf, includes, reverse, toReversed, at, with, fill, slice, concat, join, splice, toSpliced, copyWithin, filter, map, forEach, reduce, reduceRight, sort, toSorted, find, findIndex, findLast, findLastIndex, some, every"
            ))),
        }
    }

    pub(crate) fn emit_array_bounds_check(&mut self, arr_local: u32, idx_local: u32) {
        // if (index >= length) unreachable
        self.push(Instruction::LocalGet(idx_local));
        self.push(Instruction::LocalGet(arr_local));
        self.push(Instruction::I32Load(wasm_encoder::MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        })); // load length
        self.push(Instruction::I32GeU); // unsigned comparison: catches negative indices too
        self.push(Instruction::If(wasm_encoder::BlockType::Empty));
        self.push(Instruction::Unreachable);
        self.push(Instruction::End);
    }
    pub fn resolve_expr_array_elem(&self, expr: &Expression<'a>) -> Option<WasmType> {
        match expr {
            Expression::Identifier(ident) => self
                .local_array_elem_types
                .get(ident.name.as_str())
                .copied(),
            // arr.filter() / arr.sort() / arr.splice() / arr.slice() / arr.concat()
            // return arrays with the same element type as source
            Expression::CallExpression(call) => {
                if let Expression::StaticMemberExpression(member) = &call.callee {
                    let method = member.property.name.as_str();
                    // Static Array.of<T>(…) / Array.from(src[, mapFn]).
                    if let Expression::Identifier(obj) = &member.object
                        && obj.name.as_str() == "Array"
                    {
                        return self.resolve_array_static_call_elem(call, method);
                    }
                    // Object.keys / values / entries — element WasmType
                    // depends on the receiver's shape layout. keys is always
                    // i32 (string pointers); values follows the field
                    // WasmType; entries is i32 (tuple pointers).
                    if let Expression::Identifier(obj) = &member.object
                        && obj.name.as_str() == "Object"
                        && let Some(arg) = call.arguments.first()
                    {
                        return self.resolve_object_static_call_elem(method, arg.to_expression());
                    }
                    // `mapOrSet.keys()` / `.values()` materialize the
                    // insertion chain into a fresh `Array<X>`. Element type
                    // comes from the receiver's `HashTableInfo`: keys() pulls
                    // `slot_ty`, values() pulls `value_ty` (Map only).
                    if matches!(method, "keys" | "values")
                        && let Ok(recv_class) = self.resolve_expr_class(&member.object)
                        && let Some(info) =
                            self.module_ctx.hash_table_info.get(&recv_class)
                    {
                        return Some(match method {
                            "keys" => info.slot_ty.wasm_ty(),
                            "values" => info.value_ty.as_ref().unwrap_or(&info.slot_ty).wasm_ty(),
                            _ => unreachable!(),
                        });
                    }
                    match method {
                        "filter" | "sort" | "splice" | "slice" | "concat" | "toReversed"
                        | "toSorted" | "toSpliced" | "with" => {
                            self.resolve_expr_array_elem(&member.object)
                        }
                        "map" => {
                            // map changes the element type — infer from arrow return
                            if let Some(arg) = call.arguments.first()
                                && let Some(arrow) =
                                    self.try_extract_arrow_expr(arg.to_expression())
                            {
                                let src_elem = self.resolve_expr_array_elem(&member.object)?;
                                let src_class = self.resolve_expr_array_elem_class(&member.object);
                                let params = arrow
                                    .params
                                    .items
                                    .iter()
                                    .filter_map(|p| match &p.pattern {
                                        BindingPattern::BindingIdentifier(id) => {
                                            Some(id.name.as_str().to_string())
                                        }
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>();
                                return self
                                    .infer_arrow_result_type(
                                        arrow,
                                        &params,
                                        src_elem,
                                        src_class.as_deref(),
                                    )
                                    .ok();
                            }
                            None
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            }
            // `obj.<F>` / `this.<F>` where `<F>` is an `Array<T>` field on the
            // owner class. Routes through `field_array_elem_types`, populated
            // at class-registration time.
            Expression::StaticMemberExpression(member) => {
                let class_name = self.resolve_expr_class(&member.object).ok()?;
                let layout = self.module_ctx.class_registry.get(&class_name)?;
                layout
                    .field_array_elem_types
                    .get(member.property.name.as_str())
                    .copied()
            }
            _ => None,
        }
    }

    /// Element type resolution for `Array.of<T>(...)` / `Array.from(src[, mapFn])`.
    /// Mirrors the rules used during emission so chained calls (e.g.
    /// `Array.from(xs).filter(...)`) can carry element-type tracking through
    /// the same call-expression dispatch path.
    fn resolve_array_static_call_elem(
        &self,
        call: &CallExpression<'a>,
        method: &str,
    ) -> Option<WasmType> {
        match method {
            "of" => {
                if let Some(type_args) = call.type_arguments.as_ref()
                    && let Some(first) = type_args.params.first()
                {
                    return crate::types::resolve_ts_type_full(
                        first,
                        &self.module_ctx.class_names,
                        self.type_bindings.as_ref(),
                        Some(&self.module_ctx.non_i32_union_wasm_types),
                    )
                    .ok();
                }
                if let Some(first) = call.arguments.first() {
                    return self.infer_init_type(first.to_expression()).ok().map(|t| t.0);
                }
                None
            }
            "from" => {
                let src_expr = call.arguments.first()?.to_expression();

                // `Array.from({length: n}, mapFn)` — element type comes from
                // the explicit `<T>` if given, else from the mapFn return
                // inferred with value_ty defaulted to i32 (since `undefined`
                // isn't in the typed subset).
                if extract_length_only_object(src_expr).is_some() {
                    if let Some(type_args) = call.type_arguments.as_ref()
                        && let Some(first) = type_args.params.first()
                    {
                        return crate::types::resolve_ts_type(first, &self.module_ctx.class_names)
                            .ok();
                    }
                    let arrow = self.try_extract_arrow_expr(call.arguments.get(1)?.to_expression())?;
                    let params = arrow
                        .params
                        .items
                        .iter()
                        .filter_map(|p| match &p.pattern {
                            BindingPattern::BindingIdentifier(id) => {
                                Some(id.name.as_str().to_string())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    return self
                        .infer_arrow_result_type(arrow, &params, WasmType::I32, None)
                        .ok();
                }

                let src_elem = self.resolve_expr_array_elem(src_expr)?;
                // Form 1 (src only): element type preserved.
                // Form 2 (src, mapFn): inferred from the mapFn return type.
                if call.arguments.len() < 2 {
                    return Some(src_elem);
                }
                let src_class = self.resolve_expr_array_elem_class(src_expr);
                let arrow = self.try_extract_arrow_expr(call.arguments[1].to_expression())?;
                let params = arrow
                    .params
                    .items
                    .iter()
                    .filter_map(|p| match &p.pattern {
                        BindingPattern::BindingIdentifier(id) => {
                            Some(id.name.as_str().to_string())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                self.infer_arrow_result_type(arrow, &params, src_elem, src_class.as_deref())
                    .ok()
            }
            _ => None,
        }
    }

    /// Resolve the array element class name for an expression (if elements are class instances).
    pub fn resolve_expr_array_elem_class(&self, expr: &Expression<'a>) -> Option<String> {
        match expr {
            Expression::Identifier(ident) => self
                .local_array_elem_classes
                .get(ident.name.as_str())
                .cloned(),
            // Chained calls: filter/sort/splice/slice/concat preserve element class.
            // `Array.from(src)` also preserves it (shallow clone).
            Expression::CallExpression(call) => {
                if let Expression::StaticMemberExpression(member) = &call.callee {
                    let method = member.property.name.as_str();
                    if let Expression::Identifier(obj) = &member.object
                        && obj.name.as_str() == "Array"
                        && method == "from"
                        && call.arguments.len() == 1
                    {
                        return self.resolve_expr_array_elem_class(
                            call.arguments[0].to_expression(),
                        );
                    }
                    // `mapOrSet.keys()` / `.values()` carry through the
                    // class name when the column is class-typed
                    // (`BoundType::Class(name)`). `.entries()` always
                    // produces an `Array<__Tuple$K$V>` whose element class
                    // is the synthetic pair shape registered per Map/Set
                    // instantiation (see `module.rs` :: `ensure_tuple_shape`).
                    if matches!(method, "keys" | "values" | "entries")
                        && let Ok(recv_class) = self.resolve_expr_class(&member.object)
                        && let Some(info) =
                            self.module_ctx.hash_table_info.get(&recv_class)
                    {
                        if method == "entries" {
                            let elements = match info.value_ty.as_ref() {
                                Some(v_ty) => vec![info.slot_ty.clone(), v_ty.clone()],
                                None => vec![info.slot_ty.clone(), info.slot_ty.clone()],
                            };
                            let pair_class = format!(
                                "__Tuple${}",
                                crate::codegen::shapes::tuple_fingerprint_of(&elements)
                            );
                            return Some(pair_class);
                        }
                        let column = match method {
                            "keys" => &info.slot_ty,
                            "values" => info.value_ty.as_ref().unwrap_or(&info.slot_ty),
                            _ => unreachable!(),
                        };
                        if let crate::types::BoundType::Class(name) = column {
                            return Some(name.clone());
                        }
                        return None;
                    }
                    // Object.entries(p) — element class is the registered
                    // tuple shape `[string, T]` derived from p's layout.
                    // Object.keys / values produce primitive-element arrays
                    // (no element class).
                    if let Expression::Identifier(obj) = &member.object
                        && obj.name.as_str() == "Object"
                        && method == "entries"
                        && let Some(arg) = call.arguments.first()
                    {
                        return self.resolve_object_entries_elem_class(arg.to_expression());
                    }
                    match method {
                        "filter" | "sort" | "splice" | "slice" | "concat" | "toReversed"
                        | "toSorted" | "toSpliced" | "with" => {
                            self.resolve_expr_array_elem_class(&member.object)
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            }
            // `obj.<F>` / `this.<F>` where `<F>` is an `Array<C>` field for
            // some class `C`. Threads the element class through the same path
            // as `local_array_elem_classes` does for local bindings.
            Expression::StaticMemberExpression(member) => {
                let class_name = self.resolve_expr_class(&member.object).ok()?;
                let layout = self.module_ctx.class_registry.get(&class_name)?;
                layout
                    .field_array_elem_classes
                    .get(member.property.name.as_str())
                    .cloned()
            }
            _ => None,
        }
    }

    /// Element WasmType for `Object.<keys|values|entries>(arg)`. `keys` /
    /// `entries` always yield i32 element arrays (string ptrs / tuple ptrs);
    /// `values` follows the receiver shape's homogeneous field WasmType.
    fn resolve_object_static_call_elem(
        &self,
        method: &str,
        arg: &Expression<'a>,
    ) -> Option<WasmType> {
        match method {
            "keys" | "entries" => Some(WasmType::I32),
            "values" => {
                let class_name = self.resolve_expr_class(arg).ok()?;
                let layout = self.module_ctx.class_registry.get(&class_name)?;
                layout.fields.first().map(|(_, _, ty)| *ty)
            }
            _ => None,
        }
    }

    /// Tuple class for `Object.entries(arg)` elements. Returns `None` if the
    /// receiver isn't a registered shape, isn't homogeneous, or the
    /// `[string, T]` tuple shape wasn't pre-registered (in which case
    /// emission produces a clearer diagnostic anyway).
    fn resolve_object_entries_elem_class(
        &self,
        arg: &Expression<'a>,
    ) -> Option<String> {
        use crate::types::BoundType;
        let class_name = self.resolve_expr_class(arg).ok()?;
        let layout = self.module_ctx.class_registry.get(&class_name)?;
        let first_name = &layout.fields.first()?.0;
        let first_ty = layout.fields.first()?.2;
        let first_bt = field_bound_type_for_layout(layout, first_name, first_ty);
        for (n, _, ty) in &layout.fields {
            let bt = field_bound_type_for_layout(layout, n, *ty);
            if bt != first_bt {
                return None;
            }
        }
        let elements = vec![BoundType::Str, first_bt];
        let fp = crate::codegen::shapes::tuple_fingerprint_of(&elements);
        self.module_ctx
            .shape_registry
            .get_by_fingerprint(&fp)
            .map(|s| s.name.clone())
    }
}

fn field_bound_type_for_layout(
    layout: &crate::codegen::classes::ClassLayout,
    name: &str,
    wasm_ty: WasmType,
) -> crate::types::BoundType {
    use crate::types::BoundType;
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
