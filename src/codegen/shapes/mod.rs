//! Shape discovery (Phase A.1 of plan-object-literals-tuples.md).
//!
//! Walks the program to collect every structural object type that the user
//! writes — whether named via `type` / `interface` or anonymous via inline
//! `TSTypeLiteral` annotations or `ObjectExpression` literals. Each unique
//! shape (identified by its sorted set of `(field_name, field_type)` pairs)
//! turns into a single entry in the `ShapeRegistry`, which Phase A.2 will
//! register as a synthetic class layout.
//!
//! Two invariants the rest of the pipeline depends on:
//!
//! 1. **Shape identity is unordered.** `{ x: number; y: number }` and
//!    `{ y: number; x: number }` produce the same shape. The fingerprint is
//!    a sort-by-name canonical form of the field set.
//! 2. **Layout is first-declaration-wins.** The `fields` vector on `Shape`
//!    preserves the order from the first site we encountered. A later
//!    anonymous literal with the same field set aliases to the existing
//!    shape; the offsets fixed at first-seen are never rewritten.
//!
//! Discovery runs in two passes over the program:
//!
//! - **Pass 1 — named shapes.** Collects `TSTypeAliasDeclaration` and
//!   `TSInterfaceDeclaration` at the top level (including re-exports).
//!   A name collision with a class / function / another named shape is an
//!   error. Named shapes register first so subsequent anonymous forms with
//!   matching fingerprints alias to them. Interfaces with `extends Parent`
//!   are walked in topological order so parent's fields are available to
//!   prefix into the child at registration time.
//! - **Pass 2 — anonymous shapes.** Walks every annotation and every
//!   `ObjectExpression` in the program. Inline `TSTypeLiteral`s and object
//!   literals with inferable fields contribute new shapes; fingerprints that
//!   already match a registered shape (named or anonymous) are deduplicated
//!   silently.
//!
//! Field-type resolution happens eagerly at discovery using `BoundType`
//! (richer than `WasmType`: keeps `string` distinct from other `i32`s and
//! carries class names for references). Nested shapes in field types are
//! resolved depth-first so the inner shape's mangled name is available for
//! the outer shape's fingerprint.

use std::collections::{HashMap, HashSet};

use oxc_ast::ast::*;
use oxc_span::Span;

use super::generics::{GenericClassTemplate, GenericFnTemplate};
use crate::error::CompileError;
use crate::types::BoundType;

mod fingerprint;
mod walker;

#[cfg(test)]
mod tests;

pub(crate) use fingerprint::fingerprint_of;
use walker::{
    NamedShapeAst, ShapeWalker, collect_generic_shape_templates, collect_named_shape_names,
    collect_named_shapes, topo_sort_named_shapes,
};

/// A resolved field on a discovered shape.
#[derive(Debug, Clone)]
pub struct ShapeField {
    pub name: String,
    pub ty: BoundType,
}

/// Whether a shape was user-named (`type` / `interface`) or anonymous
/// (inline `TSTypeLiteral` or `ObjectExpression`). The distinction matters
/// for diagnostics and for whether the registered synthetic class exposes
/// a user-readable name or a mangled fingerprint name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeKind {
    Named,
    Anonymous,
}

/// A structural object type discovered during Pass 0a-iv.
#[derive(Debug, Clone)]
pub struct Shape {
    pub kind: ShapeKind,
    /// Registered class name: the user's name for `Named` shapes,
    /// the mangled fingerprint name (`__ObjLit$x_f64$y_f64`) for object
    /// literals, `__Tuple$i32$f64` for tuples.
    pub name: String,
    /// Canonical, sort-by-name fingerprint for object shapes; positional
    /// `ty1$ty2$...` fingerprint for tuples. The two namespaces don't collide
    /// because object fingerprints contain `_` (from `name_ty` pairs) while
    /// tuple fingerprints only contain mangle tokens separated by `$`.
    #[allow(dead_code, reason = "surfaced by Phase A.3 diagnostics")]
    pub fingerprint: String,
    /// Fields in first-seen declaration order for object shapes; positional
    /// `_0, _1, _2, ...` for tuples.
    pub fields: Vec<ShapeField>,
    /// `true` for `TSTupleType`-derived shapes (Phase D). Tuple shapes have
    /// positional identity, fields `_N`, and compile `t[N]` (literal index)
    /// to a field load on `_N`. Object shapes have name-set identity.
    pub is_tuple: bool,
}

/// Result of shape discovery. Shapes are stored in insertion order so Phase
/// A.2 can register synthetic classes deterministically.
#[derive(Debug, Default)]
pub struct ShapeRegistry {
    pub shapes: Vec<Shape>,
    pub by_fingerprint: HashMap<String, usize>,
    pub by_name: HashMap<String, usize>,
    /// Span of each inline `TSTypeLiteral` annotation encountered during Pass 2
    /// mapped to the shape index it resolves to. Populated by `walk_ts_type`;
    /// consumed by the annotation-resolution path in `types.rs`.
    pub annotation_shapes: HashMap<Span, usize>,
}

