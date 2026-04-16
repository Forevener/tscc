use wasm_encoder::{
    CodeSection, DataSection, ElementSection, Elements, ExportKind, ExportSection, FunctionSection,
    GlobalSection, GlobalType, ImportSection, MemorySection, MemoryType, Module, NameMap,
    NameSection, TableSection, TableType, TypeSection, ValType,
};

use std::collections::HashMap;

use crate::error::CompileError;

use super::module::{GlobalInit, ModuleContext};
pub(crate) fn assemble_module(
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
        type_section
            .ty()
            .function(params.iter().copied(), results.iter().copied());
    }
    module.section(&type_section);

    // Import section
    if !ctx.imports.is_empty() {
        let mut import_section = ImportSection::new();
        for (module_name, func_name, type_idx) in &ctx.imports {
            import_section.import(
                module_name,
                func_name,
                wasm_encoder::EntityType::Function(*type_idx),
            );
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
                || idx_to_name
                    .get(&(i as u32))
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
                        GlobalType {
                            val_type: ValType::I32,
                            mutable,
                            shared: false,
                        },
                        &wasm_encoder::ConstExpr::i32_const(v),
                    );
                }
                GlobalInit::I64(v) => {
                    global_section.global(
                        GlobalType {
                            val_type: ValType::I64,
                            mutable,
                            shared: false,
                        },
                        &wasm_encoder::ConstExpr::i64_const(v),
                    );
                }
                GlobalInit::F64(v) => {
                    global_section.global(
                        GlobalType {
                            val_type: ValType::F64,
                            mutable,
                            shared: false,
                        },
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
            let func_idx = ctx
                .func_map
                .get(mangled_name)
                .map(|&(idx, _)| idx)
                .unwrap_or_else(|| {
                    panic!("vtable method '{}' not found in func_map", mangled_name)
                });
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
