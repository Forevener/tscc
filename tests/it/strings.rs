use wasmtime::*;

use super::common::{compile, compile_err, read_wasm_string};

#[test]
fn string_concat_method() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let a: string = "foo";
            let b: string = "bar";
            return a.concat(b);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "foobar");
}

#[test]
fn string_literal_returns_pointer() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello";
            return s;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello");
}

#[test]
fn string_length_property() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "world!";
            return s.length;
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 6);
}

#[test]
fn string_literal_deduplication() {
    // Same string literal used twice should return the same pointer
    let wasm = compile(
        r#"
        export function test(): i32 {
            let a: string = "same";
            let b: string = "same";
            if (a == b) {
                return 1;
            }
            return 0;
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
    // Same pointer means i32.eq returns 1
    assert_eq!(test.call(&mut store, ()).unwrap(), 1);
}

#[test]
fn string_inferred_type() {
    // Type inference from string literal
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s = "inferred";
            return s.length;
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 8); // "inferred".length == 8
}

#[test]
fn string_as_function_parameter() {
    let wasm = compile(
        r#"
        function getLength(s: string): i32 {
            return s.length;
        }

        export function test(): i32 {
            return getLength("hello world");
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 11);
}

#[test]
fn string_empty_literal() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "";
            return s.length;
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 0);
}

#[test]
fn string_index_char_code() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "ABC";
            return s[0] + s[1] + s[2];
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
    // 'A'=65, 'B'=66, 'C'=67 → 198
    assert_eq!(test.call(&mut store, ()).unwrap(), 198);
}

#[test]
#[should_panic]
fn string_index_out_of_bounds() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hi";
            return s[5];
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
    test.call(&mut store, ()).unwrap(); // should trap
}

#[test]
fn string_concat() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let a: string = "hello";
            let b: string = " world";
            let c: string = a + b;
            return c;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello world");
}

#[test]
fn string_concat_chain() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let result: string = "a" + "b" + "c";
            return result;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "abc");
}

#[test]
fn string_equality() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let a: string = "hello";
            let b: string = "hello";
            let c: string = "world";
            let eq1: i32 = 0;
            let eq2: i32 = 0;
            let neq: i32 = 0;
            if (a == b) { eq1 = 1; }
            if (a == c) { eq2 = 1; }
            if (a != c) { neq = 1; }
            return eq1 * 100 + eq2 * 10 + neq;
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
    // eq1=1, eq2=0, neq=1 → 101
    assert_eq!(test.call(&mut store, ()).unwrap(), 101);
}

#[test]
fn string_comparison() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let a: string = "apple";
            let b: string = "banana";
            let r: i32 = 0;
            if (a < b) { r = r + 1; }
            if (b > a) { r = r + 10; }
            if (a <= a) { r = r + 100; }
            if (a >= a) { r = r + 1000; }
            return r;
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 1111);
}

/// String.prototype.localeCompare — byte-order lex compare. No locales or
/// options: ICU would be required for those, outside the typed-subset bar.
/// Returns -1 / 0 / 1 (the `Ordering as i32` from `__str_cmp`).
#[test]
fn string_locale_compare_basic() {
    let wasm = compile(
        r#"
        export function less(): i32    { return "apple".localeCompare("banana"); }
        export function greater(): i32 { return "banana".localeCompare("apple"); }
        export function equal(): i32   { return "cherry".localeCompare("cherry"); }
        export function prefix(): i32  { return "app".localeCompare("apple"); }
        export function suffix(): i32  { return "apple".localeCompare("app"); }
        export function empty_rhs(): i32 { return "x".localeCompare(""); }
        export function empty_lhs(): i32 { return "".localeCompare("x"); }
        export function empty_both(): i32 { return "".localeCompare(""); }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    for (name, expected) in [
        ("less", -1),
        ("greater", 1),
        ("equal", 0),
        ("prefix", -1),
        ("suffix", 1),
        ("empty_rhs", 1),
        ("empty_lhs", -1),
        ("empty_both", 0),
    ] {
        let f = instance
            .get_typed_func::<(), i32>(&mut store, name)
            .unwrap();
        assert_eq!(f.call(&mut store, ()).unwrap(), expected, "{name}");
    }
}

/// localeCompare accepts exactly one argument — the locales/options forms
/// are rejected at compile time so users don't silently get byte compares
/// when they thought they were getting collation.
#[test]
fn string_locale_compare_rejects_extra_args() {
    let err = compile_err(
        r#"
        export function test(): i32 {
            return "a".localeCompare("b", "en-US");
        }
    "#,
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("localeCompare expects exactly 1 argument"),
        "unexpected error: {msg}"
    );
}

#[test]
fn string_char_code_at() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "Hello";
            return s.charCodeAt(0) + s.charCodeAt(4);
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
    // 'H'=72, 'o'=111 → 183
    assert_eq!(test.call(&mut store, ()).unwrap(), 183);
}

