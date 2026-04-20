mod common;

use std::cell::Cell;

use wasmtime::*;

use common::{compile, compile_err, run_sink_tick};

#[test]
fn array_is_array_static() {
    let wasm = compile(
        r#"
        export function arr_local(): i32 {
            const xs: Array<i32> = new Array<i32>(0);
            xs.push(10);
            return Array.isArray(xs) ? 1 : 0;
        }
        export function arr_new(): i32 {
            const ys: Array<i32> = new Array<i32>(0);
            return Array.isArray(ys) ? 1 : 0;
        }
        export function arr_scalar(): i32 {
            const n: i32 = 42;
            return Array.isArray(n) ? 1 : 0;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    for (name, expected) in [("arr_local", 1), ("arr_new", 1), ("arr_scalar", 0)] {
        let f = instance
            .get_typed_func::<(), i32>(&mut store, name)
            .unwrap();
        assert_eq!(f.call(&mut store, ()).unwrap(), expected, "{name}");
    }
}

#[test]
fn array_pop_and_indexof_and_includes() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a: Array<i32> = new Array<i32>(8);
            a.push(10); a.push(20); a.push(30);
            sink(a.pop() as f64);      // 30
            sink(a.length as f64);     // 2
            sink(a.indexOf(10) as f64);     // 0
            sink(a.indexOf(999) as f64);    // -1
            sink(a.lastIndexOf(20) as f64); // 1
            sink(a.includes(10) ? 1.0 : 0.0); // 1
            sink(a.includes(999) ? 1.0 : 0.0); // 0
            // pop on empty returns default (0)
            const e: Array<i32> = new Array<i32>(0);
            sink(e.pop() as f64);     // 0
        }
    "#,
    );
    assert_eq!(vals, vec![30.0, 2.0, 0.0, -1.0, 1.0, 1.0, 0.0, 0.0]);
}

#[test]
fn array_reverse_and_at() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a: Array<i32> = new Array<i32>(8);
            a.push(1); a.push(2); a.push(3); a.push(4);
            a.reverse();
            sink(a[0] as f64); // 4
            sink(a[3] as f64); // 1
            sink(a.at(0) as f64);  // 4
            sink(a.at(-1) as f64); // 1
            sink(a.at(-2) as f64); // 2
        }
    "#,
    );
    assert_eq!(vals, vec![4.0, 1.0, 4.0, 1.0, 2.0]);
}

#[test]
fn array_fill_slice_concat() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a: Array<i32> = new Array<i32>(8);
            a.push(1); a.push(2); a.push(3); a.push(4); a.push(5);
            a.fill(9, 1, 4);
            sink(a[0] as f64); // 1
            sink(a[1] as f64); // 9
            sink(a[2] as f64); // 9
            sink(a[3] as f64); // 9
            sink(a[4] as f64); // 5

            const b: Array<i32> = a.slice(1, 4);
            sink(b.length as f64); // 3
            sink(b[0] as f64); // 9
            sink(b[2] as f64); // 9

            // Negative slice
            const c: Array<i32> = a.slice(-2);
            sink(c.length as f64); // 2
            sink(c[0] as f64); // 9 (a[3])
            sink(c[1] as f64); // 5 (a[4])

            // Concat
            const d: Array<i32> = b.concat(c);
            sink(d.length as f64); // 5
            sink(d[0] as f64); // 9
            sink(d[4] as f64); // 5
        }
    "#,
    );
    assert_eq!(
        vals,
        vec![
            1.0, 9.0, 9.0, 9.0, 5.0, 3.0, 9.0, 9.0, 2.0, 9.0, 5.0, 5.0, 9.0, 5.0
        ]
    );
}

#[test]
fn array_join_numeric_and_string() {
    use std::sync::{Arc, Mutex};
    let got: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    // sink_str takes a string pointer; the host reads the 4-byte length header
    // and the UTF-8 bytes that follow it.
    let wasm = compile(
        r#"
        declare function sink_str(s: string): void;

        export function tick(me: i32): void {
            const a: Array<i32> = new Array<i32>(8);
            a.push(1); a.push(2); a.push(3);
            sink_str(a.join(","));
            sink_str(a.join(" - "));
            sink_str(a.join());
            const e: Array<i32> = new Array<i32>(0);
            sink_str(e.join(","));
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, got.clone());
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "sink_str",
            |mut caller: Caller<'_, Arc<Mutex<Vec<String>>>>, s_ptr: i32| {
                let mem = caller.get_export("memory").unwrap().into_memory().unwrap();
                let mut hdr = [0u8; 4];
                mem.read(&mut caller, s_ptr as usize, &mut hdr).unwrap();
                let len = i32::from_le_bytes(hdr) as usize;
                let mut buf = vec![0u8; len];
                mem.read(&mut caller, s_ptr as usize + 4, &mut buf).unwrap();
                let s = String::from_utf8(buf).unwrap();
                caller.data().lock().unwrap().push(s);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    let vals = got.lock().unwrap().clone();
    assert_eq!(vals, vec!["1,2,3", "1 - 2 - 3", "1,2,3", ""]);
}

#[test]
fn array_find_and_some_every() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a: Array<i32> = new Array<i32>(8);
            a.push(1); a.push(2); a.push(3); a.push(4); a.push(5);

            sink(a.find((x: i32) => x > 3) as f64);       // 4
            sink(a.findIndex((x: i32) => x > 3) as f64);  // 3
            sink(a.findLast((x: i32) => x > 3) as f64);   // 5
            sink(a.findLastIndex((x: i32) => x > 3) as f64); // 4

            // Miss cases
            sink(a.find((x: i32) => x > 99) as f64);      // 0 (default)
            sink(a.findIndex((x: i32) => x > 99) as f64); // -1

            sink(a.some((x: i32) => x > 4) ? 1.0 : 0.0);  // 1
            sink(a.some((x: i32) => x > 99) ? 1.0 : 0.0); // 0
            sink(a.every((x: i32) => x > 0) ? 1.0 : 0.0); // 1
            sink(a.every((x: i32) => x > 3) ? 1.0 : 0.0); // 0
        }
    "#,
    );
    assert_eq!(
        vals,
        vec![4.0, 3.0, 5.0, 4.0, 0.0, -1.0, 1.0, 0.0, 1.0, 0.0]
    );
}

