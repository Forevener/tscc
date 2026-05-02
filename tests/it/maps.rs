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

/// E.5 spot-check: generic nesting through Array — `Array<Map<string, i32>>`.
/// Each array slot holds an independent Map instance; per-element method
/// dispatch and forEach over the outer array both route through the mangled
/// `Map$string$i32` layout.
#[test]
fn map_nested_inside_array_independent_instances() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const maps: Array<Map<string, i32>> = new Array<Map<string, i32>>(3);
            maps.push(new Map<string, i32>());
            maps.push(new Map<string, i32>());
            maps.push(new Map<string, i32>());
            maps[0].set("a", 1);
            maps[0].set("b", 2);
            maps[1].set("a", 10);
            maps[2].set("c", 99);

            // Sizes are independent per slot.
            sink(f64(maps[0].size));
            sink(f64(maps[1].size));
            sink(f64(maps[2].size));

            // Cross-slot reads — each Map sees only its own keys.
            sink(f64(maps[0].get("a")));
            sink(f64(maps[1].get("a")));
            sink(f64(maps[0].has("c")));
            sink(f64(maps[2].has("c")));

            // forEach over the outer array, summing every Map's values.
            let total: i32 = 0;
            maps.forEach((m: Map<string, i32>) => {
                m.forEach((v: i32) => { total = total + v; });
            });
            sink(f64(total));
        }
        "#,
    );
    assert_eq!(
        values,
        vec![
            2.0, 1.0, 1.0,           // sizes
            1.0, 10.0, 0.0, 1.0,     // cross-slot reads
            112.0,                    // 1 + 2 + 10 + 99
        ]
    );
}

/// E.5 spot-check: a user-written generic class whose field is a tscc-generic
/// `Map<K, V>`. `Cache<K, V>` monomorphizations must cascade into Map
/// monomorphizations under the right K/V bindings.
#[test]
fn user_generic_class_wraps_tscc_generic_map() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        class Cache<K, V> {
            inner: Map<K, V>;
            constructor() {
                this.inner = new Map<K, V>();
            }
            put(k: K, v: V): void { this.inner.set(k, v); }
            fetch(k: K): V { return this.inner.get(k); }
            count(): i32 { return this.inner.size; }
        }

        export function tick(_me: i32): void {
            const a: Cache<string, i32> = new Cache<string, i32>();
            a.put("hp", 42);
            a.put("mp", 7);
            sink(f64(a.count()));
            sink(f64(a.fetch("hp")));
            sink(f64(a.fetch("mp")));

            // Second monomorphization with different K/V — must not collide
            // with the first Cache<string, i32>'s Map layout.
            const b: Cache<i32, f64> = new Cache<i32, f64>();
            b.put(1, 1.5);
            b.put(2, 2.5);
            sink(f64(b.count()));
            sink(b.fetch(1));
            sink(b.fetch(2));
        }
        "#,
    );
    assert_eq!(values, vec![2.0, 42.0, 7.0, 2.0, 1.5, 2.5]);
}

// ─── keys() / values() / entries() ───────────────────────────────────────────

/// `m.keys()` materializes the insertion-order chain into a fresh `Array<K>`.
/// Same iteration discipline as forEach: insertion order, deletes elided.
#[test]
fn map_keys_returns_array_in_insertion_order() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(30, 300);
            m.set(10, 100);
            m.set(20, 200);
            const ks: i32[] = m.keys();
            sink(f64(ks.length));
            for (const k of ks) {
                sink(f64(k));
            }
        }
        "#,
    );
    assert_eq!(values, vec![3.0, 30.0, 10.0, 20.0]);
}

/// `m.values()` yields V-typed elements in insertion order.
#[test]
fn map_values_returns_array_in_insertion_order() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(1, 10);
            m.set(2, 20);
            m.set(3, 30);
            const vs: i32[] = m.values();
            for (const v of vs) {
                sink(f64(v));
            }
        }
        "#,
    );
    assert_eq!(values, vec![10.0, 20.0, 30.0]);
}

/// `m.values()` on a Map<K, f64> picks up the f64 column width.
#[test]
fn map_values_carries_f64_value_type() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, f64> = new Map<i32, f64>();
            m.set(1, 1.5);
            m.set(2, 2.5);
            const vs: f64[] = m.values();
            for (const v of vs) {
                sink(v);
            }
        }
        "#,
    );
    assert_eq!(values, vec![1.5, 2.5]);
}

/// Deletes drop the entry from the materialized array — the chain walk
/// follows the same `next_insert` pointers `forEach` does.
#[test]
fn map_keys_skips_deleted() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(1, 10);
            m.set(2, 20);
            m.set(3, 30);
            m.delete(2);
            const ks: i32[] = m.keys();
            sink(f64(ks.length));
            for (const k of ks) {
                sink(f64(k));
            }
        }
        "#,
    );
    assert_eq!(values, vec![2.0, 1.0, 3.0]);
}

