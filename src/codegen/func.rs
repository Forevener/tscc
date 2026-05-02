use std::collections::{HashMap, HashSet};

use oxc_ast::ast::*;
use wasm_encoder::{Function, Instruction, ValType};

use crate::error::{self, CompileError};
use crate::types::{self, ClosureSig, TypeBindings, WasmType};

use super::module::ModuleContext;
use super::unions::UnionMember;

/// One emit slot in a function body. Most of tscc pushes `Instruction` values
/// one at a time; the splicer (L_splice) pastes a pre-rewritten helper body as
/// a single `RawBytes` chunk to avoid a wasmparser→wasm_encoder op-conversion
/// table that would grow unboundedly with every helper.
pub enum EmittedChunk {
    Instruction(Instruction<'static>),
    RawBytes(Vec<u8>),
}

pub struct FuncContext<'a> {
    pub module_ctx: &'a ModuleContext,
    pub locals: HashMap<String, (u32, WasmType)>,
    pub emitted: Vec<EmittedChunk>,
    local_types: Vec<ValType>,
    param_count: u32,
    pub(crate) return_type: WasmType,
    /// Stack of (break_depth, continue_depth) for loops
    pub(crate) loop_stack: Vec<LoopLabels>,
    /// Current WASM block nesting depth
    pub(crate) block_depth: u32,
    /// If inside a method, the class name (for `this` resolution)
    pub this_class: Option<String>,
    /// Track which local variables hold class instances: var_name -> class_name
    pub local_class_types: HashMap<String, String>,
    /// Track which local variables hold arrays: var_name -> element WasmType
    pub local_array_elem_types: HashMap<String, WasmType>,
    /// Track which local array variables hold class-typed elements: var_name -> class_name
    pub local_array_elem_classes: HashMap<String, String>,
    /// Source text for error location reporting
    pub source: &'a str,
    /// Variables declared with `const` — assignment to these is a compile error
    pub const_locals: HashSet<String>,
    /// Track which local variables hold closures: var_name -> closure signature
    pub local_closure_sigs: HashMap<String, ClosureSig>,
    /// Variables that need boxing (captured by closure AND mutated) — stored via arena pointer
    pub boxed_vars: HashSet<String>,
    /// Original types of boxed variables (since their local holds an i32 pointer)
    pub boxed_var_types: HashMap<String, WasmType>,
    /// Track which local variables hold string pointers
    pub local_string_vars: HashSet<String>,
    /// Source map: (instruction_index, source_byte_offset) for DWARF debug info
    pub source_map: Vec<(usize, u32)>,
    /// When Some(idx), method calls that would normally re-evaluate their
    /// receiver expression use `LocalGet(idx)` instead. Used by optional-call
    /// codegen (`obj?.m()`) to null-check a receiver without double evaluation.
    pub method_receiver_override: Option<u32>,
    /// Type-parameter bindings for the surrounding monomorphized class or
    /// function. When a field/param/return annotation mentions a name present
    /// in this map, the binding substitutes for the annotation during type
    /// resolution. `None` for non-generic code.
    pub type_bindings: Option<TypeBindings>,
    /// If the enclosing function/method has a declared class-typed return, the
    /// class name. Consumed by `emit_return` to thread an expected-type hint
    /// into `ObjectExpression` return arguments so a `{...}` literal can
    /// resolve against the declared shape.
    pub return_class: Option<String>,
    /// Narrowing refinement environment. Owned by `stmt::emit_if` /
    /// `expr::emit_conditional` (and Sub-phase 1.5.3's `emit_switch`),
    /// which call `enter_refinement_scope` / `leave_refinement_scope`
    /// around branch bodies. Member access, coerce, and class resolution
    /// read it via `current_class_of` (single-variant fast path) and
    /// `current_refinement_of` (full `Refinement` for `Subunion` /
    /// `Never` cases) so guarded branches see the refined effective
    /// type.
    pub(crate) refinement_env: RefinementEnv,
    /// Active `for..of` cleanup frames for user-defined iterables whose
    /// iterator class declared `return()`. Pushed at the top of
    /// `emit_for_of_user_iterable`, popped at the bottom. Read by
    /// `emit_return` to call `__it.return()` on each active frame
    /// (innermost first, mirroring spec) before the wasm `Return`.
    /// Empty in the common case — zero overhead when no iterable
    /// declares `return()`.
    pub(crate) for_of_cleanups: Vec<ForOfCleanup>,
}

pub(crate) struct LoopLabels {
    pub(crate) break_depth: u32,
    pub(crate) continue_depth: u32,
}

/// One active iterator that needs cleanup on early function-return. The
/// `break`-cleanup case is handled inline by the loop's block structure
/// (no stack consultation needed); this stack only exists so `emit_return`
/// can run cleanup for outer iterables when nested inside the loop body.
#[derive(Debug, Clone)]
pub(crate) struct ForOfCleanup {
    /// Local index holding the iterator pointer.
    pub iter_local: u32,
    /// Static class name of the iterator — drives the parent-chain method
    /// lookup for `return`, including polymorphic vs monomorphic dispatch.
    pub iter_class: String,
}

impl<'a> FuncContext<'a> {
    pub fn new(
        module_ctx: &'a ModuleContext,
        params: &[(String, WasmType)],
        return_type: WasmType,
        source: &'a str,
    ) -> Self {
        let mut locals = HashMap::new();
        for (i, (name, ty)) in params.iter().enumerate() {
            locals.insert(name.clone(), (i as u32, *ty));
        }
        FuncContext {
            module_ctx,
            locals,
            emitted: Vec::new(),
            local_types: Vec::new(),
            param_count: params.len() as u32,
            return_type,
            loop_stack: Vec::new(),
            block_depth: 0,
            this_class: None,
            local_class_types: HashMap::new(),
            local_array_elem_types: HashMap::new(),
            local_array_elem_classes: HashMap::new(),
            source,
            const_locals: HashSet::new(),
            local_closure_sigs: HashMap::new(),
            boxed_vars: HashSet::new(),
            boxed_var_types: HashMap::new(),
            local_string_vars: HashSet::new(),
            source_map: Vec::new(),
            method_receiver_override: None,
            type_bindings: None,
            return_class: None,
            refinement_env: RefinementEnv::default(),
            for_of_cleanups: Vec::new(),
        }
    }

    /// Push a fresh refinement layer. Every call must be balanced by a
    /// later `leave_refinement_scope` on the same code path.
    pub(crate) fn enter_refinement_scope(&mut self) {
        self.refinement_env.enter_scope();
    }

    /// Pop the topmost refinement layer, restoring the snapshot taken by
    /// the matching `enter_refinement_scope`.
    pub(crate) fn leave_refinement_scope(&mut self) {
        self.refinement_env.leave_scope();
    }

    /// Install a refinement on the current scope. The recognizer
    /// (`recognize_narrowing_facts`) feeds facts here from `if` / `else`
    /// / ternary lifecycles, plus `switch` clauses (Sub-phase 3).
    pub(crate) fn refine_local(&mut self, name: &str, refinement: Refinement) {
        self.refinement_env.refine(name, refinement);
    }

    /// Effective single-class name for `name`. Returns `Some(c)` when the
    /// refinement is `Refinement::Class(c)`; otherwise falls back to the
    /// declared class / union name in `local_class_types`. `Subunion` and
    /// `Never` refinements **deliberately do not** collapse to a class
    /// here — callers that need the un-narrowed union name to look up in
    /// the registry get it via the fallback, while consumers that care
    /// about the refinement (member access, coerce) consult
    /// `current_refinement_of` directly.
    pub(crate) fn current_class_of(&self, name: &str) -> Option<&str> {
        if let Some(Refinement::Class(c)) = self.refinement_env.refined_of(name) {
            return Some(c.as_str());
        }
        self.local_class_types.get(name).map(String::as_str)
    }