#[test]
fn array_new_and_length() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<f64> = new Array<f64>(8);
            return arr.length;
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
    let result = test.call(&mut store, ()).unwrap();
    assert_eq!(result, 0); // new array starts with length 0
}

#[test]
fn array_push_and_length() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<f64> = new Array<f64>(8);
            arr.push(1.0);
            arr.push(2.0);
            arr.push(3.0);
            return arr.length;
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
    let result = test.call(&mut store, ()).unwrap();
    assert_eq!(result, 3);
}

#[test]
fn array_push_and_index_f64() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(10.0);
            arr.push(20.0);
            arr.push(30.0);
            return arr[1];
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
    assert!((result - 20.0).abs() < 1e-10);
}

#[test]
fn array_push_and_index_i32() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(4);
            arr.push(100);
            arr.push(200);
            arr.push(300);
            return arr[2];
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
    let result = test.call(&mut store, ()).unwrap();
    assert_eq!(result, 300);
}

#[test]
fn array_index_assignment() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(10.0);
            arr.push(20.0);
            arr[1] = 99.0;
            return arr[1];
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
    assert!((result - 99.0).abs() < 1e-10);
}

#[test]
fn array_compound_assignment() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(10.0);
            arr.push(20.0);
            arr[0] += 5.0;
            return arr[0];
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
    assert!((result - 15.0).abs() < 1e-10);
}

#[test]
fn array_for_loop_iteration() {
    // Sum all elements using a for loop
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(1.0);
            arr.push(2.0);
            arr.push(3.0);
            arr.push(4.0);
            let sum: f64 = 0.0;
            for (let i: i32 = 0; i < arr.length; i++) {
                sum += arr[i];
            }
            return sum;
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
    assert!((result - 10.0).abs() < 1e-10);
}

#[test]
fn array_bounds_check_traps() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(1.0);
            return arr[5];
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
    let result = test.call(&mut store, ());
    assert!(result.is_err(), "out-of-bounds access should trap");
}

#[test]
fn array_push_grows_beyond_capacity() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(2);
            arr.push(10);
            arr.push(20);
            arr.push(30);
            return arr[0] * 100 + arr[1] * 10 + arr[2];
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
    let result = test.call(&mut store, ()).unwrap();
    assert_eq!(result, 1230); // 10*100 + 20*10 + 30
}

#[test]
fn array_grow_from_zero_capacity() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(0);
            arr.push(11);
            arr.push(22);
            arr.push(33);
            return arr.length * 1000 + arr[0] * 100 + arr[1] * 10 + arr[2];
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
    let result = test.call(&mut store, ()).unwrap();
    assert_eq!(result, 3_000 + 1100 + 220 + 33); // length=3, vals 11,22,33
}

#[test]
fn array_grow_preserves_elements() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(2);
            arr.push(1.5);
            arr.push(2.5);
            arr.push(3.5);
            arr.push(4.5);
            arr.push(5.5);
            return arr[0] + arr[1] + arr[2] + arr[3] + arr[4];
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
    assert!((result - 17.5).abs() < 1e-10); // 1.5+2.5+3.5+4.5+5.5 = 17.5
}

#[test]
fn array_grow_multiple_doublings() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(1);
            arr.push(0);
            arr.push(1);
            arr.push(2);
            arr.push(3);
            arr.push(4);
            arr.push(5);
            arr.push(6);
            arr.push(7);
            arr.push(8);
            arr.push(9);
            let sum: i32 = 0;
            for (let i: i32 = 0; i < arr.length; i++) {
                sum += arr[i];
            }
            return sum * 10 + arr.length;
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
    let result = test.call(&mut store, ()).unwrap();
    // sum = 0+1+2+3+4+5+6+7+8+9 = 45, length = 10
    assert_eq!(result, 45 * 10 + 10); // 460
}

#[test]
fn array_grow_class_elements() {
    let wasm = compile(
        r#"
        class Point {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}
        }

        export function test(): f64 {
            let pts: Array<Point> = new Array<Point>(1);
            let p1: Point = new Point(1.0, 10.0);
            let p2: Point = new Point(2.0, 20.0);
            let p3: Point = new Point(3.0, 30.0);
            pts.push(p1);
            pts.push(p2);
            pts.push(p3);
            let a: Point = pts[0];
            let b: Point = pts[1];
            let c: Point = pts[2];
            return a.x + b.x + c.y;
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
    assert!((result - 33.0).abs() < 1e-10); // 1.0 + 2.0 + 30.0 = 33.0
}

#[test]
fn array_grow_then_filter() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(2);
            arr.push(1);
            arr.push(2);
            arr.push(3);
            arr.push(4);
            arr.push(5);
            arr.push(6);
            let evens: Array<i32> = arr.filter(x => x % 2 == 0);
            return evens.length * 100 + evens[0] * 10 + evens[1];
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
    let result = test.call(&mut store, ()).unwrap();
    // evens = [2, 4, 6], length=3, 3*100 + 2*10 + 4 = 324
    assert_eq!(result, 324);
}

