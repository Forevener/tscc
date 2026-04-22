//! `ShapeWalker` — the two-pass walk that populates `ShapeRegistry`.
//!
//! Pass 1 collects named shapes (`type` / `interface`) and topo-sorts
//! interface inheritance. Pass 2 walks every AST annotation and every
//! `ObjectExpression` for anonymous shapes and generic instantiations.
//! See `shapes::discover_shapes` in the parent module for the driver.

use std::collections::{HashMap, HashSet};

use oxc_ast::ast::*;

use super::super::generics::{GenericClassTemplate, GenericFnTemplate, resolve_bound_type};
use super::fingerprint::{fingerprint_of, property_signature_key, tuple_fingerprint_of};
use super::{ANON_SHAPE_PREFIX, Shape, ShapeField, ShapeKind, ShapeRegistry, TUPLE_SHAPE_PREFIX};
use crate::error::CompileError;
use crate::types::{BoundType, TypeBindings};

/// Pre-scan the program for generic shape templates (`type Pair<T, U> = {...}`,
/// `interface Box<T> {...}`). Templates are kept in a separate map from
/// non-generic named shapes so Pass 1 can ignore them while Pass 2 instantiates
/// on demand when it sees `Pair<i32, f64>`-style references.
pub(super) fn collect_generic_shape_templates<'a>(
    program: &'a Program<'a>,
) -> Result<HashMap<String, GenericShapeTemplate<'a>>, CompileError> {
    let mut out: HashMap<String, GenericShapeTemplate<'a>> = HashMap::new();
    fn visit_decl<'a>(
        decl: &'a Declaration<'a>,
        out: &mut HashMap<String, GenericShapeTemplate<'a>>,
    ) -> Result<(), CompileError> {
        match decl {
            Declaration::TSTypeAliasDeclaration(alias) => {
                push_alias(alias, out)?;
            }
            Declaration::TSInterfaceDeclaration(iface) => {
                push_iface(iface, out)?;
            }
            _ => {}
        }
        Ok(())
    }
    fn push_alias<'a>(
        alias: &'a TSTypeAliasDeclaration<'a>,
        out: &mut HashMap<String, GenericShapeTemplate<'a>>,
    ) -> Result<(), CompileError> {
        if alias.declare || alias.type_parameters.is_none() {
            return Ok(());
        }
        if !matches!(&alias.type_annotation, TSType::TSTypeLiteral(_)) {
            // `type Id<T> = T` — pure type aliasing, no shape to instantiate.
            return Ok(());
        }
        let name = alias.id.name.as_str().to_string();
        if out.contains_key(&name) {
            return Err(CompileError::type_err(format!(
                "duplicate generic shape template '{name}'"
            )));
        }
        let type_params = alias
            .type_parameters
            .as_ref()
            .unwrap()
            .params
            .iter()
            .map(|p| p.name.name.as_str().to_string())
            .collect();
        out.insert(
            name,
            GenericShapeTemplate {
                type_params,
                ast: GenericShapeAst::Alias(alias),
            },
        );
        Ok(())
    }
    fn push_iface<'a>(
        iface: &'a TSInterfaceDeclaration<'a>,
        out: &mut HashMap<String, GenericShapeTemplate<'a>>,
    ) -> Result<(), CompileError> {
        if iface.declare || iface.type_parameters.is_none() {
            return Ok(());
        }
        if !iface.extends.is_empty() {
            return Err(CompileError::unsupported(format!(
                "generic interface '{}' with `extends` — not yet supported (Phase E.4 \
                 defers heritage for generic interfaces)",
                iface.id.name.as_str()
            )));
        }
        let name = iface.id.name.as_str().to_string();
        if out.contains_key(&name) {
            return Err(CompileError::type_err(format!(
                "duplicate generic shape template '{name}'"
            )));
        }
        let type_params = iface
            .type_parameters
            .as_ref()
            .unwrap()
            .params
            .iter()
            .map(|p| p.name.name.as_str().to_string())
            .collect();
        out.insert(
            name,
            GenericShapeTemplate {
                type_params,
                ast: GenericShapeAst::Iface(iface),
            },
        );
        Ok(())
    }

    for stmt in &program.body {
        match stmt {
            Statement::TSTypeAliasDeclaration(alias) => push_alias(alias, &mut out)?,
            Statement::TSInterfaceDeclaration(iface) => push_iface(iface, &mut out)?,
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    visit_decl(decl, &mut out)?;
                }
            }
            Statement::ExportDefaultDeclaration(export) => {
                if let ExportDefaultDeclarationKind::TSInterfaceDeclaration(iface) =
                    &export.declaration
                {
                    push_iface(iface, &mut out)?;
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Walker
// ---------------------------------------------------------------------------

pub(super) struct ShapeWalker<'a, 'ctx> {
    /// Real class names (caller-provided). Used for collision detection so
    /// `interface Foo` clashes only when `class Foo` exists, not when
    /// `interface Foo` is being processed in the same wave.
    pub(super) real_class_names: &'ctx HashSet<String>,
    /// Real classes ∪ named-shape names (pre-scanned). Used as the lookup
    /// set for `resolve_bound_type` so a shape body can reference another
    /// shape (e.g. `type Outer = { inner: Inner }`) before Pass 0a-v
    /// merges shape names into the module-level `class_names`.
    pub(super) class_names: HashSet<String>,
    pub(super) class_templates: &'ctx HashMap<String, GenericClassTemplate<'a>>,
    pub(super) fn_templates: &'ctx HashMap<String, GenericFnTemplate<'a>>,
    /// Generic shape templates discovered in Pass 1b. Consulted during
    /// field resolution (`resolve_field_type`) and annotation walks
    /// (`walk_ts_type`) so that any `Pair<i32, f64>`-like reference triggers
    /// a concrete instantiation under the mangled name `Pair$i32$f64`.
    pub(super) generic_shape_templates: HashMap<String, GenericShapeTemplate<'a>>,
    pub(super) registry: ShapeRegistry,
}

/// Phase E.4: generic object-type template. Captures the AST body so each
/// concrete instantiation can re-resolve the field types with a fresh
/// `TypeBindings` scope. `ast` is the original declaration, not a cloned
/// body, so every lookup traverses the same allocator-owned nodes.
#[derive(Clone)]
pub(super) struct GenericShapeTemplate<'a> {
    type_params: Vec<String>,
    ast: GenericShapeAst<'a>,
}

#[derive(Clone, Copy)]
enum GenericShapeAst<'a> {
    Alias(&'a TSTypeAliasDeclaration<'a>),
    Iface(&'a TSInterfaceDeclaration<'a>),
}

