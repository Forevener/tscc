use std::collections::{HashMap, HashSet};

use oxc_ast::ast::{TSType, TSTypeAnnotation};
use wasm_encoder::ValType;

use crate::codegen::shapes::ShapeRegistry;
use crate::codegen::unions::UnionRegistry;
use crate::error::CompileError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmType {
    I32,
    F64,
    Void,
}

/// Signature of a closure/function value: parameter types + return type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureSig {
    pub param_types: Vec<WasmType>,
    pub return_type: WasmType,
}

/// Resolved concrete type that a generic type parameter is bound to in a given
/// monomorphization. Richer than `WasmType` because Map/Set keys + class fields
/// need to distinguish `string` from other i32's, and class-ref bindings carry
/// the class name for member access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundType {
    I32,
    F64,
    Bool,
    Str,
    /// Class reference; holds the concrete class name (post-monomorphization if
    /// the class was itself generic).
    Class(String),
    /// Union reference; carries the registered union name **and** the
    /// `WasmType` shared by every member. The `wasm_ty` field exists so a
    /// `Box<Half>` (where `Half = 0.5 | 1.5`) generic instantiation resolves
    /// to `F64` through the `bindings.get(...).wasm_ty()` path without
    /// consulting `UnionRegistry` at every lookup. Most unions are `I32`
    /// (shape, class, string-literal, int-literal, bool-literal); pure-`f64`
    /// literal unions are `F64`.
    Union {
        name: String,
        wasm_ty: WasmType,
    },
    /// The `never` type (Phase 1.5 sub-phase 4). Has no inhabitants — its
    /// only legal source is a `Refinement::Never` value (post-exhaustive
    /// switch). Resolves to `WasmType::I32` because the codegen pipeline
    /// assumes every type has a `WasmType`; the value is never actually
    /// loaded at runtime in a sound program.
    ///
    /// `: never` annotations flow through the `target_class: &str` channel
    /// as `NEVER_CLASS_NAME` (string) rather than as this `BoundType`
    /// variant, so this variant is dead-but-reserved for future generics /
    /// class-union narrowing work (Phase 2).
    #[allow(
        dead_code,
        reason = "reserved for Phase 2 class-union narrowing; the `: never` annotation path uses NEVER_CLASS_NAME"
    )]
    Never,
}

/// Pseudo-class name used by `get_class_type_name_*` to flow `: never`
/// annotations through the same `target_class: &str` channel that shape
/// and union targets use. Begins with `__` so it cannot collide with a
/// user-declared class or shape name.
pub const NEVER_CLASS_NAME: &str = "__Never";

impl BoundType {
    pub fn wasm_ty(&self) -> WasmType {
        match self {
            BoundType::F64 => WasmType::F64,
            BoundType::Union { wasm_ty, .. } => *wasm_ty,
            _ => WasmType::I32,
        }
    }

    /// Short token used in mangled names: `i32`, `f64`, `bool`, `string`, or the
    /// class / union name verbatim (already unique — synthetic names like
    /// `Box$i32` and `__Union$kind_circle$kind_square` are constructed to be
    /// globally distinct).
    pub fn mangle_token(&self) -> String {
        match self {
            BoundType::I32 => "i32".to_string(),
            BoundType::F64 => "f64".to_string(),
            BoundType::Bool => "bool".to_string(),
            BoundType::Str => "string".to_string(),
            BoundType::Class(name) => name.clone(),
            BoundType::Union { name, .. } => name.clone(),
            BoundType::Never => "never".to_string(),
        }
    }
}

/// Map from type-parameter name (as written in source, e.g. `T`, `K`, `V`) to
/// the concrete type it resolves to in the current monomorphization scope.
pub type TypeBindings = HashMap<String, BoundType>;

impl WasmType {
    pub fn to_val_type(self) -> Option<ValType> {
        match self {
            WasmType::I32 => Some(ValType::I32),
            WasmType::F64 => Some(ValType::F64),
            WasmType::Void => None,
        }
    }
}

pub fn resolve_type_annotation(annotation: &TSTypeAnnotation) -> Result<WasmType, CompileError> {
    resolve_ts_type(&annotation.type_annotation, &HashSet::new())
}