#[test]
fn array_grow_inplace_arena_efficiency() {
    // In-place grow: sequential pushes with no intervening allocations should
    // use arena space ~= final array size, not the sum of all intermediate copies.
    // We verify by reading __arena_ptr global from the host before/after.
    let wasm = compile(
        r#"
        export function push_ten(): void {
            let arr: Array<i32> = new Array<i32>(0);
            arr.push(0);
            arr.push(1);
            arr.push(2);
            arr.push(3);
            arr.push(4);
            arr.push(5);
            arr.push(6);
            arr.push(7);
            arr.push(8);
            arr.push(9);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let arena_ptr = instance
        .get_global(&mut store, "__arena_ptr")
        .expect("__arena_ptr global");
    let before = arena_ptr.get(&mut store).i32().unwrap();
    let push_ten = instance
        .get_typed_func::<(), ()>(&mut store, "push_ten")
        .unwrap();
    push_ten.call(&mut store, ()).unwrap();
    let after = arena_ptr.get(&mut store).i32().unwrap();
    let arena_delta = after - before;
    // With in-place grow from cap 0: 0->1->2->4->8->16
    // Final array: header(8) + 16 * 4 = 72 bytes
    // Without in-place grow, total would be: 8+4 + 8+8 + 8+16 + 8+32 + 8+64 = 164 bytes
    assert_eq!(
        arena_delta,
        8 + 16 * 4,
        "in-place grow should only use space for the final array, got {arena_delta}"
    );
}

#[test]
fn array_grow_copyabandon_with_intervening_alloc() {
    // When another allocation happens between array creation and push,
    // in-place grow can't be used — falls back to copy-and-abandon.
    // Array should still work correctly.
    let wasm = compile(
        r#"
        class Box {
            val: i32;
            constructor(val: i32) {}
        }
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(1);
            arr.push(10);
            let b: Box = new Box(999);
            arr.push(20);
            arr.push(30);
            return arr[0] * 100 + arr[1] * 10 + arr[2] + b.val;
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
    let result = test.call(&mut store, ()).unwrap();
    // 10*100 + 20*10 + 30 + 999 = 1000 + 200 + 30 + 999 = 2229
    assert_eq!(result, 2229);
}

#[test]
fn array_with_host_imports() {
    // Realistic pattern: fill an array from host data, find max
    let wasm = compile(
        r#"
        declare function get_count(): i32;
        declare function get_value(idx: i32): f64;

        export function find_max(): f64 {
            let n: i32 = get_count();
            let vals: Array<f64> = new Array<f64>(16);
            for (let i: i32 = 0; i < n; i++) {
                vals.push(get_value(i));
            }
            let max: f64 = vals[0];
            for (let i: i32 = 1; i < vals.length; i++) {
                if (vals[i] > max) {
                    max = vals[i];
                }
            }
            return max;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let values: Vec<f64> = vec![3.0, 7.0, 1.0, 9.0, 2.0];
    let mut store = Store::new(&engine, values);
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap("host", "get_count", |caller: Caller<'_, Vec<f64>>| -> i32 {
            caller.data().len() as i32
        })
        .unwrap();
    linker
        .func_wrap(
            "host",
            "get_value",
            |caller: Caller<'_, Vec<f64>>, idx: i32| -> f64 { caller.data()[idx as usize] },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let find_max = instance
        .get_typed_func::<(), f64>(&mut store, "find_max")
        .unwrap();
    let result = find_max.call(&mut store, ()).unwrap();
    assert!((result - 9.0).abs() < 1e-10);
}

#[test]
fn array_of_class_instances() {
    // Array<Entity> — store pointers to class instances
    let wasm = compile(
        r#"
        class Entity {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}
        }

        export function test(): f64 {
            let entities: Array<Entity> = new Array<Entity>(4);
            let e1: Entity = new Entity(1.0, 2.0);
            let e2: Entity = new Entity(3.0, 4.0);
            let e3: Entity = new Entity(5.0, 6.0);
            entities.push(e1);
            entities.push(e2);
            entities.push(e3);

            // Sum all x values
            let sum: f64 = 0.0;
            for (let i: i32 = 0; i < entities.length; i++) {
                let e: Entity = entities[i];
                sum += e.x;
            }
            return sum;
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
    assert!((result - 9.0).abs() < 1e-10); // 1 + 3 + 5 = 9
}

// ---- Phase 5 tests: closures + higher-order functions ----

#[test]
fn array_filter_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(8);
            arr.push(1);
            arr.push(2);
            arr.push(3);
            arr.push(4);
            arr.push(5);
            let evens: Array<i32> = arr.filter(x => x % 2 == 0);
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 2); // 2, 4
}

#[test]
fn array_filter_with_capture() {
    // Filter with a captured variable from the enclosing scope
    let wasm = compile(
        r#"
        export function test(): i32 {
            let arr: Array<i32> = new Array<i32>(8);
            arr.push(10);
            arr.push(20);
            arr.push(30);
            arr.push(40);
            arr.push(50);
            let threshold: i32 = 25;
            let big: Array<i32> = arr.filter(x => x > threshold);
            return big.length;
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 3); // 30, 40, 50
}

#[test]
fn array_filter_class_instances() {
    // Filter entities by a field — the key game scripting pattern
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }

        export function test(): i32 {
            let entities: Array<Entity> = new Array<Entity>(8);
            entities.push(new Entity(100, 1));
            entities.push(new Entity(0, 1));
            entities.push(new Entity(50, 2));
            entities.push(new Entity(75, 2));
            entities.push(new Entity(0, 1));

            let alive: Array<Entity> = entities.filter(e => e.hp > 0);
            return alive.length;
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 3); // hp > 0: 100, 50, 75
}

#[test]
fn array_filter_with_class_capture() {
    // Filter enemies by team, capturing myTeam — the canonical game AI pattern
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }

        export function count_enemies(my_team: i32): i32 {
            let entities: Array<Entity> = new Array<Entity>(8);
            entities.push(new Entity(100, 1));
            entities.push(new Entity(80, 2));
            entities.push(new Entity(50, 1));
            entities.push(new Entity(60, 2));

            let enemies: Array<Entity> = entities.filter(e => e.team != my_team && e.hp > 0);
            return enemies.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<i32, i32>(&mut store, "count_enemies")
        .unwrap();
    assert_eq!(test.call(&mut store, 1).unwrap(), 2); // team 2: hp 80, 60
    assert_eq!(test.call(&mut store, 2).unwrap(), 2); // team 1: hp 100, 50
}

#[test]
fn array_map_basic() {
    // Map f64 array to doubled values
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(1.0);
            arr.push(2.0);
            arr.push(3.0);
            let doubled: Array<f64> = arr.map(x => x * 2.0);
            return doubled[0] + doubled[1] + doubled[2];
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
    assert!((result - 12.0).abs() < 1e-10); // 2 + 4 + 6
}

#[test]
fn array_map_extract_field() {
    // Map Entity -> f64 (extract hp values) — common game pattern
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }

        export function test(): i32 {
            let entities: Array<Entity> = new Array<Entity>(4);
            entities.push(new Entity(100, 1));
            entities.push(new Entity(50, 2));
            entities.push(new Entity(75, 1));
            let hps: Array<i32> = entities.map(e => e.hp);
            return hps[0] + hps[1] + hps[2];
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 225); // 100 + 50 + 75
}

#[test]
fn array_foreach() {
    // forEach to accumulate a sum via captured mutable variable
    let wasm = compile(
        r#"
        declare function report(val: f64): void;

        export function test(): void {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(1.0);
            arr.push(2.0);
            arr.push(3.0);
            let sum: f64 = 0.0;
            arr.forEach(x => { sum += x; });
            report(sum);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, Cell::new(0.0f64));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "report",
            |mut caller: Caller<'_, Cell<f64>>, val: f64| {
                caller.data_mut().set(val);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let test = instance
        .get_typed_func::<(), ()>(&mut store, "test")
        .unwrap();
    test.call(&mut store, ()).unwrap();
    assert!((store.data().get() - 6.0).abs() < 1e-10); // 1 + 2 + 3
}

#[test]
fn array_reduce_sum() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(1.0);
            arr.push(2.0);
            arr.push(3.0);
            arr.push(4.0);
            let total: f64 = arr.reduce((sum, x) => sum + x, 0.0);
            return total;
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
    assert!((result - 10.0).abs() < 1e-10);
}

#[test]
fn array_reduce_entity_hp_sum() {
    // reduce over class instances — sum all HP values
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }

        export function test(): i32 {
            let entities: Array<Entity> = new Array<Entity>(4);
            entities.push(new Entity(100, 1));
            entities.push(new Entity(50, 2));
            entities.push(new Entity(75, 1));
            let total_hp: i32 = entities.reduce((sum, e) => sum + e.hp, 0);
            return total_hp;
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 225);
}

#[test]
fn array_sort_f64() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(3.0);
            arr.push(1.0);
            arr.push(4.0);
            arr.push(2.0);
            arr.sort((a, b) => a - b);
            // After sort: [1, 2, 3, 4]
            return arr[0] * 1000.0 + arr[1] * 100.0 + arr[2] * 10.0 + arr[3];
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
    assert!((result - 1234.0).abs() < 1e-10);
}

#[test]
fn array_sort_descending() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(3.0);
            arr.push(1.0);
            arr.push(4.0);
            arr.push(2.0);
            arr.sort((a, b) => b - a);
            // After sort descending: [4, 3, 2, 1]
            return arr[0] * 1000.0 + arr[1] * 100.0 + arr[2] * 10.0 + arr[3];
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
    assert!((result - 4321.0).abs() < 1e-10);
}

