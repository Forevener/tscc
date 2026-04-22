//! Shared memory-layout primitives for Map<K, V> and Set<T> bucket emission.
//!
//! Map and Set buckets share a structural skeleton: a key/element slot, an
//! optional value slot, and bucket-chain link pointers. The layout math — byte
//! size, natural alignment, padding — is identical between the two, as are the
//! WASM memory ops that read and write a typed slot. This module owns that
//! shared vocabulary so `map_builtins.rs` / `set_builtins.rs` and
//! `expr/map.rs` / `expr/set.rs` can stop copy-pasting it.

use wasm_encoder::{Instruction, MemArg};

use crate::types::BoundType;

/// Initial bucket capacity for a freshly-constructed Map or Set. Power of two
/// so open-addressing probing can wrap via bitmask; the rebuild path assumes
/// capacity stays a power of two.
pub const INITIAL_CAPACITY: u32 = 8;

/// Bucket state values stored in the `state: u8` byte. `clear()` resets every
/// bucket to `BUCKET_EMPTY` via a single `memory.fill`.
pub const BUCKET_EMPTY: i32 = 0;
pub const BUCKET_OCCUPIED: i32 = 1;
pub const BUCKET_TOMBSTONE: i32 = 2;

/// Sentinel for `head_idx` / `tail_idx` meaning "empty list", and for
/// `next_insert` / `prev_insert` at the ends of the insertion chain.
pub const EMPTY_LINK: i32 = -1;

/// Byte width of a `BoundType` when laid out in a bucket. Matches the widths
/// used by `ClassLayout` in `classes.rs`.
pub(crate) fn bound_size(ty: &BoundType) -> u32 {
    match ty {
        BoundType::F64 => 8,
        BoundType::I32 | BoundType::Bool | BoundType::Str | BoundType::Class(_) => 4,
    }
}

/// Byte alignment of a `BoundType`. `f64` needs 8-byte alignment for fast
/// naturally-aligned loads; everything else is a 32-bit value.
pub(crate) fn bound_align(ty: &BoundType) -> u32 {
    match ty {
        BoundType::F64 => 8,
        BoundType::I32 | BoundType::Bool | BoundType::Str | BoundType::Class(_) => 4,
    }
}

/// Round `offset` up to the next multiple of `alignment` (which must be a
/// power of two).
pub(crate) fn align_up(offset: u32, alignment: u32) -> u32 {
    (offset + alignment - 1) & !(alignment - 1)
}

/// WASM alignment hint (log2 of byte alignment) for a slot of this type:
/// 3 for `f64`, 2 for everything else (pointer / i32 / bool).
pub(crate) fn mem_align(ty: &BoundType) -> u32 {
    match ty {
        BoundType::F64 => 3,
        _ => 2,
    }
}

/// `I32Load` at `offset` with the standard 4-byte alignment hint. Used for
/// bucket header fields (count / capacity / link pointers).
pub(crate) fn load_i32(offset: u32) -> Instruction<'static> {
    Instruction::I32Load(MemArg {
        offset: offset as u64,
        align: 2,
        memory_index: 0,
    })
}

/// `I32Store` counterpart of `load_i32`.
pub(crate) fn store_i32(offset: u32) -> Instruction<'static> {
    Instruction::I32Store(MemArg {
        offset: offset as u64,
        align: 2,
        memory_index: 0,
    })
}

/// Typed load for a bucket slot: `F64Load` for `BoundType::F64`, `I32Load`
/// otherwise. Alignment hint matches `mem_align`.
pub(crate) fn load_typed(ty: &BoundType, offset: u32) -> Instruction<'static> {
    match ty {
        BoundType::F64 => Instruction::F64Load(MemArg {
            offset: offset as u64,
            align: mem_align(ty),
            memory_index: 0,
        }),
        _ => Instruction::I32Load(MemArg {
            offset: offset as u64,
            align: mem_align(ty),
            memory_index: 0,
        }),
    }
}

