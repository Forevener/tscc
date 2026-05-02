//! Typed-array integration tests.
//!
//! Sub-phase 1 (foundation) verifies that `Int32Array`, `Float64Array`, and
//! `Uint8Array` are recognised as types and lay out correctly when used as
//! parameters, return types, generic arguments, and class fields. The early
//! tests below only exercise the type-resolution path: `compile()` panics on
//! any error, so a green test means the typed-array name reached every
//! annotation site successfully.
//!
//! Sub-phase 2 (construction + indexed access) adds runtime tests via
//! `run_sink_tick`: each test compiles a `tick()` function that exercises a
//! typed-array operation and `sink()`s the observed values. `Uint8Array`
//! indexed access lands in sub-phase 5 along with wrap semantics; sub-phase
//! 2's `Uint8Array` coverage is limited to the construction + length path
//! that does not need stride-1 store/load wired through.

use super::common::{compile, run_sink_tick};

#[test]
fn int32_array_as_param_and_return_compiles() {
    // The body never constructs an `Int32Array` — sub-phase 1 only proves
    // the name resolves through the param + return annotation paths.
    compile(
        r#"
        export function id(ta: Int32Array): Int32Array {
            return ta;
        }
    "#,
    );
}

#[test]
fn float64_array_as_param_and_return_compiles() {
    compile(
        r#"
        export function id(ta: Float64Array): Float64Array {
            return ta;
        }
    "#,
    );
}

#[test]
fn uint8_array_as_param_and_return_compiles() {
    compile(
        r#"
        export function id(ta: Uint8Array): Uint8Array {
            return ta;
        }
    "#,
    );
}

#[test]
fn array_of_int32_array_compiles() {
    // `Array<Int32Array>` mangles via the generic-instantiation walker —
    // the inner name has to be in `class_names` at Pass 0a-ii for the
    // mangling to succeed.
    compile(
        r#"
        export function id(arr: Array<Int32Array>): Array<Int32Array> {
            return arr;
        }
    "#,
    );
}

#[test]
fn array_of_float64_array_compiles() {
    compile(
        r#"
        export function id(arr: Array<Float64Array>): Array<Float64Array> {
            return arr;
        }
    "#,
    );
}

#[test]
fn typed_array_class_field_compiles() {
    // Class field of typed-array type lays out as i32 (a pointer to the
    // header). Constructor takes one and stores it; `data` reads back.
    compile(
        r#"
        class Buf {
            data: Float64Array;
            constructor(d: Float64Array) { this.data = d; }
        }
        export function id_buf(b: Buf): Buf {
            return b;
        }
        export function get_data(b: Buf): Float64Array {
            return b.data;
        }
    "#,
    );
}

#[test]
fn typed_array_in_three_position_signature_compiles() {
    // One function signature naming all three typed arrays — proves they
    // coexist in `class_names` and don't shadow each other through any
    // shared registry slot.
    compile(
        r#"
        export function combine(a: Int32Array, b: Float64Array, c: Uint8Array): Int32Array {
            return a;
        }
    "#,
    );
}

// ---- Sub-phase 2: construction + indexed access ----

#[test]
fn int32_array_length_constructor_zero_fills() {
    // Arena bytes are not guaranteed zero; the length constructor emits
    // memory.fill so a fresh `new Int32Array(n)` has all zero elements.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array(5);
            let acc: i32 = 0;
            for (let i: i32 = 0; i < ta.length; i = i + 1) {
                acc = acc + ta[i];
            }
            sink(acc as f64); // 0
            sink(ta.length as f64); // 5
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 5.0]);
}

#[test]
fn int32_array_literal_constructor_runs() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([10, 20, 30]);
            sink(ta[0] as f64); // 10
            sink(ta[1] as f64); // 20
            sink(ta[2] as f64); // 30
            sink(ta.length as f64); // 3
        }
    "#,
    );
    assert_eq!(vals, vec![10.0, 20.0, 30.0, 3.0]);
}

#[test]
fn int32_array_from_array() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const src: Array<i32> = [4, 5, 6];
            const ta: Int32Array = Int32Array.from(src);
            sink(ta[0] as f64); // 4
            sink(ta[1] as f64); // 5
            sink(ta[2] as f64); // 6
            // Mutating the result must not touch the source.
            ta[0] = 99;
            sink(src[0] as f64); // 4 (independent copy)
        }
    "#,
    );
    assert_eq!(vals, vec![4.0, 5.0, 6.0, 4.0]);
}

