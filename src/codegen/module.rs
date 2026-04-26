use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use oxc_ast::ast::*;
use wasm_encoder::ValType;

use super::classes::ClassRegistry;
use crate::ArenaOverflow;
use crate::error::CompileError;
use crate::types::{self, ClosureSig, TypeBindings, WasmType};

use super::func::FuncContext;
use super::wasm_types;

/// Key identifying a unique WASM function signature: (param types, result types).
pub(crate) type TypeSigKey = (Vec<ValType>, Vec<ValType>);

pub struct ModuleContext {
    host_module: String,
    /// Map from function name to (wasm_func_index, return_type)
    pub(crate) func_map: HashMap<String, (u32, WasmType)>,
    /// Map from global name to (wasm_global_index, type)
    pub globals: HashMap<String, (u32, WasmType)>,
    /// Names of globals declared with `let` (mutable in WASM)
    pub mutable_globals: HashSet<String>,
    /// Function type signatures: (params, results)
    pub(crate) type_sigs: Vec<TypeSigKey>,
    /// Index lookup for `type_sigs` (O(1) dedup during pre-codegen registration)
    type_sig_index: HashMap<TypeSigKey, u32>,
    /// Import entries: (module, name, type_index)
    pub(crate) imports: Vec<(String, String, u32)>,
    /// Local function entries
    pub(crate) local_funcs: Vec<FuncDef>,
    next_func_index: u32,
    next_type_index: u32,
    next_global_index: u32,
    /// Ordered init values for globals
    pub global_inits: Vec<GlobalInit>,
    /// Bump pointer for compile-time static data allocation
    pub(crate) static_data_ptr: Cell<u32>,
    /// Class layouts and metadata
    pub class_registry: ClassRegistry,
    /// Set of known class names (for type resolution)
    pub class_names: HashSet<String>,
    /// Named unions whose `WasmType` is **not** `I32` — typically pure-`f64`
    /// literal unions (`type X = 0.5 | 1.5`). Consulted by the type resolver
    /// to override the default-`I32` behaviour for `class_names`-resident
    /// names. Empty in the common case (zero overhead when no `f64` unions
    /// appear in the program).
    pub non_i32_union_wasm_types: HashMap<String, WasmType>,
    /// Index of the __arena_ptr global (if arena is used)
    pub arena_ptr_global: Option<u32>,
    /// Map from "ClassName.methodName" to wasm func index
    pub method_map: HashMap<String, (u32, WasmType)>,
    /// Track variable -> class type name for property access resolution
    /// This is a simple mapping used during codegen.
    pub var_class_types: HashMap<String, String>,
    /// Functions that return closures: func_name -> return ClosureSig
    pub func_return_closure_sigs: HashMap<String, ClosureSig>,
    /// Lifted closure functions collected during codegen (interior mutability for &self codegen pass)
    pub closure_funcs: RefCell<Vec<ClosureFunc>>,
    /// Next table slot index for closures
    next_table_index: Cell<u32>,
    /// Extra type sigs registered during codegen for call_indirect (interior mutability)
    pub(crate) extra_type_sigs: RefCell<Vec<TypeSigKey>>,
    /// Index lookup for `extra_type_sigs` (O(1) dedup during codegen)
    extra_type_sig_index: RefCell<HashMap<TypeSigKey, u32>>,
    /// Static data entries: (offset, bytes) to populate the WASM data section
    pub static_data_entries: RefCell<Vec<(u32, Vec<u8>)>>,
    /// Deduplication cache for string literals: string content → memory offset
    string_literal_offsets: RefCell<HashMap<String, u32>>,
    /// Set of function names that return strings
    pub func_return_strings: HashSet<String>,
    /// Map from "ClassName$methodName" to WASM function table index (for vtable dispatch)
    pub method_table_indices: HashMap<String, u32>,
    /// Arena overflow behavior
    pub arena_overflow: ArenaOverflow,
    /// Index of the __arena_alloc helper function (if arena is used and overflow != Unchecked)
    pub arena_alloc_func: Option<u32>,
    /// Internal globals to expose under their declared name (e.g. __rng_state).
    /// Vec of (export name, global index). Emitted as exports alongside __arena_ptr.
    pub exported_globals: Vec<(String, u32)>,
    /// Body for the `__helper_arena_alloc` shim that satisfies helper wasm's
    /// `env::__tscc_arena_alloc` import. Built eagerly during helper
    /// registration (needs `&mut ctx`) and emitted later from
    /// `compile_string_helpers` (which has only `&ctx`). Cleared after use.
    pub helper_arena_alloc_body: RefCell<Option<wasm_encoder::Function>>,
    /// Monomorphized generic-class name -> type-parameter bindings. Populated
    /// during pre-codegen from `generics::collect_instantiations`. Read by
    /// `codegen_method` to push the correct bindings when compiling each
    /// monomorphized method body.
    pub class_bindings: HashMap<String, TypeBindings>,
    /// Monomorphized generic-function name -> type-parameter bindings.
    pub fn_bindings: HashMap<String, TypeBindings>,
    /// Call-site span.start -> mangled monomorphization name. Populated by
    /// `collect_instantiations` when it infers type arguments of an implicit
    /// generic function call (`identity(5)`). `emit_call` consults this map
    /// to route the call through the right monomorphization.
    pub inferred_fn_calls: HashMap<u32, String>,
    /// Mangled `Map<K, V>` / `Set<T>` name -> bucket + slot metadata.
    /// Populated in Pass 0a-iii. Dispatchers in `expr/map.rs`, `expr/set.rs`,
    /// and `emit_new_{map,set}` read this to route per-monomorphization
    /// codegen. Map vs Set is distinguished by `value_ty.is_some()`.
    pub hash_table_info: HashMap<String, super::hash_table::HashTableInfo>,
    /// Structural object types discovered in Pass 0a-iv. Each entry describes
    /// a named (`type`/`interface`) or anonymous (inline `TSTypeLiteral` /
    /// `ObjectExpression`) shape, deduped by its sort-by-name fingerprint.
    /// Phase A.2 consumes this to register synthetic class layouts.
    pub shape_registry: super::shapes::ShapeRegistry,
    /// Union types discovered in the program (`type X = A | B`, inline
    /// `function f(x: A | B)`). Populated by `discover_unions` immediately
    /// after shape discovery so that union members can resolve to registered
    /// shape names. Empty when the program contains no unions.
    pub union_registry: super::unions::UnionRegistry,
    /// Output of `register_string_helpers`, stashed here so that method-body
    /// codegen can build a `RewritePlan` to feed the L_splice splicer.
    /// `None` until that pass has run; never observed `None` from method code
    /// (helper registration runs in Pass 2, before Pass 3 codegen).
    pub helper_registration:
        RefCell<Option<super::string_builtins::HelperRegistration>>,
    /// For each free / monomorphized function, the class name of each
    /// parameter (None for non-class params). Lets `emit_call` thread an
    /// expected-type hint into `ObjectExpression` arguments so a `{...}` at a
    /// callsite resolves against the declared shape parameter.
    pub fn_param_classes: HashMap<String, Vec<Option<String>>>,
    /// For each free / monomorphized function, the class name of the return
    /// type (None for non-class returns). Consumed during body codegen to
    /// populate `FuncContext::return_class` so `return {...};` can thread the
    /// expected shape.
    pub fn_return_classes: HashMap<String, String>,
}

/// A lifted closure function to be added to the WASM module's function table.
pub struct ClosureFunc {
    /// Parameter types (including env_ptr as first param)
    pub param_types: Vec<ValType>,
    /// Result types
    pub result_types: Vec<ValType>,
    /// Compiled WASM function body
    pub body: wasm_encoder::Function,
    /// Source map: (byte_offset_in_func_body, source_byte_offset)
    pub source_map: Vec<(u32, u32)>,
}

pub(crate) struct FuncDef {
    pub(crate) name: String,
    pub(crate) type_index: u32,
    pub(crate) is_export: bool,
}

