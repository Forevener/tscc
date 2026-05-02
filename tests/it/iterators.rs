// User-defined iterables. Phase 1 covered `[Symbol.iterator]()` parsing /
// registration / error hints; Phase 2 adds `for..of` lowering against
// classes that implement the iterator protocol (`[Symbol.iterator](): It`
// where `It.next(): { value: T; done: boolean }`). Phase 2b extends this
// with the spec cleanup hook: when the iterator class declares `return()`,
// it runs on `break` and on early function-return through the loop.
// `iterator.throw()` is rejected indefinitely (gated on the exceptions
// feature, long-term roadmap).

use super::common::{compile, compile_err, run_sink_tick};

#[test]
fn parses_symbol_iterator_method() {
    // Class with `[Symbol.iterator]()` should compile end-to-end. We don't
    // exercise iteration semantics yet (Phase 2) — just confirm registration
    // does not error and emits a working module. The body returns `this` so
    // the method is well-formed even though no caller invokes it.
    let _wasm = compile(
        r#"
        class Range {
            start: i32;
            end: i32;
            constructor(start: i32, end: i32) {
                this.start = start;
                this.end = end;
            }
            [Symbol.iterator](): Range {
                return this;
            }
        }

        export function tick(me: i32): i32 {
            const r: Range = new Range(0, 10);
            return r.start + r.end;
        }
    "#,
    );
}

#[test]
fn parses_symbol_iterator_alongside_normal_methods() {
    // Mixing `[Symbol.iterator]()` with regular methods on the same class must
    // not perturb method registration / vtable allocation for the others.
    let _wasm = compile(
        r#"
        class Counter {
            n: i32;
            constructor(n: i32) { this.n = n; }
            bump(): i32 { this.n = this.n + 1; return this.n; }
            [Symbol.iterator](): Counter { return this; }
            peek(): i32 { return this.n; }
        }

        export function tick(me: i32): i32 {
            const c: Counter = new Counter(0);
            c.bump();
            c.bump();
            return c.peek();
        }
    "#,
    );
}

#[test]
fn bare_symbol_identifier_errors() {
    // `Symbol` as a value triggers the new dedicated hint, not the generic
    // "undefined variable 'Symbol'" message — verify the hint string.
    let err = compile_err(
        r#"
        export function tick(me: i32): i32 {
            const s: i32 = Symbol;
            return s;
        }
    "#,
    );
    assert!(
        err.message.contains("compile-time-only token"),
        "expected Symbol-token hint, got: {}",
        err.message
    );
    assert!(
        err.message.contains("[Symbol.iterator]"),
        "expected hint to point at the recognized form, got: {}",
        err.message
    );
}

#[test]
fn symbol_iterator_as_value_errors() {
    // Reading `Symbol.iterator` outside the class-method-key position fails
    // with the same family of hint, but mentions the property name explicitly.
    let err = compile_err(
        r#"
        export function tick(me: i32): i32 {
            const k: i32 = Symbol.iterator;
            return k;
        }
    "#,
    );
    assert!(
        err.message.contains("Symbol.iterator"),
        "expected error to name `Symbol.iterator`, got: {}",
        err.message
    );
    assert!(
        err.message.contains("compile-time-only"),
        "expected compile-time-only hint, got: {}",
        err.message
    );
}

#[test]
fn unknown_symbol_member_as_value_errors() {
    // Other `Symbol.X` accesses (not yet recognized) should also error via
    // the same hint path rather than fall through to enum-lookup or generic
    // "undefined" diagnostics. Picks `Symbol.asyncIterator` since that's the
    // most plausible future expansion.
    let err = compile_err(
        r#"
        export function tick(me: i32): i32 {
            const k: i32 = Symbol.asyncIterator;
            return k;
        }
    "#,
    );
    assert!(
        err.message.contains("compile-time-only"),
        "expected compile-time-only hint for Symbol.asyncIterator, got: {}",
        err.message
    );
}

#[test]
fn other_computed_class_method_keys_still_rejected() {
    // Computed keys other than `[Symbol.iterator]` remain unsupported, with
    // the new hint message pointing users at the one recognized form.
    let err = compile_err(
        r#"
        const k: string = "foo";
        class C {
            [k](): i32 { return 1; }
        }
        export function tick(me: i32): i32 {
            const c: C = new C();
            return 0;
        }
    "#,
    );
    assert!(
        err.message.contains("computed property key"),
        "expected computed-key error, got: {}",
        err.message
    );
    assert!(
        err.message.contains("Symbol.iterator"),
        "expected hint pointing at recognized form, got: {}",
        err.message
    );
}

// ============================================================================
// Phase 2 — `for..of` lowering for user-defined iterables.
// ============================================================================

