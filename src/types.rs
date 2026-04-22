use std::collections::{HashMap, HashSet};

use oxc_ast::ast::{TSType, TSTypeAnnotation};
use wasm_encoder::ValType;

use crate::codegen::shapes::ShapeRegistry;
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
}

impl BoundType {
    pub fn wasm_ty(&self) -> WasmType {
        match self {
            BoundType::F64 => WasmType::F64,
            _ => WasmType::I32,
        }
    }

    /// Short token used in mangled names: `i32`, `f64`, `bool`, `string`, or the
    /// class name verbatim (already unique — class names can themselves be
    /// mangled forms like `Box$i32`).
    pub fn mangle_token(&self) -> String {
        match self {
            BoundType::I32 => "i32".to_string(),
            BoundType::F64 => "f64".to_string(),
            BoundType::Bool => "bool".to_string(),
            BoundType::Str => "string".to_string(),
            BoundType::Class(name) => name.clone(),
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

pub fn resolve_type_annotation_with_classes(
    annotation: &TSTypeAnnotation,
    class_names: &HashSet<String>,
) -> Result<WasmType, CompileError> {
    resolve_ts_type(&annotation.type_annotation, class_names)
}

/// Resolve a type annotation under a type-parameter binding scope. When a
/// TSTypeReference's name matches a key in `bindings`, it substitutes the bound
/// type. Falls through to the regular resolver otherwise.
pub fn resolve_type_annotation_with_bindings(
    annotation: &TSTypeAnnotation,
    class_names: &HashSet<String>,
    bindings: Option<&TypeBindings>,
) -> Result<WasmType, CompileError> {
    resolve_ts_type_with_bindings(&annotation.type_annotation, class_names, bindings)
}

pub fn resolve_ts_type(
    ts_type: &TSType,
    class_names: &HashSet<String>,
) -> Result<WasmType, CompileError> {
    resolve_ts_type_with_bindings(ts_type, class_names, None)
}

pub fn resolve_ts_type_with_bindings(
    ts_type: &TSType,
    class_names: &HashSet<String>,
    bindings: Option<&TypeBindings>,
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
        _ => Err(CompileError::type_err(
            "unsupported type annotation".to_string(),
        )),
    }
}

/// Parse a TSFunctionType annotation into a ClosureSig.
/// E.g. `(x: i32, y: f64) => i32` → ClosureSig { param_types: [I32, F64], return_type: I32 }
pub fn get_closure_sig(
    annotation: &TSTypeAnnotation,
    class_names: &HashSet<String>,
) -> Option<ClosureSig> {
    match &annotation.type_annotation {
        TSType::TSFunctionType(func_type) => {
            let mut param_types = Vec::new();
            for param in &func_type.params.items {
                let ty = if let Some(ann) = &param.type_annotation {
                    resolve_ts_type(&ann.type_annotation, class_names).ok()?
                } else {
                    return None;
                };
                param_types.push(ty);
            }
            let return_type =
                resolve_ts_type(&func_type.return_type.type_annotation, class_names).ok()?;
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
            resolve_ts_type(first, class_names).ok()
        }
        TSType::TSArrayType(arr) => {
            // T[] — extract element type from the shorthand form
            resolve_ts_type(&arr.element_type, class_names).ok()
        }
        _ => None,
    }
}

/// Extract the class name from an Array<ClassName> type annotation.
/// Returns Some("ClassName") if element type is a class, None otherwise.
pub fn get_array_element_class(
    annotation: &TSTypeAnnotation,
    shape_registry: Option<&ShapeRegistry>,
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
    // Check if the element type is a class reference
    if let TSType::TSTypeReference(elem_ref) = first {
        let elem_name = elem_ref
            .type_name
            .get_identifier_reference()
            .map(|r| r.name.as_str())?;
        match elem_name {
            "i32" | "f64" | "bool" | "Array" | "string" | "int" | "number" => None,
            class_name => Some(class_name.to_string()),
        }
    } else if let TSType::TSTypeLiteral(lit) = first {
        shape_registry
            .and_then(|r| r.get_by_annotation(lit))
            .map(|s| s.name.clone())
    } else if let TSType::TSTupleType(tuple) = first {
        shape_registry
            .and_then(|r| r.get_by_tuple_annotation(tuple))
            .map(|s| s.name.clone())
    } else {
        None
    }
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
) -> Option<String> {
    get_class_type_name_from_ts_type_with_bindings(ts_type, None, shape_registry)
}

/// Bindings-aware variant. A type parameter bound to `BoundType::Class(name)`
/// resolves to `Some(name)`, enabling methods/fields on generic-param-typed
/// values to route through the concrete class's layout.
pub fn get_class_type_name_with_bindings(
    annotation: &TSTypeAnnotation,
    bindings: Option<&TypeBindings>,
    shape_registry: Option<&ShapeRegistry>,
) -> Option<String> {
    get_class_type_name_from_ts_type_with_bindings(
        &annotation.type_annotation,
        bindings,
        shape_registry,
    )
}

pub fn get_class_type_name_from_ts_type_with_bindings(
    ts_type: &TSType,
    bindings: Option<&TypeBindings>,
    shape_registry: Option<&ShapeRegistry>,
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
        _ => None,
    }
}