#[test]
fn string_index_of_found() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            return s.indexOf("world");
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 6);
}

#[test]
fn string_index_of_not_found() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            return s.indexOf("xyz");
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
    assert_eq!(test.call(&mut store, ()).unwrap(), -1);
}

#[test]
fn string_last_index_of_found() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world hello";
            return s.lastIndexOf("hello");
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 12);
}

#[test]
fn string_last_index_of_not_found() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            return s.lastIndexOf("xyz");
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
    assert_eq!(test.call(&mut store, ()).unwrap(), -1);
}

#[test]
fn string_last_index_of_empty_needle() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello";
            return s.lastIndexOf("");
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 5); // returns haystack length
}

#[test]
fn string_includes() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "broadcast:attack";
            let r: i32 = 0;
            if (s.includes("attack")) { r = r + 1; }
            if (s.includes("defend")) { r = r + 10; }
            if (s.includes("broadcast")) { r = r + 100; }
            return r;
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 101);
}

#[test]
fn string_starts_ends_with() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "cmd:attack:north";
            let r: i32 = 0;
            if (msg.startsWith("cmd:")) { r = r + 1; }
            if (msg.startsWith("xyz")) { r = r + 10; }
            if (msg.endsWith("north")) { r = r + 100; }
            if (msg.endsWith("south")) { r = r + 1000; }
            return r;
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 101);
}

#[test]
fn string_slice_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            let sub: string = s.slice(0, 5);
            return sub;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello");
}

#[test]
fn string_slice_no_end() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            let sub: string = s.slice(6);
            return sub;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "world");
}

#[test]
fn string_slice_negative() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            let sub: string = s.slice(-5);
            return sub;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "world");
}

#[test]
fn string_to_lower_upper() {
    let wasm = compile(
        r#"
        export function testLower(): i32 {
            let s: string = "Hello WORLD";
            return s.toLowerCase();
        }
        export function testUpper(): i32 {
            let s: string = "Hello world";
            return s.toUpperCase();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();

    let test_lower = instance
        .get_typed_func::<(), i32>(&mut store, "testLower")
        .unwrap();
    let ptr = test_lower.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello world");

    let test_upper = instance
        .get_typed_func::<(), i32>(&mut store, "testUpper")
        .unwrap();
    let ptr = test_upper.call(&mut store, ()).unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "HELLO WORLD");
}

#[test]
fn string_trim() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "  hello  ";
            return s.trim();
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello");
}