#[test]
fn array_filter_then_reduce() {
    // Chain: filter enemies then sum their HP — realistic game AI
    let wasm = compile(
        r#"
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }

        export function enemy_total_hp(my_team: i32): i32 {
            let entities: Array<Entity> = new Array<Entity>(8);
            entities.push(new Entity(100, 1));
            entities.push(new Entity(80, 2));
            entities.push(new Entity(0, 2));
            entities.push(new Entity(60, 1));
            entities.push(new Entity(40, 2));

            let enemies: Array<Entity> = entities.filter(e => e.team != my_team && e.hp > 0);
            let total: i32 = enemies.reduce((sum, e) => sum + e.hp, 0);
            return total;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let test = instance
        .get_typed_func::<i32, i32>(&mut store, "enemy_total_hp")
        .unwrap();
    // my_team=1: enemies are team 2 with hp>0: 80, 40 = 120
    assert_eq!(test.call(&mut store, 1).unwrap(), 120);
}

#[test]
fn realistic_game_ai_filter_sort() {
    // The "money shot": filter enemies, sort by distance, act on closest
    let wasm = compile(
        r#"
        class Entity {
            x: f64;
            y: f64;
            hp: i32;
            team: i32;
            distSq: f64;
            constructor(x: f64, y: f64, hp: i32, team: i32) {
                this.distSq = 0.0;
            }
        }

        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;

        export function tick(me: i32): void {
            let mx: f64 = 5.0;
            let my: f64 = 5.0;
            let my_team: i32 = 1;

            let entities: Array<Entity> = new Array<Entity>(8);
            entities.push(new Entity(1.0, 1.0, 100, 2));
            entities.push(new Entity(9.0, 9.0, 50, 2));
            entities.push(new Entity(6.0, 5.0, 80, 1));
            entities.push(new Entity(4.0, 4.0, 0, 2));
            entities.push(new Entity(3.0, 5.0, 60, 2));

            // Filter: alive enemies
            let enemies: Array<Entity> = entities.filter(e => e.team != my_team && e.hp > 0);

            // Compute distSq for each enemy
            enemies.forEach(e => {
                let dx: f64 = e.x - mx;
                let dy: f64 = e.y - my;
                e.distSq = dx * dx + dy * dy;
            });

            // Sort by distance (closest first)
            enemies.sort((a, b) => a.distSq - b.distSq);

            // Act on closest enemy
            let closest: Entity = enemies[0];
            let dx: f64 = closest.x - mx;
            let dy: f64 = closest.y - my;
            set_action(me, 1, closest.hp, dx, dy);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();

    struct S {
        action: (i32, i32, i32, f64, f64),
    }
    let mut store = Store::new(
        &engine,
        S {
            action: (0, 0, 0, 0.0, 0.0),
        },
    );
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, S>, me: i32, kind: i32, target: i32, dx: f64, dy: f64| {
                caller.data_mut().action = (me, kind, target, dx, dy);
            },
        )
        .unwrap();

    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 42).unwrap();

    let a = store.data().action;
    assert_eq!(a.0, 42); // me
    assert_eq!(a.1, 1); // kind = attack

    // Alive enemies (team != 1, hp > 0):
    //   (1,1) hp=100 team=2, distSq = (1-5)^2 + (1-5)^2 = 32
    //   (9,9) hp=50 team=2,  distSq = (9-5)^2 + (9-5)^2 = 32
    //   (3,5) hp=60 team=2,  distSq = (3-5)^2 + (5-5)^2 = 4
    // Closest is (3,5) with distSq=4, hp=60
    assert_eq!(a.2, 60); // target = closest enemy hp
    assert!((a.3 - (-2.0)).abs() < 1e-10, "dx={}", a.3); // dx = 3-5 = -2
    assert!((a.4 - 0.0).abs() < 1e-10, "dy={}", a.4); // dy = 5-5 = 0
}

// ---- Phase 6 tests: TS sugar ----

#[test]
fn array_destructuring_basic() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            let arr: Array<f64> = new Array<f64>(4);
            arr.push(10.0);
            arr.push(20.0);
            arr.push(30.0);
            const [first, second, third] = arr;
            return first + second + third;
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
    assert!((result - 60.0).abs() < 1e-10);
}