impl ModuleContext {
    pub fn new(host_module: &str) -> Self {
        ModuleContext {
            host_module: host_module.to_string(),
            func_map: HashMap::new(),
            globals: HashMap::new(),
            mutable_globals: HashSet::new(),
            type_sigs: Vec::new(),
            type_sig_index: HashMap::new(),
            imports: Vec::new(),
            local_funcs: Vec::new(),
            next_func_index: 0,
            next_type_index: 0,
            next_global_index: 0,
            global_inits: Vec::new(),
            static_data_ptr: Cell::new(0),
            class_registry: ClassRegistry::new(),
            class_names: HashSet::new(),
            non_i32_union_wasm_types: HashMap::new(),
            arena_ptr_global: None,
            method_map: HashMap::new(),
            var_class_types: HashMap::new(),
            func_return_closure_sigs: HashMap::new(),
            closure_funcs: RefCell::new(Vec::new()),
            next_table_index: Cell::new(0),
            extra_type_sigs: RefCell::new(Vec::new()),
            extra_type_sig_index: RefCell::new(HashMap::new()),
            static_data_entries: RefCell::new(Vec::new()),
            string_literal_offsets: RefCell::new(HashMap::new()),
            func_return_strings: HashSet::new(),
            method_table_indices: HashMap::new(),
            arena_overflow: ArenaOverflow::Unchecked,
            arena_alloc_func: None,
            exported_globals: Vec::new(),
            helper_arena_alloc_body: RefCell::new(None),
            class_bindings: HashMap::new(),
            fn_bindings: HashMap::new(),
            inferred_fn_calls: HashMap::new(),
            hash_table_info: HashMap::new(),
            shape_registry: super::shapes::ShapeRegistry::default(),
            union_registry: super::unions::UnionRegistry::default(),
            helper_registration: RefCell::new(None),
            fn_param_classes: HashMap::new(),
            fn_return_classes: HashMap::new(),
        }
    }

    pub fn get_func(&self, name: &str) -> Option<(u32, WasmType)> {
        self.func_map.get(name).copied()
    }

    /// Reserve the next global index without registering anything. Used by
    /// internal helpers (e.g. RNG state) that need to populate `globals`,
    /// `global_inits`, and (optionally) `exported_globals` directly because
    /// their wasm type isn't expressible via `WasmType` (e.g. i64).
    pub fn next_global_index_internal(&mut self) -> u32 {
        let idx = self.next_global_index;
        self.next_global_index += 1;
        idx
    }

    /// Initialize the arena pointer global. Called when classes are used.
    /// The arena starts after all static data.
    pub fn init_arena(&mut self) {
        if self.arena_ptr_global.is_some() {
            return; // already initialized
        }
        // Arena starts after static data (will be resolved at assembly time)
        let idx = self.next_global_index;
        self.globals
            .insert("__arena_ptr".to_string(), (idx, WasmType::I32));
        // Use a placeholder — we'll set the real value at assembly time
        self.global_inits.push(GlobalInit::I32(0)); // placeholder
        self.arena_ptr_global = Some(idx);
        self.next_global_index += 1;
    }

    /// Allocate `size` bytes of static data at compile time.
    /// Returns the byte offset where the data starts.
    pub fn alloc_static(&self, size: u32) -> u32 {
        let offset = self.static_data_ptr.get();
        // Align to 8 bytes for f64 loads
        let aligned = (offset + 7) & !7;
        self.static_data_ptr.set(aligned + size);
        aligned
    }

    /// Allocate a string literal in static data. Returns the byte offset (pointer).
    /// Deduplicates identical string content.
    pub fn alloc_static_string(&self, s: &str) -> u32 {
        // Check dedup cache
        if let Some(&offset) = self.string_literal_offsets.borrow().get(s) {
            return offset;
        }
        let bytes_len = s.len() as u32;
        let total_size = 4 + bytes_len; // 4-byte length header + UTF-8 bytes
        let offset = self.alloc_static(total_size);
        // Build the data: [length as le i32] [utf-8 bytes]
        let mut data = Vec::with_capacity(total_size as usize);
        data.extend_from_slice(&(bytes_len as i32).to_le_bytes());
        data.extend_from_slice(s.as_bytes());
        self.static_data_entries.borrow_mut().push((offset, data));
        self.string_literal_offsets
            .borrow_mut()
            .insert(s.to_string(), offset);
        offset
    }

    fn add_type_sig(&mut self, params: Vec<ValType>, results: Vec<ValType>) -> u32 {
        let key = (params, results);
        if let Some(&idx) = self.type_sig_index.get(&key) {
            return idx;
        }
        let idx = self.next_type_index;
        self.type_sig_index.insert(key.clone(), idx);
        self.type_sigs.push(key);
        self.next_type_index += 1;
        idx
    }

    pub fn add_import(
        &mut self,
        name: &str,
        params: &[WasmType],
        ret: WasmType,
    ) -> Result<u32, CompileError> {
        let wasm_params = wasm_types::wasm_params(params);
        let wasm_results = wasm_types::wasm_results(ret);
        let type_idx = self.add_type_sig(wasm_params, wasm_results);
        let func_idx = self.next_func_index;
        self.imports
            .push((self.host_module.clone(), name.to_string(), type_idx));
        self.func_map.insert(name.to_string(), (func_idx, ret));
        self.next_func_index += 1;
        Ok(func_idx)
    }

    pub fn register_func(
        &mut self,
        name: &str,
        params: &[(String, WasmType)],
        ret: WasmType,
        is_export: bool,
    ) -> Result<u32, CompileError> {
        let wasm_params: Vec<ValType> = params
            .iter()
            .filter_map(|(_, ty)| ty.to_val_type())
            .collect();
        let wasm_results = wasm_types::wasm_results(ret);
        let type_idx = self.add_type_sig(wasm_params, wasm_results);
        let func_idx = self.next_func_index;
        self.func_map.insert(name.to_string(), (func_idx, ret));
        self.local_funcs.push(FuncDef {
            name: name.to_string(),
            type_index: type_idx,
            is_export,
        });
        self.next_func_index += 1;
        Ok(func_idx)
    }

    /// Register a function using raw `ValType` signatures, bypassing the
    /// `WasmType` system. Used for precompiled helpers whose signatures may
    /// include types tscc's frontend doesn't model (e.g. i64 in compiler-
    /// inserted intrinsics). Does NOT populate `func_map` — callers who need
    /// name lookup must do it themselves.
    pub fn register_raw_func(
        &mut self,
        name: &str,
        params: Vec<ValType>,
        results: Vec<ValType>,
    ) -> u32 {
        let type_idx = self.add_type_sig(params, results);
        let func_idx = self.next_func_index;
        self.local_funcs.push(FuncDef {
            name: name.to_string(),
            type_index: type_idx,
            is_export: false,
        });
        self.next_func_index += 1;
        func_idx
    }

    /// Get or register a type signature during codegen (&self).
    /// Checks existing sigs first, then extra_type_sigs, then adds a new one.
    /// Returns the type index usable for call_indirect.
    pub fn get_or_add_type_sig(&self, params: Vec<ValType>, results: Vec<ValType>) -> u32 {
        let key = (params, results);
        // Check existing (pre-codegen) type sigs
        if let Some(&idx) = self.type_sig_index.get(&key) {
            return idx;
        }
        // Check extra sigs already added during codegen
        let mut extra_index = self.extra_type_sig_index.borrow_mut();
        if let Some(&idx) = extra_index.get(&key) {
            return idx;
        }
        // Add new
        let mut extras = self.extra_type_sigs.borrow_mut();
        let base = self.type_sigs.len() as u32;
        let idx = base + extras.len() as u32;
        extra_index.insert(key.clone(), idx);
        extras.push(key);
        idx
    }

    /// Register a lifted closure function. Returns the table index.
    /// Safe to call during codegen pass (&self) thanks to interior mutability.
    pub fn register_closure_func(
        &self,
        param_types: Vec<ValType>,
        result_types: Vec<ValType>,
        body: wasm_encoder::Function,
        source_map: Vec<(u32, u32)>,
    ) -> u32 {
        let table_idx = self.next_table_index.get();
        self.next_table_index.set(table_idx + 1);
        self.closure_funcs.borrow_mut().push(ClosureFunc {
            param_types,
            result_types,
            body,
            source_map,
        });
        table_idx
    }