#[test]
fn for_of_user_iterable_basic() {
    // Range emits 0..3 via a separate iterator class. Verifies the protocol
    // path: iterable returns a fresh iterator, iterator's `next()` produces
    // `{value, done}` shapes, loop binding reads `value` until `done` flips.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class RangeIter {
            i: i32;
            end: i32;
            constructor(start: i32, end: i32) {
                this.i = start;
                this.end = end;
            }
            next(): { value: i32; done: boolean } {
                if (this.i >= this.end) {
                    return { value: 0, done: true };
                }
                const v: i32 = this.i;
                this.i = this.i + 1;
                return { value: v, done: false };
            }
        }

        class Range {
            start: i32;
            end: i32;
            constructor(start: i32, end: i32) {
                this.start = start;
                this.end = end;
            }
            [Symbol.iterator](): RangeIter {
                return new RangeIter(this.start, this.end);
            }
        }

        export function tick(me: i32): void {
            const r: Range = new Range(0, 3);
            for (const x of r) {
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 1.0, 2.0]);
}

#[test]
fn for_of_user_iterable_self_iterating() {
    // Class that *is* its own iterator — common spec idiom: `[Symbol.iterator]()`
    // returns `this`. Same dispatch path; receiver class equals iterator class.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Counter {
            n: i32;
            limit: i32;
            constructor(limit: i32) {
                this.n = 0;
                this.limit = limit;
            }
            [Symbol.iterator](): Counter {
                return this;
            }
            next(): { value: i32; done: boolean } {
                if (this.n >= this.limit) {
                    return { value: 0, done: true };
                }
                const v: i32 = this.n;
                this.n = this.n + 1;
                return { value: v, done: false };
            }
        }

        export function tick(me: i32): void {
            const c: Counter = new Counter(4);
            for (const x of c) {
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 1.0, 2.0, 3.0]);
}

#[test]
fn for_of_user_iterable_break_works() {
    // `break` inside the body must exit the user-iterable loop the same way
    // it exits an array `for..of`. We sink three values and break — confirms
    // the loop_stack break_depth wiring.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Counter {
            n: i32;
            constructor() { this.n = 0; }
            [Symbol.iterator](): Counter { return this; }
            next(): { value: i32; done: boolean } {
                const v: i32 = this.n;
                this.n = this.n + 1;
                return { value: v, done: false };  // never done — relies on break
            }
        }

        export function tick(me: i32): void {
            const c: Counter = new Counter();
            for (const x of c) {
                if (x >= 3) { break; }
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 1.0, 2.0]);
}

#[test]
fn for_of_user_iterable_continue_works() {
    // `continue` skips the rest of the body and re-enters `next()`. Sinks
    // only odd values from 0..6 to confirm continue routes correctly.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class RangeIter {
            i: i32;
            end: i32;
            constructor(end: i32) { this.i = 0; this.end = end; }
            next(): { value: i32; done: boolean } {
                if (this.i >= this.end) { return { value: 0, done: true }; }
                const v: i32 = this.i;
                this.i = this.i + 1;
                return { value: v, done: false };
            }
        }
        class Range {
            end: i32;
            constructor(end: i32) { this.end = end; }
            [Symbol.iterator](): RangeIter { return new RangeIter(this.end); }
        }

        export function tick(me: i32): void {
            const r: Range = new Range(6);
            for (const x of r) {
                if (x % 2 == 0) { continue; }
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 3.0, 5.0]);
}

#[test]
fn for_of_user_iterable_f64_value() {
    // value type is f64 — exercises the F64Load arm of value field load and
    // the F64 elem_local declaration.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class FloatIter {
            i: i32;
            n: i32;
            constructor(n: i32) { this.i = 0; this.n = n; }
            next(): { value: f64; done: boolean } {
                if (this.i >= this.n) { return { value: 0.0, done: true }; }
                const v: f64 = (this.i as f64) * 0.5;
                this.i = this.i + 1;
                return { value: v, done: false };
            }
        }
        class FloatRange {
            n: i32;
            constructor(n: i32) { this.n = n; }
            [Symbol.iterator](): FloatIter { return new FloatIter(this.n); }
        }

        export function tick(me: i32): void {
            const r: FloatRange = new FloatRange(4);
            for (const x of r) {
                sink(x);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 0.5, 1.0, 1.5]);
}

#[test]
fn for_of_user_iterable_class_value() {
    // value type is a class instance. Loop binding picks up the class type
    // so `pt.x`-style member access works on the binding.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Point {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) { this.x = x; this.y = y; }
        }

        class PointIter {
            i: i32;
            constructor() { this.i = 0; }
            next(): { value: Point; done: boolean } {
                if (this.i >= 3) {
                    return { value: new Point(0.0, 0.0), done: true };
                }
                const k: f64 = this.i as f64;
                this.i = this.i + 1;
                return { value: new Point(k, k * 2.0), done: false };
            }
        }
        class Points {
            [Symbol.iterator](): PointIter { return new PointIter(); }
        }

        export function tick(me: i32): void {
            const ps: Points = new Points();
            for (const p of ps) {
                sink(p.x);
                sink(p.y);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 0.0, 1.0, 2.0, 2.0, 4.0]);
}