#[test]
fn array_destructuring_class_elements() {
    // Destructure Array<Entity> — elements get class type tracking
    let wasm = compile(
        r#"
        class Entity {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}
        }

        export function test(): f64 {
            let entities: Array<Entity> = new Array<Entity>(4);
            entities.push(new Entity(1.0, 2.0));
            entities.push(new Entity(3.0, 4.0));
            const [first, second] = entities;
            return first.x + second.y;
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
    assert!((result - 5.0).abs() < 1e-10); // 1.0 + 4.0
}

#[test]
fn chained_filter_map() {
    // This tests that .filter() result can be used with .map()
    let wasm = compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        class Entity {
            hp: i32;
            team: i32;
            constructor(hp: i32, team: i32) {}
        }
        export function tick(me: i32): void {
            let entities: Array<Entity> = new Array<Entity>(8);
            entities.push(new Entity(10, 1));
            entities.push(new Entity(20, 2));
            entities.push(new Entity(30, 1));
            entities.push(new Entity(40, 2));

            let enemyHp: Array<i32> = entities
                .filter(e => e.team == 2)
                .map(e => e.hp);

            let total: i32 = enemyHp.reduce((sum, hp) => sum + hp, 0);
            set_action(me, total, enemyHp.length, 0.0, 0.0);
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, (0i32, 0i32));
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, (i32, i32)>,
             _me: i32,
             kind: i32,
             target: i32,
             _dx: f64,
             _dy: f64| {
                *caller.data_mut() = (kind, target);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();
    let (total, count) = *store.data();
    assert_eq!(total, 60, "enemy hp sum should be 20+40=60");
    assert_eq!(count, 2, "should have 2 enemies");
}

#[test]
fn large_array_sort_1000_elements() {
    // Tests that merge sort handles 1000 elements correctly
    let options = tscc::CompileOptions {
        host_module: "host".to_string(),
        memory_pages: 64, // 4MB for large arrays
        ..Default::default()
    };
    let wasm = tscc::compile(
        r#"
        declare function set_action(me: i32, kind: i32, target: i32, dx: f64, dy: f64): void;
        export function tick(me: i32): void {
            let arr: Array<i32> = new Array<i32>(1024);
            for (let i: i32 = 0; i < 1000; i++) {
                arr.push(999 - i);
            }
            arr.sort((a, b) => a - b);
            set_action(me, arr[0], arr[999], f64(arr[500]), 0.0);
        }
    "#,
        &options,
    )
    .unwrap();

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    struct S {
        action: (i32, i32, i32, f64, f64),
    }
    let mut store = Store::new(
        &engine,
        S {
            action: (0, 0, 0, 0.0, 0.0),
        },
    );
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "host",
            "set_action",
            |mut caller: Caller<'_, S>, me: i32, kind: i32, target: i32, dx: f64, dy: f64| {
                caller.data_mut().action = (me, kind, target, dx, dy);
            },
        )
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance
        .get_typed_func::<i32, ()>(&mut store, "tick")
        .unwrap();
    tick.call(&mut store, 0).unwrap();

    let a = store.data().action;
    assert_eq!(a.1, 0, "first element after sort should be 0");
    assert_eq!(a.2, 999, "last element after sort should be 999");
    assert!(
        (a.3 - 500.0).abs() < 1e-10,
        "middle element should be 500, got {}",
        a.3
    );
}

#[test]
fn array_shorthand_type_syntax() {
    // T[] should be equivalent to Array<T>
    let wasm = compile(
        r#"
        export function sum(): i32 {
            const xs: i32[] = new Array<i32>(4);
            xs.push(1);
            xs.push(2);
            xs.push(3);
            let s: i32 = 0;
            for (const x of xs) { s += x; }
            return s;
        }
    "#,
    );
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let sum = instance
        .get_typed_func::<(), i32>(&mut store, "sum")
        .unwrap();
    assert_eq!(sum.call(&mut store, ()).unwrap(), 6);
}

#[test]
fn array_destructuring_rest_basic() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            const src: Array<i32> = new Array<i32>(5);
            src.push(10); src.push(20); src.push(30); src.push(40); src.push(50);
            const [first, second, ...rest] = src;
            return first + second + rest.length + rest[0] + rest[2];
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
    // 10 + 20 + 3 (rest length) + 30 (rest[0]) + 50 (rest[2]) = 113
    assert_eq!(test.call(&mut store, ()).unwrap(), 113);
}

