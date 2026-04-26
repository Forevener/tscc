//! Union types — registry, discovery, and member-set logic.
//!
//! Phase 1 scope (per `crates/tscc/docs/plan-unions.md`):
//!
//! - Object-shape unions (discriminated unions): `Circle | Square | Rect`.
//! - Same-WasmType literal unions: `'red' | 'green' | 'blue'`, `0 | 1 | 2`.
//!
//! Out of scope here: class unions (`Cat | Dog`, Phase 2), mixed-WasmType
//! primitive unions (`string | number`, deferred indefinitely), and `null` /
//! `undefined` membership (already handled value-only via the sentinel-`0`
//! convention in `expr/binary.rs`).
//!
//! ## Identity & fingerprints
//!
//! A union is identified by its sorted set of canonical member tokens.
//! `Circle | Square` and `Square | Circle` are the same union. Anonymous
//! unions appearing in inline annotations (`function f(x: A | B)`) get a
//! fingerprint-derived synthetic name `__Union$<sorted-tokens>`. Named
//! unions (`type Shape = A | B`) keep the user name and additionally alias
//! the fingerprint so an inline union with the same member set resolves to
//! the named entry.
//!
//! ## Runtime layout
//!
//! Phase 1 adds **no new runtime layout**. A union value is just an i32 — a
//! pointer to one of its variant shapes, or the integer / string-pointer
//! value of a literal member. The discriminator lives in the user-declared
//! field (for shape unions) or in the value itself (for literal unions).

use std::collections::{HashMap, HashSet};

use oxc_ast::ast::*;
use oxc_span::Span;

use super::shapes::{ShapeRegistry, TagValue};
use crate::error::CompileError;
use crate::types::WasmType;

/// One member of a registered union. Member kinds aren't mixed within a
/// single union in Phase 1 — registration rejects e.g. shape + literal in
/// the same union with a clear error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnionMember {
    /// Reference to a registered shape / class layout (the variant's name).
    Shape(String),
    /// A literal-typed member: `'red'`, `1`, `true`. The `TagValue` carries
    /// the literal value; its primitive type drives the union's overall
    /// `WasmType`.
    Literal(TagValue),
}

