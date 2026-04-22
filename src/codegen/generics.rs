//! Generics foundation: template discovery, instantiation collection, and
//! monomorphization planning.
//!
//! The pipeline is eager:
//! 1. `discover_templates` scans `program.body` for classes/functions that
//!    declare type parameters.
//! 2. `collect_instantiations` walks the AST a second time collecting every
//!    concrete instantiation (`Box<i32>`, `identity<f64>(x)`, etc.), including
//!    nested ones inside other generics.
//! 3. The caller (`compile_module`) registers each monomorphization as a
//!    regular concrete class/function in the registries, with the template
//!    bindings pushed so field/param/return types substitute correctly.
//!
//! Mangling: a generic class/function name `Foo` instantiated with `[i32, Bar]`
//! becomes `Foo$i32$Bar`. Mangled tokens come from `BoundType::mangle_token`,
//! so nested generics collapse naturally (`Map<string, Box<i32>>` → the inner
//! `Box<i32>` instantiation mangles to `Box$i32`, and the outer map's key+value
//! bindings render to `string` + `Box$i32` respectively).
//!
//! **Inference** (implicit-arg generic function calls like `identity(5)`):
//! handled inline in the expression walker. For each type parameter we find
//! the first parameter slot whose annotation is the bare type-parameter
//! reference, then read the apparent BoundType of the matching argument from
//! the AST. Supported argument shapes: literals, casts, `new` on known
//! classes, and identifiers resolved against a per-function locals env built
//! from parameters. If all type parameters resolve, the monomorphization is
//! inserted and the call site's mangled name is recorded so the call-site
//! rewriter in `emit_call` can route through it.

use std::collections::{HashMap, HashSet};

use oxc_ast::ast::*;

use crate::error::CompileError;
use crate::types::{BoundType, TypeBindings};

use super::map_builtins::{self, MapInstantiation};
use super::set_builtins::{self, SetInstantiation};

/// A generic class declaration captured for later monomorphization.
#[derive(Debug)]
pub struct GenericClassTemplate<'a> {
    pub name: String,
    /// Type-parameter names in declaration order (e.g. `["K", "V"]`).
    pub type_params: Vec<String>,
    /// AST node; preserved so method bodies can be re-compiled per instantiation
    /// and so the extends clause (including type arguments) stays reachable via
    /// `ast.super_class` / `ast.super_type_arguments`.
    pub ast: &'a Class<'a>,
}

/// A generic free function declaration captured for later monomorphization.
#[derive(Debug)]
pub struct GenericFnTemplate<'a> {
    pub name: String,
    pub type_params: Vec<String>,
    pub ast: &'a Function<'a>,
    pub is_export: bool,
}

/// One concrete use of a generic class (e.g. `Box<i32>`, `Pair<i32, MyClass>`).
#[derive(Debug, Clone)]
pub struct ClassInstantiation {
    pub template_name: String,
    pub mangled_name: String,
    pub bindings: TypeBindings,
}

/// One concrete use of a generic function (e.g. `identity<i32>`).
#[derive(Debug, Clone)]
pub struct FnInstantiation {
    pub template_name: String,
    pub mangled_name: String,
    pub bindings: TypeBindings,
}

/// Per-function parameter type scope used during inference. Maps identifier
/// name to the `BoundType` it was declared with, after bindings substitution.
pub type LocalTypeEnv = HashMap<String, BoundType>;

/// Output of `collect_instantiations`. The `inferred_call_sites` map records,
/// for each CallExpression whose type arguments were inferred, the mangled
/// monomorphization name keyed by the call's `span.start`. `emit_call` looks
/// this up at codegen time to route through the right function.
/// `map_insts` carries the compiler-owned `Map<K, V>` instantiations seen in
/// user source; they travel alongside user classes but go through a separate
/// registration path (no AST). `set_insts` is the analogous channel for
/// `Set<T>`.
pub struct CollectResult {
    pub class_insts: Vec<ClassInstantiation>,
    pub fn_insts: Vec<FnInstantiation>,
    pub inferred_call_sites: HashMap<u32, String>,
    pub map_insts: Vec<MapInstantiation>,
    pub set_insts: Vec<SetInstantiation>,
}