#[test]
fn array_destructuring_rest_empty_when_source_short() {
    let wasm = compile(
        r#"
        export function test(): i32 {
            const src: Array<i32> = new Array<i32>(2);
            src.push(7); src.push(8);
            const [a, b, ...rest] = src;
            return a + b + rest.length;
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
    // rest is empty — length 0
    assert_eq!(test.call(&mut store, ()).unwrap(), 15);
}

#[test]
fn array_destructuring_rest_f64_elements() {
    let wasm = compile(
        r#"
        export function test(): f64 {
            const src: Array<f64> = new Array<f64>(4);
            src.push(1.5); src.push(2.5); src.push(3.5); src.push(4.5);
            const [head, ...tail] = src;
            let sum: f64 = head;
            for (let i: i32 = 0; i < tail.length; ++i) {
                sum = sum + tail[i];
            }
            return sum;
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
    // 1.5 + 2.5 + 3.5 + 4.5 = 12.0
    assert_eq!(test.call(&mut store, ()).unwrap(), 12.0);
}

#[test]
fn array_destructuring_rest_only() {
    // `const [...all] = src` — rest with no prefix; should copy entire source.
    let wasm = compile(
        r#"
        export function test(): i32 {
            const src: Array<i32> = new Array<i32>(3);
            src.push(100); src.push(200); src.push(300);
            const [...all] = src;
            return all.length + all[0] + all[1] + all[2];
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
    assert_eq!(test.call(&mut store, ()).unwrap(), 3 + 100 + 200 + 300);
}

#[test]
fn array_map_with_index() {
    // map((val, idx) => val * 10 + idx) should use both element and index
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const arr: Array<f64> = new Array<f64>(3);
            arr.push(1.0); arr.push(2.0); arr.push(3.0);
            const mapped: Array<f64> = arr.map((v: f64, i: i32): f64 => v * 10.0 + f64(i));
            sink(mapped[0]); // 1*10 + 0 = 10
            sink(mapped[1]); // 2*10 + 1 = 21
            sink(mapped[2]); // 3*10 + 2 = 32
        }
    "#,
    );
    assert_eq!(vals, vec![10.0, 21.0, 32.0]);
}

#[test]
fn array_filter_with_index() {
    // filter((val, idx) => idx % 2 == 0) — keep even-indexed elements
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const arr: Array<f64> = new Array<f64>(5);
            arr.push(10.0); arr.push(20.0); arr.push(30.0); arr.push(40.0); arr.push(50.0);
            const evens: Array<f64> = arr.filter((v: f64, i: i32): i32 => i % 2 == 0);
            sink(f64(evens.length)); // 3
            sink(evens[0]); // 10
            sink(evens[1]); // 30
            sink(evens[2]); // 50
        }
    "#,
    );
    assert_eq!(vals, vec![3.0, 10.0, 30.0, 50.0]);
}

#[test]
fn array_foreach_with_index() {
    // forEach((val, idx) => sink(val + idx)) �� use both element and index
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const arr: Array<f64> = new Array<f64>(3);
            arr.push(100.0); arr.push(200.0); arr.push(300.0);
            arr.forEach((v: f64, i: i32) => { sink(v + f64(i)); });
        }
    "#,
    );
    assert_eq!(vals, vec![100.0, 201.0, 302.0]);
}

#[test]
fn array_find_index_with_index_param() {
    // findIndex((val, idx) => idx > 1 && val > 20) — use index in predicate
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const arr: Array<i32> = new Array<i32>(5);
            arr.push(10); arr.push(30); arr.push(15); arr.push(25); arr.push(5);
            const idx: i32 = arr.findIndex((v: i32, i: i32): i32 => i > 1 && v > 20);
            sink(f64(idx)); // should be 3 (value 25 at index 3, first where idx>1 && val>20)
        }
    "#,
    );
    assert_eq!(vals, vec![3.0]);
}

#[test]
fn array_some_every_with_index() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const arr: Array<i32> = new Array<i32>(3);
            arr.push(0); arr.push(1); arr.push(2);
            // some: val == idx is always true for [0,1,2]
            const s: i32 = arr.some((v: i32, i: i32): i32 => v == i);
            sink(f64(s));
            // every: val == idx is always true for [0,1,2]
            const e: i32 = arr.every((v: i32, i: i32): i32 => v == i);
            sink(f64(e));
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 1.0]);
}

#[test]
fn array_destructuring_rest_does_not_alias_source() {
    // Mutating rest must not affect the original array.
    let wasm = compile(
        r#"
        export function test(): i32 {
            const src: Array<i32> = new Array<i32>(4);
            src.push(1); src.push(2); src.push(3); src.push(4);
            const [head, ...rest] = src;
            rest[0] = 999;
            return src[1] + rest[0] + head;
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
    // src[1] still 2, rest[0] now 999, head is 1
    assert_eq!(test.call(&mut store, ()).unwrap(), 2 + 999 + 1);
}

#[test]
fn array_literal_i32_inferred() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const xs = [10, 20, 30];
            sink(xs.length as f64); // 3
            sink(xs[0] as f64);     // 10
            sink(xs[1] as f64);     // 20
            sink(xs[2] as f64);     // 30
            xs.push(40);
            sink(xs[3] as f64);     // 40 — capacity grew correctly
            sink(xs.length as f64); // 4
        }
    "#,
    );
    assert_eq!(vals, vec![3.0, 10.0, 20.0, 30.0, 40.0, 4.0]);
}

#[test]
fn array_literal_f64_inferred() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ys = [1.5, 2.5, 3.5];
            sink(ys[0]);
            sink(ys[1]);
            sink(ys[2]);
            sink(ys.length as f64);
        }
    "#,
    );
    assert_eq!(vals, vec![1.5, 2.5, 3.5, 3.0]);
}

#[test]
fn array_literal_empty_with_annotation() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const xs: number[] = [];
            sink(xs.length as f64); // 0
            xs.push(7.0);
            xs.push(9.0);
            sink(xs[0]);           // 7
            sink(xs[1]);           // 9
            sink(xs.length as f64); // 2
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 7.0, 9.0, 2.0]);
}

#[test]
fn array_literal_typed_with_annotation() {
    // When the annotation says number[] but the literal has integer-valued
    // elements, they widen to f64 automatically.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const xs: number[] = [1, 2, 3];
            sink(xs[0]);
            sink(xs[1]);
            sink(xs[2]);
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 2.0, 3.0]);
}

#[test]
fn array_literal_works_with_hof() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const xs = [1, 2, 3, 4];
            const doubled: Array<i32> = xs.map((v, _i) => v * 2);
            sink(doubled[0] as f64);
            sink(doubled[1] as f64);
            sink(doubled[2] as f64);
            sink(doubled[3] as f64);
            const sum: i32 = xs.reduce((acc, v) => acc + v, 0);
            sink(sum as f64); // 10
        }
    "#,
    );
    assert_eq!(vals, vec![2.0, 4.0, 6.0, 8.0, 10.0]);
}

#[test]
fn array_splice_remove_single() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const xs = [10, 20, 30, 40, 50];
            const removed: Array<i32> = xs.splice(2, 1);
            sink(removed.length as f64); // 1
            sink(removed[0] as f64);     // 30
            sink(xs.length as f64);      // 4
            sink(xs[0] as f64);          // 10
            sink(xs[1] as f64);          // 20
            sink(xs[2] as f64);          // 40
            sink(xs[3] as f64);          // 50
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 30.0, 4.0, 10.0, 20.0, 40.0, 50.0]);
}