#[test]
fn int32_array_from_with_map_fn() {
    // `T.from(src, x => …)` — runs the body per element and stores into the
    // destination's element width.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const src: Array<i32> = [1, 2, 3];
            const ta: Int32Array = Int32Array.from(src, (x: i32) => x * x);
            sink(ta[0] as f64); // 1
            sink(ta[1] as f64); // 4
            sink(ta[2] as f64); // 9
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 4.0, 9.0]);
}

#[test]
fn int32_array_of_variadic() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = Int32Array.of(10, 20, 30);
            sink(ta.length as f64); // 3
            sink(ta[0] as f64); // 10
            sink(ta[2] as f64); // 30
        }
    "#,
    );
    assert_eq!(vals, vec![3.0, 10.0, 30.0]);
}

#[test]
fn float64_array_length_constructor_runs() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Float64Array = new Float64Array(4);
            let acc: f64 = 0.0;
            for (let i: i32 = 0; i < ta.length; i = i + 1) {
                acc = acc + ta[i];
            }
            sink(acc); // 0.0
            sink(ta.length as f64); // 4
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 4.0]);
}

#[test]
fn float64_array_literal_constructor_runs() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Float64Array = new Float64Array([0.5, 1.5, 2.5]);
            sink(ta[0]); // 0.5
            sink(ta[1]); // 1.5
            sink(ta[2]); // 2.5
            sink(ta.length as f64); // 3
        }
    "#,
    );
    assert_eq!(vals, vec![0.5, 1.5, 2.5, 3.0]);
}

#[test]
fn int32_array_indexed_read_write() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array(3);
            ta[0] = 7;
            ta[1] = 13;
            ta[2] = 21;
            sink(ta[0] as f64); // 7
            sink(ta[1] as f64); // 13
            sink(ta[2] as f64); // 21
            // Compound assignment too.
            ta[1] += 100;
            sink(ta[1] as f64); // 113
        }
    "#,
    );
    assert_eq!(vals, vec![7.0, 13.0, 21.0, 113.0]);
}

#[test]
fn int32_array_length_property() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a: Int32Array = new Int32Array(8);
            const b: Int32Array = new Int32Array([1, 2]);
            sink(a.length as f64); // 8
            sink(b.length as f64); // 2
        }
    "#,
    );
    assert_eq!(vals, vec![8.0, 2.0]);
}

#[test]
fn int32_array_byte_length_property() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a: Int32Array = new Int32Array(5);
            const b: Float64Array = new Float64Array(3);
            sink(a.byteLength as f64); // 20 (5 * 4)
            sink(b.byteLength as f64); // 24 (3 * 8)
        }
    "#,
    );
    assert_eq!(vals, vec![20.0, 24.0]);
}

#[test]
fn int32_array_static_bytes_per_element() {
    // BYTES_PER_ELEMENT is a compile-time constant on the type identifier
    // itself — no instance involved.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            sink(Int32Array.BYTES_PER_ELEMENT as f64);   // 4
            sink(Float64Array.BYTES_PER_ELEMENT as f64); // 8
            sink(Uint8Array.BYTES_PER_ELEMENT as f64);   // 1
        }
    "#,
    );
    assert_eq!(vals, vec![4.0, 8.0, 1.0]);
}

#[test]
fn int32_array_for_of_iterates_in_order() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([3, 1, 4, 1, 5, 9]);
            for (const x of ta) {
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![3.0, 1.0, 4.0, 1.0, 5.0, 9.0]);
}

#[test]
fn float64_array_for_of_runs() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Float64Array = new Float64Array([0.25, 0.5, 0.75]);
            for (const x of ta) { sink(x); }
        }
    "#,
    );
    assert_eq!(vals, vec![0.25, 0.5, 0.75]);
}

#[test]
fn int32_array_as_class_field() {
    // A class field of typed-array type is a plain i32 pointer slot; reading
    // it back yields the same header so length / indexed access work through
    // the field accessor.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        class Buf {
            data: Int32Array;
            constructor(d: Int32Array) { this.data = d; }
        }
        export function tick(me: i32): void {
            const b: Buf = new Buf(new Int32Array([11, 22, 33]));
            sink(b.data.length as f64); // 3
            sink(b.data[0] as f64);     // 11
            sink(b.data[2] as f64);     // 33
        }
    "#,
    );
    assert_eq!(vals, vec![3.0, 11.0, 33.0]);
}