#[test]
fn for_of_user_iterable_inherited_iterator_method() {
    // Subclass inherits `[Symbol.iterator]()` from a parent class. Detection
    // walks the parent chain (`find_iterator_method` / `find_method_inherited`).
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class TwoIter {
            i: i32;
            constructor() { this.i = 0; }
            next(): { value: i32; done: boolean } {
                if (this.i >= 2) { return { value: 0, done: true }; }
                const v: i32 = this.i;
                this.i = this.i + 1;
                return { value: v, done: false };
            }
        }

        class Base {
            [Symbol.iterator](): TwoIter { return new TwoIter(); }
        }
        class Sub extends Base {
            tag: i32;
            constructor() { super(); this.tag = 99; }
        }

        export function tick(me: i32): void {
            const s: Sub = new Sub();
            for (const x of s) {
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 1.0]);
}

#[test]
fn for_of_user_iterable_polymorphic_dispatch() {
    // Polymorphic iterable (subclass overrides `[Symbol.iterator]`). The
    // dispatcher must use vtable call_indirect because the receiver's
    // declared static type is the parent class.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class FixedIter {
            i: i32;
            n: i32;
            constructor(n: i32) { this.i = 0; this.n = n; }
            next(): { value: i32; done: boolean } {
                if (this.i >= this.n) { return { value: 0, done: true }; }
                const v: i32 = this.i;
                this.i = this.i + 1;
                return { value: v, done: false };
            }
        }

        class Source {
            [Symbol.iterator](): FixedIter { return new FixedIter(2); }
        }
        class WiderSource extends Source {
            [Symbol.iterator](): FixedIter { return new FixedIter(4); }
        }

        export function tick(me: i32): void {
            const s: Source = new WiderSource();  // static type Source, dynamic WiderSource
            for (const x of s) {
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 1.0, 2.0, 3.0]);
}

// ============================================================================
// Phase 2b — `iterator.return()` cleanup hook on early `for..of` exit.
// ============================================================================

#[test]
fn for_of_return_runs_on_break() {
    // `break` inside the loop body fires the cleanup. `done=true` from
    // `next()` (normal completion) does NOT fire it — verify by running the
    // same iterator body through both paths and checking sink ordering.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class It {
            i: i32;
            constructor() { this.i = 0; }
            next(): { value: i32; done: boolean } {
                const v: i32 = this.i;
                this.i = this.i + 1;
                return { value: v, done: false };  // never done — relies on break
            }
            return(): void {
                sink(99.0);  // marker for cleanup
            }
        }
        class Iterable {
            [Symbol.iterator](): It { return new It(); }
        }

        export function tick(me: i32): void {
            const x: Iterable = new Iterable();
            for (const v of x) {
                if (v >= 2) { break; }
                sink(v as f64);
            }
        }
    "#,
    );
    // Loop sinks 0, 1, then break → cleanup sinks 99.
    assert_eq!(vals, vec![0.0, 1.0, 99.0]);
}

#[test]
fn for_of_return_skipped_on_normal_completion() {
    // Normal completion (`done=true` from `next()`) MUST NOT call `return()`.
    // We declare `return()` but exhaust the iterator instead of breaking.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class It {
            i: i32;
            constructor() { this.i = 0; }
            next(): { value: i32; done: boolean } {
                if (this.i >= 2) { return { value: 0, done: true }; }
                const v: i32 = this.i;
                this.i = this.i + 1;
                return { value: v, done: false };
            }
            return(): void {
                sink(99.0);  // would mis-fire if cleanup ran
            }
        }
        class Iterable {
            [Symbol.iterator](): It { return new It(); }
        }

        export function tick(me: i32): void {
            const x: Iterable = new Iterable();
            for (const v of x) {
                sink(v as f64);
            }
            sink(7.0);  // sentinel after the loop
        }
    "#,
    );
    // Should be 0, 1 (loop body), 7 (sentinel). NO 99 — normal exit
    // skips return().
    assert_eq!(vals, vec![0.0, 1.0, 7.0]);
}

#[test]
fn for_of_return_runs_on_early_function_return() {
    // Early `return` from the enclosing function while inside the loop
    // body must call `return()` on the iterator. The function returns a
    // value; cleanup runs without disturbing it.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class It {
            i: i32;
            constructor() { this.i = 0; }
            next(): { value: i32; done: boolean } {
                const v: i32 = this.i;
                this.i = this.i + 1;
                return { value: v, done: false };
            }
            return(): void {
                sink(99.0);
            }
        }
        class Iterable {
            [Symbol.iterator](): It { return new It(); }
        }

        function findFirstAtLeast(it: Iterable, threshold: i32): i32 {
            for (const v of it) {
                if (v >= threshold) {
                    return v;  // early return through the loop — fires cleanup
                }
            }
            return -1;
        }

        export function tick(me: i32): void {
            const x: Iterable = new Iterable();
            sink(findFirstAtLeast(x, 3) as f64);  // 3, then cleanup, then sink the value
        }
    "#,
    );
    // The function returns 3 to tick, which sinks 3.0. Cleanup sinks
    // 99.0 BEFORE the function-return value reaches tick — so the order
    // is: 99 (cleanup), 3 (returned value).
    assert_eq!(vals, vec![99.0, 3.0]);
}