/// Resolve a type annotation with class names, type-parameter bindings, and
/// union overrides. The override map carries union names whose `WasmType` is
/// not `I32` — typically pure-`f64`-literal unions (`type X = 0.5 | 1.5`) —
/// so the resolver returns the correct `WasmType` for those names instead of
/// the default-`I32` mapping that `class_names` membership would otherwise
/// give. User-facing annotation sites (variable decls, function
/// params/returns, class fields, methods) use this variant.
pub fn resolve_type_annotation_with_unions(
    annotation: &TSTypeAnnotation,
    class_names: &HashSet<String>,
    bindings: Option<&TypeBindings>,
    union_overrides: &HashMap<String, WasmType>,
) -> Result<WasmType, CompileError> {
    resolve_ts_type_full(
        &annotation.type_annotation,
        class_names,
        bindings,
        Some(union_overrides),
    )
}

pub fn resolve_ts_type(
    ts_type: &TSType,
    class_names: &HashSet<String>,
) -> Result<WasmType, CompileError> {
    resolve_ts_type_full(ts_type, class_names, None, None)
}

/// Full resolver — class names, bindings, and union overrides. Internal
/// recursion goes through this so nested annotations (`Array<Half>`,
/// `Box<Half>`) also see the override map.
pub fn resolve_ts_type_full(
    ts_type: &TSType,
    class_names: &HashSet<String>,
    bindings: Option<&TypeBindings>,
    union_overrides: Option<&HashMap<String, WasmType>>,
) -> Result<WasmType, CompileError> {
    match ts_type {
        TSType::TSTypeReference(type_ref) => {
            let name = type_ref
                .type_name
                .get_identifier_reference()
                .map(|r| r.name.as_str());
            if let Some(other) = name
                && let Some(bound) = bindings.and_then(|b| b.get(other))
            {
                return Ok(bound.wasm_ty());
            }
            match name {
                Some("i32") => Ok(WasmType::I32),
                Some("f64") => Ok(WasmType::F64),
                Some("bool") => Ok(WasmType::I32),
                Some("int") => Ok(WasmType::I32),
                Some("Array") => {
                    // Array<T> is a pointer into linear memory (i32)
                    // Element type is extracted separately via get_array_element_type()
                    Ok(WasmType::I32)
                }
                Some(other) if class_names.contains(other) => {
                    // Named-union override: pure-`f64`-literal unions resolve
                    // to `F64` rather than the default `I32` for
                    // `class_names`-resident names. The override map only
                    // carries entries whose `wasm_ty != I32`, so this is a
                    // single hashmap lookup per `class_names` hit.
                    if let Some(overrides) = union_overrides
                        && let Some(&wt) = overrides.get(other)
                    {
                        return Ok(wt);
                    }
                    // Class references are i32 pointers into linear memory
                    Ok(WasmType::I32)
                }
                Some("string") => Ok(WasmType::I32),
                Some(other) => {
                    // Generic class reference: `Box<i32>` is a class pointer
                    // even though "Box" itself isn't in class_names — the
                    // mangled instantiation is.
                    if type_ref.type_arguments.is_some() {
                        return Ok(WasmType::I32);
                    }
                    Err(CompileError::type_err(format!(
                        "unknown type '{other}' — supported types: i32, f64, int, bool, number, string, Array<T>, or a class name"
                    )))
                }
                None => Err(CompileError::type_err(
                    "complex type references not supported",
                )),
            }
        }
        TSType::TSVoidKeyword(_) => Ok(WasmType::Void),
        // `never` resolves to I32 — the value is never actually loaded in a
        // sound program (a `: never` slot is reachable only from a
        // `Refinement::Never` source), but the codegen pipeline expects every
        // type to have a concrete `WasmType`.
        TSType::TSNeverKeyword(_) => Ok(WasmType::I32),
        TSType::TSBooleanKeyword(_) => Ok(WasmType::I32),
        TSType::TSNumberKeyword(_) => Ok(WasmType::F64),
        TSType::TSStringKeyword(_) => Ok(WasmType::I32),
        TSType::TSFunctionType(_) => {
            // Function types are closure pointers (i32) in linear memory
            Ok(WasmType::I32)
        }
        TSType::TSArrayType(_) => {
            // T[] is equivalent to Array<T> — a pointer into linear memory (i32).
            Ok(WasmType::I32)
        }
        TSType::TSTypeLiteral(_) => Ok(WasmType::I32),
        TSType::TSTupleType(_) => Ok(WasmType::I32),
        TSType::TSUnionType(u) => {
            // Walk members and unify the `WasmType`. Pure-`f64`-literal
            // unions (`0.5 | 1.5`) resolve to `F64`; everything else
            // (shape / class / string-literal / int-literal / bool-literal)
            // resolves to `I32`. Mixed-WasmType combinations are rejected
            // at registration in `unions.rs::validate_uniform_wasm_ty`, so
            // by the time codegen reads back the type the members are
            // guaranteed uniform — but we still defend against the dead
            // path with `I32` on disagreement.
            let mut chosen: Option<WasmType> = None;
            for t in &u.types {
                let wt = resolve_ts_type_full(t, class_names, bindings, union_overrides)?;
                match chosen {
                    None => chosen = Some(wt),
                    Some(c) if c == wt => {}
                    Some(_) => return Ok(WasmType::I32),
                }
            }
            Ok(chosen.unwrap_or(WasmType::I32))
        }
        TSType::TSLiteralType(lit) => {
            // Literal types resolve to the underlying primitive's WasmType.
            // The literal value itself is captured by the shape walker as a
            // `tag_value` on the field; this resolver only sees the WasmType
            // because outside shape contexts (e.g. function params), the
            // literal is just a refinement of the primitive.
            use oxc_ast::ast::TSLiteral;
            match &lit.literal {
                TSLiteral::StringLiteral(_) | TSLiteral::TemplateLiteral(_) => Ok(WasmType::I32),
                TSLiteral::BooleanLiteral(_) => Ok(WasmType::I32),
                TSLiteral::NumericLiteral(n) => {
                    let is_float = n.raw.as_deref().is_some_and(|r| r.contains('.'))
                        || n.value.fract() != 0.0
                        || !((i32::MIN as f64)..=(i32::MAX as f64)).contains(&n.value);
                    Ok(if is_float { WasmType::F64 } else { WasmType::I32 })
                }
                TSLiteral::UnaryExpression(_) => Ok(WasmType::F64),
                TSLiteral::BigIntLiteral(_) => Err(CompileError::unsupported(
                    "BigInt literal type — not supported (Phase 1 union scope)",
                )),
            }
        }
        _ => Err(CompileError::type_err(
            "unsupported type annotation".to_string(),
        )),
    }
}