#[test]
fn string_trim_start_and_end() {
    let wasm = compile(
        r#"
        export function test_start(): i32 {
            let s: string = "  hello  ";
            return s.trimStart();
        }
        export function test_end(): i32 {
            let s: string = "  hello  ";
            return s.trimEnd();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();

    let ts = instance
        .get_typed_func::<(), i32>(&mut store, "test_start")
        .unwrap();
    let p1 = ts.call(&mut store, ()).unwrap();
    assert_eq!(read_wasm_string(&store, &memory, p1), "hello  ");

    let te = instance
        .get_typed_func::<(), i32>(&mut store, "test_end")
        .unwrap();
    let p2 = te.call(&mut store, ()).unwrap();
    assert_eq!(read_wasm_string(&store, &memory, p2), "  hello");
}

#[test]
fn string_at_negative_index() {
    let wasm = compile(
        r#"
        export function last(): i32 {
            let s: string = "hello";
            return s.at(-1);
        }
        export function second(): i32 {
            let s: string = "hello";
            return s.at(1);
        }
        export function from_end(): i32 {
            let s: string = "hello";
            return s.at(-2);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();

    let last = instance
        .get_typed_func::<(), i32>(&mut store, "last")
        .unwrap();
    assert_eq!(last.call(&mut store, ()).unwrap(), b'o' as i32);
    let second = instance
        .get_typed_func::<(), i32>(&mut store, "second")
        .unwrap();
    assert_eq!(second.call(&mut store, ()).unwrap(), b'e' as i32);
    let from_end = instance
        .get_typed_func::<(), i32>(&mut store, "from_end")
        .unwrap();
    assert_eq!(from_end.call(&mut store, ()).unwrap(), b'l' as i32);
}

#[test]
fn string_code_point_at_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "ABC";
            return s.codePointAt(0);
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
    assert_eq!(test.call(&mut store, ()).unwrap(), b'A' as i32);
}

#[test]
fn string_chained_methods() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "  CMD:ATTACK  ";
            let cleaned: string = msg.trim().toLowerCase();
            return cleaned;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "cmd:attack");
}

#[test]
fn string_broadcast_pattern() {
    // Realistic game entity broadcast pattern
    let wasm = compile(
        r#"
        function parseCommand(msg: string): i32 {
            if (msg.startsWith("attack:")) {
                return 1;
            }
            if (msg.startsWith("defend:")) {
                return 2;
            }
            if (msg.startsWith("move:")) {
                return 3;
            }
            return 0;
        }

        export function test(): i32 {
            let r: i32 = 0;
            r = r + parseCommand("attack:north");
            r = r + parseCommand("defend:south") * 10;
            r = r + parseCommand("move:east") * 100;
            r = r + parseCommand("unknown") * 1000;
            return r;
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
    // attack=1, defend=20, move=300, unknown=0 → 321
    assert_eq!(test.call(&mut store, ()).unwrap(), 321);
}

#[test]
fn string_function_return_type() {
    // Function returning string — caller should be able to call .length on result
    let wasm = compile(
        r#"
        function greet(name: string): string {
            return "hello " + name;
        }

        export function test(): i32 {
            let msg: string = greet("world");
            return msg.length;
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 11); // "hello world".length
}

#[test]
fn string_class_field() {
    let wasm = compile(
        r#"
        class Entity {
            name: string;
            hp: i32;
            constructor(name: string, hp: i32) {}
        }

        export function test(): i32 {
            let e: Entity = new Entity("warrior", 100);
            return e.name.length + e.hp;
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
    // "warrior".length=7 + 100 = 107
    assert_eq!(test.call(&mut store, ()).unwrap(), 107);
}

#[test]
fn string_class_field_operations() {
    let wasm = compile(
        r#"
        class Entity {
            name: string;
            message: string;
            constructor(name: string, message: string) {}
        }

        export function test(): i32 {
            let e: Entity = new Entity("knight", "attack:north");
            let r: i32 = 0;
            if (e.message.startsWith("attack:")) {
                r = 1;
            }
            if (e.name == "knight") {
                r = r + 10;
            }
            return r;
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 11);
}

#[test]
fn string_function_return_chain() {
    // Call .toUpperCase() directly on a function return value
    let wasm = compile(
        r#"
        function getMessage(): string {
            return "hello";
        }

        export function test(): i32 {
            let upper: string = getMessage().toUpperCase();
            return upper;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "HELLO");
}

#[test]
fn string_concat_with_i32() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "score: " + 42;
            return msg;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "score: 42");
}

#[test]
fn string_concat_with_f64() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "value: " + 3.14;
            return msg;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "value: 3.14");
}

#[test]
fn string_concat_negative_i32() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "hp: " + -5;
            return msg;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hp: -5");
}

#[test]
fn string_concat_i32_prefix() {
    // Number on the left
    let wasm = compile(
        r#"
        export function test(): i32 {
            let hp: i32 = 100;
            let msg: string = hp + " hp remaining";
            return msg;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "100 hp remaining");
}

#[test]
fn template_literal_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let name: string = "world";
            let msg: string = `hello ${name}!`;
            return msg;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello world!");
}

#[test]
fn template_literal_with_number() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let hp: i32 = 42;
            let msg: string = `HP: ${hp}`;
            return msg;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "HP: 42");
}

#[test]
fn template_literal_multiple_expressions() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: i32 = 10;
            let y: i32 = 20;
            let msg: string = `pos(${x}, ${y})`;
            return msg;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "pos(10, 20)");
}

#[test]
fn string_split_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "attack:north";
            let parts: Array<i32> = msg.split(":");
            return parts.length;
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
fn string_split_content() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let msg: string = "attack:north";
            let parts: Array<i32> = msg.split(":");
            let cmd: string = parts[0];
            let dir: string = parts[1];
            return cmd.length * 100 + dir.length;
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
    // "attack".length=6, "north".length=5 → 605
    assert_eq!(test.call(&mut store, ()).unwrap(), 605);
}

#[test]
fn string_split_multiple() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let csv: string = "a,b,c,d";
            let parts: Array<i32> = csv.split(",");
            return parts.length;
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 4);
}

#[test]
fn string_replace_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            let r: string = s.replace("world", "there");
            return r;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello there");
}

#[test]
fn string_replace_not_found() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello";
            let r: string = s.replace("xyz", "abc");
            return r.length;
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 5); // unchanged
}