    pub fn add_global(&mut self, name: &str, ty: WasmType, init_value: GlobalInit) -> u32 {
        let idx = self.next_global_index;
        self.globals.insert(name.to_string(), (idx, ty));
        self.global_inits.push(init_value);
        self.next_global_index += 1;
        idx
    }
}

pub enum GlobalInit {
    I32(i32),
    I64(i64),
    F64(f64),
}

#[allow(clippy::too_many_arguments)] // Mirrors the public `CompileOptions` fields.
pub fn compile_module<'a>(
    program: &Program<'a>,
    host_module: &str,
    memory_pages: u32,
    source: &'a str,
    debug: bool,
    filename: &str,
    arena_overflow: ArenaOverflow,
    expose_helpers: &HashSet<String>,
) -> Result<Vec<u8>, CompileError> {
    let mut ctx = ModuleContext::new(host_module);
    ctx.arena_overflow = arena_overflow;

    // Pass 0a-pre: pre-collect non-`I32` named-union `WasmType`s. Runs
    // *before* template discovery / class collection / instantiation
    // walking so `resolve_bound_type` (used by `mangle_parent_name` and
    // `collect_instantiations`) can produce correct
    // `BoundType::Union { name, wasm_ty }` for generic arguments like
    // `Box<Half>` where `Half = 0.5 | 1.5`. Cheap: a single pass over
    // top-level statements, inspecting only literal-typed alias bodies.
    super::unions::collect_named_union_wasm_types(program, &mut ctx.non_i32_union_wasm_types);

    // Pass 0a: discover generic templates (classes + functions with type
    // parameters). These are NOT registered as concrete classes — only their
    // monomorphized instantiations are.
    let (class_templates, fn_templates) = super::generics::discover_templates(program);

    // Pass 0a-i: collect concrete class names and extends relationships.
    // Generic templates (e.g. `class Box<T>`) are skipped — their
    // monomorphizations take their place below. A concrete class that extends
    // a generic parent (e.g. `class Foo extends Parent<i32>`) has its parent
    // name mangled here so the relationship threads into Pass 0b's topo sort.
    let mut class_info: Vec<(String, Option<String>)> = Vec::new();
    let mut class_ast_map: HashMap<String, &Class> = HashMap::new();
    for stmt in &program.body {
        if let Statement::ClassDeclaration(class) = stmt {
            let name = class
                .id
                .as_ref()
                .ok_or_else(|| CompileError::parse("class without name"))?
                .name
                .as_str()
                .to_string();
            if class_templates.contains_key(&name) {
                continue;
            }
            let parent = super::generics::mangle_parent_name(
                class,
                &ctx.class_names,
                &class_templates,
                None,
                &ctx.non_i32_union_wasm_types,
            )?;
            ctx.class_names.insert(name.clone());
            class_info.push((name.clone(), parent));
            class_ast_map.insert(name, class);
        }
    }

    // Pass 0a-ii: collect all generic instantiations (both class and function).
    // The collector walks the whole program — class fields/method bodies,
    // function bodies, top-level declarations/expressions — for any
    // TSTypeReference / NewExpression / CallExpression whose base identifier is
    // a generic template. Nested generics (Array<Box<i32>>) are expanded.
    //
    // Pre-seed named shape names into a *temporary* combined set for this
    // walker only: `Map<string, Unit>` / `Array<Pos>` etc. need the shape
    // name to resolve during generic walking even though shape registration
    // itself happens later (Pass 0a-iv/v). Kept out of `ctx.class_names` so
    // shape discovery's collision check doesn't mistake shapes for classes.
    let mut generic_lookup_names = ctx.class_names.clone();
    for name in super::shapes::prescan_shape_names(program) {
        generic_lookup_names.insert(name);
    }
    // Also seed pre-collected named unions so `Box<Half>`-style
    // instantiations can be walked even though full union discovery runs
    // later. The final population still happens in Pass 0a-vi.
    for name in ctx.non_i32_union_wasm_types.keys() {
        generic_lookup_names.insert(name.clone());
    }
    let super::generics::CollectResult {
        class_insts,
        fn_insts,
        inferred_call_sites,
        map_insts,
        set_insts,
    } = super::generics::collect_instantiations(
        program,
        &class_templates,
        &fn_templates,
        &generic_lookup_names,
        &ctx.non_i32_union_wasm_types,
    )?;
    ctx.inferred_fn_calls = inferred_call_sites;

    // Add each monomorphized class to the pipeline as if it were a concrete
    // class: its mangled name enters class_names, class_info, and
    // class_ast_map (pointing at the template's AST). If the template extends
    // a generic parent (e.g. `class Child<T> extends Parent<T>`), the parent
    // name is mangled here under the child's bindings so Pass 0b sees e.g.
    // `Child$i32 -> Parent$i32` and emits layouts in dependency order.
    for inst in &class_insts {
        let template = &class_templates[&inst.template_name];
        ctx.class_names.insert(inst.mangled_name.clone());
        let parent = super::generics::mangle_parent_name(
            template.ast,
            &ctx.class_names,
            &class_templates,
            Some(&inst.bindings),
            &ctx.non_i32_union_wasm_types,
        )?;
        class_info.push((inst.mangled_name.clone(), parent));
        class_ast_map.insert(inst.mangled_name.clone(), template.ast);
        ctx.class_bindings
            .insert(inst.mangled_name.clone(), inst.bindings.clone());
    }
    for inst in &fn_insts {
        ctx.fn_bindings
            .insert(inst.mangled_name.clone(), inst.bindings.clone());
    }

    // Pass 0a-iia: compute polymorphism flags now that `class_info` is
    // complete (concrete classes from -i + monomorphized classes from -ii).
    // Hoisted here so Pass 0a-vi (`discover_unions`) can gate class-union
    // members on `is_polymorphic`. The set is consumed unchanged by Pass 0c
    // (`register_class` / `mark_polymorphic`); moving it earlier is safe
    // because nothing between -iii and -vi reads it, and the underlying
    // (name, parent) data is already final at this point.
    let polymorphic = super::classes::find_polymorphic_classes(&class_info);

    // Pass 0a-iii: register compiler-owned Map<K, V> / Set<T> instantiations.
    // Maps and Sets live outside class_info/class_ast_map — they carry no
    // user AST, no inheritance, and no user-facing methods yet. Their header
    // layouts are synthesized directly in the ClassRegistry so field access +
    // member resolution flow through the same paths as user classes; bucket
    // layouts are stashed on `ctx.hash_table_info` for `emit_new_{map,set}`
    // and the method dispatchers to consume. Map vs Set is recovered from
    // `value_ty.is_some()`.
    for inst in map_insts.iter().chain(set_insts.iter()) {
        ctx.class_names.insert(inst.mangled_name.clone());
        super::hash_table::register_layout(&mut ctx.class_registry, &inst.mangled_name)?;
        let bucket =
            super::hash_table::BucketLayout::compute(&inst.slot_ty, inst.value_ty.as_ref());
        ctx.hash_table_info.insert(
            inst.mangled_name.clone(),
            super::hash_table::HashTableInfo {
                slot_ty: inst.slot_ty.clone(),
                value_ty: inst.value_ty.clone(),
                bucket,
            },
        );
    }

    // Pass 0a-iv: discover structural object shapes (`type`, `interface`,
    // inline `{x: number}` annotations, and `ObjectExpression` literals).
    // Shape registration as synthetic classes is Phase A.2 — this pass just
    // populates the registry so later phases have a stable inventory. See
    // `docs/plan-object-literals-tuples.md` Phase A.1.
    ctx.shape_registry = super::shapes::discover_shapes(
        program,
        &ctx.class_names,
        &class_templates,
        &fn_templates,
        &ctx.non_i32_union_wasm_types,
    )?;

    // Pass 0a-v: register each discovered shape as a synthetic ClassLayout.
    // Iteration follows shape-discovery order, so a nested shape that another
    // shape's field references is already in `ctx.class_names` by the time the
    // outer shape registers — no second pass needed.
    let shape_count = ctx.shape_registry.shapes.len();
    for i in 0..shape_count {
        let (shape_name, shape_fields) = {
            let s = &ctx.shape_registry.shapes[i];
            (s.name.clone(), s.fields.clone())
        };
        ctx.class_names.insert(shape_name.clone());
        let resolved: Vec<super::classes::LayoutField> = shape_fields
            .iter()
            .map(|f| super::classes::LayoutField {
                name: f.name.clone(),
                wasm_ty: f.ty.wasm_ty(),
                class_ref: match &f.ty {
                    crate::types::BoundType::Class(cn) => Some(cn.clone()),
                    _ => None,
                },
                is_string: matches!(f.ty, crate::types::BoundType::Str),
            })
            .collect();
        ctx.class_registry
            .register_synthetic_layout(&shape_name, &resolved)?;
    }
    // Two distinct user names declaring the same shape (e.g.
    // `interface Pair {...}` and `type PairAlias = {...}`) collapse to a
    // single registry entry, but each name is recorded in `by_name` as an
    // alias. Surface every alias in `class_names` so `let p: PairAlias = ...`
    // resolves the same as `let p: Pair = ...`.
    for alias in ctx.shape_registry.by_name.keys() {
        ctx.class_names.insert(alias.clone());
    }

    // Pass 0a-vi: discover union types (`type X = A | B`, inline `A | B`).
    // Runs after shape names are in `class_names` so union member references
    // like `Circle | Square` resolve to registered shapes. Empty registry
    // when the program contains no unions — zero-cost for non-union code.
    // Receives the `polymorphic` set (computed in Pass 0a-iia) so the gate
    // can reject class union members that don't carry a vtable pointer.
    ctx.union_registry = super::unions::discover_unions(
        program,
        &ctx.class_names,
        &ctx.shape_registry,
        &polymorphic,
    )?;
    for n in ctx.union_registry.by_name.keys() {
        ctx.class_names.insert(n.clone());
    }
    // Stash non-I32 union wasm types so the type resolver can override the
    // default `class_names`-implies-I32 mapping for pure-`f64`-literal unions
    // (`type X = 0.5 | 1.5`). Iterating `by_name` covers user-given names,
    // synthetic `__Union$...` aliases, and the secondary aliases inserted
    // when an inline union resolves to a pre-existing layout.
    for (name, &idx) in &ctx.union_registry.by_name {
        let layout = &ctx.union_registry.unions[idx];
        if layout.wasm_ty != crate::types::WasmType::I32 {
            ctx.non_i32_union_wasm_types
                .insert(name.clone(), layout.wasm_ty);
        }
    }
    // Pseudo-name for the `never` type so `: never` parameters / locals flow
    // through the same `fn_param_classes` / `local_class_types` channel that
    // shape and union targets use. Coerce checks consult `NEVER_CLASS_NAME`
    // directly — no class layout is registered.
    ctx.class_names
        .insert(crate::types::NEVER_CLASS_NAME.to_string());

    // Pass 0b: topological order for layout registration (parent before
    // child). Polymorphism flags were already computed in Pass 0a-iia so the
    // union gate could consume them; reused here unchanged.
    let sorted_classes = super::classes::topo_sort_classes(&class_info)?;

    // Pass 0c: register class layouts in dependency order (parent before child)
    for (name, parent) in &sorted_classes {
        let class = class_ast_map[name.as_str()];
        let is_poly = polymorphic.contains(name);
        if let Some(bindings) = ctx.class_bindings.get(name).cloned() {
            ctx.class_registry.register_class_with_bindings(
                class,
                &ctx.class_names,
                parent.clone(),
                is_poly,
                Some(name),
                Some(&bindings),
                Some(&ctx.shape_registry),
                Some(&ctx.union_registry),
                &ctx.non_i32_union_wasm_types,
            )?;
        } else {
            ctx.class_registry.register_class(
                class,
                &ctx.class_names,
                parent.clone(),
                is_poly,
                Some(&ctx.shape_registry),
                Some(&ctx.union_registry),
                &ctx.non_i32_union_wasm_types,
            )?;
        }
    }
    ctx.class_registry.mark_polymorphic(&polymorphic);

    // Always initialize arena — it's cheap (one global) and arrays need it even without classes.
    // Reserve the first 8 bytes as a null guard (pointer 0 = null).
    ctx.alloc_static(8);
    ctx.init_arena();

    // Pass 1: collect imports (declare function)
    for stmt in &program.body {
        match stmt {
            Statement::TSTypeAliasDeclaration(_) => continue,
            Statement::FunctionDeclaration(func_decl) if func_decl.declare => {
                collect_import_from_func(&mut ctx, func_decl)?;
            }
            _ => {}
        }
    }

    // Pass 1a: declare host imports for transcendentals actually referenced
    // (Math.sin, Math.log, etc.). Only the methods used by the program get
    // imported — see math_builtins::collect_used_transcendentals.
    {
        let used = crate::codegen::math_builtins::collect_used_transcendentals(program);
        for &(method, arity) in crate::codegen::math_builtins::MATH_TRANSCENDENTALS {
            if !used.contains(method) {
                continue;
            }
            let import_name = crate::codegen::math_builtins::import_name(method);
            let params = vec![WasmType::F64; arity as usize];
            ctx.add_import(&import_name, &params, WasmType::F64)?;
        }
    }

    // Pass 1b: register class methods as WASM functions
    // Methods get an implicit `this: i32` first parameter.
    // Non-generic classes register under their AST name; monomorphized classes
    // register under their mangled name using the template's AST with bindings.
    for stmt in &program.body {
        if let Statement::ClassDeclaration(class) = stmt {
            let name = class.id.as_ref().unwrap().name.as_str();
            if class_templates.contains_key(name) {
                continue;
            }
            register_class_methods(&mut ctx, name, class, None)?;
        }
    }
    for inst in &class_insts {
        let template = &class_templates[&inst.template_name];
        let bindings = inst.bindings.clone();
        register_class_methods(&mut ctx, &inst.mangled_name, template.ast, Some(&bindings))?;
    }

    // Pass 1c: build vtables for polymorphic classes
    // Assign WASM function table indices to methods of polymorphic classes,
    // then build vtable data in static memory.
    {
        // Helper: find the declaring class for a method by walking method_map (parent chain).
        fn find_method_declarer(
            method_map: &HashMap<String, (u32, WasmType)>,
            registry: &super::classes::ClassRegistry,
            class_name: &str,
            method_name: &str,
        ) -> Option<String> {
            let mut cur = class_name.to_string();
            loop {
                let key = format!("{cur}.{method_name}");
                if method_map.contains_key(&key) {
                    return Some(cur);
                }
                if let Some(layout) = registry.get(&cur) {
                    if let Some(ref parent) = layout.parent {
                        cur = parent.clone();
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            }
        }

        // First, assign table indices to all methods of polymorphic classes
        for (class_name, _parent) in &sorted_classes {
            if !polymorphic.contains(class_name) {
                continue;
            }
            let layout = ctx.class_registry.get(class_name).unwrap().clone();
            for method_name in &layout.vtable_methods {
                let owner = find_method_declarer(
                    &ctx.method_map,
                    &ctx.class_registry,
                    class_name,
                    method_name,
                )
                .ok_or_else(|| {
                    CompileError::codegen(format!(
                        "vtable method '{method_name}' not found in hierarchy of '{class_name}'"
                    ))
                })?;
                let mangled = format!("{owner}${method_name}");
                if !ctx.method_table_indices.contains_key(&mangled) {
                    let table_idx = ctx.next_table_index.get();
                    ctx.next_table_index.set(table_idx + 1);
                    ctx.method_table_indices.insert(mangled, table_idx);
                }
            }
        }

        // Build vtable data for each polymorphic class, collect offsets.
        // Every polymorphic class allocates at least 4 bytes (one zero word)
        // even when it has no methods — Phase 2 `instanceof` uses
        // `vtable_offset` as the runtime discriminator, so each polymorphic
        // class needs a unique address. Without the floor, methodless
        // polymorphic classes would all alias offset 0.
        let mut vtable_offsets: Vec<(String, u32)> = Vec::new();
        for (class_name, _parent) in &sorted_classes {
            if !polymorphic.contains(class_name) {
                continue;
            }
            let layout = ctx.class_registry.get(class_name).unwrap().clone();
            let num_methods = layout.vtable_methods.len();
            let alloc_bytes = ((num_methods * 4) as u32).max(4);

            let vtable_offset = ctx.alloc_static(alloc_bytes);

            let mut vtable_data = Vec::with_capacity(alloc_bytes as usize);
            for method_name in &layout.vtable_methods {
                let owner = find_method_declarer(
                    &ctx.method_map,
                    &ctx.class_registry,
                    class_name,
                    method_name,
                )
                .unwrap();
                let mangled = format!("{owner}${method_name}");
                let table_idx = ctx.method_table_indices[&mangled];
                vtable_data.extend_from_slice(&(table_idx as i32).to_le_bytes());
            }
            // Pad up to the allocated size when methodless so the data
            // segment matches `alloc_static`'s reservation. The padding
            // bytes are never read — only the offset itself is consulted.
            while vtable_data.len() < alloc_bytes as usize {
                vtable_data.push(0);
            }
            ctx.static_data_entries
                .borrow_mut()
                .push((vtable_offset, vtable_data));
            vtable_offsets.push((class_name.clone(), vtable_offset));
        }

        // Apply vtable offsets to class layouts
        for (class_name, vtable_offset) in vtable_offsets {
            ctx.class_registry
                .classes
                .get_mut(&class_name)
                .unwrap()
                .vtable_offset = vtable_offset;
        }
    }

    // Pass 2: register all free functions (get indices). Generic function
    // templates are skipped — only their monomorphizations get registered.
    for stmt in &program.body {
        match stmt {
            Statement::FunctionDeclaration(func_decl) if !func_decl.declare => {
                if let Some(id) = &func_decl.id
                    && fn_templates.contains_key(id.name.as_str())
                {
                    continue;
                }
                register_function(&mut ctx, func_decl, false)?;
            }
            Statement::ExportDefaultDeclaration(export) => {
                if let ExportDefaultDeclarationKind::FunctionDeclaration(func_decl) =
                    &export.declaration
                {
                    let name = func_decl
                        .id
                        .as_ref()
                        .map(|id| id.name.as_str())
                        .unwrap_or("default");
                    register_func_from_decl(&mut ctx, func_decl, name, true, None)?;
                }
            }
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration
                    && let Declaration::FunctionDeclaration(func_decl) = decl
                {
                    if let Some(id) = &func_decl.id
                        && fn_templates.contains_key(id.name.as_str())
                    {
                        continue;
                    }
                    register_function(&mut ctx, func_decl, true)?;
                }
            }
            _ => {}
        }
    }

    // Register each monomorphized function under its mangled name, pulling the
    // body from the template AST. `register_func_from_decl` threads bindings
    // into parameter + return-type resolution.
    for inst in &fn_insts {
        let template = &fn_templates[&inst.template_name];
        let bindings = inst.bindings.clone();
        register_func_from_decl(
            &mut ctx,
            template.ast,
            &inst.mangled_name,
            template.is_export,
            Some(&bindings),
        )?;
    }

    // Collect top-level const/let declarations as globals
    for stmt in &program.body {
        if let Statement::VariableDeclaration(var_decl) = stmt {
            let mutable = match var_decl.kind {
                VariableDeclarationKind::Const => false,
                VariableDeclarationKind::Let | VariableDeclarationKind::Var => true,
                _ => continue,
            };
            for declarator in &var_decl.declarations {
                collect_global(&mut ctx, declarator, mutable)?;
            }
        }
    }

    // Collect enum declarations as i32 constants
    for stmt in &program.body {
        if let Statement::TSEnumDeclaration(enum_decl) = stmt {
            collect_enum(&mut ctx, enum_decl)?;
        }
    }

    // Register __arena_alloc helper if overflow checking is enabled
    if arena_overflow != ArenaOverflow::Unchecked {
        let params = vec![("size".to_string(), WasmType::I32)];
        let func_idx = ctx.register_func("__arena_alloc", &params, WasmType::I32, false)?;
        ctx.arena_alloc_func = Some(func_idx);
    }

    // Register only the string runtime helpers that the program actually uses.
    // Pre-scan the AST to build the used-set; this replaces the prior
    // "register all 21, stub the unused ones" approach. `expose_helpers`
    // lets a caller force specific helpers into the bundle AND re-export
    // them by name — used by the hash-helper test suite and by any
    // debug/profiling tooling that wants direct helper access.
    let mut used_string_helpers = super::string_builtins::collect_used_helpers(program);
    for name in expose_helpers {
        used_string_helpers.insert(name.clone());
    }
    // Map<K, V> methods route hashing + (for f64 / string keys) equality to the
    // same precompiled bundle the string helpers ride on. Seed based on the
    // collected instantiations so the tree-shaker pulls in only what this
    // program's Maps actually need.
    for name in super::hash_table::required_runtime_helpers(&map_insts) {
        used_string_helpers.insert(name);
    }
    // Set<T> methods share Map's hash + equality helper dispatch, gated on
    // the element type. Seed the tree-shaker the same way.
    for name in super::hash_table::required_runtime_helpers(&set_insts) {
        used_string_helpers.insert(name);
    }
    if !used_string_helpers.is_empty() {
        ctx.init_arena();
    }
    let helper_registration = super::string_builtins::register_string_helpers(
        &mut ctx,
        &used_string_helpers,
        expose_helpers,
    );
    // Stash on ctx so method-body codegen (Pass 3 below) can build a
    // RewritePlan to drive the L_splice splicer for inline helpers like
    // `__hash_fx_i32`. We still pass `&helper_registration` by reference to
    // the later compile/assemble calls; both views observe the same data.
    ctx.helper_registration
        .replace(Some(helper_registration.clone()));

    // Register RNG helpers (state global + step function) if Math.random is used.
    let uses_random = super::math_builtins::program_uses_random(program);
    if uses_random {
        super::math_builtins::register_rng(&mut ctx);
    }

    // Pass 3: codegen — class methods first, then free functions
    // Each entry: (compiled_function, source_map)
    let mut compiled_funcs: Vec<(wasm_encoder::Function, Vec<(u32, u32)>)> = Vec::new();

    // Non-generic class methods. Order must match Pass 1b registration.
    for stmt in &program.body {
        if let Statement::ClassDeclaration(class) = stmt {
            let class_name = class.id.as_ref().unwrap().name.as_str();
            if class_templates.contains_key(class_name) {
                continue;
            }
            for element in &class.body.body {
                if let ClassElement::MethodDefinition(method) = element {
                    compiled_funcs.push(codegen_method(&ctx, class_name, method, source, None)?);
                }
            }
        }
    }

    // Monomorphized class methods — same order as Pass 1b.
    for inst in &class_insts {
        let template = &class_templates[&inst.template_name];
        let bindings = inst.bindings.clone();
        for element in &template.ast.body.body {
            if let ClassElement::MethodDefinition(method) = element {
                compiled_funcs.push(codegen_method(
                    &ctx,
                    &inst.mangled_name,
                    method,
                    source,
                    Some(&bindings),
                )?);
            }
        }
    }

    for stmt in &program.body {
        match stmt {
            Statement::FunctionDeclaration(func_decl) if !func_decl.declare => {
                if let Some(id) = &func_decl.id
                    && fn_templates.contains_key(id.name.as_str())
                {
                    continue;
                }
                compiled_funcs.push(codegen_function(&ctx, func_decl, source, None)?);
            }
            Statement::ExportDefaultDeclaration(export) => {
                if let ExportDefaultDeclarationKind::FunctionDeclaration(func_decl) =
                    &export.declaration
                {
                    compiled_funcs.push(codegen_function(&ctx, func_decl, source, None)?);
                }
            }
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration
                    && let Declaration::FunctionDeclaration(func_decl) = decl
                {
                    if let Some(id) = &func_decl.id
                        && fn_templates.contains_key(id.name.as_str())
                    {
                        continue;
                    }
                    compiled_funcs.push(codegen_function(&ctx, func_decl, source, None)?);
                }
            }
            _ => {}
        }
    }

    // Monomorphized free function bodies — same order as the fn_insts
    // registration loop.
    for inst in &fn_insts {
        let template = &fn_templates[&inst.template_name];
        let bindings = inst.bindings.clone();
        compiled_funcs.push(codegen_function(&ctx, template.ast, source, Some(&bindings))?);
    }

    // Compile __arena_alloc body if registered
    if ctx.arena_alloc_func.is_some() {
        compiled_funcs.push((compile_arena_alloc(&ctx), Vec::new()));
    }

    // Compile the string runtime helper bodies (only those that were registered).
    let string_helpers = super::string_builtins::compile_string_helpers(
        &ctx,
        &used_string_helpers,
        &helper_registration,
    );
    compiled_funcs.extend(string_helpers.into_iter().map(|f| (f, Vec::new())));

    // Compile RNG step function body if Math.random is used.
    if uses_random {
        compiled_funcs.push((super::math_builtins::compile_rng_next(&ctx), Vec::new()));
    }

    // Assemble the WASM module
    super::sections::assemble_module(
        &ctx,
        &compiled_funcs,
        memory_pages,
        source,
        debug,
        filename,
        &helper_registration,
    )
}