#[test]
fn array_splice_insert_only() {
    // Delete 0, insert 2 items — array grows beyond initial capacity (5 → 7).
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const xs = [1, 2, 3, 4, 5];
            const removed: Array<i32> = xs.splice(2, 0, 99, 100);
            sink(removed.length as f64); // 0
            sink(xs.length as f64);      // 7
            sink(xs[0] as f64);          // 1
            sink(xs[1] as f64);          // 2
            sink(xs[2] as f64);          // 99
            sink(xs[3] as f64);          // 100
            sink(xs[4] as f64);          // 3
            sink(xs[5] as f64);          // 4
            sink(xs[6] as f64);          // 5
        }
    "#,
    );
    assert_eq!(
        vals,
        vec![0.0, 7.0, 1.0, 2.0, 99.0, 100.0, 3.0, 4.0, 5.0]
    );
}

#[test]
fn array_splice_replace_same_count() {
    // Delete 2, insert 2 — in-place, no length change.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const xs = [1, 2, 3, 4, 5];
            const removed: Array<i32> = xs.splice(1, 2, 99, 88);
            sink(removed.length as f64); // 2
            sink(removed[0] as f64);     // 2
            sink(removed[1] as f64);     // 3
            sink(xs.length as f64);      // 5
            sink(xs[0] as f64);          // 1
            sink(xs[1] as f64);          // 99
            sink(xs[2] as f64);          // 88
            sink(xs[3] as f64);          // 4
            sink(xs[4] as f64);          // 5
        }
    "#,
    );
    assert_eq!(
        vals,
        vec![2.0, 2.0, 3.0, 5.0, 1.0, 99.0, 88.0, 4.0, 5.0]
    );
}

#[test]
fn array_splice_no_delete_count_removes_tail() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const xs = [1, 2, 3, 4, 5];
            const removed: Array<i32> = xs.splice(2);
            sink(removed.length as f64); // 3
            sink(removed[0] as f64);     // 3
            sink(removed[1] as f64);     // 4
            sink(removed[2] as f64);     // 5
            sink(xs.length as f64);      // 2
            sink(xs[0] as f64);          // 1
            sink(xs[1] as f64);          // 2
        }
    "#,
    );
    assert_eq!(vals, vec![3.0, 3.0, 4.0, 5.0, 2.0, 1.0, 2.0]);
}

#[test]
fn array_splice_negative_start() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const xs = [1, 2, 3, 4, 5];
            // -2 means start at index len-2 = 3
            const removed: Array<i32> = xs.splice(-2, 1);
            sink(removed.length as f64); // 1
            sink(removed[0] as f64);     // 4
            sink(xs.length as f64);      // 4
            sink(xs[3] as f64);          // 5
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 4.0, 4.0, 5.0]);
}

#[test]
fn array_splice_delete_clamped_to_length() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const xs = [10, 20, 30];
            // Request deleting more than available — clamped to len - start.
            const removed: Array<i32> = xs.splice(1, 99);
            sink(removed.length as f64); // 2
            sink(removed[0] as f64);     // 20
            sink(removed[1] as f64);     // 30
            sink(xs.length as f64);      // 1
            sink(xs[0] as f64);          // 10
        }
    "#,
    );
    assert_eq!(vals, vec![2.0, 20.0, 30.0, 1.0, 10.0]);
}

#[test]
fn array_splice_f64_elements() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const xs: number[] = [1.5, 2.5, 3.5, 4.5];
            const removed: number[] = xs.splice(1, 2, 7.0, 8.0, 9.0);
            sink(removed.length as f64); // 2
            sink(removed[0]);            // 2.5
            sink(removed[1]);            // 3.5
            sink(xs.length as f64);      // 5
            sink(xs[0]);                 // 1.5
            sink(xs[1]);                 // 7.0
            sink(xs[2]);                 // 8.0
            sink(xs[3]);                 // 9.0
            sink(xs[4]);                 // 4.5
        }
    "#,
    );
    assert_eq!(
        vals,
        vec![2.0, 2.5, 3.5, 5.0, 1.5, 7.0, 8.0, 9.0, 4.5]
    );
}

#[test]
fn array_splice_empty_delete_empty_insert_noop() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const xs = [1, 2, 3];
            const removed: Array<i32> = xs.splice(1, 0);
            sink(removed.length as f64); // 0
            sink(xs.length as f64);      // 3
            sink(xs[0] as f64);          // 1
            sink(xs[1] as f64);          // 2
            sink(xs[2] as f64);          // 3
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 3.0, 1.0, 2.0, 3.0]);
}

#[test]
fn array_literal_spread_single_source() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const src = [1, 2, 3];
            const copy = [...src];
            sink(copy.length as f64); // 3
            sink(copy[0] as f64);     // 1
            sink(copy[1] as f64);     // 2
            sink(copy[2] as f64);     // 3
        }
    "#,
    );
    assert_eq!(vals, vec![3.0, 1.0, 2.0, 3.0]);
}

#[test]
fn array_literal_spread_with_inline_head_and_tail() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const mid = [20, 30];
            const xs = [10, ...mid, 40, 50];
            sink(xs.length as f64); // 5
            sink(xs[0] as f64);     // 10
            sink(xs[1] as f64);     // 20
            sink(xs[2] as f64);     // 30
            sink(xs[3] as f64);     // 40
            sink(xs[4] as f64);     // 50
        }
    "#,
    );
    assert_eq!(vals, vec![5.0, 10.0, 20.0, 30.0, 40.0, 50.0]);
}

#[test]
fn array_literal_spread_multiple_sources() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a = [1, 2];
            const b = [3, 4];
            const c = [5, 6];
            const joined = [...a, ...b, ...c];
            sink(joined.length as f64); // 6
            sink(joined[0] as f64);     // 1
            sink(joined[2] as f64);     // 3
            sink(joined[5] as f64);     // 6
        }
    "#,
    );
    assert_eq!(vals, vec![6.0, 1.0, 3.0, 6.0]);
}

#[test]
fn array_literal_spread_f64_elements() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const src: number[] = [1.5, 2.5];
            const xs: number[] = [0.5, ...src, 3.5];
            sink(xs.length as f64); // 4
            sink(xs[0]);            // 0.5
            sink(xs[1]);            // 1.5
            sink(xs[2]);            // 2.5
            sink(xs[3]);            // 3.5
        }
    "#,
    );
    assert_eq!(vals, vec![4.0, 0.5, 1.5, 2.5, 3.5]);
}