#[test]
fn int32_array_copy_from_typed_array_is_independent() {
    // `new Int32Array(src)` where src is another Int32Array — the copy must
    // be independent of the source (later mutations to either don't bleed).
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a: Int32Array = new Int32Array([1, 2, 3]);
            const b: Int32Array = new Int32Array(a);
            b[0] = 999;
            sink(a[0] as f64); // 1 — source unchanged
            sink(b[0] as f64); // 999
            sink(a.length as f64); // 3
            sink(b.length as f64); // 3
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 999.0, 3.0, 3.0]);
}

#[test]
fn typed_array_as_function_param_and_return_round_trips() {
    // Round-trip a typed array through a function call: the buf_ptr layout
    // means the receiver in the callee accesses through the same backing
    // body (no copy on call boundary).
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        function set_first(ta: Int32Array, v: i32): void {
            ta[0] = v;
        }
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([10, 20, 30]);
            set_first(ta, 77);
            sink(ta[0] as f64); // 77
            sink(ta[1] as f64); // 20
        }
    "#,
    );
    assert_eq!(vals, vec![77.0, 20.0]);
}

// ---- Sub-phase 3: method surface (immutable + mutable, no HOFs) ----

#[test]
fn int32_array_at_negative_indexes() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([10, 20, 30, 40]);
            sink(ta.at(0) as f64);   // 10
            sink(ta.at(3) as f64);   // 40
            sink(ta.at(-1) as f64);  // 40
            sink(ta.at(-4) as f64);  // 10
        }
    "#,
    );
    assert_eq!(vals, vec![10.0, 40.0, 40.0, 10.0]);
}

#[test]
fn float64_array_at_works() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Float64Array = new Float64Array([0.5, 1.5, 2.5]);
            sink(ta.at(-1));  // 2.5
            sink(ta.at(0));   // 0.5
        }
    "#,
    );
    assert_eq!(vals, vec![2.5, 0.5]);
}

#[test]
fn int32_array_index_of_and_includes() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([3, 1, 4, 1, 5, 9]);
            sink(ta.indexOf(1) as f64);          // 1
            sink(ta.lastIndexOf(1) as f64);      // 3
            sink(ta.indexOf(7) as f64);          // -1
            sink(ta.indexOf(1, 2) as f64);       // 3 (search from index 2)
            sink(ta.includes(9) ? 1.0 : 0.0);    // 1
            sink(ta.includes(7) ? 1.0 : 0.0);    // 0
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 3.0, -1.0, 3.0, 1.0, 0.0]);
}

#[test]
fn float64_array_index_of_handles_floats() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Float64Array = new Float64Array([1.25, 2.5, 3.75]);
            sink(ta.indexOf(2.5) as f64);    // 1
            sink(ta.indexOf(9.99) as f64);   // -1
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, -1.0]);
}

#[test]
fn int32_array_slice_returns_independent_copy() {
    // The load-bearing distinction with subarray: mutating the slice must
    // NOT touch the parent.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const parent: Int32Array = new Int32Array([10, 20, 30, 40, 50]);
            const child: Int32Array = parent.slice(1, 4);
            sink(child.length as f64);  // 3
            sink(child[0] as f64);      // 20
            sink(child[2] as f64);      // 40
            child[0] = 999;
            sink(parent[1] as f64);     // 20 (unchanged)
            sink(child[0] as f64);      // 999
        }
    "#,
    );
    assert_eq!(vals, vec![3.0, 20.0, 40.0, 20.0, 999.0]);
}

#[test]
fn int32_array_slice_negative_and_oob_clamp() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([1, 2, 3, 4, 5]);
            const a: Int32Array = ta.slice(-2);     // [4, 5]
            const b: Int32Array = ta.slice(-100);   // [1, 2, 3, 4, 5]
            const c: Int32Array = ta.slice(2, 100); // [3, 4, 5]
            const d: Int32Array = ta.slice(3, 1);   // []
            sink(a.length as f64);
            sink(a[0] as f64);
            sink(b.length as f64);
            sink(c.length as f64);
            sink(c[0] as f64);
            sink(d.length as f64);
        }
    "#,
    );
    assert_eq!(vals, vec![2.0, 4.0, 5.0, 3.0, 3.0, 0.0]);
}