/// Compile the __arena_alloc(size: i32) -> i32 helper function.
/// Bumps the arena pointer and checks for overflow based on the configured strategy.
fn compile_arena_alloc(ctx: &ModuleContext) -> wasm_encoder::Function {
    use wasm_encoder::Instruction;

    let arena_idx = ctx.arena_ptr_global.unwrap();
    // Locals: param 0 = size, local 0 = ptr (the return value)
    let mut func = wasm_encoder::Function::new(vec![(1, ValType::I32)]);
    let size_param = 0u32;
    let ptr_local = 1u32;

    // ptr = arena_ptr
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr_local));

    // arena_ptr += size
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(size_param));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // Overflow check: if arena_ptr > memory.size * 65536
    match ctx.arena_overflow {
        ArenaOverflow::Grow => {
            // if arena_ptr > mem_size * 65536:
            //   needed_pages = ceil(arena_ptr / 65536) - mem_size
            //   if memory.grow(needed_pages) == -1 { unreachable }
            // Ceiling division as `((arena_ptr + 65535) >> 16) - mem_size` handles
            // allocations larger than one page — growing by exactly one page is
            // not always enough.
            func.instruction(&Instruction::GlobalGet(arena_idx));
            func.instruction(&Instruction::MemorySize(0));
            func.instruction(&Instruction::I32Const(16)); // log2(65536)
            func.instruction(&Instruction::I32Shl);
            func.instruction(&Instruction::I32GtU);
            func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
            // needed_pages = ((arena_ptr + 65535) >> 16) - mem_size
            func.instruction(&Instruction::GlobalGet(arena_idx));
            func.instruction(&Instruction::I32Const(65535));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::I32Const(16));
            func.instruction(&Instruction::I32ShrU);
            func.instruction(&Instruction::MemorySize(0));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::MemoryGrow(0));
            func.instruction(&Instruction::I32Const(-1));
            func.instruction(&Instruction::I32Eq);
            func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
            func.instruction(&Instruction::Unreachable); // host refused growth
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
        }
        ArenaOverflow::Trap => {
            // if arena_ptr > memory.size * 65536 { unreachable }
            func.instruction(&Instruction::GlobalGet(arena_idx));
            func.instruction(&Instruction::MemorySize(0));
            func.instruction(&Instruction::I32Const(16));
            func.instruction(&Instruction::I32Shl);
            func.instruction(&Instruction::I32GtU);
            func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
            func.instruction(&Instruction::Unreachable);
            func.instruction(&Instruction::End);
        }
        ArenaOverflow::Unchecked => unreachable!(),
    }

    // Return ptr
    func.instruction(&Instruction::LocalGet(ptr_local));
    func.instruction(&Instruction::End);
    func
}