    /// Full refinement for `name` (any variant), or `None` if the local
    /// is unrefined. Member access and coerce read this to handle
    /// `Subunion` / `Never` cases that `current_class_of` deliberately
    /// hides.
    pub(crate) fn current_refinement_of(&self, name: &str) -> Option<&Refinement> {
        self.refinement_env.refined_of(name)
    }

    /// Recognize narrowing facts implied by `test` in condition position.
    /// Returns `(positive, negative)` — facts that hold in the if-true
    /// branch and the else branch respectively.
    ///
    /// Matches two shapes, both in either operand order:
    ///   - `x.field === LITERAL` (discriminator predicate): `x` is a
    ///     local of union type, `field` is a discriminator on each
    ///     variant (its `tag_value` decides membership). Positive fact
    ///     fires when exactly one variant matches.
    ///   - `x === LITERAL` (literal-union predicate): `x` is a local of
    ///     union type with literal members. Positive produces no fact in
    ///     Phase 1 (there is no `Refinement::Literal` for "just this
    ///     literal" yet — see `Refinement` doc).
    ///
    /// Negative facts (Sub-phase 1.5.1) are built by
    /// [`build_negative_refinement`] from the surviving member set: 0 →
    /// `Never`, 1 shape → `Class`, ≥2 → `Subunion`, 1 literal → no fact.
    ///
    /// `!==` and `!=` swap positive / negative. `==` and `!=` are
    /// treated like `===` / `!==` (tscc's static subset has no coercion
    /// semantics). Unrecognized test shapes return `(empty, empty)`;
    /// codegen still emits the comparison correctly since the recognizer
    /// doesn't gate emission, only refinements.
    pub(crate) fn recognize_narrowing_facts(
        &self,
        test: &Expression<'a>,
    ) -> (Vec<NarrowingFact>, Vec<NarrowingFact>) {
        let bin = match peel_parens(test) {
            Expression::BinaryExpression(b) => b,
            _ => return (Vec::new(), Vec::new()),
        };

        // Phase 2 sub-phase 2 — `instanceof` predicate. Operand order is
        // fixed (`x instanceof Class`); no literal-vs-side flip needed.
        if bin.operator == BinaryOperator::Instanceof {
            return match_instanceof_pair(self, &bin.left, &bin.right)
                .unwrap_or_else(|| (Vec::new(), Vec::new()));
        }

        let negate = match bin.operator {
            BinaryOperator::StrictEquality | BinaryOperator::Equality => false,
            BinaryOperator::StrictInequality | BinaryOperator::Inequality => true,
            _ => return (Vec::new(), Vec::new()),
        };

        // Try both operand orders: either side may be the literal.
        let facts = match_eq_pair(self, &bin.left, &bin.right)
            .or_else(|| match_eq_pair(self, &bin.right, &bin.left));
        let Some((positive, negative)) = facts else {
            return (Vec::new(), Vec::new());
        };
        if negate {
            (negative, positive)
        } else {
            (positive, negative)
        }
    }

