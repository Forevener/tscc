mod common;

use common::run_sink_tick;

// ─── Phase D Step 1 — layout & construction ─────────────────────────────────

/// `new Set<T>()` compiles end-to-end, allocates the 5-field header, and
/// `.size` reads as zero on a fresh set.
#[test]
fn set_allocation_size_is_zero() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const s: Set<string> = new Set<string>();
            sink(f64(s.size));
        }
        "#,
    );
    assert_eq!(values, vec![0.0]);
}

/// Constructor initializes header fields — `capacity` = INITIAL_CAPACITY (8),
/// head/tail = -1 so `add()` can detect an empty list.
#[test]
fn set_constructor_initializes_header() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const s: Set<i32> = new Set<i32>();
            sink(f64(s.size));
            sink(f64(s.capacity));
            sink(f64(s.head_idx));
            sink(f64(s.tail_idx));
        }
        "#,
    );
    assert_eq!(values, vec![0.0, 8.0, -1.0, -1.0]);
}

/// Back-to-back constructions allocate independent headers + bucket arrays.
/// Guards against off-by-one in the bucket-array bump math.
#[test]
fn set_two_instances_do_not_alias() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const a: Set<i32> = new Set<i32>();
            const b: Set<i32> = new Set<i32>();
            sink(f64(a.capacity));
            sink(f64(a.head_idx));
            sink(f64(b.capacity));
            sink(f64(b.head_idx));
        }
        "#,
    );
    assert_eq!(values, vec![8.0, -1.0, 8.0, -1.0]);
}

// ─── Phase D Step 2 — add / has / delete / forEach ──────────────────────────

/// Happy path over i32 elements — add three distinct values, verify they're
/// all present.
#[test]
fn set_i32_basic_crud() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const s: Set<i32> = new Set<i32>();
            s.add(10);
            s.add(20);
            s.add(30);
            sink(f64(s.size));
            sink(f64(s.has(20)));
            sink(f64(s.has(999)));
            sink(f64(s.has(10)));
            sink(f64(s.has(30)));
        }
        "#,
    );
    assert_eq!(values, vec![3.0, 1.0, 0.0, 1.0, 1.0]);
}

/// Adding a duplicate is a no-op — size stays the same.
#[test]
fn set_add_duplicate_is_noop() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const s: Set<i32> = new Set<i32>();
            s.add(7);
            s.add(7);
            s.add(7);
            sink(f64(s.size));
            sink(f64(s.has(7)));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 1.0]);
}

/// Deleting an element removes it, decrements size, and opens the slot for
/// reuse via a later add(). Exercises tombstone-reuse in the probe.
#[test]
fn set_delete_then_readd() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const s: Set<i32> = new Set<i32>();
            s.add(1);
            s.add(2);
            s.add(3);
            sink(f64(s.delete(2)));
            sink(f64(s.delete(999))); // miss → 0
            sink(f64(s.size));
            sink(f64(s.has(2)));
            s.add(2);
            sink(f64(s.size));
            sink(f64(s.has(2)));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 0.0, 2.0, 0.0, 3.0, 1.0]);
}

/// f64 elements: SameValueZero semantics. All NaN bit patterns are one
/// element, +0 / -0 collapse to the same key.
#[test]
fn set_f64_same_value_zero() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const s: Set<f64> = new Set<f64>();
            const nan1: f64 = 0.0 / 0.0;
            const nan2: f64 = Math.sqrt(-1.0);
            s.add(nan1);
            s.add(nan2);
            sink(f64(s.size));       // 1 — both NaNs collapse
            sink(f64(s.has(nan2)));

            s.add(0.0);
            s.add(-0.0);
            sink(f64(s.size));       // 2 — nan + zero
            sink(f64(s.has(-0.0)));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 1.0, 2.0, 1.0]);
}

/// bool elements — two slots max but still routed through the hash machinery.
#[test]
fn set_bool_elements() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const s: Set<bool> = new Set<bool>();
            s.add(true);
            s.add(false);
            s.add(true);            // dup
            sink(f64(s.size));
            sink(f64(s.has(true)));
            sink(f64(s.has(false)));
        }
        "#,
    );
    assert_eq!(values, vec![2.0, 1.0, 1.0]);
}

/// string elements — exercises the xxh3 hash path and __str_eq equality.
#[test]
fn set_string_elements() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const s: Set<string> = new Set<string>();
            s.add("alpha");
            s.add("beta");
            s.add("gamma");
            s.add("alpha");         // dup
            sink(f64(s.size));
            sink(f64(s.has("beta")));
            sink(f64(s.has("delta")));
            sink(f64(s.has("alpha")));
        }
        "#,
    );
    assert_eq!(values, vec![3.0, 1.0, 0.0, 1.0]);
}