fn collect_import_from_func(
    ctx: &mut ModuleContext,
    func_decl: &Function,
) -> Result<(), CompileError> {
    let name = func_decl
        .id
        .as_ref()
        .ok_or_else(|| CompileError::parse("declare function without name"))?
        .name
        .as_str();

    let (params, ret) = extract_func_signature(
        func_decl,
        &ctx.class_names,
        None,
        &ctx.non_i32_union_wasm_types,
    )?;
    let param_types: Vec<WasmType> = params.iter().map(|(_, ty)| *ty).collect();
    ctx.add_import(name, &param_types, ret)?;
    Ok(())
}

fn register_function(
    ctx: &mut ModuleContext,
    func_decl: &Function,
    is_export: bool,
) -> Result<(), CompileError> {
    if func_decl.declare {
        return Ok(());
    }
    let name = func_decl
        .id
        .as_ref()
        .ok_or_else(|| CompileError::parse("function without name"))?
        .name
        .as_str();

    register_func_from_decl(ctx, func_decl, name, is_export, None)
}

fn register_func_from_decl(
    ctx: &mut ModuleContext,
    func_decl: &Function,
    name: &str,
    is_export: bool,
    bindings: Option<&TypeBindings>,
) -> Result<(), CompileError> {
    let (params, ret) = extract_func_signature(
        func_decl,
        &ctx.class_names,
        bindings,
        &ctx.non_i32_union_wasm_types,
    )?;
    // Track if this function returns a closure
    if let Some(ann) = &func_decl.return_type {
        if let Some(sig) = types::get_closure_sig(ann, &ctx.class_names, &ctx.non_i32_union_wasm_types) {
            ctx.func_return_closure_sigs.insert(name.to_string(), sig);
        }
        // Track if this function returns a string (bindings-aware so a
        // generic function returning T bound to `string` flows through).
        if types::is_string_type_with_bindings(ann, bindings) {
            ctx.func_return_strings.insert(name.to_string());
        }
        // Track class-typed returns so `return {...};` can resolve the shape.
        if let Some(class_name) = types::get_class_type_name_with_bindings(
            ann,
            bindings,
            Some(&ctx.shape_registry),
            Some(&ctx.union_registry),
        ) && ctx.class_names.contains(&class_name)
        {
            ctx.fn_return_classes.insert(name.to_string(), class_name);
        }
    }
    // Record each parameter's class name (if any) for call-site expected-type
    // threading of `{...}` arguments.
    let mut param_classes: Vec<Option<String>> = Vec::with_capacity(func_decl.params.items.len());
    for param in &func_decl.params.items {
        let class = param.type_annotation.as_ref().and_then(|ann| {
            types::get_class_type_name_with_bindings(
                ann,
                bindings,
                Some(&ctx.shape_registry),
                Some(&ctx.union_registry),
            )
            .filter(|cn| ctx.class_names.contains(cn))
        });
        param_classes.push(class);
    }
    if param_classes.iter().any(|c| c.is_some()) {
        ctx.fn_param_classes.insert(name.to_string(), param_classes);
    }
    ctx.register_func(name, &params, ret, is_export)?;
    Ok(())
}