// ============================================================
#[test]
fn string_parse_int() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            return parseInt("42") + parseInt("-7") + parseInt("  100  ");
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 135); // 42 + (-7) + 100
}

#[test]
fn string_parse_float() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            return parseFloat("3.14") + parseFloat("-0.5");
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<(), f64>(&mut store, "test")
        .unwrap();
    let result = test.call(&mut store, ()).unwrap();
    assert!((result - 2.64).abs() < 1e-10);
}

#[test]
fn string_from_char_code() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let a: string = String.fromCharCode(65);
            let b: string = String.fromCharCode(66);
            return (a + b);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "AB");
}

#[test]
fn string_repeat() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "ab".repeat(3);
            return s;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "ababab");
}

#[test]
fn string_pad_start() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "42".padStart(5, "0");
            return s;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "00042");
}

#[test]
fn string_pad_end() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hi".padEnd(5, "!");
            return s;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hi!!!");
}

// ============================================================
// Realistic end-to-end broadcast parsing with split
// ============================================================

#[test]
fn string_broadcast_parse_with_split() {
    let wasm = compile(
        r#"
        function handleBroadcast(msg: string): i32 {
            let parts: Array<i32> = msg.split(":");
            let cmd: string = parts[0];
            if (cmd == "damage") {
                let amount: string = parts[1];
                return parseInt(amount);
            }
            if (cmd == "heal") {
                let amount: string = parts[1];
                return parseInt(amount) * -1;
            }
            return 0;
        }

        export function test(): i32 {
            let r: i32 = 0;
            r = r + handleBroadcast("damage:50");
            r = r + handleBroadcast("heal:20");
            r = r + handleBroadcast("unknown:0");
            return r;
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
    // damage:50 → 50, heal:20 → -20, unknown → 0 → total = 30
    assert_eq!(test.call(&mut store, ()).unwrap(), 30);
}

#[test]
fn string_equality_uses_content_not_pointer() {
    // Two strings constructed via concat should compare equal by content.
    let wasm = compile(
        r#"
        export function same(): i32 {
            const a: string = "hel" + "lo";
            const b: string = "hello";
            return a === b ? 1 : 0;
        }
        export function diff(): i32 {
            const a: string = "foo";
            const b: string = "bar";
            return a === b ? 1 : 0;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let same = instance
        .get_typed_func::<(), i32>(&mut store, "same")
        .unwrap();
    let diff = instance
        .get_typed_func::<(), i32>(&mut store, "diff")
        .unwrap();
    assert_eq!(same.call(&mut store, ()).unwrap(), 1);
    assert_eq!(diff.call(&mut store, ()).unwrap(), 0);
}

#[test]
fn template_concat_fusion_produces_correct_result() {
    // A long template with mixed types should round-trip correctly through the
    // fused path (one arena-alloc + memcpy per piece, rather than N-1 __str_concat
    // calls). This exercises the multi-piece fusion with static quasis and
    // i32/f64 interpolated expressions.
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: i32 = 42;
            let y: f64 = 3.5;
            let name: string = "world";
            let s: string = `hi ${name}, x=${x}, y=${y}!`;
            return s.length;
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
    // "hi world, x=42, y=3.5!" = 22 chars
    let len = test.call(&mut store, ()).unwrap();
    assert_eq!(len, 22, "fused template length mismatch");
}

#[test]
fn plus_chain_fusion_omits_str_concat_helper() {
    // After fusion, `+`-chained string concatenation should NOT emit any call to
    // the __str_concat helper — the helper body shouldn't even be registered.
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "a" + "b" + "c" + "d" + "e";
            return s.length;
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
    let len = test.call(&mut store, ()).unwrap();
    assert_eq!(len, 5);

    // And the helper name shouldn't appear in the emitted module at all.
    let s = String::from_utf8_lossy(&wasm);
    assert!(
        !s.contains("__str_concat"),
        "__str_concat should not be present after fusion"
    );
}

#[test]
fn mixed_numeric_inside_plus_chain_still_numeric() {
    // (1 + 2) + "x" must treat (1+2) as a numeric addition — the fusion flattener
    // only recurses into `+` nodes whose own result is string-typed.
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = (1 + 2) + "x";
            return s.length;
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
    let len = test.call(&mut store, ()).unwrap();
    assert_eq!(len, 2, r#"(1+2) + "x" should be "3x" (length 2)"#);
}

#[test]
fn number_to_string_i32() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: i32 = 42;
            return x.toString();
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "42");
}

#[test]
fn number_to_string_f64() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 3.14;
            return x.toString();
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "3.14");
}

/// ryu-js must produce JS-spec shortest-round-trip output. The hand-written
/// fixed-point scheme truncated at 6 fractional digits and emitted wrong
/// output for values like 0.1+0.2, integer f64s, and very small fractions.
#[test]
fn number_to_string_f64_js_conformance() {
    let wasm = compile(
        r#"
        export function integer_f64(): i32 { return (1.0 as f64).toString(); }
        export function classic_floating_point(): i32 { return (0.1 + 0.2).toString(); }
        export function scientific_small(): i32 { return (0.000001 as f64).toString(); }
        export function negative_zero(): i32 { return (-0.0 as f64).toString(); }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();

    for (func_name, expected) in [
        ("integer_f64", "1"),
        ("classic_floating_point", "0.30000000000000004"),
        ("scientific_small", "0.000001"),
        ("negative_zero", "0"),
    ] {
        let f = instance
            .get_typed_func::<(), i32>(&mut store, func_name)
            .unwrap();
        let ptr = f.call(&mut store, ()).unwrap();
        assert_eq!(
            read_wasm_string(&store, &memory, ptr),
            expected,
            "toString({func_name})"
        );
    }
}

/// Number.prototype.toString(radix) — integer and fractional values in
/// non-decimal bases. Radix 10 short-circuits to __str_from_f64 at codegen
/// time, so the interesting paths are hex/binary/base-36 on both integer
/// and float receivers.
#[test]
fn number_to_string_radix_basic() {
    let wasm = compile(
        r#"
        export function i32_hex(): i32   { let x: i32 = 255; return x.toString(16); }
        export function i32_binary(): i32 { let x: i32 = 10; return x.toString(2); }
        export function i32_base36(): i32 { let x: i32 = 35; return x.toString(36); }
        export function i32_negative(): i32 { let x: i32 = -255; return x.toString(16); }
        export function f64_int_hex(): i32 { let x: f64 = 255.0; return x.toString(16); }
        export function f64_half_binary(): i32 { let x: f64 = 0.5; return x.toString(2); }
        export function f64_tenth_binary(): i32 {
            // (0.1).toString(2) emits the bounded repeating binary expansion.
            // V8 produces "0.0001100110011001100110011001100110011001100110011001101".
            let x: f64 = 0.1; return x.toString(2);
        }
        export function zero(): i32 { let x: i32 = 0; return x.toString(2); }
        export function negative_f64(): i32 { let x: f64 = -10.5; return x.toString(2); }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();

    for (func_name, expected) in [
        ("i32_hex", "ff"),
        ("i32_binary", "1010"),
        ("i32_base36", "z"),
        ("i32_negative", "-ff"),
        ("f64_int_hex", "ff"),
        ("f64_half_binary", "0.1"),
        (
            "f64_tenth_binary",
            "0.0001100110011001100110011001100110011001100110011001101",
        ),
        ("zero", "0"),
        ("negative_f64", "-1010.1"),
    ] {
        let f = instance
            .get_typed_func::<(), i32>(&mut store, func_name)
            .unwrap();
        let ptr = f.call(&mut store, ()).unwrap();
        assert_eq!(
            read_wasm_string(&store, &memory, ptr),
            expected,
            "toString({func_name})"
        );
    }
}

/// Radix 10 and out-of-range radices: radix 10 routes through __str_from_f64
/// (same output as the no-arg form); radix literal outside [2, 36] is a
/// compile-time error.
#[test]
fn number_to_string_radix_edge_cases() {
    let wasm = compile(
        r#"
        export function radix_10(): i32  { let x: f64 = 3.14; return x.toString(10); }
        export function nan_hex(): i32   { let x: f64 = 0.0 / 0.0; return x.toString(16); }
        export function inf_hex(): i32   { let x: f64 = 1.0 / 0.0; return x.toString(16); }
        export function neg_inf_hex(): i32 { let x: f64 = -1.0 / 0.0; return x.toString(16); }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();

    for (func_name, expected) in [
        ("radix_10", "3.14"),
        ("nan_hex", "NaN"),
        ("inf_hex", "Infinity"),
        ("neg_inf_hex", "-Infinity"),
    ] {
        let f = instance
            .get_typed_func::<(), i32>(&mut store, func_name)
            .unwrap();
        let ptr = f.call(&mut store, ()).unwrap();
        assert_eq!(
            read_wasm_string(&store, &memory, ptr),
            expected,
            "toString({func_name})"
        );
    }
}

/// Radix literal outside [2, 36] is rejected at compile time so typos like
/// `.toString(1)` fail loudly instead of silently falling back to base 10.
#[test]
fn number_to_string_radix_out_of_range_rejected() {
    let err = compile_err(
        r#"
        export function bad(): i32 {
            let x: i32 = 10;
            return x.toString(1);
        }
    "#,
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("radix must be between 2 and 36"),
        "unexpected error: {msg}"
    );
}

/// Correctly-rounded parseFloat via `f64::from_str`. Includes cases where
/// the hand-written parser's naïve `int + frac/div` accumulator was wrong
/// (long fractional strings near an ULP boundary) and JS-specific prefix
/// scanning that stops at the first non-numeric byte.
#[test]
fn string_parse_float_js_conformance() {
    let wasm = compile(
        r#"
        export function halfway_even(): f64 {
            return parseFloat("0.30000000000000004");
        }
        export function prefix_scan(): f64 {
            return parseFloat("  12.5abc");
        }
        export function infinity(): f64 {
            return parseFloat("-Infinity");
        }
        export function empty_is_nan(): f64 {
            return parseFloat("no digits here");
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();

    let halfway = instance
        .get_typed_func::<(), f64>(&mut store, "halfway_even")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    // Round-trip with the literal's own f64 representation.
    assert_eq!(halfway.to_bits(), 0.30000000000000004f64.to_bits());

    let prefix = instance
        .get_typed_func::<(), f64>(&mut store, "prefix_scan")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert_eq!(prefix, 12.5);

    let inf = instance
        .get_typed_func::<(), f64>(&mut store, "infinity")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert!(inf.is_infinite() && inf.is_sign_negative());

    let nan = instance
        .get_typed_func::<(), f64>(&mut store, "empty_is_nan")
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    assert!(nan.is_nan());
}

#[test]
fn number_to_fixed_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 3.14159;
            return x.toFixed(2);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "3.14");
}

#[test]
fn number_to_fixed_zero_digits() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 3.7;
            return x.toFixed(0);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "4");
}

#[test]
fn number_to_fixed_padding() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 1.5;
            return x.toFixed(4);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "1.5000");
}

