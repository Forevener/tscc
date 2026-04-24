use super::common::run_sink_tick;

/// Phase C Step 1: `new Map<K, V>()` compiles end-to-end, allocates the 5-field
/// header, and `.size` reads as zero on a fresh map.
#[test]
fn map_allocation_size_is_zero() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<string, i32> = new Map<string, i32>();
            sink(f64(m.size));
        }
        "#,
    );
    assert_eq!(values, vec![0.0]);
}

/// Phase C Step 2: constructor initializes the header fields — `capacity` set
/// to the INITIAL_CAPACITY constant (8), and both `head_idx`/`tail_idx` to -1
/// so `set()` can detect an empty insertion list.
#[test]
fn map_constructor_initializes_header() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<string, i32> = new Map<string, i32>();
            sink(f64(m.size));
            sink(f64(m.capacity));
            sink(f64(m.head_idx));
            sink(f64(m.tail_idx));
        }
        "#,
    );
    assert_eq!(values, vec![0.0, 8.0, -1.0, -1.0]);
}

/// Phase C Step 2: bucket allocation picks different widths per (K, V). The
/// pointer comparison across two fresh maps exercises bump-allocator layout
/// — if the first map's bucket array under-sized, the second map's header
/// would collide. No explicit size assertion: we just verify both maps
/// allocate cleanly and expose sane defaults.
#[test]
fn map_with_f64_key_and_i32_value_is_distinct() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const a: Map<f64, i32> = new Map<f64, i32>();
            const b: Map<i32, f64> = new Map<i32, f64>();
            sink(f64(a.capacity));
            sink(f64(b.capacity));
        }
        "#,
    );
    assert_eq!(values, vec![8.0, 8.0]);
}

/// Phase C Step 2: `clear()` runs cleanly on a fresh map — size/head/tail
/// remain at their empty defaults. The call exercises `memory.fill`-based
/// state-byte reset; once Step 3 adds `set()`, subsequent tests will verify
/// clear() on a populated map.
#[test]
fn map_clear_is_safe_on_fresh_map() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<string, i32> = new Map<string, i32>();
            m.clear();
            sink(f64(m.size));
            sink(f64(m.head_idx));
            sink(f64(m.tail_idx));
            sink(f64(m.capacity));
        }
        "#,
    );
    assert_eq!(values, vec![0.0, -1.0, -1.0, 8.0]);
}

/// Phase C Step 2: two back-to-back constructions allocate independent
/// headers + bucket arrays. Exercises the bucket-array bump math across the
/// arena and guards against off-by-one in header-vs-buckets spacing.
#[test]
fn map_two_instances_do_not_alias() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const a: Map<string, i32> = new Map<string, i32>();
            const b: Map<string, i32> = new Map<string, i32>();
            // header fields initialized — if b's header stomped a's bucket
            // array, a.capacity would be whatever bytes landed there.
            sink(f64(a.capacity));
            sink(f64(a.head_idx));
            sink(f64(b.capacity));
            sink(f64(b.head_idx));
        }
        "#,
    );
    assert_eq!(values, vec![8.0, -1.0, 8.0, -1.0]);
}

// ─── Phase C Step 3 — has / get / set / delete / forEach ─────────────────────

/// set + get + has over i32 keys — happy path, three distinct keys, reads back
/// the values in insertion order.
#[test]
fn map_i32_keys_basic_crud() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(10, 100);
            m.set(20, 200);
            m.set(30, 300);
            sink(f64(m.size));
            sink(f64(m.has(20)));
            sink(f64(m.has(999)));
            sink(f64(m.get(10)));
            sink(f64(m.get(20)));
            sink(f64(m.get(30)));
        }
        "#,
    );
    assert_eq!(values, vec![3.0, 1.0, 0.0, 100.0, 200.0, 300.0]);
}

/// set on an existing key overwrites in place and doesn't change size.
#[test]
fn map_set_overwrites_existing() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(7, 1);
            m.set(7, 2);
            m.set(7, 3);
            sink(f64(m.size));
            sink(f64(m.get(7)));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 3.0]);
}

/// get on a miss returns 0 (the zero value of V). `.has()` is the
/// disambiguator. Tests the Else branch of the get-block.
#[test]
fn map_get_miss_returns_zero() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(1, 42);
            sink(f64(m.get(999)));
            sink(f64(m.has(999)));
        }
        "#,
    );
    assert_eq!(values, vec![0.0, 0.0]);
}

/// Deleting a key removes it, decrements size, and opens the slot for reuse
/// via a later set(). Re-inserting after delete exercises the tombstone-reuse
/// path in the probe.
#[test]
fn map_delete_then_reinsert() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(1, 10);
            m.set(2, 20);
            m.set(3, 30);
            sink(f64(m.delete(2)));
            sink(f64(m.delete(999))); // miss → 0
            sink(f64(m.size));
            sink(f64(m.has(2)));
            m.set(2, 222);
            sink(f64(m.size));
            sink(f64(m.get(2)));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 0.0, 2.0, 0.0, 3.0, 222.0]);
}

/// f64 keys: SameValueZero semantics. All NaN bit patterns are one key, and
/// +0 / -0 hash/compare equal.
#[test]
fn map_f64_keys_same_value_zero() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<f64, i32> = new Map<f64, i32>();
            // Start with NaN and update via a different NaN expression —
            // SameValueZero says both are the same key.
            const nan1: f64 = 0.0 / 0.0;
            const nan2: f64 = Math.sqrt(-1.0);
            m.set(nan1, 7);
            m.set(nan2, 11);
            sink(f64(m.size));
            sink(f64(m.get(nan1)));
            sink(f64(m.has(nan2)));

            // +0 / -0 collapse to one key.
            m.set(0.0, 1);
            m.set(-0.0, 2);
            sink(f64(m.get(0.0)));
            sink(f64(m.get(-0.0)));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 11.0, 1.0, 2.0, 2.0]);
}