fn extract_func_signature(
    func_decl: &Function,
    class_names: &HashSet<String>,
    bindings: Option<&TypeBindings>,
    union_overrides: &HashMap<String, WasmType>,
) -> Result<(Vec<(String, WasmType)>, WasmType), CompileError> {
    let mut params = Vec::new();
    for param in &func_decl.params.items {
        let name = match &param.pattern {
            BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
            _ => return Err(CompileError::unsupported("destructured parameter")),
        };
        let ty = if let Some(ann) = &param.type_annotation {
            types::resolve_type_annotation_with_unions(ann, class_names, bindings, union_overrides)?
        } else {
            return Err(CompileError::type_err(format!(
                "parameter '{name}' requires a type annotation — tscc does not infer parameter types; write `{name}: i32` (or f64, bool, string, or a class name)"
            )));
        };
        params.push((name, ty));
    }

    let ret = if let Some(ann) = &func_decl.return_type {
        types::resolve_type_annotation_with_unions(ann, class_names, bindings, union_overrides)?
    } else {
        WasmType::Void
    };

    Ok((params, ret))
}

fn collect_global(
    ctx: &mut ModuleContext,
    decl: &VariableDeclarator,
    mutable: bool,
) -> Result<(), CompileError> {
    let name = match &decl.id {
        BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
        _ => return Err(CompileError::unsupported("destructured global")),
    };

    let ty = if let Some(ann) = &decl.type_annotation {
        types::resolve_type_annotation(ann)?
    } else {
        return Err(CompileError::type_err(format!(
            "global '{name}' requires type annotation"
        )));
    };

    let init = if let Some(init_expr) = &decl.init {
        match init_expr {
            Expression::NumericLiteral(lit) => match ty {
                WasmType::I32 => GlobalInit::I32(lit.value as i32),
                WasmType::F64 => GlobalInit::F64(lit.value),
                _ => return Err(CompileError::type_err("global must be i32 or f64")),
            },
            Expression::UnaryExpression(un)
                if matches!(un.operator, UnaryOperator::UnaryNegation) =>
            {
                if let Expression::NumericLiteral(lit) = &un.argument {
                    match ty {
                        WasmType::I32 => GlobalInit::I32(-(lit.value as i32)),
                        WasmType::F64 => GlobalInit::F64(-lit.value),
                        _ => return Err(CompileError::type_err("global must be i32 or f64")),
                    }
                } else {
                    return Err(CompileError::unsupported("non-constant global initializer"));
                }
            }
            Expression::BooleanLiteral(lit) => GlobalInit::I32(if lit.value { 1 } else { 0 }),
            Expression::CallExpression(call) => {
                // Handle __static_alloc(size) at compile time
                if let Expression::Identifier(ident) = &call.callee {
                    if ident.name.as_str() == "__static_alloc" && call.arguments.len() == 1 {
                        if let Expression::NumericLiteral(lit) = &call.arguments[0].to_expression()
                        {
                            let size = lit.value as u32;
                            let offset = ctx.alloc_static(size);
                            GlobalInit::I32(offset as i32)
                        } else {
                            return Err(CompileError::codegen(
                                "__static_alloc size must be a numeric literal",
                            ));
                        }
                    } else {
                        return Err(CompileError::unsupported("non-constant global initializer"));
                    }
                } else {
                    return Err(CompileError::unsupported("non-constant global initializer"));
                }
            }
            _ => return Err(CompileError::unsupported("non-constant global initializer")),
        }
    } else {
        match ty {
            WasmType::I32 => GlobalInit::I32(0),
            WasmType::F64 => GlobalInit::F64(0.0),
            _ => GlobalInit::I32(0),
        }
    };

    ctx.add_global(&name, ty, init);
    if mutable {
        ctx.mutable_globals.insert(name);
    }
    Ok(())
}

