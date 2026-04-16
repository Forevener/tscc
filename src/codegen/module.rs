use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use oxc_ast::ast::*;
use wasm_encoder::{
    CodeSection, DataSection, ElementSection, Elements, ExportKind, ExportSection, FunctionSection,
    GlobalSection, GlobalType, ImportSection, MemorySection, MemoryType, Module, NameMap,
    NameSection, TableSection, TableType, TypeSection, ValType,
};

use crate::classes::ClassRegistry;
use crate::error::CompileError;
use crate::types::{self, ClosureSig, WasmType};
use crate::ArenaOverflow;

use super::func::FuncContext;
use super::wasm_types;

/// Key identifying a unique WASM function signature: (param types, result types).
type TypeSigKey = (Vec<ValType>, Vec<ValType>);

pub struct ModuleContext {
    host_module: String,
    /// Map from function name to (wasm_func_index, return_type)
    func_map: HashMap<String, (u32, WasmType)>,
    /// Map from global name to (wasm_global_index, type)
    pub globals: HashMap<String, (u32, WasmType)>,
    /// Names of globals declared with `let` (mutable in WASM)
    pub mutable_globals: HashSet<String>,
    /// Function type signatures: (params, results)
    type_sigs: Vec<TypeSigKey>,
    /// Index lookup for `type_sigs` (O(1) dedup during pre-codegen registration)
    type_sig_index: HashMap<TypeSigKey, u32>,
    /// Import entries: (module, name, type_index)
    imports: Vec<(String, String, u32)>,
    /// Local function entries
    local_funcs: Vec<FuncDef>,
    next_func_index: u32,
    next_type_index: u32,
    next_global_index: u32,
    /// Ordered init values for globals
    pub global_inits: Vec<GlobalInit>,
    /// Bump pointer for compile-time static data allocation
    static_data_ptr: Cell<u32>,
    /// Class layouts and metadata
    pub class_registry: ClassRegistry,
    /// Set of known class names (for type resolution)
    pub class_names: HashSet<String>,
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
    extra_type_sigs: RefCell<Vec<TypeSigKey>>,
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

struct FuncDef {
    name: String,
    type_index: u32,
    is_export: bool,
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
        self.globals.insert("__arena_ptr".to_string(), (idx, WasmType::I32));
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
        self.string_literal_offsets.borrow_mut().insert(s.to_string(), offset);
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

    pub fn add_import(&mut self, name: &str, params: &[WasmType], ret: WasmType) -> Result<u32, CompileError> {
        let wasm_params = wasm_types::wasm_params(params);
        let wasm_results = wasm_types::wasm_results(ret);
        let type_idx = self.add_type_sig(wasm_params, wasm_results);
        let func_idx = self.next_func_index;
        self.imports.push((self.host_module.clone(), name.to_string(), type_idx));
        self.func_map.insert(name.to_string(), (func_idx, ret));
        self.next_func_index += 1;
        Ok(func_idx)
    }

