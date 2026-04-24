#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use wasmtime::*;

pub fn compile(source: &str) -> Vec<u8> {
    tscc::compile(source, &tscc::CompileOptions::default()).unwrap()
}

pub fn compile_err(source: &str) -> tscc::error::CompileError {
    tscc::compile(source, &tscc::CompileOptions::default()).unwrap_err()
}

pub fn compile_debug(source: &str) -> Vec<u8> {
    let options = tscc::CompileOptions {
        debug: true,
        filename: "test.ts".to_string(),
        ..Default::default()
    };
    tscc::compile(source, &options).unwrap()
}

/// Read a string from WASM memory at the given pointer.
/// Layout: [length: i32 (4 bytes)] [UTF-8 bytes...]
pub fn read_wasm_string(store: &Store<()>, memory: &Memory, ptr: i32) -> String {
    let data = memory.data(store);
    let offset = ptr as usize;
    let len_bytes: [u8; 4] = data[offset..offset + 4].try_into().unwrap();
    let len = i32::from_le_bytes(len_bytes) as usize;
    let bytes = &data[offset + 4..offset + 4 + len];
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Run a tick and collect all f64 values passed to `host.sink`.
pub fn run_sink_tick(source: &str) -> Vec<f64> {
    let got: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let wasm = compile(source);
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, got.clone());
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "sink",
            |caller: Caller<'_, Arc<Mutex<Vec<f64>>>>, x: f64| {
                caller.data().lock().unwrap().push(x);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    let v = got.lock().unwrap().clone();
    v
}

/// Find all WASM custom sections by name in a binary.
pub fn find_custom_sections(wasm: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut sections = Vec::new();
    let mut offset = 8; // Skip magic + version
    while offset < wasm.len() {
        let section_id = wasm[offset];
        offset += 1;
        let (section_size, leb_len) = decode_uleb128(&wasm[offset..]);
        offset += leb_len;
        let section_end = offset + section_size as usize;

        if section_id == 0 {
            // Custom section: name_len + name + data
            let (name_len, name_leb_len) = decode_uleb128(&wasm[offset..]);
            let name_start = offset + name_leb_len;
            let name = std::str::from_utf8(&wasm[name_start..name_start + name_len as usize])
                .unwrap_or("<invalid>");
            let data_start = name_start + name_len as usize;
            sections.push((name.to_string(), wasm[data_start..section_end].to_vec()));
        }

        offset = section_end;
    }
    sections
}

pub fn decode_uleb128(bytes: &[u8]) -> (u64, usize) {
    let mut result: u64 = 0;
    let mut shift = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return (result, i + 1);
        }
        shift += 7;
    }
    (result, bytes.len())
}
