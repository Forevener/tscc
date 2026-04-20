//! Minimal end-to-end: compile a TypeScript script with tscc, load it into
//! wasmtime, wire one host import (`log`), and call the exported entry point.
//!
//! Run with `cargo run --example hello-world` from the tscc crate root.

use wasmtime::*;

const SOURCE: &str = include_str!("hello.ts");

fn main() -> Result<()> {
    // Compile TS → WASM. In a real host you'd typically AOT-compile once
    // and ship `.wasm` files; doing it at startup here keeps the example
    // self-contained.
    let wasm = tscc::compile(SOURCE, &tscc::CompileOptions::default())
        .map_err(|e| Error::msg(format!("tscc compile failed: {e:?}")))?;

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm)?;
    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);

    // Satisfy the `declare function log(s: string): void` import from hello.ts.
    //
    // tscc passes strings as i32 pointers into the module's linear memory,
    // with the layout `[len: u32 little-endian][utf8 bytes...]`. The host
    // reads the length header, then the payload.
    linker.func_wrap("host", "log", |mut caller: Caller<'_, ()>, ptr: i32| {
        let memory = caller
            .get_export("memory")
            .and_then(|e| e.into_memory())
            .expect("module must export `memory`");
        let data = memory.data(&caller);
        let off = ptr as usize;
        let len = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        let bytes = &data[off + 4..off + 4 + len];
        println!("[guest] {}", std::str::from_utf8(bytes).unwrap());
    })?;

    let instance = linker.instantiate(&mut store, &module)?;
    let main_fn = instance.get_typed_func::<(), ()>(&mut store, "main")?;
    main_fn.call(&mut store, ())?;
    Ok(())
}