/// Collect top-level `TSTypeAliasDeclaration` / `TSInterfaceDeclaration`
/// names (and their `export`-wrapped forms) into `out`. Used to seed the
/// walker's `class_names` so inter-shape references resolve.
pub(super) fn collect_named_shape_names(stmt: &Statement<'_>, out: &mut HashSet<String>) {
    match stmt {
        Statement::TSTypeAliasDeclaration(alias) => {
            out.insert(alias.id.name.as_str().to_string());
        }
        Statement::TSInterfaceDeclaration(iface) => {
            out.insert(iface.id.name.as_str().to_string());
        }
        Statement::ExportNamedDeclaration(export) => {
            if let Some(decl) = &export.declaration {
                match decl {
                    Declaration::TSTypeAliasDeclaration(alias) => {
                        out.insert(alias.id.name.as_str().to_string());
                    }
                    Declaration::TSInterfaceDeclaration(iface) => {
                        out.insert(iface.id.name.as_str().to_string());
                    }
                    _ => {}
                }
            }
        }
        Statement::ExportDefaultDeclaration(export) => {
            if let ExportDefaultDeclarationKind::TSInterfaceDeclaration(iface) =
                &export.declaration
            {
                out.insert(iface.id.name.as_str().to_string());
            }
        }
        _ => {}
    }
}

/// Named-shape AST node — either a `type` alias or an `interface`. Collected
/// up-front so Pass 1 can drive registration in topo-sorted order.
pub(super) enum NamedShapeAst<'a> {
    Alias(&'a TSTypeAliasDeclaration<'a>),
    Iface(&'a TSInterfaceDeclaration<'a>),
}

/// Parse a `TSInterfaceDeclaration`'s `extends` list into a single parent name.
/// Phase C supports exactly zero or one parent; multiple-extends interfaces
/// are a plan follow-up. Also rejects class-as-parent and generic-argument
/// heritage (`extends Foo<T>`).
fn parse_interface_parent<'a>(
    iface: &'a TSInterfaceDeclaration<'a>,
    real_class_names: &HashSet<String>,
) -> Result<Option<String>, CompileError> {
    if iface.extends.is_empty() {
        return Ok(None);
    }
    if iface.extends.len() > 1 {
        return Err(CompileError::unsupported(format!(
            "interface '{}' extends multiple parents — not yet supported (Phase C follow-up)",
            iface.id.name.as_str()
        )));
    }
    let h = &iface.extends[0];
    if h.type_arguments.is_some() {
        return Err(CompileError::unsupported(format!(
            "interface '{}' extends with type arguments — not yet supported",
            iface.id.name.as_str()
        )));
    }
    let parent = match &h.expression {
        Expression::Identifier(id) => id.name.as_str().to_string(),
        _ => {
            return Err(CompileError::unsupported(format!(
                "interface '{}' extends a non-identifier expression",
                iface.id.name.as_str()
            )));
        }
    };
    if real_class_names.contains(&parent) {
        return Err(CompileError::type_err(format!(
            "interface '{}' extends class '{parent}' — interfaces can only extend other shape types",
            iface.id.name.as_str()
        )));
    }
    Ok(Some(parent))
}