impl ShapeRegistry {
    #[allow(dead_code, reason = "consumed by Phase A.3 annotation resolution")]
    pub fn get_by_name(&self, name: &str) -> Option<&Shape> {
        self.by_name.get(name).map(|&i| &self.shapes[i])
    }

    pub fn get_by_fingerprint(&self, fp: &str) -> Option<&Shape> {
        self.by_fingerprint.get(fp).map(|&i| &self.shapes[i])
    }

    pub fn get_by_annotation(&self, lit: &TSTypeLiteral) -> Option<&Shape> {
        self.annotation_shapes.get(&lit.span).map(|&i| &self.shapes[i])
    }

    /// Look up the tuple shape registered for a `TSTupleType` annotation (Phase D).
    /// Tuple annotations share the `annotation_shapes` side table with object
    /// `TSTypeLiteral`s — spans are unique so the overload-free lookup is safe.
    pub fn get_by_tuple_annotation(&self, tuple: &TSTupleType) -> Option<&Shape> {
        self.annotation_shapes.get(&tuple.span).map(|&i| &self.shapes[i])
    }

    #[allow(dead_code, reason = "consumed by Phase A.3 diagnostics")]
    pub fn len(&self) -> usize {
        self.shapes.len()
    }

    #[allow(dead_code, reason = "consumed by Phase A.3 diagnostics")]
    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }
}

const ANON_SHAPE_PREFIX: &str = "__ObjLit$";
const TUPLE_SHAPE_PREFIX: &str = "__Tuple$";

/// Discover all shapes in `program`. See module docs for the two-pass plan.
///
/// `class_names` must already include concrete classes, generic class
/// monomorphizations, and Map/Set monomorphizations so that shape field
/// types referencing those resolve. `class_templates` and `fn_templates`
/// are consulted when a field type references a generic class by bare name
/// (e.g. `Box<i32>` inside a shape body); the token used in the
/// fingerprint is the mangled instantiation.
pub fn discover_shapes<'a>(
    program: &'a Program<'a>,
    class_names: &HashSet<String>,
    class_templates: &HashMap<String, GenericClassTemplate<'a>>,
    fn_templates: &HashMap<String, GenericFnTemplate<'a>>,
) -> Result<ShapeRegistry, CompileError> {
    // Pre-scan named-shape names so a shape body that references another
    // shape (`type Outer = { inner: Inner }`) resolves the inner reference
    // through `resolve_bound_type` even though `Inner` won't land in the
    // module-level `class_names` until Pass 0a-v. Owning the set lets us
    // augment as new anonymous shapes get mangled names too.
    let mut combined_names: HashSet<String> = class_names.clone();
    for stmt in &program.body {
        collect_named_shape_names(stmt, &mut combined_names);
    }

    let generic_shape_templates = collect_generic_shape_templates(program)?;

    let mut walker = ShapeWalker {
        real_class_names: class_names,
        class_names: combined_names,
        class_templates,
        fn_templates,
        generic_shape_templates,
        registry: ShapeRegistry::default(),
    };

    // Pass 1: named shapes. Collect each declaration plus its (at most one)
    // `extends` parent, topo-sort on those edges, then register in order so
    // `interface Child extends Parent` sees Parent's fields at registration
    // time. Anonymous forms (Pass 2) dedupe into these user-visible names.
    let named = collect_named_shapes(program, walker.real_class_names)?;
    let ordered = topo_sort_named_shapes(&named)?;
    for idx in ordered {
        let (_, parent, ast) = &named[idx];
        match ast {
            NamedShapeAst::Alias(a) => walker.register_named_from_alias(a)?,
            NamedShapeAst::Iface(i) => {
                walker.register_named_from_interface(i, parent.as_deref())?;
            }
        }
    }

    // Pass 2: anonymous shapes and generic shape instantiations. Walk
    // everything for inline `TSTypeLiteral`, `TSTupleType`, `ObjectExpression`,
    // and `TSTypeReference<args>` — the last triggers a monomorphization of
    // any referenced generic shape template.
    for stmt in &program.body {
        walker.walk_statement(stmt)?;
    }

    Ok(walker.registry)
}

/// Walk the program and collect every named / generic shape name
/// (`type Foo = {...}`, `interface Bar {...}`, and `export`-wrapped forms) into
/// a set. Used by `module.rs` to pre-seed `class_names` before
/// `collect_instantiations` runs — Map/Set/generic-class walkers need shape
/// names in the class set so `Map<string, ShapeName>` resolves shape-typed
/// value arguments correctly even though shape registration itself happens
/// later in the pipeline.
pub fn prescan_shape_names(program: &Program<'_>) -> HashSet<String> {
    let mut out = HashSet::new();
    for stmt in &program.body {
        collect_named_shape_names(stmt, &mut out);
    }
    out
}