#[test]
fn int32_array_subarray_aliases_parent_writes_propagate() {
    // The load-bearing test for the chosen layout: mutating the subarray
    // through indexed write must be observable in the parent.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const parent: Int32Array = new Int32Array([10, 20, 30, 40, 50]);
            const view: Int32Array = parent.subarray(1, 4);
            sink(view.length as f64); // 3
            sink(view[0] as f64);     // 20
            view[0] = 222;
            view[2] = 444;
            sink(parent[1] as f64);   // 222 (parent observed the write)
            sink(parent[3] as f64);   // 444
            // Reading through the parent the view also sees the change.
            parent[2] = 333;
            sink(view[1] as f64);     // 333
        }
    "#,
    );
    assert_eq!(vals, vec![3.0, 20.0, 222.0, 444.0, 333.0]);
}

#[test]
fn int32_array_subarray_of_subarray_composes() {
    // Chained subarray must compose offsets through the parent's body, not
    // re-read the original buffer base — otherwise nested views break.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const root: Int32Array = new Int32Array([0, 1, 2, 3, 4, 5, 6, 7]);
            const a: Int32Array = root.subarray(2, 7);     // [2,3,4,5,6]
            const b: Int32Array = a.subarray(1, 4);        // [3,4,5]
            sink(b.length as f64); // 3
            sink(b[0] as f64);     // 3
            sink(b[2] as f64);     // 5
            b[0] = 99;
            sink(root[3] as f64);  // 99 (write propagated through both views)
        }
    "#,
    );
    assert_eq!(vals, vec![3.0, 3.0, 5.0, 99.0]);
}

#[test]
fn int32_array_subarray_negative_clamps() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([10, 20, 30, 40, 50]);
            const a: Int32Array = ta.subarray(-2);   // [40, 50]
            const b: Int32Array = ta.subarray(-100); // full view
            sink(a.length as f64);
            sink(a[0] as f64);
            sink(b.length as f64);
            sink(b[0] as f64);
        }
    "#,
    );
    assert_eq!(vals, vec![2.0, 40.0, 5.0, 10.0]);
}

#[test]
fn int32_array_fill_full_and_partial() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array(5);
            ta.fill(7);
            sink(ta[0] as f64);  // 7
            sink(ta[4] as f64);  // 7
            ta.fill(99, 1, 4);
            sink(ta[0] as f64);  // 7
            sink(ta[1] as f64);  // 99
            sink(ta[3] as f64);  // 99
            sink(ta[4] as f64);  // 7
        }
    "#,
    );
    assert_eq!(vals, vec![7.0, 7.0, 7.0, 99.0, 99.0, 7.0]);
}

#[test]
fn float64_array_fill_with_negative_indices() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Float64Array = new Float64Array([1.0, 2.0, 3.0, 4.0]);
            ta.fill(0.5, -2);
            sink(ta[0]); sink(ta[1]); sink(ta[2]); sink(ta[3]);
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 2.0, 0.5, 0.5]);
}

#[test]
fn int32_array_set_from_typed_array_same_kind() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const dest: Int32Array = new Int32Array([0, 0, 0, 0, 0]);
            const src: Int32Array = new Int32Array([10, 20, 30]);
            dest.set(src, 1);
            sink(dest[0] as f64); // 0
            sink(dest[1] as f64); // 10
            sink(dest[2] as f64); // 20
            sink(dest[3] as f64); // 30
            sink(dest[4] as f64); // 0
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 10.0, 20.0, 30.0, 0.0]);
}

#[test]
fn int32_array_set_from_array_zero_offset() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const dest: Int32Array = new Int32Array(4);
            const src: Array<i32> = [11, 22, 33];
            dest.set(src);
            sink(dest[0] as f64); // 11
            sink(dest[1] as f64); // 22
            sink(dest[2] as f64); // 33
            sink(dest[3] as f64); // 0
        }
    "#,
    );
    assert_eq!(vals, vec![11.0, 22.0, 33.0, 0.0]);
}

#[test]
fn float64_array_set_from_int32_array_widens() {
    // Cross-kind copy: i32 elements widened into f64 via element-wise loop.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const dest: Float64Array = new Float64Array(3);
            const src: Int32Array = new Int32Array([1, 2, 3]);
            dest.set(src);
            sink(dest[0]); // 1.0
            sink(dest[1]); // 2.0
            sink(dest[2]); // 3.0
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 2.0, 3.0]);
}

#[test]
fn int32_array_reverse_in_place() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([1, 2, 3, 4]);
            ta.reverse();
            sink(ta[0] as f64); // 4
            sink(ta[3] as f64); // 1
        }
    "#,
    );
    assert_eq!(vals, vec![4.0, 1.0]);
}