#[test]
fn for_of_return_continue_does_not_fire() {
    // `continue` is not a loop exit — cleanup must NOT fire for it.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class It {
            i: i32;
            constructor() { this.i = 0; }
            next(): { value: i32; done: boolean } {
                if (this.i >= 4) { return { value: 0, done: true }; }
                const v: i32 = this.i;
                this.i = this.i + 1;
                return { value: v, done: false };
            }
            return(): void {
                sink(99.0);
            }
        }
        class Iterable {
            [Symbol.iterator](): It { return new It(); }
        }

        export function tick(me: i32): void {
            const x: Iterable = new Iterable();
            for (const v of x) {
                if (v % 2 == 0) { continue; }
                sink(v as f64);
            }
            sink(7.0);
        }
    "#,
    );
    // Odd values of 0..3 are 1, 3. Then sentinel 7. NO 99.
    assert_eq!(vals, vec![1.0, 3.0, 7.0]);
}

#[test]
fn for_of_return_fires_outer_iterators_on_inner_return() {
    // Nested for..of: `return` inside the inner loop body must call cleanup
    // on the inner iterator FIRST (innermost first per spec), then on the
    // outer iterator, then perform the wasm Return.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class OuterIt {
            i: i32;
            constructor() { this.i = 0; }
            next(): { value: i32; done: boolean } {
                const v: i32 = this.i;
                this.i = this.i + 1;
                return { value: v, done: false };
            }
            return(): void { sink(100.0); }
        }
        class InnerIt {
            i: i32;
            constructor() { this.i = 0; }
            next(): { value: i32; done: boolean } {
                const v: i32 = this.i;
                this.i = this.i + 1;
                return { value: v, done: false };
            }
            return(): void { sink(200.0); }
        }

        class Outer {
            [Symbol.iterator](): OuterIt { return new OuterIt(); }
        }
        class Inner {
            [Symbol.iterator](): InnerIt { return new InnerIt(); }
        }

        function find(): i32 {
            const o: Outer = new Outer();
            for (const a of o) {
                const i: Inner = new Inner();
                for (const b of i) {
                    if (a + b >= 1) {
                        return 42;  // cleanup: inner first (200), then outer (100)
                    }
                }
            }
            return -1;
        }

        export function tick(me: i32): void {
            sink(find() as f64);
        }
    "#,
    );
    // a=0, b=0 -> 0+0=0, no return. a=0, b=1 -> 0+1=1 ≥ 1, return 42.
    // Cleanup order: inner (200) first, then outer (100). Then 42 is
    // returned to tick, which sinks it.
    assert_eq!(vals, vec![200.0, 100.0, 42.0]);
}