    /// Sub-phase 1.5.3 — switch-statement narrowing.
    ///
    /// Given a switch's discriminant and the full ordered case list, return
    /// per-case positive facts and the cumulative negative facts to install
    /// for the `default` body. Each case body sees the same positive
    /// refinement that an `if (disc === case_lit)` test would produce; the
    /// default body sees the original union minus every literal matched by
    /// a prior case. Cases that don't recognize a literal test contribute
    /// no positive fact and don't trim the cumulative negative — the
    /// emitter still generates code for them, just without narrowing.
    ///
    /// Recognized discriminant shapes match the if-recognizer:
    ///   - `x.field` (discriminator predicate over a shape union)
    ///   - `x` (literal-union value)
    ///
    /// Anything else returns an all-empty `SwitchNarrowing`. Fall-through
    /// is intentionally NOT modeled — each case body's refinement is its
    /// own positive (matching AssemblyScript's switch semantics rather
    /// than full TS).
    pub(crate) fn recognize_switch_facts(
        &self,
        discriminant: &Expression<'a>,
        cases: &[&SwitchCase<'a>],
    ) -> SwitchNarrowing {
        let empty = || SwitchNarrowing {
            case_facts: cases.iter().map(|_| Vec::new()).collect(),
            default_facts: Vec::new(),
        };

        // Resolve the local being narrowed and (for shape-discriminator
        // switches) the field name. Either operand shape mirrors the
        // if-recognizer; anything else falls through to "no narrowing".
        let (local_name, field_name): (String, Option<String>) = match peel_parens(discriminant) {
            Expression::StaticMemberExpression(member) => {
                let Expression::Identifier(ident) = peel_parens(&member.object) else {
                    return empty();
                };
                (
                    ident.name.as_str().to_string(),
                    Some(member.property.name.as_str().to_string()),
                )
            }
            Expression::Identifier(ident) => (ident.name.as_str().to_string(), None),
            _ => return empty(),
        };

        let Some(active_members) = active_member_set(self, &local_name) else {
            return empty();
        };

        let mut matched_shapes: Vec<String> = Vec::new();
        let mut matched_literals: Vec<crate::codegen::shapes::TagValue> = Vec::new();
        let mut any_non_shape_unmatched = false;
        let mut case_facts: Vec<Vec<NarrowingFact>> = Vec::with_capacity(cases.len());

        for case in cases {
            let Some(test) = case.test.as_ref() else {
                // Default — facts come from the cumulative negative below.
                case_facts.push(Vec::new());
                continue;
            };
            let Some(tv) = crate::codegen::expr::object::expr_to_tag_value(test) else {
                case_facts.push(Vec::new());
                continue;
            };

            let mut facts = Vec::new();
            if let Some(field_name) = field_name.as_deref() {
                // Shape-discriminator switch — each case may match one or
                // more shape members whose `field_name` carries this tag.
                let mut matched_in_case: Vec<String> = Vec::new();
                for m in &active_members {
                    match m {
                        UnionMember::Shape(shape_name) => {
                            let Some(shape) =
                                self.module_ctx.shape_registry.get_by_name(shape_name)
                            else {
                                continue;
                            };
                            let Some(field) =
                                shape.fields.iter().find(|f| f.name == field_name)
                            else {
                                continue;
                            };
                            if field.tag_value.as_ref() == Some(&tv)
                                && !matched_shapes.contains(shape_name)
                                && !matched_in_case.contains(shape_name)
                            {
                                matched_in_case.push(shape_name.clone());
                            }
                        }
                        UnionMember::Literal(_) => {
                            // A literal member can't satisfy a `x.field`
                            // predicate; same conservative bail as the
                            // if-recognizer's `any_non_shape_unmatched`.
                            any_non_shape_unmatched = true;
                        }
                    }
                }
                if matched_in_case.len() == 1 {
                    facts.push(NarrowingFact {
                        local_name: local_name.clone(),
                        refined: Refinement::Class(matched_in_case[0].clone()),
                    });
                }
                matched_shapes.extend(matched_in_case);
            } else {
                // Literal-union switch — each case may pull a literal
                // member out of the union. Phase 1 has no
                // `Refinement::Literal`, so positive facts stay empty;
                // tracking matched literals still drives the default's
                // cumulative negative.
                let mut hit = false;
                for m in &active_members {
                    if let UnionMember::Literal(lit) = m
                        && lit == &tv
                        && !matched_literals.iter().any(|m| m == &tv)
                    {
                        matched_literals.push(tv.clone());
                        hit = true;
                        break;
                    }
                }
                let _ = hit;
            }
            case_facts.push(facts);
        }

        // Cumulative default: original active set minus every shape /
        // literal a prior case matched. Reuses `build_negative_refinement`
        // so the N-arithmetic (0 → Never, 1 shape → Class, ≥2 → Subunion,
        // singleton-literal-only → no fact) stays exactly aligned with the
        // if-recognizer.
        let unmatched_shapes: Vec<String> = active_members
            .iter()
            .filter_map(|m| match m {
                UnionMember::Shape(n) if !matched_shapes.contains(n) => Some(n.clone()),
                _ => None,
            })
            .collect();
        let unmatched_literals: Vec<crate::codegen::shapes::TagValue> = active_members
            .iter()
            .filter_map(|m| match m {
                UnionMember::Literal(lit) if !matched_literals.iter().any(|x| x == lit) => {
                    Some(lit.clone())
                }
                _ => None,
            })
            .collect();
        let default_facts = match build_negative_refinement(
            &unmatched_shapes,
            &unmatched_literals,
            any_non_shape_unmatched,
        ) {
            Some(refined) => vec![NarrowingFact {
                local_name,
                refined,
            }],
            None => Vec::new(),
        };

        SwitchNarrowing {
            case_facts,
            default_facts,
        }
    }

    pub fn push(&mut self, inst: Instruction<'static>) {
        self.emitted.push(EmittedChunk::Instruction(inst));
    }

    /// Append pre-encoded opcode bytes as a single emit slot. Used by the
    /// L_splice splicer to paste a rewritten helper body without round-
    /// tripping every operator through the `Instruction` enum.
    pub fn push_raw_bytes(&mut self, bytes: Vec<u8>) {
        self.emitted.push(EmittedChunk::RawBytes(bytes));
    }

    /// Record a source location for the next chunk to be emitted. A raw-byte
    /// chunk maps to one source position for its entire byte range — fine,
    /// since the bytes come from a precompiled helper with no TS source.
    pub fn mark_loc(&mut self, source_offset: u32) {
        self.source_map.push((self.emitted.len(), source_offset));
    }

    pub fn alloc_local(&mut self, ty: WasmType) -> u32 {
        let vt = ty.to_val_type().unwrap_or(ValType::I32);
        let idx = self.param_count + self.local_types.len() as u32;
        self.local_types.push(vt);
        idx
    }

    pub fn declare_local(&mut self, name: &str, ty: WasmType) -> u32 {
        let idx = self.alloc_local(ty);
        self.locals.insert(name.to_string(), (idx, ty));
        idx
    }

    /// Emit arena allocation: pushes `size` (i32) on stack, returns pointer in a new local.
    /// If __arena_alloc is registered (overflow checking enabled), calls it.
    /// Otherwise, does inline bump (original behavior).
    pub fn emit_arena_alloc_to_local(&mut self, size_on_stack: bool) -> Result<u32, CompileError> {
        let arena_idx = self
            .module_ctx
            .arena_ptr_global
            .ok_or_else(|| CompileError::codegen("arena not initialized"))?;
        let ptr_local = self.alloc_local(WasmType::I32);

        if let Some(alloc_idx) = self.module_ctx.arena_alloc_func {
            // size is already on stack
            if !size_on_stack {
                return Err(CompileError::codegen(
                    "emit_arena_alloc_to_local requires size on stack",
                ));
            }
            self.push(Instruction::Call(alloc_idx));
            self.push(Instruction::LocalSet(ptr_local));
        } else {
            // Inline bump: ptr = arena_ptr; arena_ptr += size
            if size_on_stack {
                let size_local = self.alloc_local(WasmType::I32);
                self.push(Instruction::LocalSet(size_local));
                self.push(Instruction::GlobalGet(arena_idx));
                self.push(Instruction::LocalSet(ptr_local));
                self.push(Instruction::GlobalGet(arena_idx));
                self.push(Instruction::LocalGet(size_local));
                self.push(Instruction::I32Add);
                self.push(Instruction::GlobalSet(arena_idx));
            } else {
                return Err(CompileError::codegen(
                    "emit_arena_alloc_to_local requires size on stack",
                ));
            }
        }
        Ok(ptr_local)
    }

    /// Attach a source location to an error using a byte offset (from oxc Span).
    pub fn locate(&self, err: CompileError, offset: u32) -> CompileError {
        err.with_loc(error::offset_to_loc(self.source, offset))
    }

    /// Finish the function, returning the encoded WASM function and a source map.
    /// Source map entries are (byte_offset_in_func_body, source_byte_offset).
    pub fn finish(self) -> (Function, Vec<(u32, u32)>) {
        let local_groups: Vec<(u32, ValType)> =
            self.local_types.iter().map(|vt| (1, *vt)).collect();
        let mut func = Function::new(local_groups);

        // Track byte offset of the start of each chunk within the function body
        let mut chunk_byte_offsets: Vec<u32> = Vec::with_capacity(self.emitted.len());
        for chunk in &self.emitted {
            chunk_byte_offsets.push(func.byte_len() as u32);
            match chunk {
                EmittedChunk::Instruction(inst) => {
                    func.instruction(inst);
                }
                EmittedChunk::RawBytes(bytes) => {
                    func.raw(bytes.iter().copied());
                }
            }
        }
        func.instruction(&Instruction::End);

        // Convert source_map from chunk indices to byte offsets
        let byte_source_map: Vec<(u32, u32)> = self
            .source_map
            .iter()
            .filter_map(|&(chunk_idx, src_offset)| {
                chunk_byte_offsets
                    .get(chunk_idx)
                    .map(|&byte_off| (byte_off, src_offset))
            })
            .collect();

        (func, byte_source_map)
    }

    /// Infer the type of an expression without emitting WASM code.
    /// Used for type inference when no annotation is provided.
    pub fn infer_init_type(
        &self,
        expr: &Expression<'a>,
    ) -> Result<(WasmType, Option<String>), CompileError> {
        match expr {
            Expression::NumericLiteral(lit) => {
                if lit.raw.as_ref().is_some_and(|r| r.contains('.')) || lit.value.fract() != 0.0 {
                    Ok((WasmType::F64, None))
                } else {
                    Ok((WasmType::I32, None))
                }
            }
            Expression::BooleanLiteral(_) => Ok((WasmType::I32, None)),
            Expression::StringLiteral(_) => Ok((WasmType::I32, None)),
            Expression::NullLiteral(_) => Err(CompileError::type_err(
                "cannot infer type from null — add a type annotation",
            )),
            Expression::Identifier(ident) => {
                let name = ident.name.as_str();
                if let Some(&(_, ty)) = self.locals.get(name) {
                    let class = self.local_class_types.get(name).cloned();
                    Ok((ty, class))
                } else if let Some(&(_, ty)) = self.module_ctx.globals.get(name) {
                    let class = self.module_ctx.var_class_types.get(name).cloned();
                    Ok((ty, class))
                } else {
                    Err(CompileError::type_err(format!(
                        "cannot infer type from undefined variable '{name}'"
                    )))
                }
            }
            Expression::NewExpression(new_expr) => {
                if let Expression::Identifier(ident) = &new_expr.callee {
                    let class_name = ident.name.as_str();
                    if class_name == "Array" {
                        // Array<T> → i32, but we need the element type from type params
                        // For now, require annotation for arrays
                        return Err(CompileError::type_err(
                            "Array variables require a type annotation: Array<T>",
                        ));
                    }
                    if self.module_ctx.class_names.contains(class_name) {
                        return Ok((WasmType::I32, Some(class_name.to_string())));
                    }
                }
                Ok((WasmType::I32, None))
            }
            Expression::CallExpression(call) => {
                if let Expression::Identifier(ident) = &call.callee {
                    let name = ident.name.as_str();
                    // Type cast functions
                    if name == "f64" {
                        return Ok((WasmType::F64, None));
                    }
                    if name == "i32" {
                        return Ok((WasmType::I32, None));
                    }
                    // Look up function return type
                    if let Some((_, ret_ty)) = self.module_ctx.get_func(name) {
                        let class = self.module_ctx.fn_return_classes.get(name).cloned();
                        return Ok((ret_ty, class));
                    }
                    // Look up closure variable return type
                    if let Some(sig) = self.local_closure_sigs.get(name) {
                        return Ok((sig.return_type, None));
                    }
                }
                // Check for Math.* calls
                if let Expression::StaticMemberExpression(member) = &call.callee
                    && let Expression::Identifier(ident) = &member.object
                    && ident.name.as_str() == "Math"
                {
                    return Ok((WasmType::F64, None));
                }
                // Array.of / Array.from / Array.isArray — all return i32 (the
                // array methods yield array pointers, isArray is a bool).
                if let Expression::StaticMemberExpression(member) = &call.callee
                    && let Expression::Identifier(ident) = &member.object
                    && ident.name.as_str() == "Array"
                {
                    return Ok((WasmType::I32, None));
                }
                // Object.keys / Object.values / Object.entries — all return
                // a fresh array (i32 pointer). Element-type tracking is the
                // responsibility of `resolve_expr_array_elem` (Array<string>
                // for keys, Array<T> for values, Array<[string, T]> for
                // entries with the tuple class threaded through).
                if let Expression::StaticMemberExpression(member) = &call.callee
                    && let Expression::Identifier(ident) = &member.object
                    && ident.name.as_str() == "Object"
                    && matches!(member.property.name.as_str(), "keys" | "values" | "entries")
                {
                    return Ok((WasmType::I32, None));
                }
                Err(CompileError::type_err(
                    "cannot infer type from this expression — add a type annotation",
                ))
            }
            Expression::BinaryExpression(bin) => {
                // Comparisons always return i32
                match bin.operator {
                    BinaryOperator::LessThan
                    | BinaryOperator::LessEqualThan
                    | BinaryOperator::GreaterThan
                    | BinaryOperator::GreaterEqualThan
                    | BinaryOperator::StrictEquality
                    | BinaryOperator::Equality
                    | BinaryOperator::StrictInequality
                    | BinaryOperator::Inequality => {
                        return Ok((WasmType::I32, None));
                    }
                    _ => {}
                }
                // For arithmetic, infer from left operand
                self.infer_init_type(&bin.left)
            }
            Expression::UnaryExpression(un) => match un.operator {
                UnaryOperator::LogicalNot | UnaryOperator::BitwiseNot => Ok((WasmType::I32, None)),
                _ => self.infer_init_type(&un.argument),
            },
            Expression::ParenthesizedExpression(paren) => self.infer_init_type(&paren.expression),
            Expression::ConditionalExpression(cond) => self.infer_init_type(&cond.consequent),
            Expression::StaticMemberExpression(member) => {
                // e.field — try to resolve class and field type
                let class_name = match self.resolve_expr_class(&member.object) {
                    Ok(name) => name,
                    Err(_) => {
                        return Err(CompileError::type_err(
                            "cannot infer type — add a type annotation",
                        ));
                    }
                };
                if let Some(layout) = self.module_ctx.class_registry.get(&class_name)
                    && let Some(&(_, field_ty)) =
                        layout.field_map.get(member.property.name.as_str())
                {
                    return Ok((field_ty, None));
                }
                Err(CompileError::type_err(
                    "cannot infer type — add a type annotation",
                ))
            }
            Expression::ComputedMemberExpression(member) => {
                // Tuple `t[N]` (literal N) → slot type + class (if any).
                if let Ok(obj_class) = self.resolve_expr_class(&member.object)
                    && let Some(&shape_idx) =
                        self.module_ctx.shape_registry.by_name.get(&obj_class)
                    && self.module_ctx.shape_registry.shapes[shape_idx].is_tuple
                    && let Some(layout) = self.module_ctx.class_registry.get(&obj_class)
                {
                    let Some(idx) = tuple_init_literal_index(&member.expression) else {
                        return Err(CompileError::type_err(format!(
                            "tuple '{obj_class}' requires a literal numeric index; dynamic \
                             `t[i]` is not supported — use `Array<T>` if slots share a type"
                        )));
                    };
                    if idx >= layout.fields.len() {
                        return Err(CompileError::type_err(format!(
                            "tuple index {idx} out of bounds for '{obj_class}' (arity {})",
                            layout.fields.len()
                        )));
                    }
                    let (slot_name, _, slot_ty) = &layout.fields[idx];
                    let class = layout.field_class_types.get(slot_name).cloned();
                    return Ok((*slot_ty, class));
                }
                // Array<T> element: `arr[i]` → element type (+ optional class).
                if let Expression::Identifier(ident) = &member.object {
                    let name = ident.name.as_str();
                    if let Some(&elem_ty) = self.local_array_elem_types.get(name) {
                        let class = self.local_array_elem_classes.get(name).cloned();
                        return Ok((elem_ty, class));
                    }
                }
                Err(CompileError::type_err(
                    "cannot infer type from computed member access — add a type annotation",
                ))
            }
            // Arrow functions are closure pointers (i32)
            Expression::ArrowFunctionExpression(_) => Ok((WasmType::I32, None)),
            // Array literals [a, b, c] are pointers into the arena. Element
            // type tracking happens at the var-decl layer where we have the
            // target name; the local itself is always an i32 handle.
            Expression::ArrayExpression(a) => {
                if a.elements.is_empty() {
                    Err(CompileError::type_err(
                        "cannot infer type of empty array literal — add a type annotation: `let x: number[] = []`",
                    ))
                } else {
                    Ok((WasmType::I32, None))
                }
            }
            // Object literals are arena pointers; the declarator path resolves
            // the class name post-emit from the returned fingerprint.
            Expression::ObjectExpression(_) => Ok((WasmType::I32, None)),
            _ => Err(CompileError::type_err(
                "cannot infer type from this expression — add a type annotation",
            )),
        }
    }

    /// Infer a ClosureSig from an ArrowFunctionExpression's parameter annotations and return type.
    pub fn infer_arrow_sig(&self, arrow: &ArrowFunctionExpression<'a>) -> Option<ClosureSig> {
        let mut param_types = Vec::new();
        for param in &arrow.params.items {
            let ty = if let Some(ann) = &param.type_annotation {
                types::resolve_type_annotation_with_unions(
                    ann,
                    &self.module_ctx.class_names,
                    self.type_bindings.as_ref(),
                    &self.module_ctx.non_i32_union_wasm_types,
                )
                .ok()?
            } else {
                return None;
            };
            param_types.push(ty);
        }
        let return_type = if let Some(ann) = &arrow.return_type {
            types::resolve_type_annotation_with_unions(
                ann,
                &self.module_ctx.class_names,
                self.type_bindings.as_ref(),
                &self.module_ctx.non_i32_union_wasm_types,
            )
            .ok()?
        } else {
            // Try to infer from expression body
            if arrow.expression {
                if let Some(Statement::ExpressionStatement(e)) = arrow.body.statements.first() {
                    self.infer_init_type(&e.expression).ok().map(|(ty, _)| ty)?
                } else {
                    return None;
                }
            } else {
                WasmType::Void
            }
        };
        Some(ClosureSig {
            param_types,
            return_type,
        })
    }
}

// ── Boxing analysis ─────────────────────────────────────────────────
// Identifies variables that need boxing: captured by a closure AND mutated anywhere.

/// Analyze a function body and return the set of variable names that need boxing.
pub fn analyze_boxed_vars(body: &[Statement]) -> HashSet<String> {
    let mut captured = HashSet::new(); // vars referenced inside arrow bodies
    let mut mutated = HashSet::new(); // vars assigned or updated anywhere

    for stmt in body {
        scan_stmt_for_boxing(stmt, &mut captured, &mut mutated, false);
    }

    // Intersection: only box vars that are both captured AND mutated
    captured.intersection(&mutated).cloned().collect()
}

fn scan_stmt_for_boxing<'a>(
    stmt: &Statement<'a>,
    captured: &mut HashSet<String>,
    mutated: &mut HashSet<String>,
    in_arrow: bool,
) {
    match stmt {
        Statement::ExpressionStatement(e) => {
            scan_expr_for_boxing(&e.expression, captured, mutated, in_arrow)
        }
        Statement::ReturnStatement(r) => {
            if let Some(arg) = &r.argument {
                scan_expr_for_boxing(arg, captured, mutated, in_arrow);
            }
        }
        Statement::VariableDeclaration(v) => {
            for decl in &v.declarations {
                if let Some(init) = &decl.init {
                    scan_expr_for_boxing(init, captured, mutated, in_arrow);
                }
            }
        }
        Statement::IfStatement(i) => {
            scan_expr_for_boxing(&i.test, captured, mutated, in_arrow);
            scan_stmt_for_boxing(&i.consequent, captured, mutated, in_arrow);
            if let Some(alt) = &i.alternate {
                scan_stmt_for_boxing(alt, captured, mutated, in_arrow);
            }
        }
        Statement::WhileStatement(w) => {
            scan_expr_for_boxing(&w.test, captured, mutated, in_arrow);
            scan_stmt_for_boxing(&w.body, captured, mutated, in_arrow);
        }
        Statement::DoWhileStatement(d) => {
            scan_stmt_for_boxing(&d.body, captured, mutated, in_arrow);
            scan_expr_for_boxing(&d.test, captured, mutated, in_arrow);
        }
        Statement::ForStatement(f) => {
            if let Some(ForStatementInit::VariableDeclaration(v)) = &f.init {
                for decl in &v.declarations {
                    if let Some(init) = &decl.init {
                        scan_expr_for_boxing(init, captured, mutated, in_arrow);
                    }
                }
            }
            if let Some(test) = &f.test {
                scan_expr_for_boxing(test, captured, mutated, in_arrow);
            }
            if let Some(update) = &f.update {
                scan_expr_for_boxing(update, captured, mutated, in_arrow);
            }
            scan_stmt_for_boxing(&f.body, captured, mutated, in_arrow);
        }
        Statement::ForOfStatement(f) => {
            scan_expr_for_boxing(&f.right, captured, mutated, in_arrow);
            scan_stmt_for_boxing(&f.body, captured, mutated, in_arrow);
        }
        Statement::BlockStatement(b) => {
            for s in &b.body {
                scan_stmt_for_boxing(s, captured, mutated, in_arrow);
            }
        }
        Statement::SwitchStatement(s) => {
            scan_expr_for_boxing(&s.discriminant, captured, mutated, in_arrow);
            for case in &s.cases {
                if let Some(test) = &case.test {
                    scan_expr_for_boxing(test, captured, mutated, in_arrow);
                }
                for s in &case.consequent {
                    scan_stmt_for_boxing(s, captured, mutated, in_arrow);
                }
            }
        }
        _ => {}
    }
}

