mod common;
use wasmtime::*;
use common::compile;

#[test]
fn closure_basic_no_capture() {
    let wasm = compile(
        r#"
        export function run(): i32 {
            const add = (a: i32, b: i32): i32 => a + b;
            return add(3, 4);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 7);
}

#[test]
fn closure_with_capture() {
    let wasm = compile(
        r#"
        export function run(): i32 {
            let offset: i32 = 10;
            const addOffset = (x: i32): i32 => x + offset;
            return addOffset(5);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 15);
}

#[test]
fn closure_capture_f64() {
    let wasm = compile(
        r#"
        export function run(): f64 {
            let scale: f64 = 2.5;
            const multiply = (x: f64): f64 => x * scale;
            return multiply(4.0);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), f64>(&mut store, "run")
        .unwrap();
    let result = run.call(&mut store, ()).unwrap();
    assert!((result - 10.0).abs() < 1e-10);
}

#[test]
fn closure_multiple_captures() {
    let wasm = compile(
        r#"
        export function run(): i32 {
            let a: i32 = 100;
            let b: i32 = 20;
            let c: i32 = 3;
            const sum = (x: i32): i32 => x + a + b + c;
            return sum(0);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 123);
}

#[test]
fn closure_multiple_closures() {
    let wasm = compile(
        r#"
        export function run(): i32 {
            const double = (x: i32): i32 => x * 2;
            const triple = (x: i32): i32 => x * 3;
            return double(5) + triple(5);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 25);
}

#[test]
fn closure_capture_class_instance() {
    let wasm = compile(
        r#"
        class Vec2 {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) {}
        }

        export function run(): f64 {
            let v = new Vec2(3.0, 4.0);
            const getX = (): f64 => v.x;
            return getX();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), f64>(&mut store, "run")
        .unwrap();
    let result = run.call(&mut store, ()).unwrap();
    assert!((result - 3.0).abs() < 1e-10);
}

#[test]
fn closure_as_function_parameter() {
    let wasm = compile(
        r#"
        function apply(x: i32, fn: (a: i32) => i32): i32 {
            return fn(x);
        }

        export function run(): i32 {
            const double = (a: i32): i32 => a * 2;
            return apply(7, double);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 14);
}

#[test]
fn closure_as_param_with_capture() {
    let wasm = compile(
        r#"
        function applyToFive(fn: (a: i32) => i32): i32 {
            return fn(5);
        }

        export function run(): i32 {
            let bonus: i32 = 100;
            const addBonus = (x: i32): i32 => x + bonus;
            return applyToFive(addBonus);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 105);
}

#[test]
fn closure_inline_builtins_still_work() {
    // Regression: inline closures in array builtins should still work
    let wasm = compile(
        r#"
        export function run(): i32 {
            let arr: Array<i32> = new Array<i32>(4);
            arr.push(1);
            arr.push(2);
            arr.push(3);
            arr.push(4);
            let evens: Array<i32> = arr.filter((x: i32): bool => x % 2 == 0);
            return evens.length;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 2);
}

// ── Closure hardening tests ─────────────────────────────────────────

#[test]
fn closure_return_from_function() {
    // Factory pattern: function returns a closure
    let wasm = compile(
        r#"
        function makeAdder(n: i32): (x: i32) => i32 {
            return (x: i32): i32 => x + n;
        }

        export function run(): i32 {
            const add5 = makeAdder(5);
            return add5(10);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 15);
}

#[test]
fn closure_return_multiple_factories() {
    let wasm = compile(
        r#"
        function makeMultiplier(factor: i32): (x: i32) => i32 {
            return (x: i32): i32 => x * factor;
        }

        export function run(): i32 {
            const double = makeMultiplier(2);
            const triple = makeMultiplier(3);
            return double(10) + triple(10);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 50); // 20 + 30
}

#[test]
fn closure_nested_closure_in_block_body() {
    // Closure inside a block-body closure
    let wasm = compile(
        r#"
        export function run(): i32 {
            const outer = (x: i32): i32 => {
                const inner = (y: i32): i32 => y + x;
                return inner(10);
            };
            return outer(5);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 15);
}

#[test]
fn closure_block_body_with_logic() {
    // Block-body closure with if/else
    let wasm = compile(
        r#"
        export function run(): i32 {
            const clamp = (x: i32, lo: i32, hi: i32): i32 => {
                if (x < lo) { return lo; }
                if (x > hi) { return hi; }
                return x;
            };
            return clamp(-5, 0, 100) + clamp(50, 0, 100) + clamp(200, 0, 100);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 150); // 0 + 50 + 100
}

#[test]
fn closure_capture_another_closure() {
    // Closure captures a variable holding another closure
    let wasm = compile(
        r#"
        export function run(): i32 {
            const add1 = (x: i32): i32 => x + 1;
            const applyTwice = (x: i32): i32 => add1(add1(x));
            return applyTwice(5);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 7);
}

#[test]
fn closure_inline_arrow_as_argument() {
    // Pass an anonymous arrow directly to a function expecting a closure parameter
    let wasm = compile(
        r#"
        function apply(x: i32, fn: (a: i32) => i32): i32 {
            return fn(x);
        }

        export function run(): i32 {
            return apply(6, (a: i32): i32 => a * 7);
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 42);
}

// ── Reference-capture (boxing) tests ────────────────────────────────

#[test]
fn closure_mutation_after_capture_visible() {
    // Core test: mutation after closure creation is visible to the closure
    let wasm = compile(
        r#"
        export function run(): i32 {
            let x: i32 = 1;
            const getX = (): i32 => x;
            x = 42;
            return getX();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 42); // NOT 1 — TS reference semantics
}

#[test]
fn closure_mutation_inside_closure_visible_outside() {
    // Mutation inside closure is visible in outer scope
    let wasm = compile(
        r#"
        export function run(): i32 {
            let counter: i32 = 0;
            const increment = (): void => { counter = counter + 1; };
            increment();
            increment();
            increment();
            return counter;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 3);
}

#[test]
fn closure_boxed_f64_mutation() {
    // f64 boxing works correctly
    let wasm = compile(
        r#"
        export function run(): f64 {
            let total: f64 = 0.0;
            const addTo = (x: f64): void => { total = total + x; };
            addTo(1.5);
            addTo(2.5);
            return total;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), f64>(&mut store, "run")
        .unwrap();
    let result = run.call(&mut store, ()).unwrap();
    assert!((result - 4.0).abs() < 1e-10);
}

#[test]
fn closure_boxed_compound_assignment() {
    // Compound assignment (+=) works with boxed vars
    let wasm = compile(
        r#"
        export function run(): i32 {
            let x: i32 = 10;
            const addToX = (n: i32): void => { x += n; };
            addToX(5);
            addToX(3);
            return x;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 18);
}

#[test]
fn closure_boxed_increment() {
    // ++/-- works with boxed vars
    let wasm = compile(
        r#"
        export function run(): i32 {
            let n: i32 = 0;
            const inc = (): i32 => { n++; return n; };
            inc();
            inc();
            let result = inc();
            return result;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 3);
}

#[test]
fn closure_non_mutated_capture_stays_unboxed() {
    // Sanity: variable captured but never mutated should still work (no boxing needed)
    let wasm = compile(
        r#"
        export function run(): i32 {
            let x: i32 = 99;
            const getX = (): i32 => x;
            return getX();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 99);
}

#[test]
fn closure_two_closures_share_boxed_var() {
    // Two closures sharing the same mutable variable
    let wasm = compile(
        r#"
        export function run(): i32 {
            let shared: i32 = 0;
            const inc = (): void => { shared += 1; };
            const get = (): i32 => shared;
            inc();
            inc();
            inc();
            return get();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 3);
}

#[test]
fn closure_shadow_variable_not_captured() {
    // Inner `local` shadows outer `local` — outer should NOT be captured
    let wasm = compile(
        r#"
        export function run(): i32 {
            let local: i32 = 42;
            const fn1 = (): i32 => {
                let local: i32 = 99;
                return local;
            };
            return fn1() + local;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 141); // 99 + 42
}

#[test]
fn closure_uninitialized_boxed_var() {
    // Boxed variable without initializer should default to zero
    let wasm = compile(
        r#"
        export function run(): i32 {
            let x: i32;
            const set = (val: i32): void => { x = val; };
            const get = (): i32 => x;
            let before = get();
            set(77);
            return before + get();
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 77); // 0 + 77
}

#[test]
fn closure_arrow_internal_mutation_no_false_boxing() {
    // Variable mutated only INSIDE an arrow body should not cause outer boxing
    let wasm = compile(
        r#"
        export function run(): i32 {
            let x: i32 = 10;
            const fn1 = (): i32 => {
                let temp: i32 = x;
                temp = temp + 5;
                return temp;
            };
            return fn1() + x;
        }
    "#,
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 25); // 15 + 10
}