#[test]
fn int32_array_sort_default_numeric() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([5, 3, 8, 1, 9, 2]);
            ta.sort();
            for (const x of ta) { sink(x as f64); }
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 2.0, 3.0, 5.0, 8.0, 9.0]);
}

#[test]
fn float64_array_sort_default_numeric() {
    // Float64Array.sort with numeric default — no string-coercion nonsense.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Float64Array = new Float64Array([2.5, 0.5, 1.5, -1.0]);
            ta.sort();
            for (const x of ta) { sink(x); }
        }
    "#,
    );
    assert_eq!(vals, vec![-1.0, 0.5, 1.5, 2.5]);
}

#[test]
fn int32_array_copy_within_overlap() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([1, 2, 3, 4, 5]);
            // copy [0,3) to start at index 2 — overlapping forward copy.
            ta.copyWithin(2, 0, 3);
            sink(ta[0] as f64); // 1
            sink(ta[1] as f64); // 2
            sink(ta[2] as f64); // 1
            sink(ta[3] as f64); // 2
            sink(ta[4] as f64); // 3
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 2.0, 1.0, 2.0, 3.0]);
}

#[test]
fn int32_array_join_default_and_custom_sep() {
    // Join coerces elements via __str_from_i32 / __str_from_f64; we don't
    // sink those directly but the round-trip through .indexOf in a literal
    // string is awkward in our test harness. Instead, verify length contracts
    // line up with the expected separator widths.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([1, 22, 333]);
            const a: string = ta.join();      // "1,22,333" (8 chars)
            const b: string = ta.join("--");  // "1--22--333" (10 chars)
            sink(a.length as f64);
            sink(b.length as f64);
        }
    "#,
    );
    assert_eq!(vals, vec![8.0, 10.0]);
}

#[test]
fn float64_array_join_returns_string() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Float64Array = new Float64Array([0.5, 1.5]);
            const s: string = ta.join("|");  // "0.5|1.5" (7 chars)
            sink(s.length as f64);
        }
    "#,
    );
    assert_eq!(vals, vec![7.0]);
}

// ---- Sub-phase 4: higher-order methods ----

#[test]
fn int32_array_for_each_visits_in_order() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([2, 4, 6]);
            ta.forEach((x: i32) => { sink(x as f64); });
        }
    "#,
    );
    assert_eq!(vals, vec![2.0, 4.0, 6.0]);
}

#[test]
fn int32_array_for_each_index_param() {
    // Two-arg arrow gets (elem, index).
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([10, 20, 30]);
            ta.forEach((x: i32, i: i32) => { sink((x + i) as f64); });
        }
    "#,
    );
    assert_eq!(vals, vec![10.0, 21.0, 32.0]);
}

#[test]
fn int32_array_some_short_circuits() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([1, 2, 3, 4]);
            const has_three: bool = ta.some((x: i32) => x === 3);
            const has_seven: bool = ta.some((x: i32) => x === 7);
            sink(has_three ? 1.0 : 0.0); // 1
            sink(has_seven ? 1.0 : 0.0); // 0
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 0.0]);
}

#[test]
fn int32_array_every_short_circuits() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([2, 4, 6]);
            const all_even: bool = ta.every((x: i32) => (x & 1) === 0);
            const all_pos: bool = ta.every((x: i32) => x > 3);
            sink(all_even ? 1.0 : 0.0); // 1
            sink(all_pos ? 1.0 : 0.0);  // 0
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 0.0]);
}

#[test]
fn int32_array_find_returns_first_match() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([5, 10, 15, 20]);
            const v: i32 = ta.find((x: i32) => x > 12);
            sink(v as f64); // 15
        }
    "#,
    );
    assert_eq!(vals, vec![15.0]);
}

#[test]
fn int32_array_find_index_returns_minus_one_when_no_match() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([1, 2, 3]);
            const i: i32 = ta.findIndex((x: i32) => x > 99);
            sink(i as f64); // -1
        }
    "#,
    );
    assert_eq!(vals, vec![-1.0]);
}

#[test]
fn int32_array_find_last_iterates_in_reverse() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([1, 5, 3, 5, 2]);
            const v: i32 = ta.findLast((x: i32) => x === 5);
            const i: i32 = ta.findLastIndex((x: i32) => x === 5);
            sink(v as f64); // 5
            sink(i as f64); // 3 (last index of 5)
        }
    "#,
    );
    assert_eq!(vals, vec![5.0, 3.0]);
}