fn scan_expr_for_boxing<'a>(
    expr: &Expression<'a>,
    captured: &mut HashSet<String>,
    mutated: &mut HashSet<String>,
    in_arrow: bool,
) {
    match expr {
        Expression::Identifier(ident) => {
            if in_arrow {
                captured.insert(ident.name.as_str().to_string());
            }
        }
        Expression::AssignmentExpression(a) => {
            if let AssignmentTarget::AssignmentTargetIdentifier(ident) = &a.left {
                mutated.insert(ident.name.as_str().to_string());
                if in_arrow {
                    captured.insert(ident.name.as_str().to_string());
                }
            }
            scan_expr_for_boxing(&a.right, captured, mutated, in_arrow);
        }
        Expression::UpdateExpression(u) => {
            if let SimpleAssignmentTarget::AssignmentTargetIdentifier(ident) = &u.argument {
                mutated.insert(ident.name.as_str().to_string());
                if in_arrow {
                    captured.insert(ident.name.as_str().to_string());
                }
            }
        }
        Expression::ArrowFunctionExpression(arrow) => {
            // Collect arrow parameter names to exclude from captures
            let param_names: HashSet<String> = arrow
                .params
                .items
                .iter()
                .filter_map(|p| match &p.pattern {
                    BindingPattern::BindingIdentifier(id) => Some(id.name.as_str().to_string()),
                    _ => None,
                })
                .collect();

            // Scan the arrow body with in_arrow=true
            let mut arrow_captured = HashSet::new();
            let mut arrow_mutated = HashSet::new();
            for stmt in &arrow.body.statements {
                scan_stmt_for_boxing(stmt, &mut arrow_captured, &mut arrow_mutated, true);
            }

            // Collect locally declared variables inside the arrow body
            let mut arrow_locals = HashSet::new();
            for stmt in &arrow.body.statements {
                collect_local_decls(stmt, &mut arrow_locals);
            }

            // Remove arrow params AND local declarations — they're not outer-scope variables
            for p in &param_names {
                arrow_captured.remove(p);
                arrow_mutated.remove(p);
            }
            for local in &arrow_locals {
                arrow_captured.remove(local);
                arrow_mutated.remove(local);
            }

            // Merge: only truly outer-scoped captures/mutations propagate up
            captured.extend(arrow_captured);
            mutated.extend(arrow_mutated);
        }
        Expression::BinaryExpression(b) => {
            scan_expr_for_boxing(&b.left, captured, mutated, in_arrow);
            scan_expr_for_boxing(&b.right, captured, mutated, in_arrow);
        }
        Expression::LogicalExpression(l) => {
            scan_expr_for_boxing(&l.left, captured, mutated, in_arrow);
            scan_expr_for_boxing(&l.right, captured, mutated, in_arrow);
        }
        Expression::UnaryExpression(u) => {
            scan_expr_for_boxing(&u.argument, captured, mutated, in_arrow)
        }
        Expression::CallExpression(c) => {
            scan_expr_for_boxing(&c.callee, captured, mutated, in_arrow);
            for arg in &c.arguments {
                scan_expr_for_boxing(arg.to_expression(), captured, mutated, in_arrow);
            }
        }
        Expression::StaticMemberExpression(m) => {
            scan_expr_for_boxing(&m.object, captured, mutated, in_arrow)
        }
        Expression::ComputedMemberExpression(m) => {
            scan_expr_for_boxing(&m.object, captured, mutated, in_arrow);
            scan_expr_for_boxing(&m.expression, captured, mutated, in_arrow);
        }
        Expression::ConditionalExpression(c) => {
            scan_expr_for_boxing(&c.test, captured, mutated, in_arrow);
            scan_expr_for_boxing(&c.consequent, captured, mutated, in_arrow);
            scan_expr_for_boxing(&c.alternate, captured, mutated, in_arrow);
        }
        Expression::ParenthesizedExpression(p) => {
            scan_expr_for_boxing(&p.expression, captured, mutated, in_arrow)
        }
        Expression::NewExpression(n) => {
            for arg in &n.arguments {
                scan_expr_for_boxing(arg.to_expression(), captured, mutated, in_arrow);
            }
        }
        Expression::TSAsExpression(a) => {
            scan_expr_for_boxing(&a.expression, captured, mutated, in_arrow)
        }
        Expression::ChainExpression(c) => match &c.expression {
            ChainElement::StaticMemberExpression(m) => {
                scan_expr_for_boxing(&m.object, captured, mutated, in_arrow)
            }
            ChainElement::ComputedMemberExpression(m) => {
                scan_expr_for_boxing(&m.object, captured, mutated, in_arrow);
                scan_expr_for_boxing(&m.expression, captured, mutated, in_arrow);
            }
            ChainElement::CallExpression(c) => {
                scan_expr_for_boxing(&c.callee, captured, mutated, in_arrow);
                for arg in &c.arguments {
                    scan_expr_for_boxing(arg.to_expression(), captured, mutated, in_arrow);
                }
            }
            _ => {}
        },
        _ => {}
    }
}