#[test]
fn for_of_return_with_iter_result_return_type() {
    // Spec-style `return()` returns an `IteratorResult` shape. tscc accepts
    // any return type and discards the result — verify both compile and
    // run identically.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class It {
            i: i32;
            constructor() { this.i = 0; }
            next(): { value: i32; done: boolean } {
                const v: i32 = this.i;
                this.i = this.i + 1;
                return { value: v, done: false };
            }
            return(): { value: i32; done: boolean } {
                sink(99.0);
                return { value: 0, done: true };  // result discarded by for..of
            }
        }
        class Iterable {
            [Symbol.iterator](): It { return new It(); }
        }

        export function tick(me: i32): void {
            const x: Iterable = new Iterable();
            for (const v of x) {
                if (v >= 1) { break; }
                sink(v as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![0.0, 99.0]);
}

#[test]
fn for_of_iterator_with_throw_method_rejected() {
    // `throw()` is gated on the exceptions feature (long-term roadmap).
    // Reject with a hint that distinguishes it from the `return()` case.
    let err = compile_err(
        r#"
        class It {
            i: i32;
            constructor() { this.i = 0; }
            next(): { value: i32; done: boolean } {
                return { value: 0, done: true };
            }
            throw(): { value: i32; done: boolean } {
                return { value: 0, done: true };
            }
        }
        class Iterable {
            [Symbol.iterator](): It { return new It(); }
        }
        export function tick(me: i32): void {
            const x: Iterable = new Iterable();
            for (const v of x) {}
        }
    "#,
    );
    assert!(
        err.message.contains("throw()"),
        "expected throw() rejection to mention the method, got: {}",
        err.message
    );
    assert!(
        err.message.contains("exception"),
        "expected hint pointing at exceptions feature, got: {}",
        err.message
    );
}

#[test]
fn for_of_missing_iterator_method_errors() {
    // Class with no `[Symbol.iterator]()` and not array-shaped: must error
    // with the user-iterable hint, not the generic "not an Array" message.
    let err = compile_err(
        r#"
        class Plain {
            x: i32;
            constructor() { this.x = 0; }
        }
        export function tick(me: i32): void {
            const p: Plain = new Plain();
            for (const v of p) {}
        }
    "#,
    );
    assert!(
        err.message.contains("Symbol.iterator"),
        "expected error to mention Symbol.iterator, got: {}",
        err.message
    );
}

#[test]
fn for_of_iterator_missing_next_errors() {
    // Iterable returns a class that has no `next()` — protocol detection must
    // surface a clear error pointing at which side failed.
    let err = compile_err(
        r#"
        class NotAnIter {
            x: i32;
            constructor() { this.x = 0; }
        }
        class Iterable {
            [Symbol.iterator](): NotAnIter { return new NotAnIter(); }
        }
        export function tick(me: i32): void {
            const x: Iterable = new Iterable();
            for (const v of x) {}
        }
    "#,
    );
    assert!(
        err.message.contains("next"),
        "expected error to mention missing next() method, got: {}",
        err.message
    );
}

#[test]
fn for_of_iterator_result_missing_done_errors() {
    // Result shape lacks `done` — caught by protocol detection (field_map
    // lookup miss) before any codegen runs.
    let err = compile_err(
        r#"
        class It {
            i: i32;
            constructor() { this.i = 0; }
            next(): { value: i32 } {
                return { value: 0 };
            }
        }
        class Iterable {
            [Symbol.iterator](): It { return new It(); }
        }
        export function tick(me: i32): void {
            const x: Iterable = new Iterable();
            for (const v of x) {}
        }
    "#,
    );
    assert!(
        err.message.contains("done"),
        "expected error to mention missing done field, got: {}",
        err.message
    );
}

// ============================================================================
// Trivial-iterator inlining (roadmap: near-term).
//
// Iterables whose iterator is a single-cursor walk over a backing `Array<T>`
// are recognized at compile time and rewritten against the underlying array
// directly. The tests below check (1) behavioral correctness across the
// canonical shape variants, and (2) that the protocol path remains intact
// when any heuristic check fails.
// ============================================================================

#[test]
fn for_of_trivial_iterable_basic() {
    // The simplest qualifying shape: cursor set in constructor body, classic
    // `value-then-bump` next(). Output must match the equivalent direct
    // iteration over the backing array.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class BufIter {
            cursor: i32;
            buf: Array<i32>;
            constructor(buf: Array<i32>) {
                this.cursor = 0;
                this.buf = buf;
            }
            next(): { value: i32; done: boolean } {
                if (this.cursor >= this.buf.length) {
                    return { value: 0, done: true };
                }
                const v: i32 = this.buf[this.cursor];
                this.cursor = this.cursor + 1;
                return { value: v, done: false };
            }
        }
        class Buf {
            items: Array<i32>;
            constructor(items: Array<i32>) { this.items = items; }
            [Symbol.iterator](): BufIter { return new BufIter(this.items); }
        }

        export function tick(me: i32): void {
            const arr: Array<i32> = [10, 20, 30];
            const b: Buf = new Buf(arr);
            for (const x of b) {
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![10.0, 20.0, 30.0]);
}

#[test]
fn for_of_trivial_iterable_property_def_cursor_init() {
    // PropertyDefinition initializer (`cursor: i32 = 0;`) qualifies as the
    // cursor-init step; the constructor only writes the buffer field.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class BufIter {
            cursor: i32 = 0;
            buf: Array<i32>;
            constructor(buf: Array<i32>) {
                this.buf = buf;
            }
            next(): { value: i32; done: boolean } {
                if (this.cursor >= this.buf.length) {
                    return { value: 0, done: true };
                }
                const v: i32 = this.buf[this.cursor];
                this.cursor = this.cursor + 1;
                return { value: v, done: false };
            }
        }
        class Buf {
            items: Array<i32>;
            constructor(items: Array<i32>) { this.items = items; }
            [Symbol.iterator](): BufIter { return new BufIter(this.items); }
        }

        export function tick(me: i32): void {
            const arr: Array<i32> = [1, 2, 3, 4];
            const b: Buf = new Buf(arr);
            for (const x of b) {
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn for_of_trivial_iterable_compound_increment() {
    // Cursor advance via `this.cursor += 1` — the second of the three
    // accepted shapes. (`this.cursor++` is rejected by the codegen for
    // member-target updates, so it's not tested here.)
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class BufIter {
            cursor: i32;
            buf: Array<i32>;
            constructor(buf: Array<i32>) {
                this.cursor = 0;
                this.buf = buf;
            }
            next(): { value: i32; done: boolean } {
                if (this.cursor >= this.buf.length) {
                    return { value: 0, done: true };
                }
                const v: i32 = this.buf[this.cursor];
                this.cursor += 1;
                return { value: v, done: false };
            }
        }
        class Buf {
            items: Array<i32>;
            constructor(items: Array<i32>) { this.items = items; }
            [Symbol.iterator](): BufIter { return new BufIter(this.items); }
        }

        export function tick(me: i32): void {
            const arr: Array<i32> = [7, 8, 9];
            const b: Buf = new Buf(arr);
            for (const x of b) {
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![7.0, 8.0, 9.0]);
}

#[test]
fn for_of_trivial_iterable_f64_elements() {
    // f64 backing array — exercises the F64Load arm of the trivial path's
    // element load and the F64 elem_local declaration.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class FloatIter {
            cursor: i32;
            buf: Array<f64>;
            constructor(buf: Array<f64>) {
                this.cursor = 0;
                this.buf = buf;
            }
            next(): { value: f64; done: boolean } {
                if (this.cursor >= this.buf.length) {
                    return { value: 0.0, done: true };
                }
                const v: f64 = this.buf[this.cursor];
                this.cursor = this.cursor + 1;
                return { value: v, done: false };
            }
        }
        class FloatBuf {
            items: Array<f64>;
            constructor(items: Array<f64>) { this.items = items; }
            [Symbol.iterator](): FloatIter { return new FloatIter(this.items); }
        }

        export function tick(me: i32): void {
            const arr: Array<f64> = [0.5, 1.5, 2.5];
            const b: FloatBuf = new FloatBuf(arr);
            for (const x of b) {
                sink(x);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![0.5, 1.5, 2.5]);
}

#[test]
fn for_of_trivial_iterable_class_elements() {
    // Backing array of class instances. The loop binding picks up the
    // element class so `pt.x`-style member access works on the binding.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Point {
            x: f64;
            y: f64;
            constructor(x: f64, y: f64) { this.x = x; this.y = y; }
        }

        class PointIter {
            cursor: i32;
            buf: Array<Point>;
            constructor(buf: Array<Point>) {
                this.cursor = 0;
                this.buf = buf;
            }
            next(): { value: Point; done: boolean } {
                if (this.cursor >= this.buf.length) {
                    return { value: new Point(0.0, 0.0), done: true };
                }
                const v: Point = this.buf[this.cursor];
                this.cursor = this.cursor + 1;
                return { value: v, done: false };
            }
        }
        class Points {
            items: Array<Point>;
            constructor(items: Array<Point>) { this.items = items; }
            [Symbol.iterator](): PointIter { return new PointIter(this.items); }
        }

        export function tick(me: i32): void {
            const arr: Array<Point> = [new Point(1.0, 2.0), new Point(3.0, 4.0)];
            const ps: Points = new Points(arr);
            for (const p of ps) {
                sink(p.x);
                sink(p.y);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn for_of_trivial_iterable_break_works() {
    // `break` inside the body must exit the trivial-path loop the same way
    // it exits an array `for..of`. Sink three values then break.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class BufIter {
            cursor: i32;
            buf: Array<i32>;
            constructor(buf: Array<i32>) {
                this.cursor = 0;
                this.buf = buf;
            }
            next(): { value: i32; done: boolean } {
                if (this.cursor >= this.buf.length) {
                    return { value: 0, done: true };
                }
                const v: i32 = this.buf[this.cursor];
                this.cursor = this.cursor + 1;
                return { value: v, done: false };
            }
        }
        class Buf {
            items: Array<i32>;
            constructor(items: Array<i32>) { this.items = items; }
            [Symbol.iterator](): BufIter { return new BufIter(this.items); }
        }

        export function tick(me: i32): void {
            const arr: Array<i32> = [10, 20, 30, 40, 50];
            const b: Buf = new Buf(arr);
            for (const x of b) {
                if (x >= 30) { break; }
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![10.0, 20.0]);
}

#[test]
fn for_of_trivial_iterable_continue_works() {
    // `continue` skips the rest of the body without ending iteration.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class BufIter {
            cursor: i32;
            buf: Array<i32>;
            constructor(buf: Array<i32>) {
                this.cursor = 0;
                this.buf = buf;
            }
            next(): { value: i32; done: boolean } {
                if (this.cursor >= this.buf.length) {
                    return { value: 0, done: true };
                }
                const v: i32 = this.buf[this.cursor];
                this.cursor = this.cursor + 1;
                return { value: v, done: false };
            }
        }
        class Buf {
            items: Array<i32>;
            constructor(items: Array<i32>) { this.items = items; }
            [Symbol.iterator](): BufIter { return new BufIter(this.items); }
        }

        export function tick(me: i32): void {
            const arr: Array<i32> = [1, 2, 3, 4, 5];
            const b: Buf = new Buf(arr);
            for (const x of b) {
                if (x % 2 == 0) { continue; }
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 3.0, 5.0]);
}

#[test]
fn for_of_trivial_iterable_falls_back_when_iterator_has_return() {
    // Adding `return()` to the iterator class disqualifies it from the
    // trivial path — the protocol path with cleanup must run instead. The
    // sink emission inside `return()` proves the cleanup hook fired, which
    // only happens via the protocol path on `break`.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class BufIter {
            cursor: i32;
            buf: Array<i32>;
            constructor(buf: Array<i32>) {
                this.cursor = 0;
                this.buf = buf;
            }
            next(): { value: i32; done: boolean } {
                if (this.cursor >= this.buf.length) {
                    return { value: 0, done: true };
                }
                const v: i32 = this.buf[this.cursor];
                this.cursor = this.cursor + 1;
                return { value: v, done: false };
            }
            return(): void {
                sink(-1.0);
            }
        }
        class Buf {
            items: Array<i32>;
            constructor(items: Array<i32>) { this.items = items; }
            [Symbol.iterator](): BufIter { return new BufIter(this.items); }
        }

        export function tick(me: i32): void {
            const arr: Array<i32> = [10, 20, 30];
            const b: Buf = new Buf(arr);
            for (const x of b) {
                if (x >= 20) { break; }
                sink(x as f64);
            }
        }
    "#,
    );
    // 10.0 from the loop body, then -1.0 from the return() cleanup hook
    // — the latter is unreachable on the trivial path, so its presence
    // proves the protocol path ran.
    assert_eq!(vals, vec![10.0, -1.0]);
}

#[test]
fn for_of_trivial_iterable_falls_back_when_buffer_field_mutated() {
    // The iterable's `<bufferField>` is mutated by a method outside the
    // constructor — disqualifies the class from trivial-path detection.
    // The protocol path must still produce correct iteration output.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class BufIter {
            cursor: i32;
            buf: Array<i32>;
            constructor(buf: Array<i32>) {
                this.cursor = 0;
                this.buf = buf;
            }
            next(): { value: i32; done: boolean } {
                if (this.cursor >= this.buf.length) {
                    return { value: 0, done: true };
                }
                const v: i32 = this.buf[this.cursor];
                this.cursor = this.cursor + 1;
                return { value: v, done: false };
            }
        }
        class Buf {
            items: Array<i32>;
            constructor(items: Array<i32>) { this.items = items; }
            // This method mutates `items` outside the constructor — kills
            // the constructor-only-write invariant.
            replace(replacement: Array<i32>): void {
                this.items = replacement;
            }
            [Symbol.iterator](): BufIter { return new BufIter(this.items); }
        }

        export function tick(me: i32): void {
            const a1: Array<i32> = [1, 2, 3];
            const a2: Array<i32> = [100, 200];
            const b: Buf = new Buf(a1);
            b.replace(a2);
            for (const x of b) {
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![100.0, 200.0]);
}

#[test]
fn for_of_trivial_iterable_falls_back_when_next_shape_differs() {
    // The next() body uses `this.cursor < this.buf.length` (LessThan) inside
    // a sentinel-then-advance pattern that's structurally different from
    // the canonical shape. Trivial detection misses; protocol path runs.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class BufIter {
            cursor: i32;
            buf: Array<i32>;
            constructor(buf: Array<i32>) {
                this.cursor = 0;
                this.buf = buf;
            }
            next(): { value: i32; done: boolean } {
                // Inverted predicate: matches `<` with the order flipped
                // (this.<C> >= this.<B>.length). Different AST shape — the
                // detector compares the binary-op operator strictly.
                if (this.cursor < this.buf.length) {
                    const v: i32 = this.buf[this.cursor];
                    this.cursor = this.cursor + 1;
                    return { value: v, done: false };
                }
                return { value: 0, done: true };
            }
        }
        class Buf {
            items: Array<i32>;
            constructor(items: Array<i32>) { this.items = items; }
            [Symbol.iterator](): BufIter { return new BufIter(this.items); }
        }

        export function tick(me: i32): void {
            const arr: Array<i32> = [5, 6, 7];
            const b: Buf = new Buf(arr);
            for (const x of b) {
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![5.0, 6.0, 7.0]);
}

#[test]
fn for_of_trivial_iterable_falls_back_when_iter_method_indirect() {
    // The iterable's `[Symbol.iterator]()` does not match the strict
    // single-statement `return new IterClass(this.<F>)` shape: it stashes
    // the iterator in a temp local first. Trivial detection misses;
    // protocol path runs.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class BufIter {
            cursor: i32;
            buf: Array<i32>;
            constructor(buf: Array<i32>) {
                this.cursor = 0;
                this.buf = buf;
            }
            next(): { value: i32; done: boolean } {
                if (this.cursor >= this.buf.length) {
                    return { value: 0, done: true };
                }
                const v: i32 = this.buf[this.cursor];
                this.cursor = this.cursor + 1;
                return { value: v, done: false };
            }
        }
        class Buf {
            items: Array<i32>;
            constructor(items: Array<i32>) { this.items = items; }
            [Symbol.iterator](): BufIter {
                const it: BufIter = new BufIter(this.items);
                return it;
            }
        }

        export function tick(me: i32): void {
            const arr: Array<i32> = [11, 22, 33];
            const b: Buf = new Buf(arr);
            for (const x of b) {
                sink(x as f64);
            }
        }
    "#,
    );
    assert_eq!(vals, vec![11.0, 22.0, 33.0]);
}

#[test]
fn for_of_trivial_iterable_two_loops_share_state_per_array_semantics() {
    // Two consecutive `for..of`s over the same iterable. Inlining produces
    // a fresh `i=0` per loop, identical to the protocol path constructing
    // a fresh iterator each time. The user-observable result is the same.
    let vals = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class BufIter {
            cursor: i32;
            buf: Array<i32>;
            constructor(buf: Array<i32>) {
                this.cursor = 0;
                this.buf = buf;
            }
            next(): { value: i32; done: boolean } {
                if (this.cursor >= this.buf.length) {
                    return { value: 0, done: true };
                }
                const v: i32 = this.buf[this.cursor];
                this.cursor = this.cursor + 1;
                return { value: v, done: false };
            }
        }
        class Buf {
            items: Array<i32>;
            constructor(items: Array<i32>) { this.items = items; }
            [Symbol.iterator](): BufIter { return new BufIter(this.items); }
        }

        export function tick(me: i32): void {
            const arr: Array<i32> = [1, 2];
            const b: Buf = new Buf(arr);
            for (const x of b) { sink(x as f64); }
            for (const y of b) { sink((y * 10) as f64); }
        }
    "#,
    );
    assert_eq!(vals, vec![1.0, 2.0, 10.0, 20.0]);
}