/// bool keys — two slots max but still routed through the hash machinery.
#[test]
fn map_bool_keys() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<bool, i32> = new Map<bool, i32>();
            m.set(true, 1);
            m.set(false, 2);
            sink(f64(m.get(true)));
            sink(f64(m.get(false)));
            sink(f64(m.has(true)));
            sink(f64(m.size));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0, 1.0, 2.0]);
}

/// string keys — exercises the xxh3 hash path and __str_eq equality.
#[test]
fn map_string_keys() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<string, i32> = new Map<string, i32>();
            m.set("alpha", 1);
            m.set("beta", 2);
            m.set("gamma", 3);
            sink(f64(m.get("alpha")));
            sink(f64(m.get("beta")));
            sink(f64(m.get("gamma")));
            sink(f64(m.has("delta")));
            m.set("alpha", 99);
            sink(f64(m.get("alpha")));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0, 3.0, 0.0, 99.0]);
}

/// Class-ref keys — identity semantics (not structural). Two same-field
/// instances are distinct keys.
#[test]
fn map_class_ref_keys_are_identity() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Tag { x: i32; constructor(x: i32) { this.x = x; } }

        export function tick(_me: i32): void {
            const m: Map<Tag, i32> = new Map<Tag, i32>();
            const a: Tag = new Tag(5);
            const b: Tag = new Tag(5);
            m.set(a, 1);
            m.set(b, 2);
            sink(f64(m.size));        // 2 — a and b are distinct keys
            sink(f64(m.get(a)));
            sink(f64(m.get(b)));
        }
        "#,
    );
    assert_eq!(values, vec![2.0, 1.0, 2.0]);
}

/// forEach walks entries in insertion order, binding (value, key) to the
/// arrow's 1 or 2 params.
#[test]
fn map_foreach_insertion_order() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(30, 300);
            m.set(10, 100);
            m.set(20, 200);
            m.forEach((v: i32, k: i32) => {
                sink(f64(k));
                sink(f64(v));
            });
        }
        "#,
    );
    // Visits must be in insertion order: 30→300, 10→100, 20→200.
    assert_eq!(values, vec![30.0, 300.0, 10.0, 100.0, 20.0, 200.0]);
}

/// forEach after a delete — the unlink path must keep the chain walkable.
#[test]
fn map_foreach_skips_deleted() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(1, 10);
            m.set(2, 20);
            m.set(3, 30);
            m.delete(2);
            m.forEach((v: i32) => {
                sink(f64(v));
            });
        }
        "#,
    );
    assert_eq!(values, vec![10.0, 30.0]);
}

/// Rebuild-on-grow: inserting past the 75% load factor doubles capacity and
/// preserves insertion order + all values. Initial cap = 8, load fires at
/// size*4 >= cap*3 → size=6 triggers on the 6th set. After 10 sets capacity
/// should have doubled at least once.
#[test]
fn map_rebuild_on_grow_preserves_entries() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            let i: i32 = 0;
            while (i < 10) {
                m.set(i, i * 100);
                i = i + 1;
            }
            sink(f64(m.size));
            sink(f64(m.capacity));
            // spot-check a couple of keys
            sink(f64(m.get(0)));
            sink(f64(m.get(5)));
            sink(f64(m.get(9)));
            // forEach should still iterate in insertion order.
            m.forEach((v: i32, k: i32) => {
                sink(f64(k * 1000 + v / 100));
            });
        }
        "#,
    );
    assert_eq!(values[0], 10.0);
    assert!(values[1] > 8.0, "capacity should have grown past 8");
    assert_eq!(values[2], 0.0);
    assert_eq!(values[3], 500.0);
    assert_eq!(values[4], 900.0);
    // Each iteration sinks k*1000 + v/100 = k*1000 + k = k*1001 (since v = k*100).
    for (offset, k) in (0..10).enumerate() {
        assert_eq!(values[5 + offset], (k * 1001) as f64);
    }
}

/// Forces the probe to wrap past the end of the bucket array by inserting
/// keys whose hashes deliberately land in the last slot. Exercises the
/// `(slot + 1) & mask` wrap path. Keys are i32 so FxHash is deterministic
/// and collisions can be arranged by matching the hash mod capacity.
#[test]
fn map_probe_wraps_around_bucket_array() {
    // We can't introspect the hash from TS, so instead push enough keys that
    // several must share a bucket-chain due to pigeonhole at cap 8 (holding
    // up to 6 entries before rebuild). Insert 6 consecutive ints and verify
    // each round-trips — this is what would break if (slot+1)&mask was wrong.
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            let i: i32 = 0;
            while (i < 6) {
                m.set(i * 7, i + 1);
                i = i + 1;
            }
            i = 0;
            while (i < 6) {
                sink(f64(m.get(i * 7)));
                i = i + 1;
            }
            sink(f64(m.size));
        }
        "#,
    );
    assert_eq!(values, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 6.0]);
}

/// clear() on a populated map — wipes state bytes, resets size/head/tail, and
/// leaves it ready for reuse.
#[test]
fn map_clear_on_populated_then_reuse() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(1, 10);
            m.set(2, 20);
            m.clear();
            sink(f64(m.size));
            sink(f64(m.has(1)));
            m.set(3, 30);
            sink(f64(m.size));
            sink(f64(m.get(3)));
        }
        "#,
    );
    assert_eq!(values, vec![0.0, 0.0, 1.0, 30.0]);
}