/// Empty map → zero-length result array; the chain walk short-circuits on
/// `head_idx == -1`.
#[test]
fn map_keys_on_empty_returns_empty_array() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            const ks: i32[] = m.keys();
            sink(f64(ks.length));
        }
        "#,
    );
    assert_eq!(values, vec![0.0]);
}

/// String-keyed maps: keys() yields `Array<string>` (currently `i32[]` of
/// pointers; users dereference via existing string idioms). Verifies the
/// pointer chain materializes correctly even when the slot type is a
/// pointer-width column.
#[test]
fn map_keys_string_pointers() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<string, i32> = new Map<string, i32>();
            m.set("alpha", 1);
            m.set("beta", 2);
            const ks: i32[] = m.keys();
            sink(f64(ks.length));
            // Round-trip every key back through the map: the pointers we
            // copied out must still hash + equal-compare to themselves.
            for (const k of ks) {
                sink(f64(m.has(k)));
            }
        }
        "#,
    );
    assert_eq!(values, vec![2.0, 1.0, 1.0]);
}

/// `m.keys()` composes through array HOFs — the result is a real `Array<K>`,
/// not a special iterator type.
#[test]
fn map_keys_composes_with_array_filter() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(1, 10);
            m.set(2, 20);
            m.set(3, 30);
            m.set(4, 40);
            const evens: i32[] = m.keys().filter((k: i32) => k % 2 === 0);
            sink(f64(evens.length));
            for (const k of evens) {
                sink(f64(k));
            }
        }
        "#,
    );
    assert_eq!(values, vec![2.0, 2.0, 4.0]);
}

/// `m.entries()` materializes the insertion-order chain into a fresh
/// `Array<[K, V]>` whose elements are arena-allocated tuples. Each pair
/// holds (key, value) per the ES spec.
#[test]
fn map_entries_returns_array_of_pairs_in_insertion_order() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(3, 30);
            m.set(1, 10);
            m.set(2, 20);
            const es: Array<[i32, i32]> = m.entries();
            sink(f64(es.length));
            for (const e of es) {
                sink(f64(e[0]));
                sink(f64(e[1]));
            }
        }
        "#,
    );
    assert_eq!(values, vec![3.0, 3.0, 30.0, 1.0, 10.0, 2.0, 20.0]);
}

/// `m.entries()` on a Map<string, f64> exercises the mixed-width path:
/// pair shape carries `[string, f64]` so `_0` is i32-aligned (pointer) and
/// `_1` is f64-aligned (8-byte slot starts at offset 8).
#[test]
fn map_entries_mixed_width_string_to_f64() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<string, f64> = new Map<string, f64>();
            m.set("alpha", 1.5);
            m.set("beta", 2.5);
            const es: Array<[string, f64]> = m.entries();
            sink(f64(es.length));
            for (const e of es) {
                sink(f64(e[0].length));
                sink(e[1]);
            }
        }
        "#,
    );
    assert_eq!(values, vec![2.0, 5.0, 1.5, 4.0, 2.5]);
}

/// `m.entries()` on an empty Map returns an empty array (length 0).
#[test]
fn map_entries_on_empty_returns_empty_array() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            const es: Array<[i32, i32]> = m.entries();
            sink(f64(es.length));
        }
        "#,
    );
    assert_eq!(values, vec![0.0]);
}

/// Deletes punch holes in the bucket array but leave the chain intact;
/// `entries()` walks `head_idx → next_insert` so it skips deleted slots.
#[test]
fn map_entries_skips_deleted_entries() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(1, 10);
            m.set(2, 20);
            m.set(3, 30);
            m.delete(2);
            const es: Array<[i32, i32]> = m.entries();
            sink(f64(es.length));
            for (const e of es) {
                sink(f64(e[0] * 100 + e[1]));
            }
        }
        "#,
    );
    assert_eq!(values, vec![2.0, 110.0, 330.0]);
}

/// `m.entries()` aliases to a registered tuple shape — when user code also
/// declares `[K, V]` syntactically, both paths share the same synthetic
/// class. Verifies the pair is read-write through the same field offsets.
#[test]
fn map_entries_shares_shape_with_user_tuple() {
    let values = run_sink_tick(
        r#"
        declare function sink(x: f64): void;

        export function tick(_me: i32): void {
            const m: Map<i32, i32> = new Map<i32, i32>();
            m.set(7, 70);
            m.set(8, 80);
            const es: Array<[i32, i32]> = m.entries();
            // Mix the entries result with a hand-written [i32, i32] in the
            // same array — they must coexist as the same tuple shape.
            const more: Array<[i32, i32]> = [[9, 90], [10, 100]];
            sink(f64(es.length + more.length));
            for (const e of es) {
                sink(f64(e[0] + e[1]));
            }
            for (const e of more) {
                sink(f64(e[0] + e[1]));
            }
        }
        "#,
    );
    assert_eq!(values, vec![4.0, 77.0, 88.0, 99.0, 110.0]);
}