#[test]
fn number_to_fixed_negative() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = -2.567;
            return x.toFixed(1);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "-2.6");
}

#[test]
fn number_to_precision_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 123.456;
            return x.toPrecision(5);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "123.46");
}

#[test]
fn number_to_precision_fewer_than_int_digits() {
    // ES § 21.1.3.5: when e (== 4 here) ≥ p (== 3), output exponential form.
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 12345.0;
            return x.toPrecision(3);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "1.23e+4");
}

#[test]
fn number_to_precision_small_value() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 0.00456;
            return x.toPrecision(2);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "0.0046");
}

#[test]
fn number_to_precision_zero() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 0.0;
            return x.toPrecision(3);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "0.00");
}

#[test]
fn number_to_precision_negative() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = -45.678;
            return x.toPrecision(4);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "-45.68");
}

#[test]
fn number_to_exponential_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 123.456;
            return x.toExponential(2);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "1.23e+2");
}

#[test]
fn number_to_exponential_small_value() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 0.001;
            return x.toExponential(2);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "1.00e-3");
}

#[test]
fn number_to_exponential_zero() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 0.0;
            return x.toExponential(3);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "0.000e+0");
}

#[test]
fn number_to_exponential_negative() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = -12345.0;
            return x.toExponential(2);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "-1.23e+4");
}

