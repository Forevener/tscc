mod common;

use wasmtime::*;

use common::{compile, read_wasm_string};

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