/// Collect variable names declared in a statement (for excluding arrow-internal decls).
fn collect_local_decls(stmt: &Statement, out: &mut HashSet<String>) {
    match stmt {
        Statement::VariableDeclaration(v) => {
            for decl in &v.declarations {
                if let BindingPattern::BindingIdentifier(ident) = &decl.id {
                    out.insert(ident.name.as_str().to_string());
                }
            }
        }
        Statement::BlockStatement(b) => {
            for s in &b.body {
                collect_local_decls(s, out);
            }
        }
        Statement::IfStatement(i) => {
            collect_local_decls(&i.consequent, out);
            if let Some(alt) = &i.alternate {
                collect_local_decls(alt, out);
            }
        }
        Statement::ForStatement(f) => {
            if let Some(ForStatementInit::VariableDeclaration(v)) = &f.init {
                for decl in &v.declarations {
                    if let BindingPattern::BindingIdentifier(ident) = &decl.id {
                        out.insert(ident.name.as_str().to_string());
                    }
                }
            }
            collect_local_decls(&f.body, out);
        }
        Statement::WhileStatement(w) => collect_local_decls(&w.body, out),
        Statement::DoWhileStatement(d) => collect_local_decls(&d.body, out),
        _ => {}
    }
}

/// Peel any number of `TSParenthesizedExpression`s from `expr`, returning
/// the innermost non-parenthesized form. Used by the narrowing recognizer so
/// `(x.kind) === 'circle'` and `x.kind === ('circle')` match the same
/// patterns as their unparenthesized counterparts.
pub(crate) fn peel_parens<'a, 'b>(expr: &'b Expression<'a>) -> &'b Expression<'a> {
    let mut cur = expr;
    while let Expression::ParenthesizedExpression(p) = cur {
        cur = &p.expression;
    }
    cur
}