fn collect_enum(
    ctx: &mut ModuleContext,
    enum_decl: &TSEnumDeclaration,
) -> Result<(), CompileError> {
    let mut next_value: i32 = 0;

    for member in &enum_decl.body.members {
        let member_name = member.id.static_name();
        let name = format!("{}.{}", enum_decl.id.name.as_str(), member_name.as_str());

        let value = if let Some(init) = &member.initializer {
            match init {
                Expression::NumericLiteral(lit) => {
                    next_value = lit.value as i32 + 1;
                    lit.value as i32
                }
                Expression::UnaryExpression(un)
                    if matches!(un.operator, UnaryOperator::UnaryNegation) =>
                {
                    if let Expression::NumericLiteral(lit) = &un.argument {
                        let v = -(lit.value as i32);
                        next_value = v + 1;
                        v
                    } else {
                        return Err(CompileError::unsupported("non-constant enum initializer"));
                    }
                }
                _ => return Err(CompileError::unsupported("non-constant enum initializer")),
            }
        } else {
            let v = next_value;
            next_value += 1;
            v
        };

        ctx.add_global(&name, WasmType::I32, GlobalInit::I32(value));
    }

    Ok(())
}

fn register_class_methods(
    ctx: &mut ModuleContext,
    class_name: &str,
    class: &Class,
    bindings: Option<&TypeBindings>,
) -> Result<(), CompileError> {
    for element in &class.body.body {
        if let ClassElement::MethodDefinition(method) = element {
            let method_name_str = match &method.key {
                PropertyKey::StaticIdentifier(ident) => ident.name.as_str(),
                _ => return Err(CompileError::unsupported("computed method name")),
            };

            let func = &method.value;

            // Build params: this (i32) + declared params
            let mut params = vec![("this".to_string(), WasmType::I32)];
            for param in &func.params.items {
                let pname = match &param.pattern {
                    BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
                    _ => return Err(CompileError::unsupported("destructured method param")),
                };
                let pty = if let Some(ann) = &param.type_annotation {
                    types::resolve_type_annotation_with_unions(
                        ann,
                        &ctx.class_names,
                        bindings,
                        &ctx.non_i32_union_wasm_types,
                    )?
                } else {
                    return Err(CompileError::type_err(format!(
                        "method parameter '{pname}' requires type annotation"
                    )));
                };
                params.push((pname, pty));
            }

            let ret = if method.kind == MethodDefinitionKind::Constructor {
                // Constructor returns the this pointer
                WasmType::I32
            } else if let Some(ann) = &func.return_type {
                types::resolve_type_annotation_with_unions(
                    ann,
                    &ctx.class_names,
                    bindings,
                    &ctx.non_i32_union_wasm_types,
                )?
            } else {
                WasmType::Void
            };

            // Use a mangled name: ClassName$methodName
            let wasm_name = if method.kind == MethodDefinitionKind::Constructor {
                format!("{class_name}$constructor")
            } else {
                format!("{class_name}${method_name_str}")
            };

            let func_idx = ctx.register_func(&wasm_name, &params, ret, false)?;
            let key = format!("{class_name}.{method_name_str}");
            ctx.method_map.insert(key, (func_idx, ret));

            // Also register constructor specially
            if method.kind == MethodDefinitionKind::Constructor {
                let ctor_key = format!("{class_name}.constructor");
                ctx.method_map.insert(ctor_key, (func_idx, ret));
            }
        }
    }
    Ok(())
}