    pub fn register_func(&mut self, name: &str, params: &[(String, WasmType)], ret: WasmType, is_export: bool) -> Result<u32, CompileError> {
        let wasm_params: Vec<ValType> = params.iter().filter_map(|(_, ty)| ty.to_val_type()).collect();
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
    pub fn register_closure_func(&self, param_types: Vec<ValType>, result_types: Vec<ValType>, body: wasm_encoder::Function, source_map: Vec<(u32, u32)>) -> u32 {
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

pub fn compile_module<'a>(
    program: &Program<'a>,
    host_module: &str,
    memory_pages: u32,
    source: &'a str,
    debug: bool,
    filename: &str,
    arena_overflow: ArenaOverflow,
) -> Result<Vec<u8>, CompileError> {
    let mut ctx = ModuleContext::new(host_module);
    ctx.arena_overflow = arena_overflow;

    // Pass 0a: collect class names and extends relationships
    let mut class_info: Vec<(String, Option<String>)> = Vec::new();
    let mut class_ast_map: HashMap<String, &Class> = HashMap::new();
    for stmt in &program.body {
        if let Statement::ClassDeclaration(class) = stmt {
            let name = class.id.as_ref()
                .ok_or_else(|| CompileError::parse("class without name"))?
                .name.as_str().to_string();
            let parent = class.super_class.as_ref().map(|expr| {
                match expr {
                    Expression::Identifier(id) => Ok(id.name.as_str().to_string()),
                    _ => Err(CompileError::unsupported("non-identifier in extends clause")),
                }
            }).transpose()?;
            ctx.class_names.insert(name.clone());
            class_info.push((name.clone(), parent));
            class_ast_map.insert(name, class);
        }
    }

    // Pass 0b: determine which classes are polymorphic and topological order
    let polymorphic = crate::classes::find_polymorphic_classes(&class_info);
    let sorted_classes = crate::classes::topo_sort_classes(&class_info)?;

    // Pass 0c: register class layouts in dependency order (parent before child)
    for (name, parent) in &sorted_classes {
        let class = class_ast_map[name.as_str()];
        let is_poly = polymorphic.contains(name);
        ctx.class_registry.register_class(class, &ctx.class_names, parent.clone(), is_poly)?;
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
    // Methods get an implicit `this: i32` first parameter
    for stmt in &program.body {
        if let Statement::ClassDeclaration(class) = stmt {
            register_class_methods(&mut ctx, class)?;
        }
    }

    // Pass 1c: build vtables for polymorphic classes
    // Assign WASM function table indices to methods of polymorphic classes,
    // then build vtable data in static memory.
    {
        // Helper: find the declaring class for a method by walking method_map (parent chain).
        fn find_method_declarer(
            method_map: &HashMap<String, (u32, WasmType)>,
            registry: &crate::classes::ClassRegistry,
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
                let owner = find_method_declarer(&ctx.method_map, &ctx.class_registry, class_name, method_name)
                    .ok_or_else(|| CompileError::codegen(format!(
                        "vtable method '{method_name}' not found in hierarchy of '{class_name}'"
                    )))?;
                let mangled = format!("{owner}${method_name}");
                if !ctx.method_table_indices.contains_key(&mangled) {
                    let table_idx = ctx.next_table_index.get();
                    ctx.next_table_index.set(table_idx + 1);
                    ctx.method_table_indices.insert(mangled, table_idx);
                }
            }
        }

        // Build vtable data for each polymorphic class, collect offsets
        let mut vtable_offsets: Vec<(String, u32)> = Vec::new();
        for (class_name, _parent) in &sorted_classes {
            if !polymorphic.contains(class_name) {
                continue;
            }
            let layout = ctx.class_registry.get(class_name).unwrap().clone();
            let num_methods = layout.vtable_methods.len();
            if num_methods == 0 {
                continue;
            }

            let vtable_offset = ctx.alloc_static((num_methods * 4) as u32);

            let mut vtable_data = Vec::with_capacity(num_methods * 4);
            for method_name in &layout.vtable_methods {
                let owner = find_method_declarer(&ctx.method_map, &ctx.class_registry, class_name, method_name).unwrap();
                let mangled = format!("{owner}${method_name}");
                let table_idx = ctx.method_table_indices[&mangled];
                vtable_data.extend_from_slice(&(table_idx as i32).to_le_bytes());
            }
            ctx.static_data_entries.borrow_mut().push((vtable_offset, vtable_data));
            vtable_offsets.push((class_name.clone(), vtable_offset));
        }

        // Apply vtable offsets to class layouts
        for (class_name, vtable_offset) in vtable_offsets {
            ctx.class_registry.classes.get_mut(&class_name).unwrap().vtable_offset = vtable_offset;
        }
    }

    // Pass 2: register all free functions (get indices)
    for stmt in &program.body {
        match stmt {
            Statement::FunctionDeclaration(func_decl) if !func_decl.declare => {
                register_function(&mut ctx, func_decl, false)?;
            }
            Statement::ExportDefaultDeclaration(export) => {
                if let ExportDefaultDeclarationKind::FunctionDeclaration(func_decl) = &export.declaration {
                    let name = func_decl.id.as_ref()
                        .map(|id| id.name.as_str())
                        .unwrap_or("default");
                    register_func_from_decl(&mut ctx, func_decl, name, true)?;
                }
            }
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration
                    && let Declaration::FunctionDeclaration(func_decl) = decl {
                        register_function(&mut ctx, func_decl, true)?;
                    }
            }
            _ => {}
        }
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
    // "register all 21, stub the unused ones" approach.
    let used_string_helpers = super::string_builtins::collect_used_helpers(program);
    if !used_string_helpers.is_empty() {
        ctx.init_arena();
    }
    super::string_builtins::register_string_helpers(&mut ctx, &used_string_helpers);

    // Register RNG helpers (state global + step function) if Math.random is used.
    let uses_random = super::math_builtins::program_uses_random(program);
    if uses_random {
        super::math_builtins::register_rng(&mut ctx);
    }

    // Pass 3: codegen — class methods first, then free functions
    // Each entry: (compiled_function, source_map)
    let mut compiled_funcs: Vec<(wasm_encoder::Function, Vec<(u32, u32)>)> = Vec::new();

    for stmt in &program.body {
        if let Statement::ClassDeclaration(class) = stmt {
            let class_name = class.id.as_ref().unwrap().name.as_str();
            for element in &class.body.body {
                if let ClassElement::MethodDefinition(method) = element {
                    compiled_funcs.push(codegen_method(&ctx, class_name, method, source)?);
                }
            }
        }
    }

    for stmt in &program.body {
        match stmt {
            Statement::FunctionDeclaration(func_decl) if !func_decl.declare => {
                compiled_funcs.push(codegen_function(&ctx, func_decl, source)?);
            }
            Statement::ExportDefaultDeclaration(export) => {
                if let ExportDefaultDeclarationKind::FunctionDeclaration(func_decl) = &export.declaration {
                    compiled_funcs.push(codegen_function(&ctx, func_decl, source)?);
                }
            }
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration
                    && let Declaration::FunctionDeclaration(func_decl) = decl {
                        compiled_funcs.push(codegen_function(&ctx, func_decl, source)?);
                    }
            }
            _ => {}
        }
    }

    // Compile __arena_alloc body if registered
    if ctx.arena_alloc_func.is_some() {
        compiled_funcs.push((compile_arena_alloc(&ctx), Vec::new()));
    }

    // Compile the string runtime helper bodies (only those that were registered).
    let string_helpers = super::string_builtins::compile_string_helpers(&ctx, &used_string_helpers);
    compiled_funcs.extend(string_helpers.into_iter().map(|f| (f, Vec::new())));

    // Compile RNG step function body if Math.random is used.
    if uses_random {
        compiled_funcs.push((super::math_builtins::compile_rng_next(&ctx), Vec::new()));
    }

    // Assemble the WASM module
    assemble_module(&ctx, &compiled_funcs, memory_pages, source, debug, filename)
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

fn collect_import_from_func(ctx: &mut ModuleContext, func_decl: &Function) -> Result<(), CompileError> {
    let name = func_decl.id.as_ref()
        .ok_or_else(|| CompileError::parse("declare function without name"))?
        .name.as_str();

    let (params, ret) = extract_func_signature(func_decl, &ctx.class_names)?;
    let param_types: Vec<WasmType> = params.iter().map(|(_, ty)| *ty).collect();
    ctx.add_import(name, &param_types, ret)?;
    Ok(())
}

fn register_function(ctx: &mut ModuleContext, func_decl: &Function, is_export: bool) -> Result<(), CompileError> {
    if func_decl.declare {
        return Ok(());
    }
    let name = func_decl.id.as_ref()
        .ok_or_else(|| CompileError::parse("function without name"))?
        .name.as_str();

    register_func_from_decl(ctx, func_decl, name, is_export)
}

fn register_func_from_decl(ctx: &mut ModuleContext, func_decl: &Function, name: &str, is_export: bool) -> Result<(), CompileError> {
    let (params, ret) = extract_func_signature(func_decl, &ctx.class_names)?;
    // Track if this function returns a closure
    if let Some(ann) = &func_decl.return_type {
        if let Some(sig) = types::get_closure_sig(ann, &ctx.class_names) {
            ctx.func_return_closure_sigs.insert(name.to_string(), sig);
        }
        // Track if this function returns a string
        if types::is_string_type(ann) {
            ctx.func_return_strings.insert(name.to_string());
        }
    }
    ctx.register_func(name, &params, ret, is_export)?;
    Ok(())
}

fn extract_func_signature(func_decl: &Function, class_names: &HashSet<String>) -> Result<(Vec<(String, WasmType)>, WasmType), CompileError> {
    let mut params = Vec::new();
    for param in &func_decl.params.items {
        let name = match &param.pattern {
            BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
            _ => return Err(CompileError::unsupported("destructured parameter")),
        };
        let ty = if let Some(ann) = &param.type_annotation {
            types::resolve_type_annotation_with_classes(ann, class_names)?
        } else {
            return Err(CompileError::type_err(format!(
                "parameter '{name}' requires a type annotation — tscc does not infer parameter types; write `{name}: i32` (or f64, bool, string, or a class name)"
            )));
        };
        params.push((name, ty));
    }

    let ret = if let Some(ann) = &func_decl.return_type {
        types::resolve_type_annotation_with_classes(ann, class_names)?
    } else {
        WasmType::Void
    };

    Ok((params, ret))
}

fn collect_global(ctx: &mut ModuleContext, decl: &VariableDeclarator, mutable: bool) -> Result<(), CompileError> {
    let name = match &decl.id {
        BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
        _ => return Err(CompileError::unsupported("destructured global")),
    };

    let ty = if let Some(ann) = &decl.type_annotation {
        types::resolve_type_annotation(ann)?
    } else {
        return Err(CompileError::type_err(format!("global '{name}' requires type annotation")));
    };

    let init = if let Some(init_expr) = &decl.init {
        match init_expr {
            Expression::NumericLiteral(lit) => {
                match ty {
                    WasmType::I32 => GlobalInit::I32(lit.value as i32),
                    WasmType::F64 => GlobalInit::F64(lit.value),
                    _ => return Err(CompileError::type_err("global must be i32 or f64")),
                }
            }
            Expression::UnaryExpression(un) if matches!(un.operator, UnaryOperator::UnaryNegation) => {
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
            Expression::BooleanLiteral(lit) => {
                GlobalInit::I32(if lit.value { 1 } else { 0 })
            }
            Expression::CallExpression(call) => {
                // Handle __static_alloc(size) at compile time
                if let Expression::Identifier(ident) = &call.callee {
                    if ident.name.as_str() == "__static_alloc" && call.arguments.len() == 1 {
                        if let Expression::NumericLiteral(lit) = &call.arguments[0].to_expression() {
                            let size = lit.value as u32;
                            let offset = ctx.alloc_static(size);
                            GlobalInit::I32(offset as i32)
                        } else {
                            return Err(CompileError::codegen("__static_alloc size must be a numeric literal"));
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

fn collect_enum(ctx: &mut ModuleContext, enum_decl: &TSEnumDeclaration) -> Result<(), CompileError> {
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
                Expression::UnaryExpression(un) if matches!(un.operator, UnaryOperator::UnaryNegation) => {
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

fn register_class_methods(ctx: &mut ModuleContext, class: &Class) -> Result<(), CompileError> {
    let class_name = class.id.as_ref().unwrap().name.as_str();

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
                    types::resolve_type_annotation_with_classes(ann, &ctx.class_names)?
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
                types::resolve_type_annotation_with_classes(ann, &ctx.class_names)?
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

fn codegen_method<'a>(ctx: &ModuleContext, class_name: &str, method: &MethodDefinition<'a>, source: &'a str) -> Result<(wasm_encoder::Function, Vec<(u32, u32)>), CompileError> {
    let func = &method.value;
    let layout = ctx.class_registry.get(class_name)
        .ok_or_else(|| CompileError::codegen(format!("unknown class '{class_name}'")))?;

    // Build params: this (i32) + declared params
    let mut params = vec![("this".to_string(), WasmType::I32)];
    for param in &func.params.items {
        let pname = match &param.pattern {
            BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
            _ => return Err(CompileError::unsupported("destructured method param")),
        };
        let pty = if let Some(ann) = &param.type_annotation {
            types::resolve_type_annotation_with_classes(ann, &ctx.class_names)?
        } else {
            return Err(CompileError::type_err(format!("method param '{pname}' requires type annotation")));
        };
        params.push((pname, pty));
    }

    let ret = if method.kind == MethodDefinitionKind::Constructor {
        WasmType::I32
    } else if let Some(ann) = &func.return_type {
        types::resolve_type_annotation_with_classes(ann, &ctx.class_names)?
    } else {
        WasmType::Void
    };

    let mut func_ctx = FuncContext::new(ctx, &params, ret, source);
    // Mark `this` as referencing this class
    func_ctx.this_class = Some(class_name.to_string());

    // Track class types, array types, closure sigs, and string types for method parameters
    for param in &func.params.items {
        let pname = match &param.pattern {
            BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
            _ => continue,
        };
        if let Some(ann) = &param.type_annotation {
            if let Some(sig) = types::get_closure_sig(ann, &ctx.class_names) {
                func_ctx.local_closure_sigs.insert(pname.clone(), sig);
            }
            if let Some(param_class) = types::get_class_type_name(ann)
                && ctx.class_names.contains(&param_class) {
                    func_ctx.local_class_types.insert(pname.clone(), param_class);
                }
            if let Some(elem_ty) = types::get_array_element_type(ann, &ctx.class_names) {
                func_ctx.local_array_elem_types.insert(pname.clone(), elem_ty);
                if let Some(elem_class) = types::get_array_element_class(ann) {
                    func_ctx.local_array_elem_classes.insert(pname.clone(), elem_class);
                }
            }
            if types::is_string_type(ann) {
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
                && matches!(&call.callee, Expression::Super(_)) {
                    return true;
                }
    }
    false
}

fn emit_constructor_body<'a>(
    func_ctx: &mut FuncContext<'a>,
    layout: &crate::classes::ClassLayout,
    func: &Function<'a>,
) -> Result<(), CompileError> {
    use wasm_encoder::Instruction;

    // Validate: child classes MUST call super()
    if layout.parent.is_some() {
        let body_has_super = func.body.as_ref()
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
            let param_idx = func_ctx.locals.get(pname).map(|&(idx, _)| idx)
                .ok_or_else(|| CompileError::codegen(format!("constructor param '{pname}' not found")))?;
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

fn codegen_function<'a>(ctx: &ModuleContext, func_decl: &Function<'a>, source: &'a str) -> Result<(wasm_encoder::Function, Vec<(u32, u32)>), CompileError> {
    if func_decl.declare {
        return Err(CompileError::codegen("cannot codegen declare function"));
    }

    let (params, ret) = extract_func_signature(func_decl, &ctx.class_names)?;
    let mut func_ctx = FuncContext::new(ctx, &params, ret, source);

    // Track class types, array types, closure sigs, and string types for function parameters
    for param in &func_decl.params.items {
        let pname = match &param.pattern {
            BindingPattern::BindingIdentifier(ident) => ident.name.as_str().to_string(),
            _ => continue,
        };
        if let Some(ann) = &param.type_annotation {
            if let Some(sig) = types::get_closure_sig(ann, &ctx.class_names) {
                func_ctx.local_closure_sigs.insert(pname.clone(), sig);
            }
            if let Some(param_class) = types::get_class_type_name(ann)
                && ctx.class_names.contains(&param_class) {
                    func_ctx.local_class_types.insert(pname.clone(), param_class);
                }
            if let Some(elem_ty) = types::get_array_element_type(ann, &ctx.class_names) {
                func_ctx.local_array_elem_types.insert(pname.clone(), elem_ty);
                if let Some(elem_class) = types::get_array_element_class(ann) {
                    func_ctx.local_array_elem_classes.insert(pname.clone(), elem_class);
                }
            }
            if types::is_string_type(ann) {
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

fn assemble_module(
    ctx: &ModuleContext,
    compiled_funcs: &[(wasm_encoder::Function, Vec<(u32, u32)>)],
    memory_pages: u32,
    source: &str,
    debug: bool,
    filename: &str,
) -> Result<Vec<u8>, CompileError> {
    let mut module = Module::new();

    let closure_funcs = ctx.closure_funcs.borrow();
    let has_closures = !closure_funcs.is_empty();

    // Build combined type signatures: existing sigs + extra (call_indirect) sigs + closure-specific sigs
    let mut all_type_sigs = ctx.type_sigs.clone();
    all_type_sigs.extend(ctx.extra_type_sigs.borrow().iter().cloned());
    let mut closure_type_indices = Vec::new();
    for cf in closure_funcs.iter() {
        // Find or add the type sig for this closure
        let sig = (cf.param_types.clone(), cf.result_types.clone());
        let type_idx = if let Some(idx) = all_type_sigs.iter().position(|s| *s == sig) {
            idx as u32
        } else {
            let idx = all_type_sigs.len() as u32;
            all_type_sigs.push(sig);
            idx
        };
        closure_type_indices.push(type_idx);
    }

    // Type section
    let mut type_section = TypeSection::new();
    for (params, results) in &all_type_sigs {
        type_section.ty().function(
            params.iter().copied(),
            results.iter().copied(),
        );
    }
    module.section(&type_section);

    // Import section
    if !ctx.imports.is_empty() {
        let mut import_section = ImportSection::new();
        for (module_name, func_name, type_idx) in &ctx.imports {
            import_section.import(module_name, func_name, wasm_encoder::EntityType::Function(*type_idx));
        }
        module.section(&import_section);
    }

    // Function section (local functions + closure functions)
    let mut func_section = FunctionSection::new();
    for func_def in &ctx.local_funcs {
        func_section.function(func_def.type_index);
    }
    for &type_idx in &closure_type_indices {
        func_section.function(type_idx);
    }
    module.section(&func_section);

    // Table section (for vtable methods and/or closures)
    let num_method_table_entries = ctx.method_table_indices.len() as u64;
    let has_table = has_closures || num_method_table_entries > 0;
    if has_table {
        let total_table_size = num_method_table_entries + closure_funcs.len() as u64;
        let mut table_section = TableSection::new();
        table_section.table(TableType {
            element_type: wasm_encoder::RefType::FUNCREF,
            minimum: total_table_size,
            maximum: Some(total_table_size),
            table64: false,
            shared: false,
        });
        module.section(&table_section);
    }

    // Memory section
    let mut mem_section = MemorySection::new();
    mem_section.memory(MemoryType {
        minimum: memory_pages as u64,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    module.section(&mem_section);

    // Global section
    if !ctx.global_inits.is_empty() {
        // Build reverse map: global_index -> declared name (for mutability lookup)
        let mut idx_to_name: HashMap<u32, &str> = HashMap::new();
        for (name, &(idx, _)) in &ctx.globals {
            idx_to_name.insert(idx, name.as_str());
        }

        let mut global_section = GlobalSection::new();
        for (i, init) in ctx.global_inits.iter().enumerate() {
            // __arena_ptr is mutable (host resets it after each call)
            let is_arena_ptr = ctx.arena_ptr_global == Some(i as u32);
            let mutable = is_arena_ptr
                || idx_to_name.get(&(i as u32))
                    .is_some_and(|n| ctx.mutable_globals.contains(*n));

            // If this is __arena_ptr, set its initial value to after static data
            let init = if is_arena_ptr {
                let arena_start = ctx.static_data_ptr.get();
                let aligned = (arena_start + 7) & !7;
                GlobalInit::I32(aligned as i32)
            } else {
                match init {
                    GlobalInit::I32(v) => GlobalInit::I32(*v),
                    GlobalInit::I64(v) => GlobalInit::I64(*v),
                    GlobalInit::F64(v) => GlobalInit::F64(*v),
                }
            };

            match init {
                GlobalInit::I32(v) => {
                    global_section.global(
                        GlobalType { val_type: ValType::I32, mutable, shared: false },
                        &wasm_encoder::ConstExpr::i32_const(v),
                    );
                }
                GlobalInit::I64(v) => {
                    global_section.global(
                        GlobalType { val_type: ValType::I64, mutable, shared: false },
                        &wasm_encoder::ConstExpr::i64_const(v),
                    );
                }
                GlobalInit::F64(v) => {
                    global_section.global(
                        GlobalType { val_type: ValType::F64, mutable, shared: false },
                        &wasm_encoder::ConstExpr::f64_const(v),
                    );
                }
            }
        }
        module.section(&global_section);
    }

    // Export section
    let mut export_section = ExportSection::new();
    export_section.export("memory", ExportKind::Memory, 0);
    if let Some(arena_idx) = ctx.arena_ptr_global {
        export_section.export("__arena_ptr", ExportKind::Global, arena_idx);
    }
    for (name, idx) in &ctx.exported_globals {
        export_section.export(name, ExportKind::Global, *idx);
    }
    for func_def in &ctx.local_funcs {
        if func_def.is_export {
            let func_idx = ctx.func_map[&func_def.name].0;
            export_section.export(&func_def.name, ExportKind::Func, func_idx);
        }
    }
    module.section(&export_section);

    // Element section (populates the function table with method + closure func indices)
    if has_table {
        let mut elem_section = ElementSection::new();

        // Build combined table: method entries (slots 0..M-1) + closure entries (slots M..M+C-1)
        let mut all_table_func_indices: Vec<u32> = vec![0; num_method_table_entries as usize];

        // Fill method table entries: table_index -> wasm func_index
        for (mangled_name, &table_idx) in &ctx.method_table_indices {
            // mangled_name is "ClassName$methodName", look up the wasm func_index
            let func_idx = ctx.func_map.get(mangled_name)
                .map(|&(idx, _)| idx)
                .unwrap_or_else(|| panic!("vtable method '{}' not found in func_map", mangled_name));
            all_table_func_indices[table_idx as usize] = func_idx;
        }

        // Append closure entries
        let closure_func_base = ctx.imports.len() as u32 + ctx.local_funcs.len() as u32;
        for i in 0..closure_funcs.len() as u32 {
            all_table_func_indices.push(closure_func_base + i);
        }

        elem_section.active(
            Some(0), // table index 0
            &wasm_encoder::ConstExpr::i32_const(0),
            Elements::Functions(std::borrow::Cow::Borrowed(&all_table_func_indices)),
        );
        module.section(&elem_section);
    }

    // Code section (local functions + closure functions)
    let mut code_section = CodeSection::new();
    for (func, _source_map) in compiled_funcs {
        code_section.function(func);
    }
    for cf in closure_funcs.iter() {
        code_section.function(&cf.body);
    }
    module.section(&code_section);

    // Data section (string literals and other static data)
    let static_entries = ctx.static_data_entries.borrow();
    if !static_entries.is_empty() {
        let mut data_section = DataSection::new();
        for (offset, bytes) in static_entries.iter() {
            data_section.active(
                0, // memory index
                &wasm_encoder::ConstExpr::i32_const(*offset as i32),
                bytes.iter().copied(),
            );
        }
        module.section(&data_section);
    }

    // Name section (always emit — cheap and useful for stack traces)
    {
        let mut names = NameSection::new();
        let mut func_names = NameMap::new();
        for func_def in &ctx.local_funcs {
            let func_idx = ctx.func_map[&func_def.name].0;
            func_names.append(func_idx, &func_def.name);
        }
        // Also name imported functions
        for (_, func_name, _) in &ctx.imports {
            if let Some(&(idx, _)) = ctx.func_map.get(func_name) {
                func_names.append(idx, func_name);
            }
        }
        // Name closure functions: closure$0, closure$1, etc.
        let closure_func_base = ctx.imports.len() as u32 + ctx.local_funcs.len() as u32;
        for (i, _cf) in closure_funcs.iter().enumerate() {
            let func_idx = closure_func_base + i as u32;
            func_names.append(func_idx, &format!("closure${i}"));
        }
        names.functions(&func_names);
        module.section(&names);
    }

    let mut wasm_bytes = module.finish();

    // DWARF debug sections (only when debug mode is enabled)
    if debug {
        use super::dwarf;
        use crate::error::offset_to_loc;

        if let Some(code_info) = dwarf::find_code_section(&wasm_bytes) {
            // Build line mappings: (wasm_absolute_address, source_line, source_column)
            let mut line_mappings: Vec<(u32, u32, u32)> = Vec::new();
            let num_imports = ctx.imports.len();

            // Local functions (indices in compiled_funcs correspond to local_funcs)
            for (func_idx, (_func, source_map)) in compiled_funcs.iter().enumerate() {
                let wasm_func_idx = num_imports + func_idx;
                if wasm_func_idx >= code_info.func_body_offsets.len() {
                    continue;
                }
                let body_offset = code_info.func_body_offsets[wasm_func_idx] as u32;
                for &(byte_offset_in_body, src_byte_offset) in source_map {
                    let loc = offset_to_loc(source, src_byte_offset);
                    let wasm_addr = body_offset + byte_offset_in_body;
                    line_mappings.push((wasm_addr, loc.line, loc.col));
                }
            }

            // Closure functions
            let closure_base = num_imports + compiled_funcs.len();
            for (i, cf) in closure_funcs.iter().enumerate() {
                let wasm_func_idx = closure_base + i;
                if wasm_func_idx >= code_info.func_body_offsets.len() {
                    continue;
                }
                let body_offset = code_info.func_body_offsets[wasm_func_idx] as u32;
                for &(byte_offset_in_body, src_byte_offset) in &cf.source_map {
                    let loc = offset_to_loc(source, src_byte_offset);
                    let wasm_addr = body_offset + byte_offset_in_body;
                    line_mappings.push((wasm_addr, loc.line, loc.col));
                }
            }

            // Sort by address and deduplicate consecutive same-line+column entries
            line_mappings.sort_by_key(|&(addr, _, _)| addr);
            line_mappings.dedup_by(|b, a| a.1 == b.1 && a.2 == b.2);

            let code_start = code_info.func_body_offsets.first().copied().unwrap_or(0) as u32;
            let code_end = code_info.section_end as u32;

            // Build and append DWARF sections
            let debug_abbrev = dwarf::build_debug_abbrev();
            let debug_line = dwarf::build_debug_line(filename, &line_mappings, code_end);
            let debug_info = dwarf::build_debug_info(filename, 0, code_start, code_end);

            dwarf::append_custom_section(&mut wasm_bytes, ".debug_abbrev", &debug_abbrev);
            dwarf::append_custom_section(&mut wasm_bytes, ".debug_info", &debug_info);
            dwarf::append_custom_section(&mut wasm_bytes, ".debug_line", &debug_line);
        }
    }

    Ok(wasm_bytes)
}
