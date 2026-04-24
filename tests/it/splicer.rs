//! End-to-end tests for the L_splice splicer's production-hardening path.
//!
//! `__inline_test_sum_below` (defined in `helpers/src/inline.rs`) is written
//! to exercise the three features that the POC subset originally rejected:
//!
//! - **Return inside a nested control frame.** `if n <= 0 { return 0; }`
//!   places a `return` at nesting depth 1. The splicer rewrites it to
//!   `br 1`, targeting the wrapping block that replaces the helper's
//!   function frame.
//! - **Declared locals beyond params.** The loop's mutable `acc` and `i`
//!   force rustc to emit declared locals, so the splicer must allocate
//!   fresh caller-side slots and renumber every `local.get/set/tee`.
//! - **Nested block/loop/br_if.** rustc lowers the `while` loop to a
//!   block+loop pair with a `br_if` at the top; those helper-internal
//!   branch depths must remain untouched across the wrap.
//!
//! If any of the three rewrites is wrong the wasm module fails validation
//! or produces wrong answers for these reference values.

use std::collections::HashSet;

use wasmtime::*;

const NOOP_SOURCE: &str = "export function _noop(): i32 { return 0; }";

fn compile_with_exposed(names: &[&str]) -> Vec<u8> {
    let expose: HashSet<String> = names.iter().map(|s| s.to_string()).collect();
    let options = tscc::CompileOptions {
        expose_helpers: expose,
        ..Default::default()
    };
    tscc::compile(NOOP_SOURCE, &options).unwrap()
}

fn setup_instance(wasm: &[u8]) -> (Store<()>, Instance) {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    (store, instance)
}

fn sum_below_reference(n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }
    let mut acc: i32 = 0;
    let mut i: i32 = 0;
    while i < n {
        acc = acc.wrapping_add(i);
        i = i.wrapping_add(1);
    }
    acc
}

#[test]
fn splicer_golden_sum_below() {
    let wasm = compile_with_exposed(&["__inline_test_sum_below"]);
    let (mut store, instance) = setup_instance(&wasm);
    let f = instance
        .get_typed_func::<i32, i32>(&mut store, "__inline_test_sum_below")
        .unwrap();

    // Covers all three splicer features in one call set:
    //   n <= 0  → exercises the early return at nesting depth 1
    //   n > 0   → exercises the loop (declared locals + nested br_if)
    for &n in &[0, -1, -1000, 1, 2, 5, 10, 100, 1000] {
        let got = f.call(&mut store, n).unwrap();
        let want = sum_below_reference(n);
        assert_eq!(got, want, "__inline_test_sum_below({n})");
    }
}
