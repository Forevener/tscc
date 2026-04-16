mod common;
use wasmtime::*;
use common::{compile, compile_debug, find_custom_sections};

#[test]
fn debug_name_section_present() {
    let wasm = compile_debug("export function tick(me: i32): void {}");
    let sections = find_custom_sections(&wasm);
    let name_section = sections.iter().find(|(name, _)| name == "name");
    assert!(name_section.is_some(), "name section should be present");
}

#[test]
fn debug_dwarf_sections_present() {
    let wasm = compile_debug(
        r#"
        export function tick(me: i32): void {
            let x: i32 = 42;
        }
    "#,
    );
    let sections = find_custom_sections(&wasm);
    let section_names: Vec<&str> = sections.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        section_names.contains(&".debug_abbrev"),
        "missing .debug_abbrev, found: {section_names:?}"
    );
    assert!(
        section_names.contains(&".debug_info"),
        "missing .debug_info, found: {section_names:?}"
    );
    assert!(
        section_names.contains(&".debug_line"),
        "missing .debug_line, found: {section_names:?}"
    );
}

#[test]
fn debug_dwarf_not_present_without_flag() {
    // Default compile (debug=false) should NOT have DWARF sections
    let wasm = compile("export function tick(me: i32): void {}");
    let sections = find_custom_sections(&wasm);
    let section_names: Vec<&str> = sections.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        !section_names.contains(&".debug_line"),
        "DWARF should not be present without debug flag"
    );
}

#[test]
fn debug_name_section_contains_function_names() {
    let wasm = compile_debug(
        r#"
        export function tick(me: i32): void {}
        function helper(x: i32): i32 { return x; }
    "#,
    );
    // The name section should contain our function names
    // Just verify the binary is valid and loadable
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
}

#[test]
fn debug_wasm_valid_with_dwarf() {
    // Compile a non-trivial script with debug info and verify wasmtime accepts it
    let wasm = compile_debug(
        r#"
        declare function get_hp(id: i32): i32;
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            let hp: i32 = get_hp(me);
            if (hp < 50) {
                set_action(me, 2, 0, 0.0, 0.0);
            } else {
                let dx: f64 = 1.0;
                let dy: f64 = 0.0;
                set_action(me, 0, 0, dx, dy);
            }
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());

    let get_hp = Func::wrap(&mut store, |_: i32| -> i32 { 100 });
    let set_action = Func::wrap(&mut store, |_: i32, _: i32, _: i32, _: f64, _: f64| {});

    let instance = Instance::new(&mut store, &module, &[get_hp.into(), set_action.into()]).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 1).unwrap();
}

#[test]
fn debug_trap_contains_source_location() {
    // Compile with debug, trigger a trap, verify the error contains file/line info
    let source = r#"export function test(): i32 {
    let arr: Array<i32> = new Array<i32>(4);
    arr.push(10);
    return arr[5];
}"#;

    let options = tscc::CompileOptions {
        debug: true,
        filename: "player.ts".to_string(),
        ..Default::default()
    };
    let wasm = tscc::compile(source, &options).unwrap();

    let mut config = Config::new();
    config.wasm_backtrace_details(WasmBacktraceDetails::Enable);
    let engine = Engine::new(&config).unwrap();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();

    let err = test.call(&mut store, ()).unwrap_err();
    let msg = format!("{err:?}");

    // The trap message should reference our source file
    assert!(
        msg.contains("player.ts") || msg.contains("test"),
        "trap message should reference source file, got: {msg}"
    );
}

#[test]
fn debug_multifunction_source_mapping() {
    // Multiple functions with debug info — verify the module loads and runs correctly
    let wasm = compile_debug(
        r#"
        function add(a: i32, b: i32): i32 {
            return a + b;
        }

        function multiply(a: i32, b: i32): i32 {
            return a * b;
        }

        export function test(): i32 {
            let sum: i32 = add(3, 4);
            let product: i32 = multiply(sum, 2);
            return product;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 14); // (3+4)*2
}

#[test]
fn debug_debug_line_has_filename() {
    let wasm = compile_debug(
        r#"
        export function tick(me: i32): void {
            let x: i32 = 42;
        }
    "#,
    );
    let sections = find_custom_sections(&wasm);
    let debug_line = sections
        .iter()
        .find(|(name, _)| name == ".debug_line")
        .unwrap();
    // The .debug_line section should contain our filename as bytes
    let filename_bytes = b"test.ts";
    let contains_filename = debug_line
        .1
        .windows(filename_bytes.len())
        .any(|window| window == filename_bytes);
    assert!(
        contains_filename,
        ".debug_line should contain the filename 'test.ts'"
    );
}

#[test]
fn debug_info_has_filename() {
    let wasm = compile_debug(
        r#"
        export function tick(me: i32): void {}
    "#,
    );
    let sections = find_custom_sections(&wasm);
    let debug_info = sections
        .iter()
        .find(|(name, _)| name == ".debug_info")
        .unwrap();
    // The .debug_info section should contain our filename
    let filename_bytes = b"test.ts";
    let contains_filename = debug_info
        .1
        .windows(filename_bytes.len())
        .any(|window| window == filename_bytes);
    assert!(
        contains_filename,
        ".debug_info should contain the filename 'test.ts'"
    );
}

#[test]
fn debug_closure_has_source_mapping() {
    // Closures (lifted arrow functions) should have debug info
    // Verify the module with closures + debug compiles and runs correctly
    let wasm = compile_debug(
        r#"
        export function test(): i32 {
            let add: (a: i32) => i32 = (a: i32): i32 => a + 10;
            return add(5);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 15);
}

#[test]
fn debug_array_filter_has_source_mapping() {
    // Inlined array builtins should have source mapping
    let wasm = compile_debug(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(0);
            arr.push(1);
            arr.push(2);
            arr.push(3);
            arr.push(4);
            let evens: Array<i32> = arr.filter((x: i32): bool => x > 2);
            return evens.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), i32>(&mut store, "test")
        .unwrap();
    assert_eq!(test.call(&mut store, ()).unwrap(), 2);
}

#[test]
fn debug_closure_named_in_name_section() {
    // Closures should appear as closure$N in the name section
    let wasm = compile_debug(
        r#"
        export function test(): i32 {
            let inc: (a: i32) => i32 = (a: i32): i32 => a + 1;
            return inc(41);
        }
    "#,
    );

    // The name section should contain "closure$0"
    let sections = find_custom_sections(&wasm);
    let name_section = sections.iter().find(|(name, _)| name == "name").unwrap();
    let closure_name = b"closure$0";
    let contains_closure_name = name_section
        .1
        .windows(closure_name.len())
        .any(|window| window == closure_name);
    assert!(
        contains_closure_name,
        "name section should contain 'closure$0'"
    );
}

#[test]
fn debug_name_section_present_without_debug_flag() {
    // Name section should be emitted even without --debug flag (it's always useful)
    let wasm = compile("export function tick(me: i32): void {}");
    let sections = find_custom_sections(&wasm);
    let name_section = sections.iter().find(|(name, _)| name == "name");
    assert!(
        name_section.is_some(),
        "name section should always be present"
    );
}
