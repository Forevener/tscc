use std::collections::HashSet;

use oxc_ast::ast::{TSType, TSTypeAnnotation};
use wasm_encoder::ValType;

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

pub fn resolve_ts_type(
    ts_type: &TSType,
    class_names: &HashSet<String>,
) -> Result<WasmType, CompileError> {
    match ts_type {
        TSType::TSTypeReference(type_ref) => {
            let name = type_ref
                .type_name
                .get_identifier_reference()
                .map(|r| r.name.as_str());
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
                Some(other) => Err(CompileError::type_err(format!(
                    "unknown type '{other}' — supported types: i32, f64, int, bool, number, string, Array<T>, or a class name"
                ))),
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
pub fn get_array_element_class(annotation: &TSTypeAnnotation) -> Option<String> {
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
    } else {
        None
    }
}

/// Check if a type annotation is the `string` type.
pub fn is_string_type(annotation: &TSTypeAnnotation) -> bool {
    match &annotation.type_annotation {
        TSType::TSStringKeyword(_) => true,
        TSType::TSTypeReference(type_ref) => {
            let name = type_ref
                .type_name
                .get_identifier_reference()
                .map(|r| r.name.as_str());
            name == Some("string")
        }
        _ => false,
    }
}

/// Extract the type name string from a TS type annotation.
/// Returns Some("ClassName") for class references, None for primitives.
pub fn get_class_type_name(annotation: &TSTypeAnnotation) -> Option<String> {
    get_class_type_name_from_ts_type(&annotation.type_annotation)
}

/// Extract class type name from a TSType (used by both annotation and as-expression paths).
pub fn get_class_type_name_from_ts_type(ts_type: &TSType) -> Option<String> {
    match ts_type {
        TSType::TSTypeReference(type_ref) => {
            let name = type_ref
                .type_name
                .get_identifier_reference()
                .map(|r| r.name.as_str());
            match name {
                Some("i32" | "f64" | "bool" | "string" | "int" | "number" | "Array") => None,
                Some(class_name) => Some(class_name.to_string()),
                None => None,
            }
        }
        _ => None,
    }
}