// === Half-away-from-zero rounding (JS spec § 21.1.3.3 step 6) ===
// These cases hit exact-tie binary fractions where Rust's default
// `{:.*}`/`{:.*e}` would round half-to-even and diverge from V8.

#[test]
fn number_to_fixed_tie_positive_rounds_away() {
    // (2.5).toFixed(0) → "3" in V8 (pick larger n on tie); banker's gives "2".
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 2.5;
            return x.toFixed(0);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "3");
}

#[test]
fn number_to_fixed_tie_negative_rounds_away() {
    // (-2.5).toFixed(0) → "-3" per spec (rule operates on |n|, sign re-attached).
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = -2.5;
            return x.toFixed(0);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "-3");
}

#[test]
fn number_to_fixed_carry_out_of_int_part() {
    // (9.5).toFixed(0): mantissa rounds 9 → 10, length grows.
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 9.5;
            return x.toFixed(0);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "10");
}

#[test]
fn number_to_fixed_one_dot_oh_oh_five() {
    // (1.005).toFixed(2) → "1.00" because the closest f64 to 1.005 is
    // 1.0049999...989, which rounds to 1.00 (not 1.01). V8 agrees.
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 1.005;
            return x.toFixed(2);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "1.00");
}

#[test]
fn number_to_precision_tie_rounds_away() {
    // (2.5).toPrecision(1) → "3" (tie at the only significant digit).
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 2.5;
            return x.toPrecision(1);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "3");
}