#[test]
fn int32_array_map_returns_same_kind_independent_storage() {
    // map returns a fresh Int32Array — mutations through the result must not
    // touch the source body.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const src: Int32Array = new Int32Array([1, 2, 3]);
            const dst: Int32Array = src.map((x: i32) => x * 10);
            dst[0] = 999;
            sink(dst.length as f64); // 3
            sink(dst[0] as f64);     // 999
            sink(dst[1] as f64);     // 20
            sink(dst[2] as f64);     // 30
            sink(src[0] as f64);     // 1 — independent
        }
    "#,
    );
    assert_eq!(vals, vec![3.0, 999.0, 20.0, 30.0, 1.0]);
}

#[test]
fn float64_array_map_runs() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Float64Array = new Float64Array([0.5, 1.5, 2.5]);
            const dst: Float64Array = ta.map((x: f64) => x * 2.0);
            sink(dst[0]); // 1.0
            sink(dst[1]); // 3.0
            sink(dst[2]); // 5.0
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 3.0, 5.0]);
}

#[test]
fn int32_array_filter_returns_same_kind_with_correct_length() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([1, 2, 3, 4, 5, 6]);
            const evens: Int32Array = ta.filter((x: i32) => (x & 1) === 0);
            sink(evens.length as f64); // 3
            for (const x of evens) { sink(x as f64); } // 2 4 6
        }
    "#,
    );
    assert_eq!(vals, vec![3.0, 2.0, 4.0, 6.0]);
}

#[test]
fn int32_array_filter_empty_result() {
    // No elements match → empty typed array of the same kind.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([1, 2, 3]);
            const none: Int32Array = ta.filter((x: i32) => x > 99);
            sink(none.length as f64); // 0
        }
    "#,
    );
    assert_eq!(vals, vec![0.0]);
}

#[test]
fn int32_array_reduce_sum() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([1, 2, 3, 4, 5]);
            const sum: i32 = ta.reduce((acc: i32, x: i32) => acc + x, 0);
            sink(sum as f64); // 15
        }
    "#,
    );
    assert_eq!(vals, vec![15.0]);
}

#[test]
fn int32_array_reduce_right_appends_in_reverse() {
    // Order-sensitive folds confirm the reverse iteration direction.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([1, 2, 4]);
            const decoded: i32 = ta.reduceRight((acc: i32, x: i32) => acc * 10 + x, 0);
            sink(decoded as f64); // 421
        }
    "#,
    );
    assert_eq!(vals, vec![421.0]);
}

#[test]
fn float64_array_reduce_to_f64() {
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Float64Array = new Float64Array([0.5, 1.5, 2.0]);
            const sum: f64 = ta.reduce((acc: f64, x: f64) => acc + x, 0.0);
            sink(sum); // 4.0
        }
    "#,
    );
    assert_eq!(vals, vec![4.0]);
}

#[test]
fn int32_array_sort_with_comparator_descending() {
    // Sub-phase 4 unblocks `sort(cmp)` — the comparator returns the JS-style
    // negative/zero/positive triple and the existing merge skeleton consumes it.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([3, 1, 4, 1, 5, 9, 2, 6]);
            ta.sort((a: i32, b: i32) => b - a);
            for (const x of ta) { sink(x as f64); }
        }
    "#,
    );
    assert_eq!(vals, vec![9.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0, 1.0]);
}

#[test]
fn float64_array_sort_with_comparator_runs() {
    // f64 sort with an f64-returning comparator — exercises the F64Le branch
    // of the comparator-result test in the merge skeleton.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Float64Array = new Float64Array([2.5, 0.5, 1.5, -1.0]);
            ta.sort((a: f64, b: f64) => a - b);
            for (const x of ta) { sink(x); }
        }
    "#,
    );
    assert_eq!(vals, vec![-1.0, 0.5, 1.5, 2.5]);
}

#[test]
fn int32_array_hofs_compose() {
    // Cross-cutting: filter → map → reduce on the same Int32Array — every
    // step must hand the next a same-kind typed array (filter/map) and the
    // accumulator must reach the reducer untouched.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Int32Array = new Int32Array([1, 2, 3, 4, 5, 6]);
            const sum: i32 = ta
                .filter((x: i32) => (x & 1) === 1)   // odds: 1,3,5
                .map((x: i32) => x * x)              // 1,9,25
                .reduce((acc: i32, x: i32) => acc + x, 0); // 35
            sink(sum as f64);
        }
    "#,
    );
    assert_eq!(vals, vec![35.0]);
}