fn codegen_method<'a>(
    ctx: &ModuleContext,
    class_name: &str,
    method: &MethodDefinition<'a>,
    source: &'a str,
    bindings: Option<&TypeBindings>,
) -> Result<(wasm_encoder::Function, Vec<(u32, u32)>), CompileError> {
    let func = &method.value;
    let layout = ctx
        .class_registry
        .get(class_name)
        .ok_or_else(|| CompileError::codegen(format!("unknown class '{class_name}'")))?;

    // Build params: this (i32) + declared params
    let mut params = vec![("this".to_string(), WasmType::I32)];
    for param in &func.params.items {
        let pname = match &param.pattern {
            BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
            _ => return Err(CompileError::unsupported("destructured method param")),
        };
        let pty = if let Some(ann) = &param.type_annotation {
            types::resolve_type_annotation_with_unions(
                ann,
                &ctx.class_names,
                bindings,
                &ctx.non_i32_union_wasm_types,
            )?
        } else {
            return Err(CompileError::type_err(format!(
                "method param '{pname}' requires type annotation"
            )));
        };
        params.push((pname, pty));
    }

    let ret = if method.kind == MethodDefinitionKind::Constructor {
        WasmType::I32
    } else if let Some(ann) = &func.return_type {
        types::resolve_type_annotation_with_unions(
            ann,
            &ctx.class_names,
            bindings,
            &ctx.non_i32_union_wasm_types,
        )?
    } else {
        WasmType::Void
    };

    let mut func_ctx = FuncContext::new(ctx, &params, ret, source);
    // Mark `this` as referencing this class (mangled, for generic monomorphizations)
    func_ctx.this_class = Some(class_name.to_string());
    func_ctx.type_bindings = bindings.cloned();
    // Thread declared return class so `return {...};` resolves to it.
    if method.kind != MethodDefinitionKind::Constructor
        && let Some(ann) = &func.return_type
        && let Some(rc) = types::get_class_type_name_with_bindings(
            ann,
            bindings,
            Some(&ctx.shape_registry),
            Some(&ctx.union_registry),
        )
        && ctx.class_names.contains(&rc)
    {
        func_ctx.return_class = Some(rc);
    }

    // Track class types, array types, closure sigs, and string types for method parameters
    for param in &func.params.items {
        let pname = match &param.pattern {
            BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
            _ => continue,
        };
        if let Some(ann) = &param.type_annotation {
            if let Some(sig) = types::get_closure_sig(ann, &ctx.class_names, &ctx.non_i32_union_wasm_types) {
                func_ctx.local_closure_sigs.insert(pname.clone(), sig);
            }
            if let Some(param_class) = types::get_class_type_name_with_bindings(
                ann,
                bindings,
                Some(&ctx.shape_registry),
                Some(&ctx.union_registry),
            ) && ctx.class_names.contains(&param_class)
            {
                func_ctx
                    .local_class_types
                    .insert(pname.clone(), param_class);
            }
            if let Some(elem_ty) = types::get_array_element_type(ann, &ctx.class_names, &ctx.non_i32_union_wasm_types) {
                func_ctx
                    .local_array_elem_types
                    .insert(pname.clone(), elem_ty);
                if let Some(elem_class) = types::get_array_element_class_with_bindings(
                    ann,
                    bindings,
                    Some(&ctx.shape_registry),
                    Some(&ctx.union_registry),
                ) {
                    func_ctx
                        .local_array_elem_classes
                        .insert(pname.clone(), elem_class);
                }
            }
            if types::is_string_type_with_bindings(ann, bindings) {
                func_ctx.local_string_vars.insert(pname.clone());
            }
        }
    }

    // Analyze which variables need boxing (captured + mutated)
    if let Some(body) = &func.body {
        func_ctx.boxed_vars = super::func::analyze_boxed_vars(&body.statements);
    }

    if method.kind == MethodDefinitionKind::Constructor {
        // Constructor body: first store fields from constructor args, then user body
        emit_constructor_body(&mut func_ctx, layout, func)?;
    } else if let Some(body) = &func.body {
        for stmt in &body.statements {
            func_ctx.emit_statement(stmt)?;
        }
    }

    Ok(func_ctx.finish())
}

/// Check if a list of statements contains a super() call (top-level only, not nested in functions).
fn has_super_call(stmts: &[Statement]) -> bool {
    for stmt in stmts {
        if let Statement::ExpressionStatement(expr_stmt) = stmt
            && let Expression::CallExpression(call) = &expr_stmt.expression
            && matches!(&call.callee, Expression::Super(_))
        {
            return true;
        }
    }
    false
}

fn emit_constructor_body<'a>(
    func_ctx: &mut FuncContext<'a>,
    layout: &super::classes::ClassLayout,
    func: &Function<'a>,
) -> Result<(), CompileError> {
    use wasm_encoder::Instruction;

    // Validate: child classes MUST call super()
    if layout.parent.is_some() {
        let body_has_super = func
            .body
            .as_ref()
            .is_some_and(|b| has_super_call(&b.statements));
        if !body_has_super {
            return Err(CompileError::codegen(format!(
                "constructor of class '{}' extends '{}' but does not call super()",
                layout.name,
                layout.parent.as_deref().unwrap()
            )));
        }
    }

    // The constructor has `this` at local 0 (already pointing to allocated memory).
    // Store each constructor argument to the corresponding field.
    // Constructor params (after this) map to fields by name.
    // For child classes, only auto-store OWN fields (parent fields are stored via super()).
    for param in &func.params.items {
        let pname = match &param.pattern {
            BindingPattern::BindingIdentifier(ident) => ident.name.as_str(),
            _ => continue,
        };
        // Skip inherited fields — they should be set by super() call
        if layout.parent.is_some() && !layout.own_field_names.contains(pname) {
            continue;
        }
        if let Some(&(offset, ty)) = layout.field_map.get(pname) {
            // Store param value to this + offset
            func_ctx.push(Instruction::LocalGet(0)); // this pointer
            let param_idx = func_ctx
                .locals
                .get(pname)
                .map(|&(idx, _)| idx)
                .ok_or_else(|| {
                    CompileError::codegen(format!("constructor param '{pname}' not found"))
                })?;
            func_ctx.push(Instruction::LocalGet(param_idx));
            match ty {
                WasmType::F64 => {
                    func_ctx.push(Instruction::F64Store(wasm_encoder::MemArg {
                        offset: offset as u64,
                        align: 3,
                        memory_index: 0,
                    }));
                }
                WasmType::I32 => {
                    func_ctx.push(Instruction::I32Store(wasm_encoder::MemArg {
                        offset: offset as u64,
                        align: 2,
                        memory_index: 0,
                    }));
                }
                _ => {}
            }
        }
    }

    // Execute the constructor body (for any additional statements like `this.field = expr`)
    if let Some(body) = &func.body {
        for stmt in &body.statements {
            func_ctx.emit_statement(stmt)?;
        }
    }

    // Return this pointer
    func_ctx.push(Instruction::LocalGet(0));
    func_ctx.push(Instruction::Return);

    Ok(())
}

fn codegen_function<'a>(
    ctx: &ModuleContext,
    func_decl: &Function<'a>,
    source: &'a str,
    bindings: Option<&TypeBindings>,
) -> Result<(wasm_encoder::Function, Vec<(u32, u32)>), CompileError> {
    if func_decl.declare {
        return Err(CompileError::codegen("cannot codegen declare function"));
    }

    let (params, ret) = extract_func_signature(
        func_decl,
        &ctx.class_names,
        bindings,
        &ctx.non_i32_union_wasm_types,
    )?;
    let mut func_ctx = FuncContext::new(ctx, &params, ret, source);
    func_ctx.type_bindings = bindings.cloned();
    // Thread declared return class so `return {...};` resolves to it.
    if let Some(ann) = &func_decl.return_type
        && let Some(rc) = types::get_class_type_name_with_bindings(
            ann,
            bindings,
            Some(&ctx.shape_registry),
            Some(&ctx.union_registry),
        )
        && ctx.class_names.contains(&rc)
    {
        func_ctx.return_class = Some(rc);
    }

    // Track class types, array types, closure sigs, and string types for function parameters
    for param in &func_decl.params.items {
        let pname = match &param.pattern {
            BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
            _ => continue,
        };
        if let Some(ann) = &param.type_annotation {
            if let Some(sig) = types::get_closure_sig(ann, &ctx.class_names, &ctx.non_i32_union_wasm_types) {
                func_ctx.local_closure_sigs.insert(pname.clone(), sig);
            }
            if let Some(param_class) = types::get_class_type_name_with_bindings(
                ann,
                bindings,
                Some(&ctx.shape_registry),
                Some(&ctx.union_registry),
            ) && ctx.class_names.contains(&param_class)
            {
                func_ctx
                    .local_class_types
                    .insert(pname.clone(), param_class);
            }
            if let Some(elem_ty) = types::get_array_element_type(ann, &ctx.class_names, &ctx.non_i32_union_wasm_types) {
                func_ctx
                    .local_array_elem_types
                    .insert(pname.clone(), elem_ty);
                if let Some(elem_class) = types::get_array_element_class_with_bindings(
                    ann,
                    bindings,
                    Some(&ctx.shape_registry),
                    Some(&ctx.union_registry),
                ) {
                    func_ctx
                        .local_array_elem_classes
                        .insert(pname.clone(), elem_class);
                }
            }
            if types::is_string_type_with_bindings(ann, bindings) {
                func_ctx.local_string_vars.insert(pname.clone());
            }
        }
    }

    if let Some(body) = &func_decl.body {
        // Analyze which variables need boxing (captured + mutated)
        func_ctx.boxed_vars = super::func::analyze_boxed_vars(&body.statements);
        for stmt in &body.statements {
            func_ctx.emit_statement(stmt)?;
        }
    }

    Ok(func_ctx.finish())
}