/// Parse a TSFunctionType annotation into a ClosureSig.
/// E.g. `(x: i32, y: f64) => i32` → ClosureSig { param_types: [I32, F64], return_type: I32 }
/// `union_overrides` carries non-`I32` union name → `WasmType` so a callback
/// like `(x: Half) => f64` over a pure-`f64`-literal union resolves correctly.
pub fn get_closure_sig(
    annotation: &TSTypeAnnotation,
    class_names: &HashSet<String>,
    union_overrides: &HashMap<String, WasmType>,
) -> Option<ClosureSig> {
    match &annotation.type_annotation {
        TSType::TSFunctionType(func_type) => {
            let mut param_types = Vec::new();
            for param in &func_type.params.items {
                let ty = if let Some(ann) = &param.type_annotation {
                    resolve_ts_type_full(
                        &ann.type_annotation,
                        class_names,
                        None,
                        Some(union_overrides),
                    )
                    .ok()?
                } else {
                    return None;
                };
                param_types.push(ty);
            }
            let return_type = resolve_ts_type_full(
                &func_type.return_type.type_annotation,
                class_names,
                None,
                Some(union_overrides),
            )
            .ok()?;
            Some(ClosureSig {
                param_types,
                return_type,
            })
        }
        _ => None,
    }
}

/// Extract the element type from an Array<T> type annotation.
/// Returns Some(WasmType) if the annotation is Array<T>, None otherwise.
pub fn get_array_element_type(
    annotation: &TSTypeAnnotation,
    class_names: &HashSet<String>,
    union_overrides: &HashMap<String, WasmType>,
) -> Option<WasmType> {
    match &annotation.type_annotation {
        TSType::TSTypeReference(type_ref) => {
            let name = type_ref
                .type_name
                .get_identifier_reference()
                .map(|r| r.name.as_str());
            if name != Some("Array") {
                return None;
            }
            // Extract T from Array<T>
            let params = type_ref.type_arguments.as_ref()?;
            let first = params.params.first()?;
            resolve_ts_type_full(first, class_names, None, Some(union_overrides)).ok()
        }
        TSType::TSArrayType(arr) => {
            // T[] — extract element type from the shorthand form
            resolve_ts_type_full(&arr.element_type, class_names, None, Some(union_overrides)).ok()
        }
        _ => None,
    }
}