/// Gather every top-level named-shape declaration (plus re-export wrappers)
/// with its (at most one) parent name. Result order matches program order.
pub(super) fn collect_named_shapes<'a>(
    program: &'a Program<'a>,
    real_class_names: &HashSet<String>,
) -> Result<Vec<(String, Option<String>, NamedShapeAst<'a>)>, CompileError> {
    fn push_alias<'a>(
        alias: &'a TSTypeAliasDeclaration<'a>,
        out: &mut Vec<(String, Option<String>, NamedShapeAst<'a>)>,
    ) {
        out.push((
            alias.id.name.as_str().to_string(),
            None,
            NamedShapeAst::Alias(alias),
        ));
    }
    fn push_iface<'a>(
        iface: &'a TSInterfaceDeclaration<'a>,
        parent: Option<String>,
        out: &mut Vec<(String, Option<String>, NamedShapeAst<'a>)>,
    ) {
        out.push((
            iface.id.name.as_str().to_string(),
            parent,
            NamedShapeAst::Iface(iface),
        ));
    }

    let mut out = Vec::new();
    for stmt in &program.body {
        match stmt {
            Statement::TSTypeAliasDeclaration(alias) => push_alias(alias, &mut out),
            Statement::TSInterfaceDeclaration(iface) => {
                let parent = parse_interface_parent(iface, real_class_names)?;
                push_iface(iface, parent, &mut out);
            }
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    match decl {
                        Declaration::TSTypeAliasDeclaration(alias) => push_alias(alias, &mut out),
                        Declaration::TSInterfaceDeclaration(iface) => {
                            let parent = parse_interface_parent(iface, real_class_names)?;
                            push_iface(iface, parent, &mut out);
                        }
                        _ => {}
                    }
                }
            }
            Statement::ExportDefaultDeclaration(export) => {
                if let ExportDefaultDeclarationKind::TSInterfaceDeclaration(iface) =
                    &export.declaration
                {
                    let parent = parse_interface_parent(iface, real_class_names)?;
                    push_iface(iface, parent, &mut out);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Topological sort of named shapes by `extends` edges: parent before child.
/// Returns indices into the input slice. A cycle (`A extends B extends A` or
/// `A extends A`) is an error; an unknown parent is silently tolerated (the
/// downstream `merge_parent_fields` emits the user-visible error when it fails
/// to look the parent up).
pub(super) fn topo_sort_named_shapes(
    shapes: &[(String, Option<String>, NamedShapeAst<'_>)],
) -> Result<Vec<usize>, CompileError> {
    let name_to_idx: HashMap<&str, usize> = shapes
        .iter()
        .enumerate()
        .map(|(i, (n, _, _))| (n.as_str(), i))
        .collect();

    let mut order = Vec::with_capacity(shapes.len());
    let mut visited = vec![false; shapes.len()];
    let mut visiting = vec![false; shapes.len()];

    fn visit(
        i: usize,
        shapes: &[(String, Option<String>, NamedShapeAst<'_>)],
        name_to_idx: &HashMap<&str, usize>,
        visited: &mut [bool],
        visiting: &mut [bool],
        order: &mut Vec<usize>,
    ) -> Result<(), CompileError> {
        if visited[i] {
            return Ok(());
        }
        if visiting[i] {
            return Err(CompileError::type_err(format!(
                "circular interface inheritance involving '{}'",
                shapes[i].0
            )));
        }
        visiting[i] = true;
        if let Some(parent_name) = &shapes[i].1
            && let Some(&parent_idx) = name_to_idx.get(parent_name.as_str())
        {
            visit(parent_idx, shapes, name_to_idx, visited, visiting, order)?;
        }
        visiting[i] = false;
        visited[i] = true;
        order.push(i);
        Ok(())
    }

    for i in 0..shapes.len() {
        visit(i, shapes, &name_to_idx, &mut visited, &mut visiting, &mut order)?;
    }
    Ok(order)
}

impl<'a> ShapeWalker<'a, '_> {
    // -- named-shape registration -------------------------------------------

    pub(super) fn register_named_from_alias(
        &mut self,
        alias: &'a TSTypeAliasDeclaration<'a>,
    ) -> Result<(), CompileError> {
        if alias.type_parameters.is_some() {
            // Generic shape aliases are captured in `generic_shape_templates`
            // during Pass 1b and instantiated on demand in Pass 2.
            return Ok(());
        }
        if alias.declare {
            // `declare type Foo = {...}` — an ambient declaration, host-side
            // contract. Not compiled into a synthetic class.
            return Ok(());
        }
        let TSType::TSTypeLiteral(lit) = &alias.type_annotation else {
            // `type Id = i32` and similar primitive aliases — pure type-system
            // convenience, no shape to discover. tscc doesn't track these
            // today; leave that orthogonal to object-literal work.
            return Ok(());
        };
        let name = alias.id.name.as_str().to_string();
        self.reject_top_level_collision(&name, alias.span.start)?;
        let fields = self.resolve_type_literal_fields(lit, None)?;
        let _ = self.insert_shape(ShapeKind::Named, Some(name), fields)?;
        Ok(())
    }

    pub(super) fn register_named_from_interface(
        &mut self,
        iface: &'a TSInterfaceDeclaration<'a>,
        parent: Option<&str>,
    ) -> Result<(), CompileError> {
        if iface.type_parameters.is_some() {
            // Generic interfaces are captured in `generic_shape_templates` and
            // instantiated on demand.
            return Ok(());
        }
        if iface.declare {
            return Ok(()); // ambient
        }
        let name = iface.id.name.as_str().to_string();
        self.reject_top_level_collision(&name, iface.span.start)?;
        let own_fields = self.resolve_interface_body_fields(&iface.body, None)?;
        let fields = match parent {
            None => own_fields,
            Some(parent_name) => self.merge_parent_fields(&name, parent_name, own_fields)?,
        };
        let _ = self.insert_shape(ShapeKind::Named, Some(name), fields)?;
        Ok(())
    }

    /// Instantiate a generic shape template with concrete type arguments,
    /// registering the resulting shape under a mangled name `Name$arg1$arg2`.
    /// Idempotent: a second call with the same args is a no-op and returns
    /// the existing mangled name.
    ///
    /// Field resolution runs with a fresh `TypeBindings` scope, so template
    /// bodies that reference `T` substitute correctly. Nested generic-shape
    /// instantiations recurse through this same path.
    fn instantiate_generic_shape(
        &mut self,
        name: &str,
        args: &'a oxc_ast::ast::TSTypeParameterInstantiation<'a>,
        outer_bindings: Option<&TypeBindings>,
    ) -> Result<String, CompileError> {
        // Resolve each type argument to a BoundType, honoring outer bindings
        // (so `Pair<T>` inside `Box<T>`'s body resolves T against the outer
        // monomorphization scope).
        let mut arg_types: Vec<BoundType> = Vec::with_capacity(args.params.len());
        for arg in &args.params {
            arg_types.push(self.resolve_type_arg(arg, outer_bindings)?);
        }

        let template = self.generic_shape_templates.get(name).cloned().ok_or_else(|| {
            CompileError::type_err(format!("'{name}' is not a generic shape template"))
        })?;

        if arg_types.len() != template.type_params.len() {
            return Err(CompileError::type_err(format!(
                "generic shape '{name}' expects {} type argument(s), got {}",
                template.type_params.len(),
                arg_types.len()
            )));
        }

        let tokens: Vec<String> = arg_types.iter().map(|bt| bt.mangle_token()).collect();
        let mangled = format!("{name}${}", tokens.join("$"));

        if self.registry.by_name.contains_key(&mangled) {
            return Ok(mangled);
        }

        // Build the inner binding scope for field resolution.
        let mut inner_bindings = TypeBindings::new();
        for (tp, arg_ty) in template.type_params.iter().zip(arg_types.iter()) {
            inner_bindings.insert(tp.clone(), arg_ty.clone());
        }

        // Pre-insert the mangled name into class_names so any self-referential
        // field (e.g. `interface Node<T> { next: Node<T> }`) with bindings that
        // resolve to the same mangled form can look itself up. The nested
        // `instantiate_generic_shape` call returns the same mangled form idempotently.
        self.class_names.insert(mangled.clone());

        let fields = match template.ast {
            GenericShapeAst::Alias(alias) => {
                let TSType::TSTypeLiteral(lit) = &alias.type_annotation else {
                    return Err(CompileError::type_err(format!(
                        "generic shape '{name}' is not an object-literal alias"
                    )));
                };
                self.resolve_type_literal_fields(lit, Some(&inner_bindings))?
            }
            GenericShapeAst::Iface(iface) => {
                self.resolve_interface_body_fields(&iface.body, Some(&inner_bindings))?
            }
        };

        let _ = self.insert_shape(ShapeKind::Named, Some(mangled.clone()), fields)?;
        Ok(mangled)
    }

    /// Resolve a single type argument, routing nested literals / tuples /
    /// generic-shape refs through their proper registration paths.
    fn resolve_type_arg(
        &mut self,
        ts_type: &'a TSType<'a>,
        bindings: Option<&TypeBindings>,
    ) -> Result<BoundType, CompileError> {
        match ts_type {
            TSType::TSTypeLiteral(lit) => {
                let fields = self.resolve_type_literal_fields(lit, bindings)?;
                let (n, _) = self.insert_shape(ShapeKind::Anonymous, None, fields)?;
                self.class_names.insert(n.clone());
                Ok(BoundType::Class(n))
            }
            TSType::TSTupleType(tuple) => {
                let n = self.register_tuple_shape(tuple, bindings)?;
                self.class_names.insert(n.clone());
                Ok(BoundType::Class(n))
            }
            TSType::TSTypeReference(tref) => {
                let tname = tref
                    .type_name
                    .get_identifier_reference()
                    .map(|r| r.name.as_str());
                if let Some(tn) = tname
                    && self.generic_shape_templates.contains_key(tn)
                {
                    let Some(inner_args) = tref.type_arguments.as_ref() else {
                        return Err(CompileError::type_err(format!(
                            "generic shape '{tn}' used without type arguments"
                        )));
                    };
                    let m = self.instantiate_generic_shape(tn, inner_args, bindings)?;
                    return Ok(BoundType::Class(m));
                }
                resolve_bound_type(ts_type, &self.class_names, bindings)
            }
            _ => resolve_bound_type(ts_type, &self.class_names, bindings),
        }
    }

    /// Prefix `parent_name`'s resolved fields onto `own_fields`, rejecting any
    /// shadowing. Parent must already be registered — caller guarantees this
    /// via topo-sorted iteration order.
    fn merge_parent_fields(
        &self,
        child_name: &str,
        parent_name: &str,
        own_fields: Vec<ShapeField>,
    ) -> Result<Vec<ShapeField>, CompileError> {
        let parent_idx = self.registry.by_name.get(parent_name).ok_or_else(|| {
            CompileError::type_err(format!(
                "interface '{child_name}' extends '{parent_name}', but '{parent_name}' is not a known shape type"
            ))
        })?;
        let parent_fields = self.registry.shapes[*parent_idx].fields.clone();
        let parent_names: HashSet<&str> = parent_fields.iter().map(|f| f.name.as_str()).collect();
        for f in &own_fields {
            if parent_names.contains(f.name.as_str()) {
                return Err(CompileError::type_err(format!(
                    "interface '{child_name}' redeclares field '{}' inherited from '{parent_name}' — \
                     overriding inherited fields is not yet supported",
                    f.name
                )));
            }
        }
        let mut merged = parent_fields;
        merged.extend(own_fields);
        Ok(merged)
    }

    /// Fail if `name` already names a class, generic template, or another
    /// registered shape. Shapes share `class_names` with classes because
    /// downstream member-access / destructuring paths resolve via the same
    /// registry — a collision would silently overwrite.
    fn reject_top_level_collision(&self, name: &str, span_start: u32) -> Result<(), CompileError> {
        let _ = span_start; // reserved for future source-location-aware diagnostics
        if self.real_class_names.contains(name) {
            return Err(CompileError::type_err(format!(
                "'{name}' is already declared as a class — cannot also be a shape type"
            )));
        }
        if self.class_templates.contains_key(name) {
            return Err(CompileError::type_err(format!(
                "'{name}' is already declared as a generic class — cannot also be a shape type"
            )));
        }
        if self.fn_templates.contains_key(name) {
            return Err(CompileError::type_err(format!(
                "'{name}' is already declared as a generic function — cannot also be a shape type"
            )));
        }
        if self.registry.by_name.contains_key(name) {
            return Err(CompileError::type_err(format!(
                "duplicate shape type '{name}' — each `type` / `interface` name must be unique"
            )));
        }
        Ok(())
    }

    // -- field resolution from TS types -------------------------------------

    fn resolve_type_literal_fields(
        &mut self,
        lit: &'a TSTypeLiteral<'a>,
        bindings: Option<&TypeBindings>,
    ) -> Result<Vec<ShapeField>, CompileError> {
        self.resolve_signature_fields(&lit.members, bindings)
    }

    fn resolve_interface_body_fields(
        &mut self,
        body: &'a TSInterfaceBody<'a>,
        bindings: Option<&TypeBindings>,
    ) -> Result<Vec<ShapeField>, CompileError> {
        self.resolve_signature_fields(&body.body, bindings)
    }

    fn resolve_signature_fields(
        &mut self,
        sigs: &'a [TSSignature<'a>],
        bindings: Option<&TypeBindings>,
    ) -> Result<Vec<ShapeField>, CompileError> {
        let mut fields = Vec::with_capacity(sigs.len());
        let mut seen_names: HashSet<String> = HashSet::with_capacity(sigs.len());
        for sig in sigs {
            match sig {
                TSSignature::TSPropertySignature(prop) => {
                    let name = property_signature_key(prop)?;
                    if prop.optional {
                        return Err(CompileError::unsupported(format!(
                            "optional property '{name}?' in shape type — not yet supported (deferred with union types)"
                        )));
                    }
                    if prop.computed {
                        return Err(CompileError::unsupported(
                            "computed property key in shape type",
                        ));
                    }
                    if seen_names.contains(&name) {
                        return Err(CompileError::type_err(format!(
                            "duplicate property '{name}' in shape type"
                        )));
                    }
                    let ann = prop.type_annotation.as_ref().ok_or_else(|| {
                        CompileError::type_err(format!(
                            "shape property '{name}' requires a type annotation"
                        ))
                    })?;
                    let ty = self.resolve_field_type(&ann.type_annotation, bindings)?;
                    seen_names.insert(name.clone());
                    fields.push(ShapeField { name, ty });
                }
                TSSignature::TSMethodSignature(_) => {
                    return Err(CompileError::unsupported(
                        "method signatures in shape / interface types — only property signatures are supported",
                    ));
                }
                TSSignature::TSIndexSignature(_) => {
                    return Err(CompileError::unsupported(
                        "index signatures (`[k: string]: V`) in shape / interface types — use `Map<K, V>` instead",
                    ));
                }
                TSSignature::TSCallSignatureDeclaration(_)
                | TSSignature::TSConstructSignatureDeclaration(_) => {
                    return Err(CompileError::unsupported(
                        "call / construct signatures in shape / interface types",
                    ));
                }
            }
        }
        Ok(fields)
    }

    /// Resolve a field's TSType into a `BoundType`, including the inline
    /// `TSTypeLiteral` case which recursively registers a nested anonymous
    /// shape and returns `BoundType::Class(mangled)`.
    fn resolve_field_type(
        &mut self,
        ts_type: &'a TSType<'a>,
        bindings: Option<&TypeBindings>,
    ) -> Result<BoundType, CompileError> {
        match ts_type {
            TSType::TSTypeLiteral(lit) => {
                // Nested anonymous shape. Register depth-first so the outer
                // field can reference its mangled name. Under a binding scope,
                // the nested shape's fields substitute T → concrete, producing
                // a distinct anonymous shape per instantiation.
                let inner_fields = self.resolve_type_literal_fields(lit, bindings)?;
                let (inserted, _) = self.insert_shape(ShapeKind::Anonymous, None, inner_fields)?;
                Ok(BoundType::Class(inserted))
            }
            TSType::TSTupleType(tuple) => {
                // Nested tuple shape — register depth-first so the outer
                // field references the tuple's mangled name.
                let inserted = self.register_tuple_shape(tuple, bindings)?;
                Ok(BoundType::Class(inserted))
            }
            TSType::TSTypeReference(tref) => {
                // Generic shape reference (`Pair<T, U>` inside a template body,
                // or a direct `Pair<i32, f64>` in a field type). Instantiate on
                // demand — the result is a concrete shape under a mangled name.
                let tname = tref
                    .type_name
                    .get_identifier_reference()
                    .map(|r| r.name.as_str());
                if let Some(tn) = tname
                    && self.generic_shape_templates.contains_key(tn)
                {
                    let Some(inner_args) = tref.type_arguments.as_ref() else {
                        return Err(CompileError::type_err(format!(
                            "generic shape '{tn}' used without type arguments"
                        )));
                    };
                    let m = self.instantiate_generic_shape(tn, inner_args, bindings)?;
                    return Ok(BoundType::Class(m));
                }
                resolve_bound_type(ts_type, &self.class_names, bindings)
            }
            _ => resolve_bound_type(ts_type, &self.class_names, bindings),
        }
    }

    /// Register (or dedupe into) a tuple shape for a `TSTupleType` node.
    /// Returns the tuple's registered class name (`__Tuple$...`). Element
    /// types are resolved depth-first — nested tuples / inline object
    /// literals register their own shapes before the outer tuple's
    /// fingerprint is computed. Updates `annotation_shapes` with the tuple
    /// span so downstream type resolution can look it up.
    fn register_tuple_shape(
        &mut self,
        tuple: &'a TSTupleType<'a>,
        bindings: Option<&TypeBindings>,
    ) -> Result<String, CompileError> {
        if tuple.element_types.is_empty() {
            return Err(CompileError::unsupported(
                "empty tuple type `[]` — not yet supported (add at least one element)",
            ));
        }
        let mut element_tys: Vec<BoundType> = Vec::with_capacity(tuple.element_types.len());
        for el in &tuple.element_types {
            match el {
                TSTupleElement::TSOptionalType(_) => {
                    return Err(CompileError::unsupported(
                        "optional tuple element `T?` — not yet supported (deferred with union types)",
                    ));
                }
                TSTupleElement::TSRestType(_) => {
                    return Err(CompileError::unsupported(
                        "rest tuple element `...T[]` — not yet supported (Phase E)",
                    ));
                }
                TSTupleElement::TSNamedTupleMember(named) => {
                    // Phase E.5: accept `[x: T, y: U]`. Labels are purely
                    // documentation — tuple identity stays positional, so we
                    // discard `named.label` and resolve `element_type` like
                    // any other tuple slot. A trailing `?` (`[x?: T]`) flows
                    // through to the outer union-deferred rejection below.
                    if named.optional {
                        return Err(CompileError::unsupported(
                            "optional named tuple element `x?: T` — not yet supported (deferred with union types)",
                        ));
                    }
                    let ts = named.element_type.as_ts_type().ok_or_else(|| {
                        CompileError::unsupported(
                            "unsupported named tuple element form (rest / nested-named)",
                        )
                    })?;
                    element_tys.push(self.resolve_field_type(ts, bindings)?);
                }
                _ => {
                    let ts = el
                        .as_ts_type()
                        .ok_or_else(|| CompileError::unsupported("unsupported tuple element form"))?;
                    element_tys.push(self.resolve_field_type(ts, bindings)?);
                }
            }
        }
        let fields: Vec<ShapeField> = element_tys
            .iter()
            .enumerate()
            .map(|(i, ty)| ShapeField {
                name: format!("_{i}"),
                ty: ty.clone(),
            })
            .collect();
        let fp = tuple_fingerprint_of(&element_tys);
        let name = format!("{TUPLE_SHAPE_PREFIX}{fp}");
        let (registered_name, index) =
            self.insert_tuple(name.clone(), fp, fields)?;
        self.registry.annotation_shapes.insert(tuple.span, index);
        Ok(registered_name)
    }

    /// Insert (or dedupe into) a tuple entry. Unlike `insert_shape`, tuples
    /// have positional identity and use a pre-computed positional fingerprint;
    /// the object-shape fingerprint space doesn't collide (different token
    /// format).
    fn insert_tuple(
        &mut self,
        name: String,
        fingerprint: String,
        fields: Vec<ShapeField>,
    ) -> Result<(String, usize), CompileError> {
        if let Some(&existing_idx) = self.registry.by_fingerprint.get(&fingerprint) {
            let existing_name = self.registry.shapes[existing_idx].name.clone();
            return Ok((existing_name, existing_idx));
        }
        let idx = self.registry.shapes.len();
        self.registry.shapes.push(Shape {
            kind: ShapeKind::Anonymous,
            name: name.clone(),
            fingerprint: fingerprint.clone(),
            fields,
            is_tuple: true,
        });
        self.registry.by_fingerprint.insert(fingerprint, idx);
        self.registry.by_name.insert(name.clone(), idx);
        Ok((name, idx))
    }

    // -- anonymous-shape discovery (Pass 2) ---------------------------------

    pub(super) fn walk_statement(&mut self, stmt: &'a Statement<'a>) -> Result<(), CompileError> {
        match stmt {
            Statement::VariableDeclaration(var_decl) => {
                for d in &var_decl.declarations {
                    if let Some(ann) = &d.type_annotation {
                        self.walk_ts_type(&ann.type_annotation)?;
                    }
                    if let Some(init) = &d.init {
                        self.walk_expression(init)?;
                    }
                }
            }
            Statement::FunctionDeclaration(func) if !func.declare => {
                self.walk_function(func)?;
            }
            Statement::ClassDeclaration(class) => {
                self.walk_class(class)?;
            }
            Statement::ExpressionStatement(s) => {
                self.walk_expression(&s.expression)?;
            }
            Statement::ReturnStatement(r) => {
                if let Some(a) = &r.argument {
                    self.walk_expression(a)?;
                }
            }
            Statement::IfStatement(iff) => {
                self.walk_expression(&iff.test)?;
                self.walk_statement(&iff.consequent)?;
                if let Some(alt) = &iff.alternate {
                    self.walk_statement(alt)?;
                }
            }
            Statement::WhileStatement(w) => {
                self.walk_expression(&w.test)?;
                self.walk_statement(&w.body)?;
            }
            Statement::DoWhileStatement(w) => {
                self.walk_statement(&w.body)?;
                self.walk_expression(&w.test)?;
            }
            Statement::ForStatement(f) => {
                if let Some(init) = &f.init {
                    match init {
                        ForStatementInit::VariableDeclaration(vd) => {
                            for d in &vd.declarations {
                                if let Some(ann) = &d.type_annotation {
                                    self.walk_ts_type(&ann.type_annotation)?;
                                }
                                if let Some(e) = &d.init {
                                    self.walk_expression(e)?;
                                }
                            }
                        }
                        _ => {
                            if let Some(e) = init.as_expression() {
                                self.walk_expression(e)?;
                            }
                        }
                    }
                }
                if let Some(t) = &f.test {
                    self.walk_expression(t)?;
                }
                if let Some(u) = &f.update {
                    self.walk_expression(u)?;
                }
                self.walk_statement(&f.body)?;
            }
            Statement::ForOfStatement(f) => {
                self.walk_expression(&f.right)?;
                self.walk_statement(&f.body)?;
            }
            Statement::BlockStatement(b) => {
                for s in &b.body {
                    self.walk_statement(s)?;
                }
            }
            Statement::SwitchStatement(s) => {
                self.walk_expression(&s.discriminant)?;
                for case in &s.cases {
                    if let Some(t) = &case.test {
                        self.walk_expression(t)?;
                    }
                    for stmt in &case.consequent {
                        self.walk_statement(stmt)?;
                    }
                }
            }
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    match decl {
                        Declaration::VariableDeclaration(vd) => {
                            for d in &vd.declarations {
                                if let Some(ann) = &d.type_annotation {
                                    self.walk_ts_type(&ann.type_annotation)?;
                                }
                                if let Some(init) = &d.init {
                                    self.walk_expression(init)?;
                                }
                            }
                        }
                        Declaration::FunctionDeclaration(func) if !func.declare => {
                            self.walk_function(func)?;
                        }
                        Declaration::ClassDeclaration(class) => {
                            self.walk_class(class)?;
                        }
                        _ => {}
                    }
                }
            }
            Statement::ExportDefaultDeclaration(export) => {
                if let ExportDefaultDeclarationKind::FunctionDeclaration(func) =
                    &export.declaration
                {
                    self.walk_function(func)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn walk_function(&mut self, func: &'a Function<'a>) -> Result<(), CompileError> {
        for param in &func.params.items {
            if let Some(ann) = &param.type_annotation {
                self.walk_ts_type(&ann.type_annotation)?;
            }
        }
        if let Some(ret) = &func.return_type {
            self.walk_ts_type(&ret.type_annotation)?;
        }
        if let Some(body) = &func.body {
            for stmt in &body.statements {
                self.walk_statement(stmt)?;
            }
        }
        Ok(())
    }

    fn walk_class(&mut self, class: &'a Class<'a>) -> Result<(), CompileError> {
        for element in &class.body.body {
            match element {
                ClassElement::PropertyDefinition(prop) => {
                    if let Some(ann) = &prop.type_annotation {
                        self.walk_ts_type(&ann.type_annotation)?;
                    }
                    if let Some(val) = &prop.value {
                        self.walk_expression(val)?;
                    }
                }
                ClassElement::MethodDefinition(m) => {
                    self.walk_function(&m.value)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn walk_expression(&mut self, expr: &'a Expression<'a>) -> Result<(), CompileError> {
        match expr {
            Expression::ObjectExpression(obj) => {
                // Drive the inference+registration first so descendants see
                // the registry already populated with this shape's fingerprint.
                self.try_register_anonymous_from_object(obj)?;
                // Recurse into values so nested literals also register.
                for prop in &obj.properties {
                    if let ObjectPropertyKind::ObjectProperty(p) = prop {
                        self.walk_expression(&p.value)?;
                    }
                }
            }
            Expression::ArrayExpression(arr) => {
                for elem in &arr.elements {
                    if let Some(e) = elem.as_expression() {
                        self.walk_expression(e)?;
                    }
                }
            }
            Expression::ParenthesizedExpression(p) => self.walk_expression(&p.expression)?,
            Expression::UnaryExpression(u) => self.walk_expression(&u.argument)?,
            Expression::BinaryExpression(b) => {
                self.walk_expression(&b.left)?;
                self.walk_expression(&b.right)?;
            }
            Expression::LogicalExpression(l) => {
                self.walk_expression(&l.left)?;
                self.walk_expression(&l.right)?;
            }
            Expression::ConditionalExpression(c) => {
                self.walk_expression(&c.test)?;
                self.walk_expression(&c.consequent)?;
                self.walk_expression(&c.alternate)?;
            }
            Expression::AssignmentExpression(a) => self.walk_expression(&a.right)?,
            Expression::UpdateExpression(_) => {}
            Expression::CallExpression(call) => {
                self.walk_expression(&call.callee)?;
                if let Some(args) = call.type_arguments.as_ref() {
                    for a in &args.params {
                        self.walk_ts_type(a)?;
                    }
                }
                for arg in &call.arguments {
                    if let Some(e) = arg.as_expression() {
                        self.walk_expression(e)?;
                    }
                }
            }
            Expression::NewExpression(n) => {
                self.walk_expression(&n.callee)?;
                if let Some(args) = n.type_arguments.as_ref() {
                    for a in &args.params {
                        self.walk_ts_type(a)?;
                    }
                }
                for arg in &n.arguments {
                    if let Some(e) = arg.as_expression() {
                        self.walk_expression(e)?;
                    }
                }
            }
            Expression::StaticMemberExpression(m) => self.walk_expression(&m.object)?,
            Expression::ComputedMemberExpression(m) => {
                self.walk_expression(&m.object)?;
                self.walk_expression(&m.expression)?;
            }
            Expression::ChainExpression(c) => match &c.expression {
                ChainElement::CallExpression(call) => {
                    self.walk_expression(&call.callee)?;
                    for arg in &call.arguments {
                        if let Some(e) = arg.as_expression() {
                            self.walk_expression(e)?;
                        }
                    }
                }
                ChainElement::TSNonNullExpression(n) => self.walk_expression(&n.expression)?,
                ChainElement::StaticMemberExpression(m) => self.walk_expression(&m.object)?,
                ChainElement::ComputedMemberExpression(m) => {
                    self.walk_expression(&m.object)?;
                    self.walk_expression(&m.expression)?;
                }
                ChainElement::PrivateFieldExpression(p) => self.walk_expression(&p.object)?,
            },
            Expression::TSAsExpression(a) => {
                self.walk_ts_type(&a.type_annotation)?;
                self.walk_expression(&a.expression)?;
            }
            Expression::ArrowFunctionExpression(arrow) => {
                for param in &arrow.params.items {
                    if let Some(ann) = &param.type_annotation {
                        self.walk_ts_type(&ann.type_annotation)?;
                    }
                }
                if let Some(ret) = &arrow.return_type {
                    self.walk_ts_type(&ret.type_annotation)?;
                }
                for stmt in &arrow.body.statements {
                    self.walk_statement(stmt)?;
                }
            }
            Expression::TemplateLiteral(tpl) => {
                for expr in &tpl.expressions {
                    self.walk_expression(expr)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Record any inline `TSTypeLiteral` nested inside a TS type.
    fn walk_ts_type(&mut self, ts_type: &'a TSType<'a>) -> Result<(), CompileError> {
        match ts_type {
            TSType::TSTypeLiteral(lit) => {
                let fields = self.resolve_type_literal_fields(lit, None)?;
                let (_, index) = self.insert_shape(ShapeKind::Anonymous, None, fields)?;
                self.registry.annotation_shapes.insert(lit.span, index);
            }
            TSType::TSTupleType(tuple) => {
                self.register_tuple_shape(tuple, None)?;
            }
            TSType::TSTypeReference(type_ref) => {
                // Generic shape instantiation: `Pair<i32, f64>` here triggers
                // registration of the concrete shape under the mangled name.
                let name = type_ref
                    .type_name
                    .get_identifier_reference()
                    .map(|r| r.name.as_str());
                if let (Some(n), Some(args)) = (name, type_ref.type_arguments.as_ref())
                    && self.generic_shape_templates.contains_key(n)
                {
                    self.instantiate_generic_shape(n, args, None)?;
                }
                if let Some(args) = type_ref.type_arguments.as_ref() {
                    for a in &args.params {
                        self.walk_ts_type(a)?;
                    }
                }
            }
            TSType::TSArrayType(arr) => self.walk_ts_type(&arr.element_type)?,
            TSType::TSUnionType(u) => {
                for t in &u.types {
                    self.walk_ts_type(t)?;
                }
            }
            TSType::TSIntersectionType(i) => {
                for t in &i.types {
                    self.walk_ts_type(t)?;
                }
            }
            TSType::TSParenthesizedType(p) => self.walk_ts_type(&p.type_annotation)?,
            TSType::TSFunctionType(f) => {
                for param in &f.params.items {
                    if let Some(ann) = &param.type_annotation {
                        self.walk_ts_type(&ann.type_annotation)?;
                    }
                }
                self.walk_ts_type(&f.return_type.type_annotation)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Try to register a shape for an `ObjectExpression` by inferring each
    /// field's type from its initializer. Returns silently if any field's
    /// type cannot be determined standalone — Phase A.2's emitter will
    /// register such shapes lazily once full codegen context is available.
    fn try_register_anonymous_from_object(
        &mut self,
        obj: &'a ObjectExpression<'a>,
    ) -> Result<(), CompileError> {
        let mut fields = Vec::with_capacity(obj.properties.len());
        let mut seen: HashSet<String> = HashSet::with_capacity(obj.properties.len());
        for prop in &obj.properties {
            let ObjectPropertyKind::ObjectProperty(p) = prop else {
                // Spread / method / getter — deferred to Phase E. Skip the
                // whole literal (don't register a partial shape).
                return Ok(());
            };
            if p.method || p.computed {
                return Ok(());
            }
            let key = match &p.key {
                PropertyKey::StaticIdentifier(id) => id.name.as_str().to_string(),
                PropertyKey::StringLiteral(s) => s.value.as_str().to_string(),
                _ => return Ok(()),
            };
            if seen.contains(&key) {
                return Err(CompileError::type_err(format!(
                    "duplicate property '{key}' in object literal"
                )));
            }
            let Some(ty) = self.infer_expr_bound_type(&p.value)? else {
                // Not inferable standalone. Leave to emit-time lazy
                // registration (planned for Phase A.2).
                return Ok(());
            };
            seen.insert(key.clone());
            fields.push(ShapeField { name: key, ty });
        }
        let _ = self.insert_shape(ShapeKind::Anonymous, None, fields)?;
        Ok(())
    }

    /// Lightweight type inference for `ObjectExpression` field values. Only
    /// handles the forms that can be resolved without a codegen context:
    /// literals, casts, nested object literals, and `new ClassName()` on a
    /// known class. Returns `Ok(None)` for anything else — Phase A.2 will
    /// handle these at emit time when full locals/function context is
    /// available.
    fn infer_expr_bound_type(
        &mut self,
        expr: &'a Expression<'a>,
    ) -> Result<Option<BoundType>, CompileError> {
        Ok(match expr {
            Expression::NumericLiteral(lit) => {
                let is_float = lit.raw.as_ref().is_some_and(|r| r.contains('.'))
                    || lit.value.fract() != 0.0
                    || !((i32::MIN as f64)..=(i32::MAX as f64)).contains(&lit.value);
                Some(if is_float {
                    BoundType::F64
                } else {
                    BoundType::I32
                })
            }
            Expression::BooleanLiteral(_) => Some(BoundType::Bool),
            Expression::StringLiteral(_) | Expression::TemplateLiteral(_) => Some(BoundType::Str),
            Expression::NullLiteral(_) => None,
            Expression::UnaryExpression(u) if matches!(u.operator, UnaryOperator::UnaryNegation) => {
                self.infer_expr_bound_type(&u.argument)?
            }
            Expression::ParenthesizedExpression(p) => self.infer_expr_bound_type(&p.expression)?,
            Expression::TSAsExpression(a) => {
                // Trust the user's cast, provided we can resolve the target.
                resolve_bound_type(&a.type_annotation, &self.class_names, None).ok()
            }
            Expression::NewExpression(n) => {
                let Expression::Identifier(id) = &n.callee else {
                    return Ok(None);
                };
                let base = id.name.as_str();
                if self.class_names.contains(base) && n.type_arguments.is_none() {
                    return Ok(Some(BoundType::Class(base.to_string())));
                }
                if let Some(tpl) = self.class_templates.get(base)
                    && let Some(args) = n.type_arguments.as_ref()
                {
                    if args.params.len() != tpl.type_params.len() {
                        return Ok(None);
                    }
                    let mut tokens = Vec::with_capacity(args.params.len());
                    for a in &args.params {
                        match resolve_bound_type(a, &self.class_names, None) {
                            Ok(bt) => tokens.push(bt.mangle_token()),
                            Err(_) => return Ok(None),
                        }
                    }
                    return Ok(Some(BoundType::Class(format!(
                        "{base}${}",
                        tokens.join("$")
                    ))));
                }
                None
            }
            Expression::ObjectExpression(inner) => {
                // Recursively register the nested shape and return its name.
                let start_len = self.registry.shapes.len();
                self.try_register_anonymous_from_object(inner)?;
                // If registration bailed (some field not inferable), we
                // can't name the nested shape and therefore can't name the
                // outer either.
                if self.registry.shapes.len() == start_len {
                    // Could also be that the nested shape's fingerprint
                    // matched an already-registered one. Handle that via
                    // fingerprint recomputation, if the fingerprint is
                    // inferable.
                    return Ok(
                        self.try_fingerprint_for_object(inner)?
                            .and_then(|fp| {
                                self.registry
                                    .get_by_fingerprint(&fp)
                                    .map(|s| BoundType::Class(s.name.clone()))
                            }),
                    );
                }
                Some(BoundType::Class(
                    self.registry.shapes.last().unwrap().name.clone(),
                ))
            }
            _ => None,
        })
    }

    /// Like `try_register_anonymous_from_object` but read-only: compute the
    /// fingerprint if every field is inferable. Used by `infer_expr_bound_type`
    /// when a nested literal aliases to an already-registered shape.
    fn try_fingerprint_for_object(
        &mut self,
        obj: &'a ObjectExpression<'a>,
    ) -> Result<Option<String>, CompileError> {
        let mut fields: Vec<ShapeField> = Vec::with_capacity(obj.properties.len());
        for prop in &obj.properties {
            let ObjectPropertyKind::ObjectProperty(p) = prop else {
                return Ok(None);
            };
            if p.method || p.computed {
                return Ok(None);
            }
            let key = match &p.key {
                PropertyKey::StaticIdentifier(id) => id.name.as_str().to_string(),
                PropertyKey::StringLiteral(s) => s.value.as_str().to_string(),
                _ => return Ok(None),
            };
            let Some(ty) = self.infer_expr_bound_type(&p.value)? else {
                return Ok(None);
            };
            fields.push(ShapeField { name: key, ty });
        }
        Ok(Some(fingerprint_of(&fields)))
    }

    // -- shared insertion ---------------------------------------------------

    /// Insert a shape into the registry, honoring dedup by fingerprint and
    /// name-collision rules. Returns the final registered name — either the
    /// caller-provided one, the canonical name of an existing shape the
    /// fingerprint aliased into, or a freshly-derived `__ObjLit$...` mangled
    /// name for anonymous first-occurrences.
    fn insert_shape(
        &mut self,
        kind: ShapeKind,
        requested_name: Option<String>,
        fields: Vec<ShapeField>,
    ) -> Result<(String, usize), CompileError> {
        if fields.is_empty() {
            return Err(CompileError::unsupported(
                "empty object shape (`{}`) is not yet supported — add at least one field",
            ));
        }
        let fp = fingerprint_of(&fields);

        if let Some(&existing_idx) = self.registry.by_fingerprint.get(&fp) {
            // Shape already known. If caller supplied a *new* name (Named),
            // make it an alias into the canonical shape. Otherwise dedupe.
            let existing_name = self.registry.shapes[existing_idx].name.clone();
            if let Some(req) = requested_name {
                if self.registry.shapes[existing_idx].kind == ShapeKind::Anonymous {
                    // The anonymous name was a placeholder; upgrade to the
                    // named one. First-seen layout wins, but the user's
                    // visible name wins over the mangled fingerprint.
                    let old_name = self.registry.shapes[existing_idx].name.clone();
                    self.registry.shapes[existing_idx].kind = ShapeKind::Named;
                    self.registry.shapes[existing_idx].name = req.clone();
                    self.registry.by_name.remove(&old_name);
                    self.registry.by_name.insert(req.clone(), existing_idx);
                    return Ok((req, existing_idx));
                }
                // Existing shape is already named; the requested name is a
                // second distinct user name. Alias it for lookup so either
                // name resolves to the same layout.
                if req != existing_name {
                    self.registry.by_name.insert(req.clone(), existing_idx);
                }
                return Ok((existing_name, existing_idx));
            }
            return Ok((existing_name, existing_idx));
        }

        // Fresh shape.
        let name = match requested_name {
            Some(n) => n,
            None => format!("{ANON_SHAPE_PREFIX}{fp}"),
        };
        let idx = self.registry.shapes.len();
        self.registry.shapes.push(Shape {
            kind,
            name: name.clone(),
            fingerprint: fp.clone(),
            fields,
            is_tuple: false,
        });
        self.registry.by_fingerprint.insert(fp, idx);
        self.registry.by_name.insert(name.clone(), idx);
        Ok((name, idx))
    }
}