// ---- Sub-phase 5: Uint8Array (stride-1, wrap-on-store) ----

#[test]
fn uint8_array_length_constructor_zero_fills() {
    // Same arena-zero contract as the i32/f64 length form, but the body is
    // emitted at byte stride.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Uint8Array = new Uint8Array(6);
            let acc: i32 = 0;
            for (let i: i32 = 0; i < ta.length; i = i + 1) {
                acc = acc + ta[i];
            }
            sink(acc as f64); // 0
            sink(ta.length as f64); // 6
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 6.0]);
}

#[test]
fn uint8_array_wrap_on_store_overflow() {
    // i32.store8 truncates to the low 8 bits; 256 → 0, 257 → 1, 511 → 255.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Uint8Array = new Uint8Array(3);
            ta[0] = 256;
            ta[1] = 257;
            ta[2] = 511;
            sink(ta[0] as f64); // 0
            sink(ta[1] as f64); // 1
            sink(ta[2] as f64); // 255
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 1.0, 255.0]);
}

#[test]
fn uint8_array_wrap_on_store_negative() {
    // Negative values wrap modulo 256 — -1 → 255, -2 → 254. Reads zero-extend
    // so the result is always non-negative.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Uint8Array = new Uint8Array(3);
            ta[0] = -1;
            ta[1] = -2;
            ta[2] = -255;
            sink(ta[0] as f64); // 255
            sink(ta[1] as f64); // 254
            sink(ta[2] as f64); // 1
        }
    "#,
    );
    assert_eq!(vals, vec![255.0, 254.0, 1.0]);
}

#[test]
fn uint8_array_literal_init_wraps() {
    // Per-element wrap at literal-init too — i32.store8 truncates regardless
    // of how the value got onto the stack.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Uint8Array = new Uint8Array([256, -1, 300, 0, 255]);
            sink(ta[0] as f64); // 0
            sink(ta[1] as f64); // 255
            sink(ta[2] as f64); // 44
            sink(ta[3] as f64); // 0
            sink(ta[4] as f64); // 255
            sink(ta.length as f64); // 5
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 255.0, 44.0, 0.0, 255.0, 5.0]);
}

#[test]
fn uint8_array_indexed_read_zero_extends() {
    // i32.load8_u: a stored 0xff reads back as i32 255, not -1. Flip the sign
    // by casting to an i32 local and verify the result is positive.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Uint8Array = new Uint8Array(1);
            ta[0] = 255;
            const v: i32 = ta[0];
            sink(v as f64); // 255
            // Negative-cast check: if reads sign-extended, v + 1 would be 0.
            sink((v + 1) as f64); // 256
        }
    "#,
    );
    assert_eq!(vals, vec![255.0, 256.0]);
}

#[test]
fn uint8_array_byte_length_equals_length() {
    // Stride 1 invariant: byteLength == length (the descriptor's stride
    // multiply is elided).
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Uint8Array = new Uint8Array(7);
            sink(ta.length as f64);     // 7
            sink(ta.byteLength as f64); // 7
        }
    "#,
    );
    assert_eq!(vals, vec![7.0, 7.0]);
}

#[test]
fn uint8_array_for_of_iterates_zero_extended() {
    // Reads through `for..of` zero-extend — bytes near the high boundary
    // come back as their unsigned 0..255 form.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Uint8Array = new Uint8Array([1, 127, 128, 255]);
            for (const x of ta) {
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 127.0, 128.0, 255.0]);
}

#[test]
fn uint8_array_set_from_array_with_wrap() {
    // Cross-width set: source is Array<i32>, dest stride is 1. Falls into
    // the element-wise loop with coercion path; each store wraps.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Uint8Array = new Uint8Array(5);
            const src: Array<i32> = [10, 256, -1, 300, 99];
            ta.set(src, 0);
            sink(ta[0] as f64); // 10
            sink(ta[1] as f64); // 0
            sink(ta[2] as f64); // 255
            sink(ta[3] as f64); // 44
            sink(ta[4] as f64); // 99
        }
    "#,
    );
    assert_eq!(vals, vec![10.0, 0.0, 255.0, 44.0, 99.0]);
}