/// Walk the program body and collect all generic class/function templates.
/// Non-generic declarations are ignored — they flow through the existing passes
/// unchanged.
pub fn discover_templates<'a>(
    program: &'a Program<'a>,
) -> (
    HashMap<String, GenericClassTemplate<'a>>,
    HashMap<String, GenericFnTemplate<'a>>,
) {
    let mut classes = HashMap::new();
    let mut fns = HashMap::new();

    for stmt in &program.body {
        match stmt {
            Statement::ClassDeclaration(class) => {
                if let Some(template) = classify_class(class) {
                    classes.insert(template.name.clone(), template);
                }
            }
            Statement::FunctionDeclaration(func) if !func.declare => {
                if let Some(template) = classify_fn(func, false) {
                    fns.insert(template.name.clone(), template);
                }
            }
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    match decl {
                        Declaration::ClassDeclaration(class) => {
                            if let Some(template) = classify_class(class) {
                                classes.insert(template.name.clone(), template);
                            }
                        }
                        Declaration::FunctionDeclaration(func) if !func.declare => {
                            if let Some(template) = classify_fn(func, true) {
                                fns.insert(template.name.clone(), template);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    (classes, fns)
}

fn classify_class<'a>(class: &'a Class<'a>) -> Option<GenericClassTemplate<'a>> {
    let params = class.type_parameters.as_ref()?;
    if params.params.is_empty() {
        return None;
    }
    let name = class.id.as_ref()?.name.as_str().to_string();
    let type_params: Vec<String> = params
        .params
        .iter()
        .map(|p| p.name.name.as_str().to_string())
        .collect();
    Some(GenericClassTemplate {
        name,
        type_params,
        ast: class,
    })
}

fn classify_fn<'a>(func: &'a Function<'a>, is_export: bool) -> Option<GenericFnTemplate<'a>> {
    let params = func.type_parameters.as_ref()?;
    if params.params.is_empty() {
        return None;
    }
    let name = func.id.as_ref()?.name.as_str().to_string();
    let type_params: Vec<String> = params
        .params
        .iter()
        .map(|p| p.name.name.as_str().to_string())
        .collect();
    Some(GenericFnTemplate {
        name,
        type_params,
        ast: func,
        is_export,
    })
}

/// Resolve a TSType argument into a `BoundType`, honoring an outer binding
/// scope (so `Box<T>` inside `class Foo<T>` binds through correctly).
/// Returns an error if the type is unsupported as a generic argument (e.g. a
/// function type).
pub fn resolve_bound_type(
    ts_type: &TSType,
    class_names: &HashSet<String>,
    bindings: Option<&TypeBindings>,
) -> Result<BoundType, CompileError> {
    match ts_type {
        TSType::TSNumberKeyword(_) => Ok(BoundType::F64),
        TSType::TSBooleanKeyword(_) => Ok(BoundType::Bool),
        TSType::TSStringKeyword(_) => Ok(BoundType::Str),
        TSType::TSTypeReference(type_ref) => {
            let name = type_ref
                .type_name
                .get_identifier_reference()
                .map(|r| r.name.as_str())
                .ok_or_else(|| {
                    CompileError::type_err("unsupported generic argument — namespaced type")
                })?;
            // Outer binding scope first — e.g. `T` inside `class Foo<T>`.
            if let Some(bound) = bindings.and_then(|b| b.get(name)) {
                return Ok(bound.clone());
            }
            match name {
                "i32" | "int" => Ok(BoundType::I32),
                "f64" | "number" => Ok(BoundType::F64),
                "bool" => Ok(BoundType::Bool),
                "string" => Ok(BoundType::Str),
                "Array" => Err(CompileError::unsupported(
                    "Array<T> as a generic parameter binding is not yet supported",
                )),
                other => {
                    // Class reference — either concrete or a mangled generic instantiation.
                    if let Some(args) = type_ref.type_arguments.as_ref() {
                        // Mangle the nested instantiation (e.g. Box<i32>).
                        let mut token_parts = Vec::with_capacity(args.params.len());
                        for p in &args.params {
                            let bt = resolve_bound_type(p, class_names, bindings)?;
                            token_parts.push(bt.mangle_token());
                        }
                        return Ok(BoundType::Class(format!(
                            "{other}${}",
                            token_parts.join("$")
                        )));
                    }
                    if class_names.contains(other) {
                        return Ok(BoundType::Class(other.to_string()));
                    }
                    Err(CompileError::type_err(format!(
                        "unknown type '{other}' as generic argument"
                    )))
                }
            }
        }
        _ => Err(CompileError::unsupported(
            "unsupported TS type as generic argument",
        )),
    }
}

/// Compute the `class_info` parent name for a class's `extends` clause,
/// mangling the parent if it is a generic template instantiated with concrete
/// type arguments. When the class does not extend anything, returns `Ok(None)`.
/// When the parent is concrete (no type arguments written), returns the raw
/// name. `bindings` is the enclosing monomorphization scope — used to resolve
/// `T` inside `extends Parent<T>` against the current instantiation.
pub fn mangle_parent_name(
    class: &Class,
    class_names: &HashSet<String>,
    class_templates: &HashMap<String, GenericClassTemplate>,
    bindings: Option<&TypeBindings>,
) -> Result<Option<String>, CompileError> {
    let Some(super_class) = &class.super_class else {
        return Ok(None);
    };
    let Expression::Identifier(id) = super_class else {
        return Err(CompileError::unsupported(
            "non-identifier in extends clause",
        ));
    };
    let raw = id.name.as_str();
    let Some(super_args) = class.super_type_arguments.as_deref() else {
        return Ok(Some(raw.to_string()));
    };
    let Some(template) = class_templates.get(raw) else {
        // Parent has type arguments written but isn't a generic template —
        // let downstream surface the normal "unknown class" diagnostic.
        return Ok(Some(raw.to_string()));
    };
    if super_args.params.len() != template.type_params.len() {
        return Err(CompileError::type_err(format!(
            "generic class '{raw}' expects {} type argument(s), got {}",
            template.type_params.len(),
            super_args.params.len()
        )));
    }
    let mut tokens = Vec::with_capacity(super_args.params.len());
    for a in &super_args.params {
        let bt = resolve_bound_type(a, class_names, bindings)?;
        tokens.push(bt.mangle_token());
    }
    Ok(Some(format!("{raw}${}", tokens.join("$"))))
}

/// Walk the program AST collecting every class and function instantiation.
/// Dedupes by mangled name. Handles nested generics — if a type reference's
/// arguments themselves contain generic references, those are collected too.
/// Generic function calls without explicit type arguments are inferred when
/// possible (see module-level doc).
pub fn collect_instantiations<'a>(
    program: &'a Program<'a>,
    class_templates: &HashMap<String, GenericClassTemplate<'a>>,
    fn_templates: &HashMap<String, GenericFnTemplate<'a>>,
    class_names: &HashSet<String>,
) -> Result<CollectResult, CompileError> {
    let mut walker = Walker {
        class_templates,
        fn_templates,
        class_names,
        cls: InstantiationSet::default(),
        fns: InstantiationSet::default(),
        inferred_sites: HashMap::new(),
        maps: Vec::new(),
        maps_seen: HashSet::new(),
        sets: Vec::new(),
        sets_seen: HashSet::new(),
    };

    let empty_locals = LocalTypeEnv::new();
    for stmt in &program.body {
        walker.walk_statement(stmt, None, &empty_locals)?;
    }

    // Fixed-point: walking a template's body with its bindings may surface new
    // instantiations of *other* templates. Keep iterating until no new ones
    // appear. In practice this terminates quickly — each template has a finite
    // number of referenced templates and each binding is bounded by the set of
    // concrete types that appear in the whole program.
    let mut worklist: Vec<(String, TypeBindings)> = walker
        .cls
        .order
        .iter()
        .filter_map(|m| {
            walker
                .cls
                .bindings_by_mangled
                .get(m)
                .map(|(t, b)| (t.clone(), b.clone()))
        })
        .collect();
    while let Some((template_name, bindings)) = worklist.pop() {
        let template = match walker.class_templates.get(&template_name) {
            Some(t) => t,
            None => continue,
        };
        let before_len = walker.cls.order.len();
        walker.walk_class_body(template.ast, Some(&bindings))?;
        for mangled in walker.cls.order.iter().skip(before_len).cloned().collect::<Vec<_>>() {
            if let Some((tn, bs)) = walker.cls.bindings_by_mangled.get(&mangled) {
                worklist.push((tn.clone(), bs.clone()));
            }
        }
    }

    Ok(CollectResult {
        class_insts: walker.cls.into_vec_class(),
        fn_insts: walker.fns.into_vec_fn(),
        inferred_call_sites: walker.inferred_sites,
        map_insts: walker.maps,
        set_insts: walker.sets,
    })
}

#[derive(Default)]
struct InstantiationSet {
    seen: HashSet<String>,
    order: Vec<String>,
    bindings_by_mangled: HashMap<String, (String, TypeBindings)>,
}

impl InstantiationSet {
    fn insert(&mut self, template_name: &str, mangled: String, bindings: TypeBindings) -> bool {
        if self.seen.insert(mangled.clone()) {
            self.order.push(mangled.clone());
            self.bindings_by_mangled
                .insert(mangled, (template_name.to_string(), bindings));
            true
        } else {
            false
        }
    }

    fn into_vec_class(self) -> Vec<ClassInstantiation> {
        self.order
            .into_iter()
            .map(|mangled| {
                let (template_name, bindings) = self
                    .bindings_by_mangled
                    .get(&mangled)
                    .cloned()
                    .unwrap_or_else(|| (String::new(), HashMap::new()));
                ClassInstantiation {
                    template_name,
                    mangled_name: mangled,
                    bindings,
                }
            })
            .collect()
    }

    fn into_vec_fn(self) -> Vec<FnInstantiation> {
        self.order
            .into_iter()
            .map(|mangled| {
                let (template_name, bindings) = self
                    .bindings_by_mangled
                    .get(&mangled)
                    .cloned()
                    .unwrap_or_else(|| (String::new(), HashMap::new()));
                FnInstantiation {
                    template_name,
                    mangled_name: mangled,
                    bindings,
                }
            })
            .collect()
    }
}

/// Internal walker carrying immutable AST-level context and the mutable
/// accumulators. Every `walk_*` method also takes the current type-parameter
/// `bindings` and the per-function `locals` env so recursive descent can thread
/// them without rebuilding per call.
struct Walker<'a, 'ctx> {
    class_templates: &'ctx HashMap<String, GenericClassTemplate<'a>>,
    fn_templates: &'ctx HashMap<String, GenericFnTemplate<'a>>,
    class_names: &'ctx HashSet<String>,
    cls: InstantiationSet,
    fns: InstantiationSet,
    inferred_sites: HashMap<u32, String>,
    /// Map<K, V> instantiations — order-preserving insertion with dedup keyed
    /// on the mangled name.
    maps: Vec<MapInstantiation>,
    maps_seen: HashSet<String>,
    /// Set<T> instantiations — same dedup discipline as `maps`.
    sets: Vec<SetInstantiation>,
    sets_seen: HashSet<String>,
}

impl Walker<'_, '_> {
    fn record_map_inst(&mut self, key_ty: BoundType, value_ty: BoundType) {
        let mangled = map_builtins::mangle_map_name(&key_ty, &value_ty);
        if self.maps_seen.insert(mangled.clone()) {
            self.maps.push(MapInstantiation {
                mangled_name: mangled,
                key_ty,
                value_ty,
            });
        }
    }

    fn record_set_inst(&mut self, elem_ty: BoundType) {
        let mangled = set_builtins::mangle_set_name(&elem_ty);
        if self.sets_seen.insert(mangled.clone()) {
            self.sets.push(SetInstantiation {
                mangled_name: mangled,
                elem_ty,
            });
        }
    }
}

impl<'a, 'ctx> Walker<'a, 'ctx> {
    fn walk_statement(
        &mut self,
        stmt: &'a Statement<'a>,
        bindings: Option<&TypeBindings>,
        locals: &LocalTypeEnv,
    ) -> Result<(), CompileError> {
        match stmt {
            Statement::VariableDeclaration(var_decl) => {
                for declarator in &var_decl.declarations {
                    if let Some(ann) = &declarator.type_annotation {
                        self.walk_ts_type(&ann.type_annotation, bindings)?;
                    }
                    if let Some(init) = &declarator.init {
                        self.walk_expression(init, bindings, locals)?;
                    }
                }
            }
            Statement::FunctionDeclaration(func) if !func.declare => {
                if func.type_parameters.is_none() {
                    let fn_locals = build_fn_locals(func, self.class_names, bindings);
                    self.walk_function_body(func, bindings, &fn_locals)?;
                }
            }
            Statement::ClassDeclaration(class) => {
                if class.type_parameters.is_none() {
                    self.walk_class_body(class, None)?;
                }
            }
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    match decl {
                        Declaration::FunctionDeclaration(func) if !func.declare => {
                            if func.type_parameters.is_none() {
                                let fn_locals = build_fn_locals(func, self.class_names, bindings);
                                self.walk_function_body(func, bindings, &fn_locals)?;
                            }
                        }
                        Declaration::ClassDeclaration(class) => {
                            if class.type_parameters.is_none() {
                                self.walk_class_body(class, None)?;
                            }
                        }
                        Declaration::VariableDeclaration(var_decl) => {
                            for declarator in &var_decl.declarations {
                                if let Some(init) = &declarator.init {
                                    self.walk_expression(init, bindings, locals)?;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Statement::ExpressionStatement(s) => {
                self.walk_expression(&s.expression, bindings, locals)?;
            }
            Statement::ReturnStatement(ret) => {
                if let Some(arg) = &ret.argument {
                    self.walk_expression(arg, bindings, locals)?;
                }
            }
            Statement::IfStatement(iff) => {
                self.walk_expression(&iff.test, bindings, locals)?;
                self.walk_statement(&iff.consequent, bindings, locals)?;
                if let Some(alt) = &iff.alternate {
                    self.walk_statement(alt, bindings, locals)?;
                }
            }
            Statement::WhileStatement(w) => {
                self.walk_expression(&w.test, bindings, locals)?;
                self.walk_statement(&w.body, bindings, locals)?;
            }
            Statement::DoWhileStatement(w) => {
                self.walk_statement(&w.body, bindings, locals)?;
                self.walk_expression(&w.test, bindings, locals)?;
            }
            Statement::ForStatement(f) => {
                if let Some(init) = &f.init {
                    match init {
                        ForStatementInit::VariableDeclaration(vd) => {
                            for d in &vd.declarations {
                                if let Some(e) = &d.init {
                                    self.walk_expression(e, bindings, locals)?;
                                }
                            }
                        }
                        _ => {
                            if let Some(e) = init.as_expression() {
                                self.walk_expression(e, bindings, locals)?;
                            }
                        }
                    }
                }
                if let Some(t) = &f.test {
                    self.walk_expression(t, bindings, locals)?;
                }
                if let Some(u) = &f.update {
                    self.walk_expression(u, bindings, locals)?;
                }
                self.walk_statement(&f.body, bindings, locals)?;
            }
            Statement::ForOfStatement(f) => {
                self.walk_expression(&f.right, bindings, locals)?;
                self.walk_statement(&f.body, bindings, locals)?;
            }
            Statement::BlockStatement(b) => {
                for s in &b.body {
                    self.walk_statement(s, bindings, locals)?;
                }
            }
            Statement::SwitchStatement(s) => {
                self.walk_expression(&s.discriminant, bindings, locals)?;
                for case in &s.cases {
                    if let Some(t) = &case.test {
                        self.walk_expression(t, bindings, locals)?;
                    }
                    for stmt in &case.consequent {
                        self.walk_statement(stmt, bindings, locals)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn walk_class_body(
        &mut self,
        class: &'a Class<'a>,
        bindings: Option<&TypeBindings>,
    ) -> Result<(), CompileError> {
        // Seed the parent instantiation (if the class extends a generic
        // template). Done before the body so the worklist picks the parent up
        // and monomorphizes it in its own right.
        self.walk_parent_clause(class, bindings)?;
        for element in &class.body.body {
            match element {
                ClassElement::PropertyDefinition(prop) => {
                    if let Some(ann) = &prop.type_annotation {
                        self.walk_ts_type(&ann.type_annotation, bindings)?;
                    }
                    if let Some(val) = &prop.value {
                        let empty = LocalTypeEnv::new();
                        self.walk_expression(val, bindings, &empty)?;
                    }
                }
                ClassElement::MethodDefinition(method) => {
                    for param in &method.value.params.items {
                        if let Some(ann) = &param.type_annotation {
                            self.walk_ts_type(&ann.type_annotation, bindings)?;
                        }
                    }
                    if let Some(ann) = &method.value.return_type {
                        self.walk_ts_type(&ann.type_annotation, bindings)?;
                    }
                    if let Some(body) = &method.value.body {
                        let locals = build_fn_locals(&method.value, self.class_names, bindings);
                        for stmt in &body.statements {
                            self.walk_statement(stmt, bindings, &locals)?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn walk_function_body(
        &mut self,
        func: &'a Function<'a>,
        bindings: Option<&TypeBindings>,
        locals: &LocalTypeEnv,
    ) -> Result<(), CompileError> {
        for param in &func.params.items {
            if let Some(ann) = &param.type_annotation {
                self.walk_ts_type(&ann.type_annotation, bindings)?;
            }
        }
        if let Some(ann) = &func.return_type {
            self.walk_ts_type(&ann.type_annotation, bindings)?;
        }
        if let Some(body) = &func.body {
            for stmt in &body.statements {
                self.walk_statement(stmt, bindings, locals)?;
            }
        }
        Ok(())
    }

    /// If `class` extends a generic template (e.g. `extends Parent<T>`),
    /// resolve its type arguments under the current bindings and seed the
    /// mangled parent instantiation into the collection set. Also walks each
    /// argument so nested generics within the extends clause register.
    fn walk_parent_clause(
        &mut self,
        class: &'a Class<'a>,
        bindings: Option<&TypeBindings>,
    ) -> Result<(), CompileError> {
        let Some(super_class) = &class.super_class else {
            return Ok(());
        };
        let Expression::Identifier(id) = super_class else {
            return Ok(());
        };
        let Some(super_args) = class.super_type_arguments.as_deref() else {
            return Ok(());
        };
        // Walk args unconditionally so any nested generics get registered,
        // even if the parent itself is concrete.
        for arg in &super_args.params {
            self.walk_ts_type(arg, bindings)?;
        }
        let parent_name = id.name.as_str();
        let Some(template) = self.class_templates.get(parent_name) else {
            return Ok(());
        };
        if super_args.params.len() != template.type_params.len() {
            return Err(CompileError::type_err(format!(
                "generic class '{parent_name}' expects {} type argument(s), got {}",
                template.type_params.len(),
                super_args.params.len()
            )));
        }
        let mut concrete = Vec::with_capacity(super_args.params.len());
        let mut tokens = Vec::with_capacity(super_args.params.len());
        for a in &super_args.params {
            let bt = resolve_bound_type(a, self.class_names, bindings)?;
            tokens.push(bt.mangle_token());
            concrete.push(bt);
        }
        let mangled = format!("{parent_name}${}", tokens.join("$"));
        let mut tb = TypeBindings::new();
        for (tp_name, c) in template.type_params.iter().zip(concrete.into_iter()) {
            tb.insert(tp_name.clone(), c);
        }
        self.cls.insert(parent_name, mangled, tb);
        Ok(())
    }

    fn walk_ts_type(
        &mut self,
        ts_type: &TSType<'a>,
        bindings: Option<&TypeBindings>,
    ) -> Result<(), CompileError> {
        if let TSType::TSTypeReference(type_ref) = ts_type {
            let name = type_ref
                .type_name
                .get_identifier_reference()
                .map(|r| r.name.as_str());
            if let (Some(name), Some(args)) = (name, type_ref.type_arguments.as_ref()) {
                if map_builtins::is_map_base(name) {
                    if args.params.len() != map_builtins::MAP_ARITY {
                        return Err(CompileError::type_err(format!(
                            "Map<K, V> expects 2 type arguments, got {}",
                            args.params.len()
                        )));
                    }
                    let key_ty = resolve_bound_type(&args.params[0], self.class_names, bindings)?;
                    let value_ty =
                        resolve_bound_type(&args.params[1], self.class_names, bindings)?;
                    self.record_map_inst(key_ty, value_ty);
                    for arg in &args.params {
                        self.walk_ts_type(arg, bindings)?;
                    }
                    return Ok(());
                }
                if set_builtins::is_set_base(name) {
                    if args.params.len() != set_builtins::SET_ARITY {
                        return Err(CompileError::type_err(format!(
                            "Set<T> expects 1 type argument, got {}",
                            args.params.len()
                        )));
                    }
                    let elem_ty = resolve_bound_type(&args.params[0], self.class_names, bindings)?;
                    self.record_set_inst(elem_ty);
                    for arg in &args.params {
                        self.walk_ts_type(arg, bindings)?;
                    }
                    return Ok(());
                }
                if let Some(template) = self.class_templates.get(name) {
                    if args.params.len() != template.type_params.len() {
                        return Err(CompileError::type_err(format!(
                            "generic class '{name}' expects {} type argument(s), got {}",
                            template.type_params.len(),
                            args.params.len()
                        )));
                    }
                    let mut concrete = Vec::with_capacity(args.params.len());
                    let mut tokens = Vec::with_capacity(args.params.len());
                    for arg in &args.params {
                        let bt = resolve_bound_type(arg, self.class_names, bindings)?;
                        tokens.push(bt.mangle_token());
                        concrete.push(bt);
                    }
                    let mangled = format!("{name}${}", tokens.join("$"));
                    let mut tb = TypeBindings::new();
                    for (tp_name, concrete_ty) in
                        template.type_params.iter().zip(concrete.into_iter())
                    {
                        tb.insert(tp_name.clone(), concrete_ty);
                    }
                    self.cls.insert(name, mangled, tb);
                }
                for arg in &args.params {
                    self.walk_ts_type(arg, bindings)?;
                }
            }
        }
        Ok(())
    }

    fn walk_expression(
        &mut self,
        expr: &Expression<'a>,
        bindings: Option<&TypeBindings>,
        locals: &LocalTypeEnv,
    ) -> Result<(), CompileError> {
        match expr {
            Expression::NewExpression(new_expr) => {
                if let Expression::Identifier(ident) = &new_expr.callee {
                    let name = ident.name.as_str();
                    if map_builtins::is_map_base(name)
                        && let Some(args) = new_expr.type_arguments.as_ref()
                    {
                        if args.params.len() != map_builtins::MAP_ARITY {
                            return Err(CompileError::type_err(format!(
                                "Map<K, V> expects 2 type arguments, got {}",
                                args.params.len()
                            )));
                        }
                        let key_ty =
                            resolve_bound_type(&args.params[0], self.class_names, bindings)?;
                        let value_ty =
                            resolve_bound_type(&args.params[1], self.class_names, bindings)?;
                        self.record_map_inst(key_ty, value_ty);
                        for arg in &args.params {
                            self.walk_ts_type(arg, bindings)?;
                        }
                    }
                    if set_builtins::is_set_base(name)
                        && let Some(args) = new_expr.type_arguments.as_ref()
                    {
                        if args.params.len() != set_builtins::SET_ARITY {
                            return Err(CompileError::type_err(format!(
                                "Set<T> expects 1 type argument, got {}",
                                args.params.len()
                            )));
                        }
                        let elem_ty =
                            resolve_bound_type(&args.params[0], self.class_names, bindings)?;
                        self.record_set_inst(elem_ty);
                        for arg in &args.params {
                            self.walk_ts_type(arg, bindings)?;
                        }
                    }
                    if let Some(template) = self.class_templates.get(name) {
                        if let Some(args) = new_expr.type_arguments.as_ref() {
                            if args.params.len() != template.type_params.len() {
                                return Err(CompileError::type_err(format!(
                                    "generic class '{name}' expects {} type argument(s), got {}",
                                    template.type_params.len(),
                                    args.params.len()
                                )));
                            }
                            let mut concrete = Vec::with_capacity(args.params.len());
                            let mut tokens = Vec::with_capacity(args.params.len());
                            for a in &args.params {
                                let bt = resolve_bound_type(a, self.class_names, bindings)?;
                                tokens.push(bt.mangle_token());
                                concrete.push(bt);
                            }
                            let mangled = format!("{name}${}", tokens.join("$"));
                            let mut tb = TypeBindings::new();
                            for (tp_name, c) in
                                template.type_params.iter().zip(concrete.into_iter())
                            {
                                tb.insert(tp_name.clone(), c);
                            }
                            self.cls.insert(name, mangled, tb);
                        } else {
                            return Err(CompileError::type_err(format!(
                                "`new {name}(...)` requires explicit type arguments; inference for generic constructors is not yet implemented — write `new {name}<T>(...)`"
                            )));
                        }
                    }
                }
                for arg in &new_expr.arguments {
                    if let Some(e) = arg.as_expression() {
                        self.walk_expression(e, bindings, locals)?;
                    }
                }
            }
            Expression::CallExpression(call) => {
                if let Expression::Identifier(ident) = &call.callee {
                    let name = ident.name.as_str();
                    if let Some(template) = self.fn_templates.get(name) {
                        if let Some(args) = call.type_arguments.as_ref() {
                            // Explicit form
                            if args.params.len() != template.type_params.len() {
                                return Err(CompileError::type_err(format!(
                                    "generic function '{name}' expects {} type argument(s), got {}",
                                    template.type_params.len(),
                                    args.params.len()
                                )));
                            }
                            let mut concrete = Vec::with_capacity(args.params.len());
                            let mut tokens = Vec::with_capacity(args.params.len());
                            for a in &args.params {
                                let bt = resolve_bound_type(a, self.class_names, bindings)?;
                                tokens.push(bt.mangle_token());
                                concrete.push(bt);
                            }
                            let mangled = format!("{name}${}", tokens.join("$"));
                            let mut tb = TypeBindings::new();
                            for (tp_name, c) in
                                template.type_params.iter().zip(concrete.into_iter())
                            {
                                tb.insert(tp_name.clone(), c);
                            }
                            self.fns.insert(name, mangled, tb);
                        } else {
                            // Implicit form — try to infer T from argument types.
                            self.try_infer_fn_call(call, name, bindings, locals)?;
                        }
                    }
                }
                // Recurse into arguments even if we already recorded the
                // instantiation, to catch nested generic expressions.
                for arg in &call.arguments {
                    if let Some(e) = arg.as_expression() {
                        self.walk_expression(e, bindings, locals)?;
                    }
                }
                self.walk_expression(&call.callee, bindings, locals)?;
            }
            Expression::BinaryExpression(b) => {
                self.walk_expression(&b.left, bindings, locals)?;
                self.walk_expression(&b.right, bindings, locals)?;
            }
            Expression::LogicalExpression(b) => {
                self.walk_expression(&b.left, bindings, locals)?;
                self.walk_expression(&b.right, bindings, locals)?;
            }
            Expression::ConditionalExpression(c) => {
                self.walk_expression(&c.test, bindings, locals)?;
                self.walk_expression(&c.consequent, bindings, locals)?;
                self.walk_expression(&c.alternate, bindings, locals)?;
            }
            Expression::AssignmentExpression(a) => {
                self.walk_expression(&a.right, bindings, locals)?;
            }
            Expression::ParenthesizedExpression(p) => {
                self.walk_expression(&p.expression, bindings, locals)?;
            }
            Expression::UnaryExpression(u) => {
                self.walk_expression(&u.argument, bindings, locals)?;
            }
            Expression::StaticMemberExpression(m) => {
                self.walk_expression(&m.object, bindings, locals)?;
            }
            Expression::ComputedMemberExpression(m) => {
                self.walk_expression(&m.object, bindings, locals)?;
                self.walk_expression(&m.expression, bindings, locals)?;
            }
            Expression::ArrayExpression(a) => {
                for el in &a.elements {
                    if let Some(e) = el.as_expression() {
                        self.walk_expression(e, bindings, locals)?;
                    }
                }
            }
            Expression::TSAsExpression(as_expr) => {
                self.walk_expression(&as_expr.expression, bindings, locals)?;
                self.walk_ts_type(&as_expr.type_annotation, bindings)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Try to infer the type arguments of a generic function call from its
    /// argument types. On success inserts the monomorphization and records the
    /// call site's mangled name. On partial/no success, silently returns Ok so
    /// codegen reports a clean "undefined function" error.
    fn try_infer_fn_call(
        &mut self,
        call: &CallExpression<'a>,
        name: &str,
        bindings: Option<&TypeBindings>,
        locals: &LocalTypeEnv,
    ) -> Result<(), CompileError> {
        let template = match self.fn_templates.get(name) {
            Some(t) => t,
            None => return Ok(()),
        };

        let mut resolved: Vec<Option<BoundType>> = vec![None; template.type_params.len()];

        for (tp_idx, tp_name) in template.type_params.iter().enumerate() {
            // Find the first parameter slot whose annotation is the bare
            // reference to this type parameter.
            let matching_slot = template
                .ast
                .params
                .items
                .iter()
                .enumerate()
                .find_map(|(i, param)| {
                    let ann = param.type_annotation.as_ref()?;
                    if annotation_is_type_param(&ann.type_annotation, tp_name) {
                        Some(i)
                    } else {
                        None
                    }
                });
            let Some(slot) = matching_slot else {
                // No parameter position mentions this type parameter; we can't
                // infer it from arguments.
                return Ok(());
            };
            let Some(arg) = call.arguments.get(slot) else {
                return Ok(());
            };
            let Some(arg_expr) = arg.as_expression() else {
                return Ok(());
            };
            let Some(bt) = self.infer_arg_bound_type(arg_expr, bindings, locals) else {
                return Ok(());
            };
            resolved[tp_idx] = Some(bt);
        }

        let mut concrete = Vec::with_capacity(template.type_params.len());
        let mut tokens = Vec::with_capacity(template.type_params.len());
        for bt in resolved.into_iter() {
            let bt = match bt {
                Some(b) => b,
                None => return Ok(()),
            };
            tokens.push(bt.mangle_token());
            concrete.push(bt);
        }
        let mangled = format!("{name}${}", tokens.join("$"));
        let mut tb = TypeBindings::new();
        for (tp_name, c) in template.type_params.iter().zip(concrete.into_iter()) {
            tb.insert(tp_name.clone(), c);
        }
        self.fns.insert(name, mangled.clone(), tb);
        self.inferred_sites.insert(call.span.start, mangled);
        Ok(())
    }

    /// Best-effort BoundType inference for a call-site argument. Returns None
    /// when the expression's type can't be determined from the local scope or
    /// visible AST shape (e.g. arbitrary function call result).
    fn infer_arg_bound_type(
        &self,
        expr: &Expression<'a>,
        bindings: Option<&TypeBindings>,
        locals: &LocalTypeEnv,
    ) -> Option<BoundType> {
        match expr {
            Expression::NumericLiteral(lit) => {
                // Matches `emit_numeric_literal`: integer literal without a dot
                // in its raw spelling lowers to i32; everything else to f64.
                if lit.raw.as_ref().is_some_and(|r| r.contains('.')) {
                    Some(BoundType::F64)
                } else if lit.value.fract() == 0.0
                    && lit.value >= i32::MIN as f64
                    && lit.value <= i32::MAX as f64
                {
                    Some(BoundType::I32)
                } else {
                    Some(BoundType::F64)
                }
            }
            Expression::BooleanLiteral(_) => Some(BoundType::Bool),
            Expression::StringLiteral(_) | Expression::TemplateLiteral(_) => Some(BoundType::Str),
            Expression::Identifier(id) => locals.get(id.name.as_str()).cloned(),
            Expression::TSAsExpression(as_expr) => {
                resolve_bound_type(&as_expr.type_annotation, self.class_names, bindings).ok()
            }
            Expression::ParenthesizedExpression(p) => {
                self.infer_arg_bound_type(&p.expression, bindings, locals)
            }
            Expression::NewExpression(new_expr) => {
                let Expression::Identifier(id) = &new_expr.callee else {
                    return None;
                };
                let cls_name = id.name.as_str();
                if let Some(template) = self.class_templates.get(cls_name) {
                    let args = new_expr.type_arguments.as_ref()?;
                    if args.params.len() != template.type_params.len() {
                        return None;
                    }
                    let mut tokens = Vec::with_capacity(args.params.len());
                    for a in &args.params {
                        tokens.push(
                            resolve_bound_type(a, self.class_names, bindings)
                                .ok()?
                                .mangle_token(),
                        );
                    }
                    return Some(BoundType::Class(format!("{cls_name}${}", tokens.join("$"))));
                }
                if self.class_names.contains(cls_name) {
                    Some(BoundType::Class(cls_name.to_string()))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Check whether a TS type annotation is exactly the bare reference to the
/// named type parameter (e.g. `T` matches `"T"`). Does not unfold nested
/// generics — by design, simple inference only binds T when a parameter is
/// declared `x: T` directly.
fn annotation_is_type_param(ts_type: &TSType, tp_name: &str) -> bool {
    if let TSType::TSTypeReference(type_ref) = ts_type {
        let name = type_ref
            .type_name
            .get_identifier_reference()
            .map(|r| r.name.as_str());
        name == Some(tp_name) && type_ref.type_arguments.is_none()
    } else {
        false
    }
}

/// Build a per-function locals env from the parameter list, applying the
/// current bindings scope so that a parameter typed `x: T` where `T` is
/// bound to `i32` lands in the env as `x → I32`.
fn build_fn_locals(
    func: &Function,
    class_names: &HashSet<String>,
    bindings: Option<&TypeBindings>,
) -> LocalTypeEnv {
    let mut env = LocalTypeEnv::new();
    for param in &func.params.items {
        let BindingPattern::BindingIdentifier(id) = &param.pattern else {
            continue;
        };
        let Some(ann) = param.type_annotation.as_ref() else {
            continue;
        };
        let Ok(bt) = resolve_bound_type(&ann.type_annotation, class_names, bindings) else {
            continue;
        };
        env.insert(id.name.as_str().to_string(), bt);
    }
    env
}