/// Extract the class name from an Array<ClassName> type annotation.
/// Returns Some("ClassName") if element type is a class, None otherwise.
/// Bindings-aware element-class extraction. Delegates to
/// `get_class_type_name_from_ts_type_with_bindings` so that generic
/// instantiations like `Map<string, i32>` mangle correctly (`Map$string$i32`)
/// and so that a type-parameter element like `Array<T>` inside a monomorphized
/// generic resolves to its bound class name.
pub fn get_array_element_class_with_bindings(
    annotation: &TSTypeAnnotation,
    bindings: Option<&TypeBindings>,
    shape_registry: Option<&ShapeRegistry>,
    union_registry: Option<&UnionRegistry>,
) -> Option<String> {
    let first = match &annotation.type_annotation {
        TSType::TSTypeReference(type_ref) => {
            let name = type_ref
                .type_name
                .get_identifier_reference()
                .map(|r| r.name.as_str());
            if name != Some("Array") {
                return None;
            }
            let params = type_ref.type_arguments.as_ref()?;
            params.params.first()?
        }
        TSType::TSArrayType(arr) => &arr.element_type,
        _ => return None,
    };
    get_class_type_name_from_ts_type_with_bindings(first, bindings, shape_registry, union_registry)
}

/// Bindings-aware string-type check. A type parameter bound to `BoundType::Str`
/// counts as string here so that field/param tracking flags it for string-aware
/// codegen paths.
pub fn is_string_type_with_bindings(
    annotation: &TSTypeAnnotation,
    bindings: Option<&TypeBindings>,
) -> bool {
    match &annotation.type_annotation {
        TSType::TSStringKeyword(_) => true,
        TSType::TSTypeReference(type_ref) => {
            let name = type_ref
                .type_name
                .get_identifier_reference()
                .map(|r| r.name.as_str());
            if name == Some("string") {
                return true;
            }
            if let Some(other) = name
                && let Some(bound) = bindings.and_then(|b| b.get(other))
            {
                return matches!(bound, BoundType::Str);
            }
            false
        }
        _ => false,
    }
}

/// Extract class type name from a TSType (used by both annotation and as-expression paths).
pub fn get_class_type_name_from_ts_type(
    ts_type: &TSType,
    shape_registry: Option<&ShapeRegistry>,
    union_registry: Option<&UnionRegistry>,
) -> Option<String> {
    get_class_type_name_from_ts_type_with_bindings(ts_type, None, shape_registry, union_registry)
}

/// Bindings-aware variant. A type parameter bound to `BoundType::Class(name)`
/// resolves to `Some(name)`, enabling methods/fields on generic-param-typed
/// values to route through the concrete class's layout.
pub fn get_class_type_name_with_bindings(
    annotation: &TSTypeAnnotation,
    bindings: Option<&TypeBindings>,
    shape_registry: Option<&ShapeRegistry>,
    union_registry: Option<&UnionRegistry>,
) -> Option<String> {
    get_class_type_name_from_ts_type_with_bindings(
        &annotation.type_annotation,
        bindings,
        shape_registry,
        union_registry,
    )
}

pub fn get_class_type_name_from_ts_type_with_bindings(
    ts_type: &TSType,
    bindings: Option<&TypeBindings>,
    shape_registry: Option<&ShapeRegistry>,
    union_registry: Option<&UnionRegistry>,
) -> Option<String> {
    match ts_type {
        TSType::TSTypeReference(type_ref) => {
            let name = type_ref
                .type_name
                .get_identifier_reference()
                .map(|r| r.name.as_str());
            match name {
                Some("i32" | "f64" | "bool" | "string" | "int" | "number" | "Array") => None,
                Some(other) => {
                    if let Some(BoundType::Class(class_name)) =
                        bindings.and_then(|b| b.get(other))
                    {
                        return Some(class_name.clone());
                    }
                    if let Some(BoundType::Union { name: union_name, .. }) =
                        bindings.and_then(|b| b.get(other))
                    {
                        return Some(union_name.clone());
                    }
                    // If the reference carries type arguments, return the mangled
                    // monomorphized name so downstream class-registry lookups route
                    // to the concrete instantiation.
                    if let Some(args) = type_ref.type_arguments.as_ref() {
                        let mut tokens = Vec::with_capacity(args.params.len());
                        for param in &args.params {
                            let tok = mangle_ts_type_token(param, bindings)?;
                            tokens.push(tok);
                        }
                        return Some(format!("{other}${}", tokens.join("$")));
                    }
                    // Named union (`type Shape = A | B`): return the union's
                    // registered (canonical) name so downstream coerce / member
                    // access can look it up in `union_registry`. Checked
                    // before the shape fallback because a `type` alias for a
                    // union is never simultaneously a shape.
                    if let Some(layout) = union_registry.and_then(|r| r.get_by_name(other)) {
                        return Some(layout.name.clone());
                    }
                    // Shape-alias canonicalization: when two distinct user
                    // names declare the same shape (e.g. interface + type),
                    // each name lives in `by_name` but the layout sits under
                    // the canonical shape name. Always return the canonical
                    // so downstream `class_registry.get(...)` succeeds.
                    if let Some(shape) = shape_registry.and_then(|r| r.get_by_name(other)) {
                        return Some(shape.name.clone());
                    }
                    Some(other.to_string())
                }
                None => None,
            }
        }
        TSType::TSTypeLiteral(lit) => {
            shape_registry?.get_by_annotation(lit).map(|s| s.name.clone())
        }
        TSType::TSTupleType(tuple) => shape_registry?
            .get_by_tuple_annotation(tuple)
            .map(|s| s.name.clone()),
        TSType::TSUnionType(u) => {
            // Inline union annotation (e.g. `function f(x: A | B)`). Look up
            // the registered layout via the annotation's span and return its
            // canonical name. A miss means `discover_unions` didn't reach
            // this annotation — currently treated as "not a union" so the
            // caller's downstream type-check produces the original error.
            union_registry?
                .get_by_annotation(u)
                .map(|layout| layout.name.clone())
        }
        // `: never` flows through the same `target_class: &str` channel as
        // shapes / unions / classes. `coerce.rs` matches on this constant.
        TSType::TSNeverKeyword(_) => Some(NEVER_CLASS_NAME.to_string()),
        _ => None,
    }
}