#[test]
fn array_literal_spread_from_empty_source() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const empty: Array<i32> = new Array<i32>(0);
            const xs = [1, ...empty, 2, ...empty];
            sink(xs.length as f64); // 2
            sink(xs[0] as f64);     // 1
            sink(xs[1] as f64);     // 2
        }
    "#,
    );
    assert_eq!(vals, vec![2.0, 1.0, 2.0]);
}

#[test]
fn array_literal_spread_element_type_mismatch_errors() {
    let err = compile_err(
        r#"
        export function bad(): i32 {
            const xs: number[] = [1.5];
            const ys: Array<i32> = [...xs]; // f64 spread into i32 literal
            return ys.length;
        }
    "#,
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("does not match") || msg.contains("element type"),
        "expected type-mismatch error, got: {msg}"
    );
}

#[test]
fn array_literal_empty_without_annotation_errors() {
    let err = compile_err(
        r#"
        export function bad(): i32 {
            const xs = [];
            return xs.length;
        }
    "#,
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("empty array literal"),
        "expected empty-literal error, got: {msg}"
    );
}

#[test]
fn array_of_basic_and_explicit_type() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            // Inferred i32 element type from first arg.
            const xs = Array.of(10, 20, 30);
            sink(xs.length as f64);
            sink(xs[0] as f64);
            sink(xs[2] as f64);

            // Explicit <f64> widens integer literals.
            const ys = Array.of<f64>(1, 2, 3);
            sink(ys.length as f64);
            sink(ys[0]);
            sink(ys[2]);

            // Empty with explicit type.
            const zs = Array.of<i32>();
            sink(zs.length as f64);
        }
    "#,
    );
    assert_eq!(
        vals,
        vec![3.0, 10.0, 30.0, 3.0, 1.0, 3.0, 0.0]
    );
}

#[test]
fn array_of_empty_without_type_errors() {
    let err = compile_err(
        r#"
        export function bad(): i32 {
            const xs = Array.of();
            return xs.length;
        }
    "#,
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("Array.of"),
        "expected Array.of error, got: {msg}"
    );
}

#[test]
fn array_from_shallow_clone_and_map() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const src: Array<i32> = [1, 2, 3, 4];

            // Form 1: shallow clone. Mutating the clone must not touch src.
            const clone = Array.from(src);
            clone.push(99);
            sink(src.length as f64);   // 4
            sink(clone.length as f64); // 5
            sink(clone[4] as f64);     // 99

            // Form 2: map. Keep element type i32 (arithmetic preserves type).
            const doubled = Array.from(src, (x, i) => x * 2 + i);
            sink(doubled.length as f64); // 4
            sink(doubled[0] as f64);     // 2  = 1*2 + 0
            sink(doubled[3] as f64);     // 11 = 4*2 + 3
        }
    "#,
    );
    assert_eq!(vals, vec![4.0, 5.0, 99.0, 4.0, 2.0, 11.0]);
}

#[test]
fn array_from_rejects_non_array_source() {
    let err = compile_err(
        r#"
        export function bad(): i32 {
            const n: i32 = 5;
            const xs = Array.from(n);
            return xs.length;
        }
    "#,
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("Array.from") || msg.contains("array"),
        "expected Array.from source error, got: {msg}"
    );
}

#[test]
fn array_shift_basic() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a: Array<i32> = new Array<i32>(8);
            a.push(10); a.push(20); a.push(30);
            sink(a.shift() as f64);  // 10
            sink(a.length as f64);   // 2
            sink(a[0] as f64);       // 20
            sink(a[1] as f64);       // 30
            sink(a.shift() as f64);  // 20
            sink(a.shift() as f64);  // 30
            sink(a.length as f64);   // 0
            // shift on empty returns 0 (like pop)
            sink(a.shift() as f64);  // 0
        }
    "#,
    );
    assert_eq!(vals, vec![10.0, 2.0, 20.0, 30.0, 20.0, 30.0, 0.0, 0.0]);
}

#[test]
fn array_unshift_in_place_and_growth() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            // In-place path: capacity = 8, plenty of room.
            const a: Array<i32> = new Array<i32>(8);
            a.push(3); a.push(4); a.push(5);
            const len1: i32 = a.unshift(1, 2); // insert two at front
            sink(len1 as f64);   // 5
            sink(a[0] as f64);   // 1
            sink(a[1] as f64);   // 2
            sink(a[2] as f64);   // 3
            sink(a[4] as f64);   // 5

            // Growth path: capacity tight, forces copy-and-abandon.
            let b: Array<i32> = [10, 20, 30]; // cap = 3
            const len2: i32 = b.unshift(7, 8, 9);
            sink(len2 as f64);   // 6
            sink(b.length as f64); // 6
            sink(b[0] as f64);   // 7
            sink(b[2] as f64);   // 9
            sink(b[5] as f64);   // 30

            // Zero-arg unshift is a no-op that returns the current length.
            const len3: i32 = b.unshift();
            sink(len3 as f64);   // 6
        }
    "#,
    );
    assert_eq!(
        vals,
        vec![5.0, 1.0, 2.0, 3.0, 5.0, 6.0, 6.0, 7.0, 9.0, 30.0, 6.0]
    );
}

#[test]
fn array_reduce_right() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const xs: Array<i32> = [1, 2, 3, 4];

            // Build a right-to-left concatenation index string so we can
            // tell reduce from reduceRight. Use numeric accumulator:
            // ((((0*10 + 4)*10 + 3)*10 + 2)*10 + 1) = 4321
            const rev: i32 = xs.reduceRight((acc, x) => acc * 10 + x, 0);
            sink(rev as f64); // 4321

            // Classic sum — same value from either direction.
            const sum: i32 = xs.reduceRight((acc, x) => acc + x, 0);
            sink(sum as f64); // 10

            // Empty array yields the initial value.
            const empty: Array<i32> = new Array<i32>(0);
            sink(empty.reduceRight((acc, x) => acc + x, 42) as f64);
        }
    "#,
    );
    assert_eq!(vals, vec![4321.0, 10.0, 42.0]);
}