/// Class-ref elements — identity semantics. Two same-field instances are
/// distinct elements.
#[test]
fn set_class_ref_identity() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Tag { x: i32; constructor(x: i32) { this.x = x; } }

        export function tick(_me: i32): void {
            const s: Set<Tag> = new Set<Tag>();
            const a: Tag = new Tag(5);
            const b: Tag = new Tag(5);
            s.add(a);
            s.add(b);
            s.add(a);               // dup of a by identity
            sink(f64(s.size));      // 2 — a and b are distinct
            sink(f64(s.has(a)));
            sink(f64(s.has(b)));
        }
        "#,
    );
    assert_eq!(values, vec![2.0, 1.0, 1.0]);
}

/// forEach walks elements in insertion order, binding the arrow's single
/// param to each element.
#[test]
fn set_foreach_insertion_order() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const s: Set<i32> = new Set<i32>();
            s.add(30);
            s.add(10);
            s.add(20);
            s.forEach((v: i32) => {
                sink(f64(v));
            });
        }
        "#,
    );
    assert_eq!(values, vec![30.0, 10.0, 20.0]);
}

/// forEach after delete — the unlink path must keep the chain walkable.
#[test]
fn set_foreach_skips_deleted() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const s: Set<i32> = new Set<i32>();
            s.add(1);
            s.add(2);
            s.add(3);
            s.delete(2);
            s.forEach((v: i32) => {
                sink(f64(v));
            });
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 3.0]);
}

/// Rebuild-on-grow: inserting past the 75% load factor doubles capacity and
/// preserves insertion order + all values.
#[test]
fn set_rebuild_on_grow_preserves_entries() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const s: Set<i32> = new Set<i32>();
            let i: i32 = 0;
            while (i < 10) {
                s.add(i);
                i = i + 1;
            }
            sink(f64(s.size));
            sink(f64(s.capacity));
            sink(f64(s.has(0)));
            sink(f64(s.has(5)));
            sink(f64(s.has(9)));
            s.forEach((v: i32) => {
                sink(f64(v));
            });
        }
        "#,
    );
    assert_eq!(values[0], 10.0);
    assert!(values[1] > 8.0, "capacity should have grown past 8");
    assert_eq!(values[2], 1.0);
    assert_eq!(values[3], 1.0);
    assert_eq!(values[4], 1.0);
    // Insertion order preserved across rebuild.
    for (offset, v) in (0..10).enumerate() {
        assert_eq!(values[5 + offset], v as f64);
    }
}

/// Forces the probe to share a bucket-chain via pigeonhole at cap 8. Six
/// consecutive ints must each round-trip — this is what would break if the
/// `(slot + 1) & mask` wrap were wrong.
#[test]
fn set_probe_wraps_around_bucket_array() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const s: Set<i32> = new Set<i32>();
            let i: i32 = 0;
            while (i < 6) {
                s.add(i * 7);
                i = i + 1;
            }
            i = 0;
            while (i < 6) {
                sink(f64(s.has(i * 7)));
                i = i + 1;
            }
            sink(f64(s.size));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 6.0]);
}

/// clear() on a populated set — wipes state bytes, resets size/head/tail,
/// leaves it ready for reuse.
#[test]
fn set_clear_on_populated_then_reuse() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const s: Set<i32> = new Set<i32>();
            s.add(1);
            s.add(2);
            s.clear();
            sink(f64(s.size));
            sink(f64(s.has(1)));
            s.add(3);
            sink(f64(s.size));
            sink(f64(s.has(3)));
        }
        "#,
    );
    assert_eq!(values, vec![0.0, 0.0, 1.0, 1.0]);
}

/// Two Sets with different element types share the same header shape but
/// have their own bucket arrays — no cross-contamination.
#[test]
fn set_with_different_element_types_coexist() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const nums: Set<i32> = new Set<i32>();
            const strs: Set<string> = new Set<string>();
            nums.add(42);
            strs.add("hello");
            sink(f64(nums.size));
            sink(f64(nums.has(42)));
            sink(f64(strs.size));
            sink(f64(strs.has("hello")));
            sink(f64(nums.has(0)));      // int set doesn't see the string
            sink(f64(strs.has("")));     // string set doesn't see the int
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 1.0, 1.0, 1.0, 0.0, 0.0]);
}