impl UnionMember {
    /// Stable, fingerprint-safe encoding for set-equivalence comparison.
    /// Shape members use the shape's canonical name; literal members use
    /// `TagValue::canonical()`. The two namespaces don't collide because
    /// every literal token starts with `s_` / `n_` / `i_` / `b_`, and no
    /// shape name uses those prefixes (synthetic shape names are
    /// `__ObjLit$...` / `__Tuple$...`; user names are `[A-Za-z_][...]`).
    pub fn canonical(&self) -> String {
        match self {
            UnionMember::Shape(n) => n.clone(),
            UnionMember::Literal(tv) => tv.canonical(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UnionLayout {
    /// Canonical name: user-given for `type` aliases, mangled
    /// (`__Union$<fingerprint>`) for inline unions.
    pub name: String,
    /// Sort-by-canonical-token fingerprint. Two unions with the same member
    /// set produce the same fingerprint regardless of source order.
    #[allow(dead_code, reason = "kept for parity with ShapeRegistry; consumed by Sub-phase 4+")]
    pub fingerprint: String,
    /// Members in source / first-seen order. Phase 1 invariant: every
    /// member shares the same `WasmType` (validated at registration).
    pub members: Vec<UnionMember>,
    /// The `WasmType` shared by every member. Computed during
    /// `validate_uniform_wasm_ty` and stashed here so the type resolver and
    /// downstream codegen don't need to re-walk the AST. `I32` for shape
    /// unions, class unions, string-literal unions, int-literal unions, and
    /// bool-literal unions; `F64` for pure-`f64`-literal unions
    /// (`0.5 | 1.5 | 2.5`).
    pub wasm_ty: WasmType,
}

impl UnionLayout {
    /// `true` if this union's member set is a (non-strict) subset of
    /// `other`'s. Used to type-check union → union widening assignments.
    pub fn is_subset_of(&self, other: &UnionLayout) -> bool {
        let other_set: HashSet<String> =
            other.members.iter().map(|m| m.canonical()).collect();
        self.members.iter().all(|m| other_set.contains(&m.canonical()))
    }

    /// `true` if `member_canonical` matches one of this union's members.
    pub fn contains(&self, member_canonical: &str) -> bool {
        self.members.iter().any(|m| m.canonical() == member_canonical)
    }
}

#[derive(Debug, Default)]
pub struct UnionRegistry {
    pub unions: Vec<UnionLayout>,
    pub by_name: HashMap<String, usize>,
    pub by_fingerprint: HashMap<String, usize>,
    /// Span → index for inline `TSUnionType` annotations, so downstream
    /// resolvers can map an AST node back to its registered union.
    pub annotation_unions: HashMap<Span, usize>,
}

impl UnionRegistry {
    pub fn get_by_name(&self, name: &str) -> Option<&UnionLayout> {
        self.by_name.get(name).map(|&i| &self.unions[i])
    }

    pub fn get_by_annotation(&self, u: &TSUnionType) -> Option<&UnionLayout> {
        self.annotation_unions.get(&u.span).map(|&i| &self.unions[i])
    }

    #[allow(dead_code, reason = "consumed by Sub-phase 4+ (narrowing fingerprint lookup)")]
    pub fn get_by_fingerprint(&self, fp: &str) -> Option<&UnionLayout> {
        self.by_fingerprint.get(fp).map(|&i| &self.unions[i])
    }
}

const ANON_UNION_PREFIX: &str = "__Union$";

/// Pre-discovery pass: scan top-level `type X = …` aliases for unions whose
/// `WasmType` is **not** `I32` (typically pure-`f64`-literal unions like
/// `type Half = 0.5 | 1.5`) and record `name → WasmType`. Runs in Pass 0a-i.5
/// — *before* generic instantiation collection — so `resolve_bound_type` can
/// produce a correct `BoundType::Union { name, wasm_ty }` for `Box<Half>`-
/// style generic arguments. Cheap: a single pass over top-level statements,
/// inspecting only literal-typed alias bodies.
pub fn collect_named_union_wasm_types<'a>(
    program: &'a Program<'a>,
    out: &mut HashMap<String, crate::types::WasmType>,
) {
    for stmt in &program.body {
        let Statement::TSTypeAliasDeclaration(alias) = stmt else { continue };
        if alias.declare || alias.type_parameters.is_some() {
            continue;
        }
        let TSType::TSUnionType(u) = &alias.type_annotation else {
            continue;
        };
        let Some(wasm_ty) = inline_union_wasm_ty_from_ast(u) else { continue };
        if wasm_ty != crate::types::WasmType::I32 {
            out.insert(alias.id.name.as_str().to_string(), wasm_ty);
        }
    }
}

/// Compute the unified `WasmType` of a `TSUnionType` from the AST alone.
/// Returns `None` for mixed-WasmType members (which `validate_uniform_wasm_ty`
/// will reject later with a user-facing error). Mirrors the per-member rule
/// of `validate_uniform_wasm_ty` but on the parser AST so it can run in
/// pre-discovery passes.
fn inline_union_wasm_ty_from_ast(u: &TSUnionType) -> Option<crate::types::WasmType> {
    use crate::types::WasmType;
    let mut chosen: Option<WasmType> = None;
    for t in &u.types {
        let wt = member_wasm_ty_from_ast(t)?;
        match chosen {
            None => chosen = Some(wt),
            Some(c) if c == wt => {}
            Some(_) => return None,
        }
    }
    chosen
}

fn member_wasm_ty_from_ast(t: &TSType) -> Option<crate::types::WasmType> {
    use crate::types::WasmType;
    match t {
        TSType::TSLiteralType(lit) => {
            let (bt, _) = crate::codegen::shapes::literal_type_to_tag(&lit.literal).ok()?;
            Some(bt.wasm_ty())
        }
        TSType::TSParenthesizedType(p) => member_wasm_ty_from_ast(&p.type_annotation),
        // Type-reference members (shape / class / nested-union alias) all I32.
        TSType::TSTypeReference(_) => Some(WasmType::I32),
        _ => None,
    }
}

/// Run union discovery on a program. Must run **after** shape discovery
/// and class registration so that union member references like
/// `Circle | Square` can resolve to registered shape / class names. Visits
/// every TS-type position (declarations, function param/return, class
/// fields, inline annotations on declarators) and registers each
/// `TSUnionType` node it encounters.
///
/// Idempotent for fingerprint-equivalent occurrences: a second `A | B`
/// inline elsewhere aliases to the first (or to a named union with the same
/// member set).
pub fn discover_unions<'a>(
    program: &'a Program<'a>,
    class_names: &HashSet<String>,
    shape_registry: &ShapeRegistry,
    polymorphic: &HashSet<String>,
) -> Result<UnionRegistry, CompileError> {
    let mut walker = UnionWalker {
        class_names,
        shape_registry,
        polymorphic,
        registry: UnionRegistry::default(),
        _phantom: std::marker::PhantomData,
    };

    // Pass 1: named unions (`type X = A | B`). Register first so inline
    // forms with the same member set alias into the named entry.
    for stmt in &program.body {
        walker.visit_named_union_decl(stmt)?;
    }

    // Pass 2: every type position in the program, depth-first. Inline
    // unions get registered with mangled names.
    for stmt in &program.body {
        walker.visit_statement(stmt)?;
    }

    Ok(walker.registry)
}

struct UnionWalker<'a, 'b> {
    class_names: &'b HashSet<String>,
    shape_registry: &'b ShapeRegistry,
    /// Names of classes that carry a vtable pointer at offset 0 — populated
    /// by `find_polymorphic_classes` in Pass 0a-iia. A class is polymorphic
    /// iff it participates in an inheritance hierarchy (has a parent or is
    /// some other class's parent). Class union members must be polymorphic
    /// so runtime narrowing (`instanceof`) has a discriminator to inspect.
    polymorphic: &'b HashSet<String>,
    registry: UnionRegistry,
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl<'a> UnionWalker<'a, '_> {
    fn visit_named_union_decl(
        &mut self,
        stmt: &'a Statement<'a>,
    ) -> Result<(), CompileError> {
        let alias = match stmt {
            Statement::TSTypeAliasDeclaration(a) => a,
            Statement::ExportNamedDeclaration(e) => {
                if let Some(Declaration::TSTypeAliasDeclaration(a)) = &e.declaration {
                    a
                } else {
                    return Ok(());
                }
            }
            _ => return Ok(()),
        };
        if alias.declare || alias.type_parameters.is_some() {
            return Ok(());
        }
        let TSType::TSUnionType(u) = &alias.type_annotation else {
            return Ok(());
        };
        let name = alias.id.name.as_str().to_string();
        let members = self.resolve_members(&u.types)?;
        let wasm_ty = validate_uniform_wasm_ty(&name, &members)?;
        validate_class_member_polymorphism(
            &name,
            &members,
            self.shape_registry,
            self.polymorphic,
        )?;
        let fingerprint = fingerprint_members(&members);
        self.insert(Some(name), fingerprint, members, wasm_ty, Some(u.span))?;
        Ok(())
    }

    fn visit_statement(&mut self, stmt: &'a Statement<'a>) -> Result<(), CompileError> {
        match stmt {
            Statement::VariableDeclaration(v) => {
                for d in &v.declarations {
                    if let Some(ann) = &d.type_annotation {
                        self.visit_ts_type(&ann.type_annotation)?;
                    }
                }
            }
            Statement::FunctionDeclaration(f) if !f.declare => {
                self.visit_function(f)?;
            }
            Statement::FunctionDeclaration(f) if f.declare => {
                // Ambient declarations still expose param / return types
                // that may contain unions used in user code — register them.
                self.visit_function(f)?;
            }
            Statement::ClassDeclaration(c) => {
                self.visit_class(c)?;
            }
            Statement::TSTypeAliasDeclaration(a) => {
                if !a.declare && a.type_parameters.is_none() {
                    self.visit_ts_type(&a.type_annotation)?;
                }
            }
            Statement::TSInterfaceDeclaration(i) => {
                if !i.declare && i.type_parameters.is_none() {
                    for sig in &i.body.body {
                        if let TSSignature::TSPropertySignature(p) = sig
                            && let Some(ann) = &p.type_annotation
                        {
                            self.visit_ts_type(&ann.type_annotation)?;
                        }
                    }
                }
            }
            Statement::ExportNamedDeclaration(e) => {
                if let Some(decl) = &e.declaration {
                    self.visit_declaration(decl)?;
                }
            }
            Statement::ExportDefaultDeclaration(e) => {
                if let ExportDefaultDeclarationKind::FunctionDeclaration(f) = &e.declaration {
                    self.visit_function(f)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn visit_declaration(
        &mut self,
        decl: &'a Declaration<'a>,
    ) -> Result<(), CompileError> {
        match decl {
            Declaration::VariableDeclaration(v) => {
                for d in &v.declarations {
                    if let Some(ann) = &d.type_annotation {
                        self.visit_ts_type(&ann.type_annotation)?;
                    }
                }
            }
            Declaration::FunctionDeclaration(f) => self.visit_function(f)?,
            Declaration::ClassDeclaration(c) => self.visit_class(c)?,
            Declaration::TSTypeAliasDeclaration(a) => {
                if !a.declare && a.type_parameters.is_none() {
                    self.visit_ts_type(&a.type_annotation)?;
                }
            }
            Declaration::TSInterfaceDeclaration(i) => {
                if !i.declare && i.type_parameters.is_none() {
                    for sig in &i.body.body {
                        if let TSSignature::TSPropertySignature(p) = sig
                            && let Some(ann) = &p.type_annotation
                        {
                            self.visit_ts_type(&ann.type_annotation)?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn visit_function(&mut self, f: &'a Function<'a>) -> Result<(), CompileError> {
        for param in &f.params.items {
            if let Some(ann) = &param.type_annotation {
                self.visit_ts_type(&ann.type_annotation)?;
            }
        }
        if let Some(ret) = &f.return_type {
            self.visit_ts_type(&ret.type_annotation)?;
        }
        if let Some(body) = &f.body {
            for stmt in &body.statements {
                self.visit_statement(stmt)?;
            }
        }
        Ok(())
    }

    fn visit_class(&mut self, c: &'a Class<'a>) -> Result<(), CompileError> {
        for el in &c.body.body {
            match el {
                ClassElement::PropertyDefinition(p) => {
                    if let Some(ann) = &p.type_annotation {
                        self.visit_ts_type(&ann.type_annotation)?;
                    }
                }
                ClassElement::MethodDefinition(m) => {
                    self.visit_function(&m.value)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn visit_ts_type(&mut self, ts_type: &'a TSType<'a>) -> Result<(), CompileError> {
        match ts_type {
            TSType::TSUnionType(u) => {
                // Recurse into members first (a union of unions is rare but
                // possible — flatten by registering inner unions too).
                for t in &u.types {
                    self.visit_ts_type(t)?;
                }
                let members = self.resolve_members(&u.types)?;
                let wasm_ty = validate_uniform_wasm_ty("(inline union)", &members)?;
                validate_class_member_polymorphism(
                    "(inline union)",
                    &members,
                    self.shape_registry,
                    self.polymorphic,
                )?;
                let fingerprint = fingerprint_members(&members);
                self.insert(None, fingerprint, members, wasm_ty, Some(u.span))?;
            }
            TSType::TSArrayType(a) => self.visit_ts_type(&a.element_type)?,
            TSType::TSTupleType(t) => {
                for el in &t.element_types {
                    if let Some(ts) = el.as_ts_type() {
                        self.visit_ts_type(ts)?;
                    }
                }
            }
            TSType::TSTypeLiteral(lit) => {
                for sig in &lit.members {
                    if let TSSignature::TSPropertySignature(p) = sig
                        && let Some(ann) = &p.type_annotation
                    {
                        self.visit_ts_type(&ann.type_annotation)?;
                    }
                }
            }
            TSType::TSTypeReference(r) => {
                if let Some(args) = r.type_arguments.as_ref() {
                    for a in &args.params {
                        self.visit_ts_type(a)?;
                    }
                }
            }
            TSType::TSParenthesizedType(p) => self.visit_ts_type(&p.type_annotation)?,
            TSType::TSFunctionType(f) => {
                for param in &f.params.items {
                    if let Some(ann) = &param.type_annotation {
                        self.visit_ts_type(&ann.type_annotation)?;
                    }
                }
                self.visit_ts_type(&f.return_type.type_annotation)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn resolve_members(
        &self,
        types: &'a [TSType<'a>],
    ) -> Result<Vec<UnionMember>, CompileError> {
        let mut out = Vec::with_capacity(types.len());
        for t in types {
            out.push(self.resolve_member(t)?);
        }
        Ok(out)
    }

    fn resolve_member(&self, t: &'a TSType<'a>) -> Result<UnionMember, CompileError> {
        match t {
            TSType::TSLiteralType(lit) => {
                let (_, tv) = super::shapes::literal_type_to_tag(&lit.literal)?;
                Ok(UnionMember::Literal(tv))
            }
            TSType::TSTypeReference(r) => {
                let name = r
                    .type_name
                    .get_identifier_reference()
                    .map(|id| id.name.as_str().to_string())
                    .ok_or_else(|| {
                        CompileError::type_err(
                            "union member must be a simple type reference, literal, or shape",
                        )
                    })?;
                if self.shape_registry.by_name.contains_key(&name)
                    || self.class_names.contains(&name)
                {
                    return Ok(UnionMember::Shape(name));
                }
                Err(CompileError::type_err(format!(
                    "union member '{name}' is not a known shape, class, or literal type — \
                     Phase 1 unions accept object-shape members and same-WasmType literal members only"
                )))
            }
            TSType::TSParenthesizedType(p) => self.resolve_member(&p.type_annotation),
            _ => Err(CompileError::unsupported(
                "union member kind not supported in Phase 1 (use object shapes or literal types)",
            )),
        }
    }

    fn insert(
        &mut self,
        named: Option<String>,
        fingerprint: String,
        members: Vec<UnionMember>,
        wasm_ty: WasmType,
        annotation_span: Option<Span>,
    ) -> Result<usize, CompileError> {
        if let Some(&existing) = self.registry.by_fingerprint.get(&fingerprint) {
            // Alias the user name into the existing entry if a new name was
            // supplied — same pattern as ShapeRegistry's first-seen-wins.
            if let Some(n) = named
                && !self.registry.by_name.contains_key(&n)
            {
                self.registry.by_name.insert(n, existing);
            }
            // Also alias the synthetic `__Union$<fingerprint>` name when it
            // isn't already the canonical one. Generic-instantiation
            // collection (Pass 0a-ii) runs before `discover_unions` and
            // computes the synthetic name from the AST; later passes then
            // look up `BoundType::Union(__Union$...)` in `by_name` and must
            // find this entry, even when a `type` alias registered first
            // took the canonical slot.
            let synthetic = format!("{ANON_UNION_PREFIX}{fingerprint}");
            self.registry
                .by_name
                .entry(synthetic)
                .or_insert(existing);
            if let Some(span) = annotation_span {
                self.registry.annotation_unions.insert(span, existing);
            }
            return Ok(existing);
        }

        let name = named
            .clone()
            .unwrap_or_else(|| format!("{ANON_UNION_PREFIX}{fingerprint}"));
        if let Some(n) = &named
            && self.registry.by_name.contains_key(n)
        {
            return Err(CompileError::type_err(format!(
                "duplicate union type '{n}' — each `type` name must be unique"
            )));
        }
        let idx = self.registry.unions.len();
        self.registry.unions.push(UnionLayout {
            name: name.clone(),
            fingerprint: fingerprint.clone(),
            members,
            wasm_ty,
        });
        self.registry.by_fingerprint.insert(fingerprint, idx);
        self.registry.by_name.insert(name, idx);
        if let Some(span) = annotation_span {
            self.registry.annotation_unions.insert(span, idx);
        }
        Ok(idx)
    }
}

fn fingerprint_members(members: &[UnionMember]) -> String {
    let mut tokens: Vec<String> = members.iter().map(|m| m.canonical()).collect();
    tokens.sort();
    tokens.dedup();
    tokens.join("$")
}

/// Reject mixed-WasmType members; return the unified `WasmType` shared by
/// every member. Pure-`f64`-literal unions (`0.5 | 1.5`) resolve to
/// `WasmType::F64`; everything else (shape / class / string-literal /
/// int-literal / bool-literal members) resolves to `WasmType::I32`. Mixed
/// pointer + `f64` members are still rejected — that's the deferred
/// tagged-runtime work (`string | number` and friends).
fn validate_uniform_wasm_ty(
    union_label: &str,
    members: &[UnionMember],
) -> Result<WasmType, CompileError> {
    let mut chosen: Option<WasmType> = None;
    for m in members {
        let wt = match m {
            UnionMember::Shape(_) => WasmType::I32,
            UnionMember::Literal(TagValue::Str(_)) => WasmType::I32,
            UnionMember::Literal(TagValue::I32(_)) => WasmType::I32,
            UnionMember::Literal(TagValue::Bool(_)) => WasmType::I32,
            UnionMember::Literal(TagValue::F64(_)) => WasmType::F64,
        };
        match chosen {
            None => chosen = Some(wt),
            Some(c) if c == wt => {}
            Some(_) => {
                return Err(CompileError::type_err(format!(
                    "union '{union_label}' mixes WasmType variants — only \
                     same-WasmType unions are supported. For `string | number` \
                     style unions, use a discriminated wrapper such as \
                     `{{ tag: 'num'; n: f64 }} | {{ tag: 'str'; s: string }}`."
                )));
            }
        }
    }
    Ok(chosen.unwrap_or(WasmType::I32))
}

/// Reject class union members that aren't polymorphic. Phase 2 class unions
/// discriminate via the vtable pointer at offset 0; non-polymorphic classes
/// don't carry one, so a union of unrelated leaf classes is undecidable at
/// runtime. Shapes are skipped — they discriminate via a user-declared
/// `kind` field, not a vtable. Mixed shape + class unions are fine as long
/// as every class member is polymorphic.
fn validate_class_member_polymorphism(
    union_label: &str,
    members: &[UnionMember],
    shape_registry: &ShapeRegistry,
    polymorphic: &HashSet<String>,
) -> Result<(), CompileError> {
    for m in members {
        let UnionMember::Shape(name) = m else { continue };
        if shape_registry.by_name.contains_key(name) {
            continue;
        }
        if polymorphic.contains(name) {
            continue;
        }
        return Err(CompileError::type_err(format!(
            "class union '{union_label}' contains non-polymorphic class '{name}' — \
             class union members must participate in an inheritance hierarchy so \
             every value carries a vtable pointer for runtime narrowing. Add a \
             common base class (e.g. `class Animal {{}}` with \
             `class {name} extends Animal {{}}`) and the gate is satisfied."
        )));
    }
    Ok(())
}
