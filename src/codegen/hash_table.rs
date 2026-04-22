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