#[test]
fn number_to_exponential_tie_rounds_away() {
    // (1.5).toExponential(0) → "2e+0" (tie at the only significant digit).
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 1.5;
            return x.toExponential(0);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "2e+0");
}

#[test]
fn number_to_exponential_carry_renormalizes_exponent() {
    // (9.5).toExponential(0): mantissa 9 → 10, renormalize to "1e+1".
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 9.5;
            return x.toExponential(0);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "1e+1");
}

#[test]
fn string_replace_all() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "aXbXcXd";
            return s.replaceAll("X", "-");
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "a-b-c-d");
}

#[test]
fn string_replace_all_longer_replacement() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "abab";
            return s.replaceAll("a", "XY");
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "XYbXYb");
}

#[test]
fn string_replace_all_no_match() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello";
            return s.replaceAll("z", "X");
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "hello");
}

#[test]
fn string_replace_all_remove() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let s: string = "a--b--c";
            return s.replaceAll("--", "");
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "abc");
}

#[test]
fn string_helper_tree_shaking_shrinks_module() {
    // A program with zero string operations should not include any string helper
    // bodies. Compare against a program that uses string concat — the latter must
    // be larger, and the former must be free of string-helper function names.
    let bare = compile("export function tick(me: i32): void {}");
    let with_strings = compile(
        r#"
        export function tick(me: i32): void {
            let s: string = "hi" + "!";
        }
    "#,
    );
    assert!(
        with_strings.len() > bare.len(),
        "expected string-using module to be larger: bare={} with_strings={}",
        bare.len(),
        with_strings.len()
    );

    // name section should not mention string helpers in the bare module
    let bare_str = String::from_utf8_lossy(&bare);
    assert!(
        !bare_str.contains("__str_concat"),
        "bare module should not carry __str_concat"
    );
    assert!(
        !bare_str.contains("__str_eq"),
        "bare module should not carry __str_eq"
    );

    // Using only indexOf should pull in __str_indexOf but not __str_slice/__str_repeat.
    let only_index_of = compile(
        r#"
        export function test(): i32 {
            let s: string = "hello world";
            return s.indexOf("world");
        }
    "#,
    );
    let s = String::from_utf8_lossy(&only_index_of);
    assert!(s.contains("__str_indexOf"), "should include __str_indexOf");
    assert!(
        !s.contains("__str_repeat"),
        "should not include __str_repeat"
    );
    assert!(
        !s.contains("__str_padStart"),
        "should not include __str_padStart"
    );
    assert!(!s.contains("__str_split"), "should not include __str_split");
}

// === toPrecision dispatch per ES § 21.1.3.5 step 10 ===
// Switch to exponential form when e < -6 or e ≥ p; otherwise fixed.

#[test]
fn number_to_precision_large_value_uses_exp() {
    // (1234567).toPrecision(4) → e=6, p=4, e≥p ⇒ "1.235e+6".
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 1234567.0;
            return x.toPrecision(4);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "1.235e+6");
}

#[test]
fn number_to_precision_boundary_e_lt_p_fixed() {
    // (9999).toPrecision(4) → e=3, p=4, e<p ⇒ fixed "9999".
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 9999.0;
            return x.toPrecision(4);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "9999");
}

#[test]
fn number_to_precision_carry_into_exp() {
    // (9999.5).toPrecision(4): rounds to n=1000, e=4 (tie rule picks larger n),
    // so e≥p ⇒ exponential "1.000e+4". Exercises post-carry exp bump.
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 9999.5;
            return x.toPrecision(4);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "1.000e+4");
}

#[test]
fn number_to_precision_tiny_value_uses_exp() {
    // (0.0000001).toPrecision(2) → e=-7, e<-6 ⇒ "1.0e-7".
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 0.0000001;
            return x.toPrecision(2);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "1.0e-7");
}

#[test]
fn number_to_precision_boundary_e_neg_six_fixed() {
    // (0.000001).toPrecision(2) → e=-6, e is NOT < -6 ⇒ fixed "0.0000010".
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 0.000001;
            return x.toPrecision(2);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "0.0000010");
}

#[test]
fn number_to_precision_negative_large_value_uses_exp() {
    // (-98765).toPrecision(3) → e=4, e≥p ⇒ exponential, sign re-attached.
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = -98765.0;
            return x.toPrecision(3);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "-9.88e+4");
}

