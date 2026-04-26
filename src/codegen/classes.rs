use std::collections::HashMap;

use oxc_ast::ast::*;

use std::collections::HashSet;

use super::shapes::ShapeRegistry;
use super::unions::UnionRegistry;
use crate::error::CompileError;
use crate::types::{self, TypeBindings, WasmType};

/// Layout of a single class in linear memory.
#[derive(Debug, Clone)]
pub struct ClassLayout {
    pub name: String,
    /// Total size in bytes (aligned to 8)
    pub size: u32,
    /// Field name -> (byte_offset, type)
    pub fields: Vec<(String, u32, WasmType)>,
    /// Field name -> (byte_offset, type) for fast lookup
    pub field_map: HashMap<String, (u32, WasmType)>,
    /// Field name -> class name (for fields that are class instance pointers)
    pub field_class_types: HashMap<String, String>,
    /// Fields that are string-typed
    pub field_string_types: HashSet<String>,
    /// Method name -> (param_types including this, return_type)
    pub methods: HashMap<String, MethodSig>,
    /// Parent class name (for single inheritance)
    pub parent: Option<String>,
    /// Whether this class is part of an inheritance hierarchy (has extends or is extended)
    pub is_polymorphic: bool,
    /// Ordered method names in vtable slot order (parent slots first, then child-new)
    pub vtable_methods: Vec<String>,
    /// Method name -> vtable slot index (for fast lookup)
    pub vtable_method_map: HashMap<String, usize>,
    /// Byte offset of this class's vtable in static data (set after vtable construction)
    pub vtable_offset: u32,
    /// Fields declared directly in this class (not inherited from parent)
    pub own_field_names: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct MethodSig {
    pub params: Vec<(String, WasmType)>,
    pub return_type: WasmType,
    /// If the method returns a class instance, the class name
    pub return_class: Option<String>,
    /// For each parameter, the class name if the param is class-typed. Lets
    /// method-call sites thread an expected-type hint into `{...}` arguments.
    pub param_classes: Vec<Option<String>>,
}

/// Resolved field info ready for synthetic-layout assembly. Used by Phase A.2
/// shape registration; could later be reused if class registration is
/// refactored to a similar shape.
#[derive(Debug)]
pub struct LayoutField {
    pub name: String,
    pub wasm_ty: WasmType,
    /// Set when the field's type is a class or shape reference.
    /// Recorded into `ClassLayout::field_class_types`.
    pub class_ref: Option<String>,
    /// Set when the field's type is `string`.
    /// Recorded into `ClassLayout::field_string_types`.
    pub is_string: bool,
}

/// Registry of all class layouts.
#[derive(Debug, Default)]
pub struct ClassRegistry {
    pub classes: HashMap<String, ClassLayout>,
    /// Classes that participate in an inheritance hierarchy (have extends or are extended)
    pub polymorphic_classes: HashSet<String>,
}

impl ClassRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, name: &str) -> Option<&ClassLayout> {
        self.classes.get(name)
    }

    /// Check if `child` is a subclass of `parent` (walks the parent chain).
    pub fn is_subclass_of(&self, child: &str, parent: &str) -> bool {
        let mut current = child;
        while let Some(layout) = self.classes.get(current) {
            if let Some(ref p) = layout.parent {
                if p == parent {
                    return true;
                }
                current = p;
            } else {
                return false;
            }
        }
        false
    }

    /// Find which class in the hierarchy owns a method (walks up from `class_name`).
    pub fn resolve_method_owner(&self, class_name: &str, method_name: &str) -> Option<String> {
        let mut current = class_name;
        loop {
            if let Some(layout) = self.classes.get(current) {
                if layout.methods.contains_key(method_name) {
                    return Some(current.to_string());
                }
                if let Some(ref parent) = layout.parent {
                    current = parent;
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
    }

    /// Mark a set of classes as polymorphic (participates in inheritance).
    pub fn mark_polymorphic(&mut self, names: &HashSet<String>) {
        self.polymorphic_classes = names.clone();
        for name in names {
            if let Some(layout) = self.classes.get_mut(name) {
                layout.is_polymorphic = true;
            }
        }
    }

    #[allow(
        clippy::map_entry,
        clippy::too_many_arguments,
        reason = "override and new-slot branches are not symmetric; args are positional by design"
    )]
    pub fn register_class(
        &mut self,
        class: &Class,
        class_names: &HashSet<String>,
        parent_name: Option<String>,
        is_polymorphic: bool,
        shape_registry: Option<&ShapeRegistry>,
        union_registry: Option<&UnionRegistry>,
        union_overrides: &HashMap<String, crate::types::WasmType>,
    ) -> Result<(), CompileError> {
        self.register_class_with_bindings(
            class,
            class_names,
            parent_name,
            is_polymorphic,
            None,
            None,
            shape_registry,
            union_registry,
            union_overrides,
        )
    }

    /// Bindings-aware class registration: when `override_name` is supplied, the
    /// synthesized layout is stored under that mangled name instead of the raw
    /// AST identifier (used for monomorphized generic classes). `bindings`
    /// substitute type parameters during field/method type resolution.
    #[allow(
        clippy::map_entry,
        clippy::too_many_arguments,
        reason = "override and new-slot branches are not symmetric; args are positional by design"
    )]
    pub fn register_class_with_bindings(
        &mut self,
        class: &Class,
        class_names: &HashSet<String>,
        parent_name: Option<String>,
        is_polymorphic: bool,
        override_name: Option<&str>,
        bindings: Option<&TypeBindings>,
        shape_registry: Option<&ShapeRegistry>,
        union_registry: Option<&UnionRegistry>,
        union_overrides: &HashMap<String, crate::types::WasmType>,
    ) -> Result<(), CompileError> {
        let name = if let Some(n) = override_name {
            n.to_string()
        } else {
            class
                .id
                .as_ref()
                .ok_or_else(|| CompileError::parse("class without name"))?
                .name
                .as_str()
                .to_string()
        };

        // Start with inherited fields from parent (if any)
        let mut fields = Vec::new();
        let mut field_map = HashMap::new();
        let mut field_class_types = HashMap::new();
        let mut field_string_types = HashSet::new();
        let mut methods = HashMap::new();
        let mut own_field_names = HashSet::new();
        let mut vtable_methods: Vec<String> = Vec::new();
        let mut vtable_method_map: HashMap<String, usize> = HashMap::new();

        // If polymorphic, reserve offset 0..3 for vtable pointer
        let mut offset: u32 = if is_polymorphic { 4 } else { 0 };

        // Inherit from parent
        if let Some(ref parent) = parent_name {
            let parent_layout = self.classes.get(parent)
                .ok_or_else(|| CompileError::codegen(format!(
                    "parent class '{}' not registered (classes must be declared parent-before-child)", parent
                )))?
                .clone();

            // Inherit fields at the same offsets
            fields = parent_layout.fields.clone();
            field_map = parent_layout.field_map.clone();
            field_class_types = parent_layout.field_class_types.clone();
            field_string_types = parent_layout.field_string_types.clone();

            // Inherit methods (child overrides will replace these)
            methods = parent_layout.methods.clone();

            // Inherit vtable layout
            vtable_methods = parent_layout.vtable_methods.clone();
            vtable_method_map = parent_layout.vtable_method_map.clone();

            // Continue field offsets after parent's fields
            // Parent's size includes alignment padding; use the raw end of last field instead
            if let Some((_name, last_offset, last_ty)) = parent_layout.fields.last() {
                offset = last_offset
                    + match last_ty {
                        WasmType::F64 => 8,
                        WasmType::I32 => 4,
                        _ => 4,
                    };
            }
            // If parent had no own fields but is polymorphic, offset stays at 4
        }

        for element in &class.body.body {
            match element {
                ClassElement::PropertyDefinition(prop) => {
                    let field_name = property_key_name(&prop.key)?;
                    let ty = if let Some(ann) = &prop.type_annotation {
                        types::resolve_type_annotation_with_unions(
                            ann,
                            class_names,
                            bindings,
                            union_overrides,
                        )?
                    } else {
                        return Err(CompileError::type_err(format!(
                            "class field '{field_name}' requires a type annotation"
                        )));
                    };

                    // Track field class type if it's a class reference
                    if let Some(ann) = &prop.type_annotation {
                        if let Some(class_type) = types::get_class_type_name_with_bindings(
                            ann,
                            bindings,
                            shape_registry,
                            union_registry,
                        ) && class_names.contains(&class_type)
                        {
                            field_class_types.insert(field_name.clone(), class_type);
                        }
                        // Track string fields
                        if types::is_string_type_with_bindings(ann, bindings) {
                            field_string_types.insert(field_name.clone());
                        }
                    }

                    // Align offset based on type
                    let align = match ty {
                        WasmType::F64 => 8,
                        WasmType::I32 => 4,
                        _ => 4,
                    };
                    offset = (offset + align - 1) & !(align - 1);

                    fields.push((field_name.clone(), offset, ty));
                    field_map.insert(field_name.clone(), (offset, ty));
                    own_field_names.insert(field_name);

                    offset += match ty {
                        WasmType::F64 => 8,
                        WasmType::I32 => 4,
                        _ => 4,
                    };
                }
                ClassElement::MethodDefinition(method) => {
                    let method_name = property_key_name(&method.key)?;
                    let func = &method.value;

                    if method.kind == MethodDefinitionKind::Constructor {
                        // Validate constructor params have type annotations
                        for param in &func.params.items {
                            let pname = match &param.pattern {
                                BindingPattern::BindingIdentifier(ident) => {
                                    ident.name.as_str().to_string()
                                }
                                _ => {
                                    return Err(CompileError::unsupported(
                                        "destructured constructor param",
                                    ));
                                }
                            };
                            if let Some(ann) = &param.type_annotation {
                                types::resolve_type_annotation_with_unions(
                                    ann,
                                    class_names,
                                    bindings,
                                    union_overrides,
                                )?;
                            } else {
                                return Err(CompileError::type_err(format!(
                                    "constructor parameter '{pname}' requires type annotation"
                                )));
                            }
                        }
                    } else {
                        // Regular method
                        let mut params = Vec::new();
                        let mut param_classes: Vec<Option<String>> = Vec::new();
                        for param in &func.params.items {
                            let pname = match &param.pattern {
                                BindingPattern::BindingIdentifier(ident) => {
                                    ident.name.as_str().to_string()
                                }
                                _ => {
                                    return Err(CompileError::unsupported(
                                        "destructured method param",
                                    ));
                                }
                            };
                            let pty = if let Some(ann) = &param.type_annotation {
                                types::resolve_type_annotation_with_unions(
                                    ann,
                                    class_names,
                                    bindings,
                                    union_overrides,
                                )?
                            } else {
                                return Err(CompileError::type_err(format!(
                                    "method parameter '{pname}' requires type annotation"
                                )));
                            };
                            let pclass = param.type_annotation.as_ref().and_then(|ann| {
                                types::get_class_type_name_with_bindings(
                                    ann,
                                    bindings,
                                    shape_registry,
                                    union_registry,
                                )
                                .filter(|cn| class_names.contains(cn))
                            });
                            params.push((pname, pty));
                            param_classes.push(pclass);
                        }
                        let ret = if let Some(ann) = &func.return_type {
                            types::resolve_type_annotation_with_unions(
                                ann,
                                class_names,
                                bindings,
                                union_overrides,
                            )?
                        } else {
                            WasmType::Void
                        };
                        // Track return class type
                        let return_class = func
                            .return_type
                            .as_ref()
                            .and_then(|ann| {
                                types::get_class_type_name_with_bindings(
                                    ann,
                                    bindings,
                                    shape_registry,
                                    union_registry,
                                )
                            })
                            .filter(|cn| class_names.contains(cn));
                        methods.insert(
                            method_name.clone(),
                            MethodSig {
                                params,
                                return_type: ret,
                                return_class,
                                param_classes,
                            },
                        );

                        // Update vtable: override existing slot or append new
                        if is_polymorphic {
                            if vtable_method_map.contains_key(&method_name) {
                                // Override: validate signature matches parent's
                                if let Some(ref parent) = parent_name
                                    && let Some(parent_layout) = self.classes.get(parent)
                                    && let Some(parent_sig) =
                                        parent_layout.methods.get(&method_name)
                                {
                                    let child_sig = methods.get(&method_name).unwrap();
                                    let parent_types: Vec<WasmType> =
                                        parent_sig.params.iter().map(|(_, t)| *t).collect();
                                    let child_types: Vec<WasmType> =
                                        child_sig.params.iter().map(|(_, t)| *t).collect();
                                    if parent_types != child_types {
                                        return Err(CompileError::type_err(format!(
                                            "override of method '{}' in class '{}' has different parameter types than parent '{}'",
                                            method_name, name, parent
                                        )));
                                    }
                                    if parent_sig.return_type != child_sig.return_type {
                                        return Err(CompileError::type_err(format!(
                                            "override of method '{}' in class '{}' has different return type than parent '{}'",
                                            method_name, name, parent
                                        )));
                                    }
                                }
                            } else {
                                // New method: append to vtable
                                let slot = vtable_methods.len();
                                vtable_methods.push(method_name.clone());
                                vtable_method_map.insert(method_name, slot);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Align total size to 8 bytes
        let size = if offset == 0 { 0 } else { (offset + 7) & !7 };

        self.classes.insert(
            name.clone(),
            ClassLayout {
                name,
                size,
                fields,
                field_map,
                field_class_types,
                field_string_types,
                methods,
                parent: parent_name,
                is_polymorphic,
                vtable_methods,
                vtable_method_map,
                vtable_offset: 0, // set later during vtable construction
                own_field_names,
            },
        );

        Ok(())
    }

    /// Build a methodless, vtable-less, parent-less `ClassLayout` from a
    /// pre-resolved field list and insert it under `name`. Field offsets
    /// follow the slice order; alignment + total-size formulas mirror
    /// `register_class_with_bindings`.
    pub fn register_synthetic_layout(
        &mut self,
        name: &str,
        fields: &[LayoutField],
    ) -> Result<(), CompileError> {
        let mut field_vec: Vec<(String, u32, WasmType)> = Vec::with_capacity(fields.len());
        let mut field_map: HashMap<String, (u32, WasmType)> = HashMap::with_capacity(fields.len());
        let mut field_class_types: HashMap<String, String> = HashMap::new();
        let mut field_string_types: HashSet<String> = HashSet::new();
        let mut own_field_names: HashSet<String> = HashSet::with_capacity(fields.len());
        let mut offset: u32 = 0;

        for f in fields {
            let align = match f.wasm_ty {
                WasmType::F64 => 8,
                WasmType::I32 => 4,
                _ => 4,
            };
            offset = (offset + align - 1) & !(align - 1);

            field_vec.push((f.name.clone(), offset, f.wasm_ty));
            field_map.insert(f.name.clone(), (offset, f.wasm_ty));
            own_field_names.insert(f.name.clone());

            if let Some(cn) = &f.class_ref {
                field_class_types.insert(f.name.clone(), cn.clone());
            }
            if f.is_string {
                field_string_types.insert(f.name.clone());
            }

            offset += match f.wasm_ty {
                WasmType::F64 => 8,
                WasmType::I32 => 4,
                _ => 4,
            };
        }

        let size = if offset == 0 { 0 } else { (offset + 7) & !7 };

        self.classes.insert(
            name.to_string(),
            ClassLayout {
                name: name.to_string(),
                size,
                fields: field_vec,
                field_map,
                field_class_types,
                field_string_types,
                methods: HashMap::new(),
                parent: None,
                is_polymorphic: false,
                vtable_methods: Vec::new(),
                vtable_method_map: HashMap::new(),
                vtable_offset: 0,
                own_field_names,
            },
        );

        Ok(())
    }
}

/// Topological sort of class declarations: parent classes come before children.
/// Returns (class_name, parent_name_option) pairs in dependency order.
pub fn topo_sort_classes(
    class_info: &[(String, Option<String>)],
) -> Result<Vec<(String, Option<String>)>, CompileError> {
    let name_set: HashSet<&str> = class_info.iter().map(|(n, _)| n.as_str()).collect();
    let mut result = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut visiting: HashSet<String> = HashSet::new();

    let info_map: HashMap<&str, Option<&str>> = class_info
        .iter()
        .map(|(n, p)| (n.as_str(), p.as_deref()))
        .collect();

    fn visit(
        name: &str,
        info_map: &HashMap<&str, Option<&str>>,
        name_set: &HashSet<&str>,
        visited: &mut HashSet<String>,
        visiting: &mut HashSet<String>,
        result: &mut Vec<(String, Option<String>)>,
    ) -> Result<(), CompileError> {
        if visited.contains(name) {
            return Ok(());
        }
        if visiting.contains(name) {
            return Err(CompileError::codegen(format!(
                "circular inheritance involving '{name}'"
            )));
        }
        visiting.insert(name.to_string());

        if let Some(Some(parent)) = info_map.get(name) {
            if name_set.contains(parent) {
                visit(parent, info_map, name_set, visited, visiting, result)?;
            } else {
                return Err(CompileError::codegen(format!(
                    "class '{name}' extends unknown class '{parent}'"
                )));
            }
        }

        visiting.remove(name);
        visited.insert(name.to_string());
        let parent = info_map.get(name).and_then(|p| p.map(|s| s.to_string()));
        result.push((name.to_string(), parent));
        Ok(())
    }

    for (name, _) in class_info {
        visit(
            name,
            &info_map,
            &name_set,
            &mut visited,
            &mut visiting,
            &mut result,
        )?;
    }

    Ok(result)
}

/// Determine which classes are polymorphic (participate in inheritance).
pub fn find_polymorphic_classes(class_info: &[(String, Option<String>)]) -> HashSet<String> {
    let mut result = HashSet::new();
    for (name, parent) in class_info {
        if let Some(parent) = parent {
            result.insert(name.clone());
            result.insert(parent.clone());
        }
    }
    result
}

fn property_key_name(key: &PropertyKey) -> Result<String, CompileError> {
    match key {
        PropertyKey::StaticIdentifier(ident) => Ok(ident.name.as_str().to_string()),
        _ => Err(CompileError::unsupported("computed property key")),
    }
}