#[test]
fn for_of_trivial_iterable_emits_less_code_than_protocol_path() {
    // Smoke test that the optimization actually fires: two programs that
    // differ only in a structural feature that defeats trivial detection
    // (the protocol version has a `return()` cleanup hook on the iterator)
    // must produce different-sized wasm. The trivial path skips emitting
    // the per-call-site `[Symbol.iterator]()` + `next()` + cleanup setup,
    // which is dozens of bytes per for..of in the tick function. A simple
    // size comparison is a reliable signal that detection took the fast
    // path; a regression that breaks trivial-detection would surface as
    // "wasm grew unexpectedly," matching the protocol baseline.
    let trivial_src = r#"
        declare function sink(x: f64): void;

        class BufIter {
            cursor: i32;
            buf: Array<i32>;
            constructor(buf: Array<i32>) {
                this.cursor = 0;
                this.buf = buf;
            }
            next(): { value: i32; done: boolean } {
                if (this.cursor >= this.buf.length) {
                    return { value: 0, done: true };
                }
                const v: i32 = this.buf[this.cursor];
                this.cursor = this.cursor + 1;
                return { value: v, done: false };
            }
        }
        class Buf {
            items: Array<i32>;
            constructor(items: Array<i32>) { this.items = items; }
            [Symbol.iterator](): BufIter { return new BufIter(this.items); }
        }

        export function tick(me: i32): void {
            const arr: Array<i32> = [1, 2, 3, 4, 5];
            const b: Buf = new Buf(arr);
            for (const x of b) { sink(x as f64); }
        }
    "#;

    // Same program, but with `return(): void {}` on the iterator — disables
    // trivial detection (cleanup hook required) without changing the loop's
    // visible output (return() is dropped on normal completion).
    let protocol_src = r#"
        declare function sink(x: f64): void;

        class BufIter {
            cursor: i32;
            buf: Array<i32>;
            constructor(buf: Array<i32>) {
                this.cursor = 0;
                this.buf = buf;
            }
            next(): { value: i32; done: boolean } {
                if (this.cursor >= this.buf.length) {
                    return { value: 0, done: true };
                }
                const v: i32 = this.buf[this.cursor];
                this.cursor = this.cursor + 1;
                return { value: v, done: false };
            }
            return(): void {}
        }
        class Buf {
            items: Array<i32>;
            constructor(items: Array<i32>) { this.items = items; }
            [Symbol.iterator](): BufIter { return new BufIter(this.items); }
        }

        export function tick(me: i32): void {
            const arr: Array<i32> = [1, 2, 3, 4, 5];
            const b: Buf = new Buf(arr);
            for (const x of b) { sink(x as f64); }
        }
    "#;

    let trivial_wasm = compile(trivial_src);
    let protocol_wasm = compile(protocol_src);
    assert!(
        trivial_wasm.len() < protocol_wasm.len(),
        "trivial-iterator inlining should emit less wasm than the protocol path; \
         got trivial={} bytes, protocol={} bytes",
        trivial_wasm.len(),
        protocol_wasm.len(),
    );
}