// === ES default-argument behavior for number formatters ===
// ES § 21.1.3.3-5: toFixed/toPrecision/toExponential all accept zero args.
//   toFixed()        → toFixed(0)
//   toPrecision()    → ToString(x)  (shortest round-trip)
//   toExponential()  → shortest round-trip in exponential form

#[test]
fn number_to_fixed_default_is_zero_digits() {
    // (3.7).toFixed() → "4"  (same as toFixed(0), half-away-from-zero).
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 3.7;
            return x.toFixed();
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "4");
}

#[test]
fn number_to_precision_default_is_to_string() {
    // (0.1 + 0.2).toPrecision() → "0.30000000000000004"  (identical to
    // toString — no rounding to a fixed precision).
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 0.1 + 0.2;
            return x.toPrecision();
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(
        read_wasm_string(&store, &memory, ptr),
        "0.30000000000000004"
    );
}

#[test]
fn number_to_exponential_default_is_shortest() {
    // (0.000001).toExponential() → "1e-6"  (shortest round-trip in exp form).
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 0.000001;
            return x.toExponential();
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "1e-6");
}

#[test]
fn number_to_exponential_default_fractional() {
    // (123.456).toExponential() → "1.23456e+2"  (digits from ryu-js shortest).
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 123.456;
            return x.toExponential();
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "1.23456e+2");
}

#[test]
fn number_to_exponential_default_negative() {
    // (-0.5).toExponential() → "-5e-1".
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = -0.5;
            return x.toExponential();
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "-5e-1");
}

#[test]
fn number_to_exponential_default_zero() {
    // (0).toExponential() → "0e+0" (no sign, even for -0).
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 0.0;
            return x.toExponential();
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "0e+0");
}

#[test]
fn number_to_exponential_default_large_magnitude() {
    // (1e21).toExponential() → "1e+21" (ryu already emits exp form here).
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 1e21;
            return x.toExponential();
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "1e+21");
}

#[test]
fn number_to_precision_one_digit_carry_to_exp() {
    // (9.5).toPrecision(1): rounds to n=1, e=1 ⇒ e≥p ⇒ "1e+1".
    let wasm = compile(
        r#"
        export function test(): i32 {
            let x: f64 = 9.5;
            return x.toPrecision(1);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "1e+1");
}

#[test]
fn string_from_char_code_variadic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            return String.fromCharCode(72, 101, 108, 108, 111);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "Hello");
}

#[test]
fn string_from_char_code_empty() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            return String.fromCharCode();
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "");
}

#[test]
fn string_from_code_point_ascii_and_emoji() {
    // Covers all four UTF-8 encoding widths: 1-byte ASCII, 2-byte Latin,
    // 3-byte BMP, and 4-byte supplementary.
    let wasm = compile(
        r#"
        export function test(): i32 {
            // "Aé€😀" — 1 + 2 + 3 + 4 bytes.
            return String.fromCodePoint(65, 233, 8364, 128512);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "Aé€😀");
}

#[test]
fn string_from_code_point_single() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            return String.fromCodePoint(128640);
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "🚀");
}

#[test]
fn string_from_code_point_empty() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            return String.fromCodePoint();
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "");
}

#[test]
fn string_from_code_point_traps_out_of_range() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            return String.fromCodePoint(0x110000);
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
    let err = test.call(&mut store, ()).unwrap_err();
    // wasmtime surfaces the trap via the helper's stack frame; the concrete
    // message varies by runtime version, so just assert the helper is on the
    // backtrace.
    let msg = format!("{err}");
    assert!(
        msg.contains("__utf8_encode_cp") || msg.contains("unreachable"),
        "expected trap for out-of-range code point, got: {msg}"
    );
}

#[test]
fn string_raw_no_interpolation() {
    // String.raw preserves raw source — escapes stay as literal backslash+char.
    // (tscc's regular template literal currently also uses `.raw`, so both
    // forms happen to match today; String.raw is a dedicated path so the
    // future cooked-string fix doesn't break raw's semantics.)
    let wasm = compile(
        r#"
        export function test(): i32 {
            return String.raw`Hello\nWorld`;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "Hello\\nWorld");
}

#[test]
fn string_raw_with_interpolation() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            const n: i32 = 42;
            return String.raw`answer=${n}`;
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
    let ptr = test.call(&mut store, ()).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(read_wasm_string(&store, &memory, ptr), "answer=42");
}

#[test]
fn string_raw_rejects_unknown_tag() {
    let err = compile_err(
        r#"
        export function bad(): i32 {
            return String.notRaw`hi`;
        }
    "#,
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("String.raw"),
        "expected String.raw error, got: {msg}"
    );
}