/// Try to recognize `expr_side OP literal_side` as a narrowing predicate and
/// return the `(positive_facts, negative_facts)` it implies. Caller tries
/// this both ways so either operand order matches. Returns `None` when the
/// pair doesn't look like one of the two recognised shapes (`x.field === L`
/// or `x === L`) or when the relevant local isn't union-typed.
fn match_eq_pair<'a>(
    ctx: &FuncContext<'a>,
    expr_side: &Expression<'a>,
    literal_side: &Expression<'a>,
) -> Option<(Vec<NarrowingFact>, Vec<NarrowingFact>)> {
    let tv = crate::codegen::expr::object::expr_to_tag_value(literal_side)?;

    match peel_parens(expr_side) {
        // Sub-phase 5: discriminator predicate — `x.field === LIT`.
        Expression::StaticMemberExpression(member) => {
            let Expression::Identifier(ident) = peel_parens(&member.object) else {
                return None;
            };
            let local = ident.name.as_str();
            // Sub-phase 1.5.1: when the local has already been refined to
            // a sub-union, recognize against the refined member set so
            // nested narrowing composes (an inner predicate sees the
            // outer's leftover members, not the original full union).
            // `Class` and `Never` refinements bail — Class is already a
            // single variant (no further union narrowing), Never is a
            // dead branch (no useful facts).
            let active_members = active_member_set(ctx, local)?;
            let field_name = member.property.name.as_str();

            let mut matched: Vec<String> = Vec::new();
            let mut unmatched_shapes: Vec<String> = Vec::new();
            let mut any_non_shape_unmatched = false;
            for m in &active_members {
                match m {
                    crate::codegen::unions::UnionMember::Shape(shape_name) => {
                        let shape = ctx.module_ctx.shape_registry.get_by_name(shape_name)?;
                        let field = shape.fields.iter().find(|f| f.name == field_name)?;
                        if field.tag_value.as_ref() == Some(&tv) {
                            matched.push(shape_name.clone());
                        } else {
                            unmatched_shapes.push(shape_name.clone());
                        }
                    }
                    // A shape.field predicate against a literal member makes
                    // no sense — bail rather than produce a misleading fact.
                    crate::codegen::unions::UnionMember::Literal(_) => {
                        any_non_shape_unmatched = true;
                    }
                }
            }

            let mut positive = Vec::new();
            let mut negative = Vec::new();
            if matched.len() == 1 {
                positive.push(NarrowingFact {
                    local_name: local.to_string(),
                    refined: Refinement::Class(matched.remove(0)),
                });
            }
            // Negative refinement (Sub-phase 1.5.1): N-arithmetic over the
            // surviving member set. Mixed shape/literal unions fall back to
            // a "shapes-only" negative when no literal can reach this branch
            // (the discriminator predicate is on a shape field, so literal
            // members aren't affected by the matching logic — they always
            // survive to the negative side).
            if let Some(neg) = build_negative_refinement(
                &unmatched_shapes,
                /*surviving_literals=*/ &[],
                any_non_shape_unmatched,
            ) {
                negative.push(NarrowingFact {
                    local_name: local.to_string(),
                    refined: neg,
                });
            }
            Some((positive, negative))
        }
        // Sub-phase 6: literal-union predicate — `x === LIT`.
        Expression::Identifier(ident) => {
            let local = ident.name.as_str();
            let active_members = active_member_set(ctx, local)?;

            // Split members by whether they match the literal. Shape members
            // never match a value-literal (the shape has identity, not a
            // singleton value in Phase 1).
            let mut matched_count = 0usize;
            let mut unmatched_shapes: Vec<String> = Vec::new();
            let mut unmatched_literals: Vec<crate::codegen::shapes::TagValue> = Vec::new();
            for m in &active_members {
                match m {
                    crate::codegen::unions::UnionMember::Literal(lit) => {
                        if lit == &tv {
                            matched_count += 1;
                        } else {
                            unmatched_literals.push(lit.clone());
                        }
                    }
                    crate::codegen::unions::UnionMember::Shape(sn) => {
                        unmatched_shapes.push(sn.clone());
                    }
                }
            }

            // Positive: no class-name refinement — Phase 1 has no `BoundType`
            // for a singleton literal. Future work (see `Refinement` doc) can
            // extend `Refinement::Class` to a `Refinement::Literal(TagValue)`
            // for primitive-union narrowing.
            let positive = Vec::new();
            let mut negative = Vec::new();
            // Negative refinement (Sub-phase 1.5.1): if the literal didn't
            // match anything in the union, the predicate is always false in
            // the true branch and always true in the else branch — no useful
            // refinement either way (caller's predicate is dead code, but
            // that's a separate diagnostic). Otherwise, build the surviving
            // member set and refine.
            if matched_count >= 1
                && let Some(neg) = build_negative_refinement(
                    &unmatched_shapes,
                    &unmatched_literals,
                    /*any_non_shape_unmatched=*/ false,
                )
            {
                negative.push(NarrowingFact {
                    local_name: local.to_string(),
                    refined: neg,
                });
            }
            Some((positive, negative))
        }
        _ => None,
    }
}