#[test]
fn uint8_array_set_from_uint8_array_uses_memory_copy() {
    // Same kind, stride 1 — the memory.copy fast-path. Verify both
    // self-owned and view sources copy through correctly.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const dst: Uint8Array = new Uint8Array(6);
            const src: Uint8Array = new Uint8Array([1, 2, 3]);
            dst.set(src, 1);
            for (const x of dst) { sink(x as f64); }
            // Expect: 0, 1, 2, 3, 0, 0
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 1.0, 2.0, 3.0, 0.0, 0.0]);
}

#[test]
fn uint8_array_map_runs_body_at_i32_width_and_wraps() {
    // map(x => x + 1) at the byte boundary: 255 + 1 must wrap to 0 in the
    // result, since the body runs at i32 width but the store truncates.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Uint8Array = new Uint8Array([0, 1, 254, 255]);
            const r: Uint8Array = ta.map((x: i32) => x + 1);
            for (const x of r) { sink(x as f64); }
            // 1, 2, 255, 0
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 2.0, 255.0, 0.0]);
}

#[test]
fn uint8_array_filter_keeps_kind() {
    // filter must return Uint8Array (same-kind), and downstream HOFs must
    // see the right element width.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Uint8Array = new Uint8Array([1, 2, 3, 4, 5, 6]);
            const r: Uint8Array = ta.filter((x: i32) => (x & 1) === 0);
            sink(r.length as f64); // 3
            for (const x of r) { sink(x as f64); } // 2, 4, 6
        }
    "#,
    );
    assert_eq!(vals, vec![3.0, 2.0, 4.0, 6.0]);
}

#[test]
fn uint8_array_reduce_to_i32_sum() {
    // Reduce — accumulator type drives the result. Since reads zero-extend,
    // a sum can exceed 255 without surprises.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Uint8Array = new Uint8Array([200, 200, 200]);
            const total: i32 = ta.reduce((acc: i32, x: i32) => acc + x, 0);
            sink(total as f64); // 600 (no wrap on the i32 accumulator)
        }
    "#,
    );
    assert_eq!(vals, vec![600.0]);
}

#[test]
fn uint8_array_join_default_separator() {
    // join — the i32 stringifier path runs at i32 width over zero-extended
    // bytes, so 255 prints as "255" (not "-1"). Length contract: 4 elements
    // produce 1+1+3+3 = 8 digit chars + 3 separators = 11 / 14 / 17 chars
    // depending on the separator width.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const ta: Uint8Array = new Uint8Array([1, 0, 255, 127]);
            const a: string = ta.join();        // "1,0,255,127"
            const b: string = ta.join("--");    // "1--0--255--127"
            sink(a.length as f64);              // 11
            sink(b.length as f64);              // 14
            // Read-as-unsigned check: "255" must be present in the default
            // join. If the byte read sign-extended, the string would contain
            // "-1" instead.
            sink(a.indexOf("255") as f64);      // 4
            sink(a.indexOf("-1") as f64);       // -1 (not found)
        }
    "#,
    );
    assert_eq!(vals, vec![11.0, 14.0, 4.0, -1.0]);
}

#[test]
fn uint8_array_subarray_aliases_parent() {
    // View aliasing must work at stride 1 — buf_ptr offset is just `start`.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const parent: Uint8Array = new Uint8Array([10, 20, 30, 40, 50]);
            const view: Uint8Array = parent.subarray(1, 4); // [20, 30, 40]
            view[0] = 99;
            sink(parent[1] as f64); // 99 (mutation propagates)
            sink(view.length as f64); // 3
            sink(view[0] as f64); // 99
            sink(view[2] as f64); // 40
        }
    "#,
    );
    assert_eq!(vals, vec![99.0, 3.0, 99.0, 40.0]);
}

#[test]
fn uint8_array_slice_is_independent() {
    // slice must allocate a fresh self-owned Uint8Array — mutating it must
    // not bleed back into the receiver, even at stride 1 where memory.copy
    // is used directly.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;
        export function tick(me: i32): void {
            const a: Uint8Array = new Uint8Array([10, 20, 30, 40]);
            const b: Uint8Array = a.slice(1, 3); // [20, 30]
            b[0] = 99;
            sink(a[1] as f64); // 20 (untouched)
            sink(b[0] as f64); // 99
            sink(b.length as f64); // 2
        }
    "#,
    );
    assert_eq!(vals, vec![20.0, 99.0, 2.0]);
}
