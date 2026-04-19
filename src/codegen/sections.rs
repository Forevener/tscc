use wasm_encoder::{
    CodeSection, DataSection, ElementSection, Elements, ExportKind, ExportSection, FunctionSection,
    GlobalSection, GlobalType, ImportSection, MemorySection, MemoryType, Module, NameMap,
    NameSection, TableSection, TableType, TypeSection, ValType,
};

use std::collections::HashMap;

use crate::error::CompileError;

use super::module::{GlobalInit, ModuleContext};
use super::precompiled;
use super::string_builtins::HelperRegistration;

pub(crate) fn assemble_module(
    ctx: &ModuleContext,
    compiled_funcs: &[(wasm_encoder::Function, Vec<(u32, u32)>)],
    memory_pages: u32,
    source: &str,
    debug: bool,
    filename: &str,
    helper_reg: &HelperRegistration,
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

    // Table section (vtable methods, closures, and helper call_indirect targets).
    //
    // Layout: table 0 is tscc's (methods + closures). Table 1, when present,
    // is the helpers' — rustc's `call_indirect` emissions in helper bodies are
    // byte-rewritten from table 0 → table 1 by `precompiled::build_function`,
    // so the two tables never share indices.
    let num_method_table_entries = ctx.method_table_indices.len() as u64;
    let has_tscc_table = has_closures || num_method_table_entries > 0;
    let has_helper_table = helper_reg.requires_table && precompiled::PRECOMPILED_TABLE.is_some();
    let helper_table_size = if has_helper_table {
        precompiled::PRECOMPILED_TABLE.as_ref().unwrap().min as u64
    } else {
        0
    };
    if has_tscc_table || has_helper_table {
        let mut table_section = TableSection::new();
        // Table 0: always present when either side needs a table (so helpers
        // can rely on their renumbered table index 1 existing).
        let tscc_table_size = num_method_table_entries + closure_funcs.len() as u64;
        table_section.table(TableType {
            element_type: wasm_encoder::RefType::FUNCREF,
            minimum: tscc_table_size,
            maximum: Some(tscc_table_size),
            table64: false,
            shared: false,
        });
        if has_helper_table {
            table_section.table(TableType {
                element_type: wasm_encoder::RefType::FUNCREF,
                minimum: helper_table_size,
                maximum: Some(helper_table_size),
                table64: false,
                shared: false,
            });
        }
        module.section(&table_section);
    }

    // Memory section. When helper Data segments are pulled in, they live at
    // rustc's default `__data_end = 0x100000` (so 17+ pages minimum). The
    // caller-supplied `memory_pages` only gets raised, never lowered — a host
    // that requested a huge initial size still gets it.
    let helper_min_pages = if helper_reg.requires_data {
        precompiled::required_memory_pages()
    } else {
        0
    };
    let effective_memory_pages = memory_pages.max(helper_min_pages);

    // Collision guard: tscc packs static data from offset 0 upward. Helper
    // Data segments sit at rustc-assigned offsets (typically `0x100000+`). If
    // tscc's packed data ever reaches a helper segment, the two would clobber
    // each other. Check once here and surface a clear error.
    if helper_reg.requires_data {
        let tscc_high_water = ctx.static_data_ptr.get();
        let helper_low_water = precompiled::PRECOMPILED_DATA
            .iter()
            .map(|d| d.offset)
            .min()
            .unwrap_or(u32::MAX);
        if tscc_high_water > helper_low_water {
            return Err(CompileError::codegen(format!(
                "static data (high water {tscc_high_water:#x}) overlaps helper Data segment \
                 at {helper_low_water:#x}; reduce compile-time constants or split the program"
            )));
        }
    }

    let mut mem_section = MemorySection::new();
    mem_section.memory(MemoryType {
        minimum: effective_memory_pages as u64,
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

    // Element section (populates the function tables).
    if has_tscc_table || has_helper_table {
        let mut elem_section = ElementSection::new();

        // --- Table 0 (tscc): method entries then closure entries ---
        if has_tscc_table {
            let mut all_table_func_indices: Vec<u32> =
                vec![0; num_method_table_entries as usize];

            for (mangled_name, &table_idx) in &ctx.method_table_indices {
                let func_idx = ctx
                    .func_map
                    .get(mangled_name)
                    .map(|&(idx, _)| idx)
                    .unwrap_or_else(|| {
                        panic!("vtable method '{}' not found in func_map", mangled_name)
                    });
                all_table_func_indices[table_idx as usize] = func_idx;
            }

            let closure_func_base = ctx.imports.len() as u32 + ctx.local_funcs.len() as u32;
            for i in 0..closure_funcs.len() as u32 {
                all_table_func_indices.push(closure_func_base + i);
            }

            elem_section.active(
                Some(0),
                &wasm_encoder::ConstExpr::i32_const(0),
                Elements::Functions(std::borrow::Cow::Borrowed(&all_table_func_indices)),
            );
        } else if has_helper_table {
            // Table 0 is empty but must still exist because helpers' renumbered
            // table index is 1. Nothing to emit here — an empty table needs no
            // element segment.
        }

        // --- Table 1 (helpers): element segments copied from the bundle ---
        // Rustc uses active element segments with MVP Functions form. We copy
        // the offset and function-index list, remapping every function index
        // through the bundle registration map (bundle idx → tscc func idx).
        // Segments that reference bundle indices which weren't registered get
        // skipped entirely — such entries represent table slots that point
        // at functions no registered helper can reach, so leaving them as 0
        // (the default-init state of a funcref slot) is correct. The dynamic-
        // dispatch expansion in `precompiled::expand_with_dynamic_dispatch`
        // already pulls in every function reachable from table 1 when any
        // helper uses `call_indirect`, so a skip here should be rare.
        if has_helper_table {
            // `seg.func_indices` lives in helper FULL function space (imports
            // first, then internals). `func_index_map` is sized to match that
            // exact layout, so we can look up directly without the bundle-
            // index offset dance.
            for seg in precompiled::PRECOMPILED_ELEMENTS {
                let remapped: Vec<u32> = seg
                    .func_indices
                    .iter()
                    .map(|&full_helper_idx| {
                        let tscc_idx =
                            helper_reg.func_index_map[full_helper_idx as usize];
                        if tscc_idx == u32::MAX {
                            // Point unregistered slots at a sentinel (func 0).
                            // The slot is unreachable by construction; this
                            // just keeps the wasm valid.
                            0
                        } else {
                            tscc_idx
                        }
                    })
                    .collect();
                elem_section.active(
                    Some(1),
                    &wasm_encoder::ConstExpr::i32_const(seg.offset as i32),
                    Elements::Functions(std::borrow::Cow::Owned(remapped)),
                );
            }
        }

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

    // Data section (string literals and other static data — both tscc-emitted
    // and helper-bundled). Helper Data segments are emitted verbatim at their
    // original rustc-assigned offsets (typically `0x100000+`); the collision
    // guard above ensures tscc's packed-from-0 data does not reach them.
    let static_entries = ctx.static_data_entries.borrow();
    let emit_helper_data = helper_reg.requires_data;
    if !static_entries.is_empty() || emit_helper_data {
        let mut data_section = DataSection::new();
        for (offset, bytes) in static_entries.iter() {
            data_section.active(
                0,
                &wasm_encoder::ConstExpr::i32_const(*offset as i32),
                bytes.iter().copied(),
            );
        }
        if emit_helper_data {
            for seg in precompiled::PRECOMPILED_DATA {
                data_section.active(
                    0,
                    &wasm_encoder::ConstExpr::i32_const(seg.offset as i32),
                    seg.bytes.iter().copied(),
                );
            }
        }
        module.section(&data_section);
    }

    // Name section (always emit — cheap and useful for stack traces)
    {
        let mut names = NameSection::new();
        let mut func_names = NameMap::new();
        // local_funcs[i] lives at wasm index imports.len() + i. Can't use
        // func_map here because register_raw_func (precompiled-helper internals)
        // deliberately skips it.
        let imports_len = ctx.imports.len() as u32;
        for (i, func_def) in ctx.local_funcs.iter().enumerate() {
            let func_idx = imports_len + i as u32;
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