/// Active member set for a union-typed local, honoring any active
/// refinement. Returns `None` when no useful narrowing is possible:
/// the local has a `Class` or `Never` refinement (already narrowed past
/// the union, or unreachable), or the local isn't union-typed at all.
/// `Subunion` returns the refined members; un-refined locals return
/// the full member set from the registry.
///
/// Cleanly composes: the recognizer always sees "what variants does the
/// current scope still consider possible," so nested predicates
/// re-narrow against the leftover set rather than the original union.
/// Phase 2 sub-phase 2 — partition a union's active members against an
/// `instanceof RightClass` predicate. Returns `(positive, negative)` facts
/// in the same shape as `match_eq_pair` so the caller can splice them into
/// the recognizer's branch refinements unchanged.
///
/// Matching rule per the plan: a class member matches iff its name equals
/// `RightClass` or `is_subclass_of(member, RightClass)` is true. Shape
/// members and literal members never match — a shape carries no vtable
/// pointer and a literal isn't a class.
///
/// Bails (`None`) when:
/// - the local has no static union type,
/// - the right operand isn't a bare identifier referring to a registered
///   class.
///
/// An empty matched set is *not* a bail — the predicate is statically
/// false, the positive branch becomes `Never` (consumed by exhaustiveness),
/// and the negative branch keeps the original membership.
fn match_instanceof_pair<'a>(
    ctx: &FuncContext<'a>,
    expr_side: &Expression<'a>,
    class_side: &Expression<'a>,
) -> Option<(Vec<NarrowingFact>, Vec<NarrowingFact>)> {
    use crate::codegen::unions::UnionMember;

    let Expression::Identifier(local_id) = peel_parens(expr_side) else {
        return None;
    };
    let local = local_id.name.as_str().to_string();

    let Expression::Identifier(class_id) = peel_parens(class_side) else {
        return None;
    };
    let right_class = class_id.name.as_str().to_string();
    if !ctx
        .module_ctx
        .class_registry
        .classes
        .contains_key(&right_class)
    {
        return None;
    }

    // Active members: a refined local feeds its current member set; a
    // `Class(C)` refinement collapses to a singleton `[C]` so an exhaustive
    // if-chain like `instanceof Cat ; else instanceof Dog ; else
    // assertNever(p)` can drive the second predicate's else branch all the
    // way to `Never`. Without this fallback, the outer `instanceof` already
    // narrows N=2 unions to `Class(Dog)`, and the inner predicate would see
    // `None` from `active_member_set` and emit no facts.
    let active_members = if let Some(m) = active_member_set(ctx, &local) {
        m
    } else if let Some(Refinement::Class(c)) = ctx.current_refinement_of(&local) {
        vec![UnionMember::Shape(c.clone())]
    } else {
        return None;
    };

    let mut matched: Vec<String> = Vec::new();
    let mut unmatched_shapes_or_classes: Vec<String> = Vec::new();
    let mut unmatched_literals: Vec<crate::codegen::shapes::TagValue> = Vec::new();
    for m in &active_members {
        match m {
            UnionMember::Shape(name) => {
                let is_shape = ctx.module_ctx.shape_registry.by_name.contains_key(name);
                let is_match = !is_shape
                    && (name == &right_class
                        || ctx
                            .module_ctx
                            .class_registry
                            .is_subclass_of(name, &right_class));
                if is_match {
                    matched.push(name.clone());
                } else {
                    unmatched_shapes_or_classes.push(name.clone());
                }
            }
            UnionMember::Literal(lit) => {
                unmatched_literals.push(lit.clone());
            }
        }
    }

    let positive_refined = match matched.len() {
        0 => Refinement::Never,
        1 => Refinement::Class(matched.remove(0)),
        _ => {
            let mut members: Vec<UnionMember> =
                matched.into_iter().map(UnionMember::Shape).collect();
            members.sort_by_key(|m| m.canonical());
            members.dedup_by(|a, b| a.canonical() == b.canonical());
            Refinement::Subunion(members)
        }
    };
    let positive = vec![NarrowingFact {
        local_name: local.clone(),
        refined: positive_refined,
    }];

    let negative = match build_negative_refinement(
        &unmatched_shapes_or_classes,
        &unmatched_literals,
        /*any_non_shape_unmatched=*/ false,
    ) {
        Some(refined) => vec![NarrowingFact {
            local_name: local,
            refined,
        }],
        None => Vec::new(),
    };

    Some((positive, negative))
}

fn active_member_set(
    ctx: &FuncContext<'_>,
    local: &str,
) -> Option<Vec<crate::codegen::unions::UnionMember>> {
    match ctx.current_refinement_of(local) {
        Some(Refinement::Subunion(m)) => Some(m.clone()),
        Some(Refinement::Class(_) | Refinement::Never) => None,
        None => {
            let class = ctx.local_class_types.get(local)?;
            ctx.module_ctx
                .union_registry
                .get_by_name(class)
                .map(|u| u.members.clone())
        }
    }
}

/// Compose a `Refinement` for the negative side of an equality predicate
/// from the surviving shape names and literal values. Returns `None` to
/// signal "no useful refinement" — three distinct cases:
///
/// 1. `any_non_shape_unmatched` is set: a `x.f === LIT` predicate ran
///    against a union containing literal members. Literal members have no
///    fields, so the predicate is undefined on them and we can't safely
///    say which side they fall on. Skip refinement (conservative).
/// 2. The surviving set has exactly one literal member and zero shapes:
///    Phase 1 has no `BoundType` for a singleton literal, so we can't
///    install a useful refinement. (Future literal-union work would
///    extend `Refinement::Class` with a `Refinement::Literal(TagValue)`
///    sibling.)
/// 3. Otherwise the result is `Class` (1 shape, 0 literals), `Subunion`
///    (≥2 members), or `Never` (0 members — predicate matched every
///    variant, so the negative branch is unreachable).
fn build_negative_refinement(
    unmatched_shapes: &[String],
    surviving_literals: &[crate::codegen::shapes::TagValue],
    any_non_shape_unmatched: bool,
) -> Option<Refinement> {
    if any_non_shape_unmatched {
        return None;
    }
    let total = unmatched_shapes.len() + surviving_literals.len();
    if total == 0 {
        return Some(Refinement::Never);
    }
    if total == 1 {
        if let [name] = unmatched_shapes {
            return Some(Refinement::Class(name.clone()));
        }
        // Singleton literal — no Phase 1 refinement representation.
        return None;
    }
    let mut members: Vec<crate::codegen::unions::UnionMember> = unmatched_shapes
        .iter()
        .cloned()
        .map(crate::codegen::unions::UnionMember::Shape)
        .chain(
            surviving_literals
                .iter()
                .cloned()
                .map(crate::codegen::unions::UnionMember::Literal),
        )
        .collect();
    members.sort_by_key(|m| m.canonical());
    members.dedup_by(|a, b| a.canonical() == b.canonical());
    Some(Refinement::Subunion(members))
}

/// Extract a non-negative integer literal from a tuple-index expression used
/// in `infer_init_type`. Mirrors the helpers in `class.rs` and `member.rs`;
/// if more sites need it, promote to a shared utility.
fn tuple_init_literal_index(expr: &Expression<'_>) -> Option<usize> {
    match expr {
        Expression::ParenthesizedExpression(p) => tuple_init_literal_index(&p.expression),
        Expression::NumericLiteral(lit) => {
            let v = lit.value;
            if v.fract() != 0.0 || v < 0.0 {
                return None;
            }
            Some(v as usize)
        }
        _ => None,
    }
}

/// One narrowing fact recovered from a control-flow predicate. The
/// `refined` payload is a [`Refinement`] so the recognizer can express
/// the full range: a single variant (`Class`), a multi-member sub-union
/// (`Subunion`), or the unreachable case (`Never`).
#[derive(Debug, Clone)]
pub(crate) struct NarrowingFact {
    /// The local variable being refined (binding name in the source).
    pub local_name: String,
    /// The refinement to install in the active scope.
    pub refined: Refinement,
}