/// Internal: render a TSType as a mangle token (e.g. `i32`, `Box$i32`). Returns
/// None for types that cannot participate in a mangled name (e.g. function
/// types). This mirrors `BoundType::mangle_token` for non-bound references.
fn mangle_ts_type_token(ts_type: &TSType, bindings: Option<&TypeBindings>) -> Option<String> {
    match ts_type {
        TSType::TSNumberKeyword(_) => Some("f64".to_string()),
        TSType::TSBooleanKeyword(_) => Some("bool".to_string()),
        TSType::TSStringKeyword(_) => Some("string".to_string()),
        TSType::TSTypeReference(type_ref) => {
            let name = type_ref
                .type_name
                .get_identifier_reference()
                .map(|r| r.name.as_str())?;
            if let Some(bound) = bindings.and_then(|b| b.get(name)) {
                return Some(bound.mangle_token());
            }
            match name {
                "i32" | "int" => Some("i32".to_string()),
                "f64" | "number" => Some("f64".to_string()),
                "bool" => Some("bool".to_string()),
                "string" => Some("string".to_string()),
                other => {
                    if let Some(args) = type_ref.type_arguments.as_ref() {
                        let mut tokens = Vec::with_capacity(args.params.len());
                        for param in &args.params {
                            tokens.push(mangle_ts_type_token(param, bindings)?);
                        }
                        Some(format!("{other}${}", tokens.join("$")))
                    } else {
                        Some(other.to_string())
                    }
                }
            }
        }
        // Inline union — same canonical-name computation as
        // `generics::resolve_bound_type`'s `TSUnionType` arm. Mirrors the
        // `UnionRegistry` fingerprint scheme so `Box<A | B>` at
        // variable-annotation time mangles to the same name the generic-
        // instantiation walker produced.
        TSType::TSUnionType(u) => {
            let mut tokens = Vec::with_capacity(u.types.len());
            for t in &u.types {
                tokens.push(union_member_mangle_token(t, bindings)?);
            }
            tokens.sort();
            tokens.dedup();
            Some(format!("__Union${}", tokens.join("$")))
        }
        TSType::TSParenthesizedType(p) => mangle_ts_type_token(&p.type_annotation, bindings),
        TSType::TSNeverKeyword(_) => Some("never".to_string()),
        _ => None,
    }
}

/// Canonical token for a union member at mangle time. Parallels
/// `generics::union_member_canonical_token` but returns `Option` to fit the
/// existing `mangle_ts_type_token` error-silent convention.
fn union_member_mangle_token(ts_type: &TSType, bindings: Option<&TypeBindings>) -> Option<String> {
    match ts_type {
        TSType::TSTypeReference(r) => {
            let name = r
                .type_name
                .get_identifier_reference()
                .map(|id| id.name.as_str())?;
            if let Some(bound) = bindings.and_then(|b| b.get(name)) {
                return Some(bound.mangle_token());
            }
            Some(name.to_string())
        }
        TSType::TSLiteralType(lit) => {
            let (_, tv) = crate::codegen::shapes::literal_type_to_tag(&lit.literal).ok()?;
            Some(tv.canonical())
        }
        TSType::TSParenthesizedType(p) => union_member_mangle_token(&p.type_annotation, bindings),
        _ => None,
    }
}