/// Typed store counterpart of `load_typed`.
pub(crate) fn store_typed(ty: &BoundType, offset: u32) -> Instruction<'static> {
    match ty {
        BoundType::F64 => Instruction::F64Store(MemArg {
            offset: offset as u64,
            align: mem_align(ty),
            memory_index: 0,
        }),
        _ => Instruction::I32Store(MemArg {
            offset: offset as u64,
            align: mem_align(ty),
            memory_index: 0,
        }),
    }
}

/// Per-monomorphization bucket layout shared by `Map<K, V>` and `Set<T>`.
/// All offsets are byte offsets from the start of the bucket; `total_size`
/// is padded to `max(alignof(slot), alignof(value?), 4)` so an array of
/// buckets stays naturally aligned.
///
/// Bucket layout in memory:
///
/// ```text
/// +-- 0 ------------- state: u8
/// |   (pad)
/// +-- slot_offset --- slot:  K or T
/// +-- next_offset --- next_insert: i32
/// +-- prev_offset --- prev_insert: i32
/// +-- value_offset -- value: V            (Map only; None for Sets)
/// +-- total_size ---  (next bucket starts here)
/// ```
///
/// `value_offset` is `Some` for Map buckets and `None` for Set buckets; the
/// probing / insertion-chain scaffolding is otherwise identical.
#[derive(Debug, Clone, Copy)]
pub struct BucketLayout {
    pub state_offset: u32,
    pub slot_offset: u32,
    pub next_offset: u32,
    pub prev_offset: u32,
    pub value_offset: Option<u32>,
    pub total_size: u32,
}

/// One concrete use of `Map<K, V>` or `Set<T>` discovered in user source.
/// `slot_ty` is the hashed key (Map) or element (Set); `value_ty` is `Some`
/// for Maps and `None` for Sets. Shape picked by the generics collector in
/// `generics::collect_instantiations` and carried through to the
/// registration pass in `compile_module`.
#[derive(Debug, Clone)]
pub struct HashTableInstantiation {
    pub mangled_name: String,
    pub slot_ty: BoundType,
    pub value_ty: Option<BoundType>,
}

/// Everything `emit_new_map` / `emit_new_set` and the method dispatchers
/// need to know about a single `Map<K, V>` or `Set<T>` monomorphization.
/// Stored in `ModuleContext::{map_info,set_info}` keyed on `mangled_name`.
/// `value_ty.is_some()` distinguishes Map from Set at the (rare) call site
/// that handles both.
#[derive(Debug, Clone)]
pub struct HashTableInfo {
    pub slot_ty: BoundType,
    pub value_ty: Option<BoundType>,
    pub bucket: BucketLayout,
}

impl HashTableInfo {
    /// Map-only accessor for the value slot type. Panics on Set (which has
    /// no value slot); callers should already be on a map-specific codepath.
    pub fn expect_value_ty(&self) -> &BoundType {
        self.value_ty
            .as_ref()
            .expect("HashTableInfo::expect_value_ty called on a Set (no value slot)")
    }
}

impl BucketLayout {
    /// Compute the layout. Pass `Some(value_ty)` for a Map bucket and `None`
    /// for a Set bucket. `slot_ty` is the hashed key (Map) or element (Set).
    pub fn compute(slot_ty: &BoundType, value_ty: Option<&BoundType>) -> Self {
        let slot_align = bound_align(slot_ty);
        let state_offset = 0;
        let slot_offset = align_up(state_offset + 1, slot_align);
        let next_offset = align_up(slot_offset + bound_size(slot_ty), 4);
        let prev_offset = next_offset + 4;
        let (value_offset, unpadded_end, value_align) = match value_ty {
            Some(vt) => {
                let va = bound_align(vt);
                let vo = align_up(prev_offset + 4, va);
                (Some(vo), vo + bound_size(vt), va)
            }
            None => (None, prev_offset + 4, 4),
        };
        let bucket_align = slot_align.max(value_align).max(4);
        let total_size = align_up(unpadded_end, bucket_align);
        BucketLayout {
            state_offset,
            slot_offset,
            next_offset,
            prev_offset,
            value_offset,
            total_size,
        }
    }
}