/// Sub-phase 1.5.3 — narrowing facts harvested from a `switch` statement.
/// `case_facts` is parallel to the source case list (one entry per case,
/// including default slots — those entries are empty since the default
/// reads from `default_facts` instead). `default_facts` is the cumulative
/// negative refinement: discriminant minus every literal matched by a
/// prior case.
#[derive(Debug, Clone, Default)]
pub(crate) struct SwitchNarrowing {
    pub case_facts: Vec<Vec<NarrowingFact>>,
    pub default_facts: Vec<NarrowingFact>,
}

/// Compile-time-only override for a local's effective type inside a
/// guarded scope. Refinements **never** materialize in the wasm output
/// and **never** appear in `UnionRegistry` — sub-unions produced here
/// don't need a stable name because they don't escape to the source
/// language.
///
/// The three variants cover everything Phase 1.5 + Phase 2 require
/// without rework: `Class` is the single-variant fast path (Phase 1 + the
/// majority of `instanceof` outcomes); `Subunion` carries an arbitrary
/// member list (mixed shapes / classes / literals — `UnionMember`
/// already discriminates them); `Never` is the empty-set sink that
/// exhaustiveness reduces to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refinement {
    /// Exactly one shape or class variant.
    Class(String),
    /// A multi-member sub-union — at least two members, sorted by
    /// `UnionMember::canonical()` and deduped at construction so
    /// equivalent refinements compare equal.
    Subunion(Vec<UnionMember>),
    /// Unreachable: every variant of the original union has been ruled
    /// out by prior branches. Member access on a `Never`-refined local
    /// is a compile error; assignment to a `: never` slot (Sub-phase 4)
    /// is the only legal use site.
    Never,
}

/// Refinement environment for the narrowing skeleton (Sub-phase 4).
/// Each `enter_scope` snapshots the current refinement map; each
/// `leave_scope` restores the snapshot. `refine` installs an override
/// that lasts until the matching `leave_scope`. Lookups consult the
/// active map only.
///
/// Lifecycle is owned by `stmt::emit_if` and `expr::emit_conditional`:
/// they call `enter_scope` immediately before emitting a branch's body
/// and `leave_scope` immediately after. Push/pop balance is the codegen
/// caller's responsibility.
///
/// The map is keyed by source-name (not wasm `LocalSlot`) because all
/// refinement consumers (member access, coerce) read declared types via
/// `local_class_types`, which is the same key space.
#[derive(Default, Debug)]
pub(crate) struct RefinementEnv {
    refined: HashMap<String, Refinement>,
    saved: Vec<HashMap<String, Refinement>>,
}

impl RefinementEnv {
    pub(crate) fn enter_scope(&mut self) {
        self.saved.push(self.refined.clone());
    }

    /// Restore the snapshot taken by the matching `enter_scope`. If no
    /// snapshot is on the stack (caller bug — every leave must match an
    /// earlier enter) the env resets to empty rather than panicking, so
    /// codegen errors surface as type / runtime failures downstream
    /// rather than as a panic in the test harness.
    pub(crate) fn leave_scope(&mut self) {
        self.refined = self.saved.pop().unwrap_or_default();
    }

    pub(crate) fn refine(&mut self, name: &str, refinement: Refinement) {
        self.refined.insert(name.to_string(), refinement);
    }

    pub(crate) fn refined_of(&self, name: &str) -> Option<&Refinement> {
        self.refined.get(name)
    }

    /// Number of snapshots currently on the stack — i.e. how many
    /// `enter_scope` calls haven't yet been matched by `leave_scope`.
    /// Used by debug-time balance assertions.
    #[cfg(test)]
    pub(crate) fn depth(&self) -> usize {
        self.saved.len()
    }
}

#[cfg(test)]
mod refinement_tests {
    use super::*;
    use crate::codegen::shapes::TagValue;
    use crate::codegen::unions::UnionMember;

    fn cls(n: &str) -> Refinement {
        Refinement::Class(n.to_string())
    }

    #[test]
    fn empty_env_returns_none_and_zero_depth() {
        let env = RefinementEnv::default();
        assert_eq!(env.refined_of("x"), None);
        assert_eq!(env.depth(), 0);
    }

    #[test]
    fn enter_refine_leave_restores_to_empty() {
        let mut env = RefinementEnv::default();
        env.enter_scope();
        env.refine("sh", cls("Circle"));
        assert_eq!(env.refined_of("sh"), Some(&cls("Circle")));
        assert_eq!(env.depth(), 1);
        env.leave_scope();
        assert_eq!(env.refined_of("sh"), None);
        assert_eq!(env.depth(), 0);
    }

    #[test]
    fn nested_scopes_compose_and_pop_in_order() {
        let mut env = RefinementEnv::default();
        env.enter_scope();
        env.refine("a", cls("A1"));
        env.enter_scope();
        env.refine("a", cls("A2")); // shadow inner
        env.refine("b", cls("B1")); // new in inner only
        assert_eq!(env.refined_of("a"), Some(&cls("A2")));
        assert_eq!(env.refined_of("b"), Some(&cls("B1")));
        env.leave_scope();
        // Outer scope restored — `a` reverts, `b` is gone.
        assert_eq!(env.refined_of("a"), Some(&cls("A1")));
        assert_eq!(env.refined_of("b"), None);
        env.leave_scope();
        assert_eq!(env.refined_of("a"), None);
    }

    #[test]
    fn sibling_scopes_do_not_leak_refinements() {
        // Models if/else: positive facts in the true branch must not be
        // visible inside the else branch. Each sibling enter snapshots
        // the same outer state; the leftover from the first branch must
        // not be reused by the second.
        let mut env = RefinementEnv::default();
        env.enter_scope();
        env.refine("sh", cls("Circle"));
        assert_eq!(env.refined_of("sh"), Some(&cls("Circle")));
        env.leave_scope();

        env.enter_scope();
        assert_eq!(env.refined_of("sh"), None);
        env.refine("sh", cls("Square"));
        assert_eq!(env.refined_of("sh"), Some(&cls("Square")));
        env.leave_scope();

        assert_eq!(env.refined_of("sh"), None);
    }

    #[test]
    fn extra_leave_resets_to_empty_without_panicking() {
        // Defensive: a codegen bug that emits an unmatched `leave_scope`
        // must not panic in tests — it just clears the env. The
        // observable effect is "narrowing is lost", which downstream
        // type-checks will surface as a clear error rather than as a
        // runtime crash.
        let mut env = RefinementEnv::default();
        env.refine("sh", cls("Circle"));
        env.leave_scope();
        assert_eq!(env.refined_of("sh"), None);
        assert_eq!(env.depth(), 0);
    }

    #[test]
    fn refinement_variants_round_trip() {
        // All three Refinement variants survive the env's
        // snapshot/restore round trip. Subunion equality respects the
        // canonical-sort invariant: members declared in different orders
        // compare equal iff the canonical token sets match.
        let mut env = RefinementEnv::default();
        let sub = Refinement::Subunion(vec![
            UnionMember::Shape("Square".to_string()),
            UnionMember::Shape("Rect".to_string()),
        ]);
        let lit_sub = Refinement::Subunion(vec![
            UnionMember::Literal(TagValue::Str("red".to_string())),
            UnionMember::Literal(TagValue::Str("green".to_string())),
        ]);
        env.enter_scope();
        env.refine("sh", sub.clone());
        env.refine("color", lit_sub.clone());
        env.refine("dead", Refinement::Never);
        assert_eq!(env.refined_of("sh"), Some(&sub));
        assert_eq!(env.refined_of("color"), Some(&lit_sub));
        assert_eq!(env.refined_of("dead"), Some(&Refinement::Never));
        env.leave_scope();
        assert_eq!(env.refined_of("sh"), None);
    }
}
