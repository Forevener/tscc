//! Typed-array pseudo-classes (`Int32Array`, `Float64Array`, `Uint8Array`).
//!
//! See `docs/plan-typed-arrays.md` for the full plan and the layout-decision
//! rationale. Sub-phase 1 (this module's introduction) wires up:
//!
//! 1. A `TypedArrayDescriptor` table that names each typed-array variant and
//!    carries the per-variant element metadata sub-phase 2 onward will need.
//! 2. A registration helper that synthesizes a `ClassLayout` per variant, so
//!    `class_names.contains("Int32Array")`, `class_registry.get("Int32Array")`,
//!    and downstream lookups all succeed.
//!
//! The synthesized layout sets `is_typed_array: true` so future sub-phases can
//! gate construction / method / vtable paths off the regular class machinery.
//! The shape of the in-memory header — `[len: u32 @ +0][buf_ptr: u32 @ +4]` —
//! is identical across all three variants; only element-level codegen varies.

use std::collections::{HashMap, HashSet};

use wasm_encoder::{Instruction, MemArg};

use super::classes::{ClassLayout, ClassRegistry};
use super::module::ModuleContext;
use crate::error::CompileError;
use crate::types::WasmType;

/// Header size in bytes for every typed-array variant. Same shape as
/// `Array<T>`'s header — first word is `len`, second word is `buf_ptr`
/// (instead of `cap`). For self-owned typed arrays the body immediately
/// follows the header in the same arena allocation, so `buf_ptr = self + 8`.
pub const TYPED_ARRAY_HEADER_SIZE: u32 = 8;

/// Byte offset of the `len` word within a typed-array header.
pub const TYPED_ARRAY_LEN_OFFSET: u32 = 0;

/// Byte offset of the `buf_ptr` word within a typed-array header.
pub const TYPED_ARRAY_BUF_PTR_OFFSET: u32 = 4;

/// Discriminator for the three typed-array variants this phase ships. Sub-phase
/// 5 fills out `Uint8Array`'s sub-i32 store/load handling; sub-phase 2 covers
/// `Int32Array` and `Float64Array`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypedArrayKind {
    Int32,
    Float64,
    Uint8,
}

/// Element-level load opcode. Selected per typed-array variant; `I32U8` is the
/// `i32.load8_u` form that Uint8Array uses (sub-phase 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedLoad {
    I32,
    F64,
    I32U8,
}

/// Element-level store opcode. `I32U8` is `i32.store8`, which truncates the
/// low 8 bits of an i32 value — that's where Uint8Array's wrap semantics come
/// from for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedStore {
    I32,
    F64,
    I32U8,
}

/// Per-variant metadata. The fields surfaced here are everything sub-phases
/// 2–5 need to dispatch construction, indexed access, and HOFs without
/// re-pattern-matching on `TypedArrayKind` at every site.
///
/// Layout (`[len][buf_ptr]`, 8-byte header) is **not** carried here — every
/// variant shares it. Only element-level codegen varies.
#[derive(Debug, Clone, Copy)]
pub struct TypedArrayDescriptor {
    /// Registered identifier, exactly as the user writes it in source
    /// (`Int32Array`, `Float64Array`, `Uint8Array`).
    pub name: &'static str,
    /// Discriminator for codegen branches that need to recognize a specific
    /// variant outside the load/store dispatch — e.g. sub-phase 5's
    /// `Uint8Array` literal-init wrap, where compile-time const folding can
    /// pre-mask each element. Sub-phase 2 doesn't read this directly because
    /// `load_op` / `store_op` / `byte_stride` already encode everything the
    /// element-emit sites need.
    #[allow(
        dead_code,
        reason = "consumed by sub-phase 5 (Uint8Array constant-fold wrap) and any future variant-specific recognizer"
    )]
    pub kind: TypedArrayKind,
    /// Language-level type of `ta[i]`. `Int32` and `Uint8` both surface as
    /// `WasmType::I32` to user code (Uint8Array reads zero-extend a byte
    /// into i32; writes truncate via `i32.store8`). `Float64` is `F64`.
    pub elem_wasm_ty: WasmType,
    /// Bytes per element in the body. 4 / 8 / 1.
    pub byte_stride: u32,
    /// Mirrored as `Int32Array.BYTES_PER_ELEMENT` (sub-phase 2).
    /// Always equals `byte_stride`; kept as a separate field so the
    /// user-facing constant is named at the descriptor level, not derived.
    pub bytes_per_element: u32,
    /// `MemArg.align` value for element loads/stores — log2(byte_stride):
    /// 0 (1-byte), 2 (4-byte), 3 (8-byte). Wasm uses align as a power-of-two
    /// hint, not a requirement; getting it right matches what the validator
    /// expects for the chosen load/store opcode.
    pub align: u32,
    /// Element load opcode dispatch.
    pub load_op: TypedLoad,
    /// Element store opcode dispatch.
    pub store_op: TypedStore,
}

impl TypedArrayDescriptor {
    /// Build the per-element load instruction at the given byte offset
    /// (typically 0 — typed-array indexed access bakes the index into the
    /// address calculation, not the immediate). Sites still vary the offset
    /// for header reads, so it stays a parameter.
    pub fn load_inst(&self, offset: u64) -> Instruction<'static> {
        let memarg = MemArg {
            offset,
            align: self.align,
            memory_index: 0,
        };
        match self.load_op {
            TypedLoad::I32 => Instruction::I32Load(memarg),
            TypedLoad::F64 => Instruction::F64Load(memarg),
            TypedLoad::I32U8 => Instruction::I32Load8U(memarg),
        }
    }

    /// Build the per-element store instruction at the given byte offset.
    pub fn store_inst(&self, offset: u64) -> Instruction<'static> {
        let memarg = MemArg {
            offset,
            align: self.align,
            memory_index: 0,
        };
        match self.store_op {
            TypedStore::I32 => Instruction::I32Store(memarg),
            TypedStore::F64 => Instruction::F64Store(memarg),
            TypedStore::I32U8 => Instruction::I32Store8(memarg),
        }
    }
}

pub const INT32_ARRAY: TypedArrayDescriptor = TypedArrayDescriptor {
    name: "Int32Array",
    kind: TypedArrayKind::Int32,
    elem_wasm_ty: WasmType::I32,
    byte_stride: 4,
    bytes_per_element: 4,
    align: 2,
    load_op: TypedLoad::I32,
    store_op: TypedStore::I32,
};

pub const FLOAT64_ARRAY: TypedArrayDescriptor = TypedArrayDescriptor {
    name: "Float64Array",
    kind: TypedArrayKind::Float64,
    elem_wasm_ty: WasmType::F64,
    byte_stride: 8,
    bytes_per_element: 8,
    align: 3,
    load_op: TypedLoad::F64,
    store_op: TypedStore::F64,
};

pub const UINT8_ARRAY: TypedArrayDescriptor = TypedArrayDescriptor {
    name: "Uint8Array",
    kind: TypedArrayKind::Uint8,
    // Reads zero-extend to i32; user-visible element type is i32.
    elem_wasm_ty: WasmType::I32,
    byte_stride: 1,
    bytes_per_element: 1,
    align: 0,
    load_op: TypedLoad::I32U8,
    store_op: TypedStore::I32U8,
};

/// All typed-array descriptors in registration order. Iterated at module
/// init to populate `class_names` and `class_registry` and at descriptor-
/// lookup time when only the kind is known.
pub const TYPED_ARRAY_DESCRIPTORS: &[&TypedArrayDescriptor] =
    &[&INT32_ARRAY, &FLOAT64_ARRAY, &UINT8_ARRAY];

/// Lookup helper: returns the descriptor for a registered typed-array name,
/// or `None` if `name` doesn't refer to a typed array. Cheap (linear scan
/// over three entries); the alternative — a HashMap on `ModuleContext` —
/// would add init overhead to every program for a constant-three-entry
/// table.
pub fn descriptor_for(name: &str) -> Option<&'static TypedArrayDescriptor> {
    TYPED_ARRAY_DESCRIPTORS
        .iter()
        .copied()
        .find(|d| d.name == name)
}

/// Register all typed-array pseudo-classes on `ctx`. Called once at the
/// start of `compile_module`, before any pass that walks user types
/// (so generic-instantiation collection sees the names in `class_names`,
/// shape-discovery skips them, and field-type resolution treats
/// `arr: Float64Array;` as an i32 pointer).
pub fn register_typed_arrays(ctx: &mut ModuleContext) -> Result<(), CompileError> {
    for desc in TYPED_ARRAY_DESCRIPTORS {
        ctx.class_names.insert(desc.name.to_string());
        register_typed_array_layout(&mut ctx.class_registry, desc)?;
    }
    Ok(())
}

/// Insert a methodless, parent-less, vtable-less `ClassLayout` for one
/// typed-array variant. The layout carries no user-visible fields — the
/// `[len][buf_ptr]` header offsets are constants of the typed-array codegen
/// path, not field-map lookups, so leaving `fields` empty keeps the layout
/// honest while still anchoring the name in `class_registry`.
///
/// `size` is the header size (8). Sub-phase 2's construction path
/// short-circuits before reading `size` (it computes `8 + len * stride`
/// directly), so this value is never the source of a real allocation —
/// it's recorded for diagnostic clarity.
fn register_typed_array_layout(
    registry: &mut ClassRegistry,
    desc: &TypedArrayDescriptor,
) -> Result<(), CompileError> {
    let name = desc.name.to_string();
    if registry.get(&name).is_some() {
        return Err(CompileError::codegen(format!(
            "typed-array pseudo-class '{name}' already registered"
        )));
    }
    registry.classes.insert(
        name.clone(),
        ClassLayout {
            name,
            size: TYPED_ARRAY_HEADER_SIZE,
            fields: Vec::new(),
            field_map: HashMap::new(),
            field_class_types: HashMap::new(),
            field_string_types: HashSet::new(),
            methods: HashMap::new(),
            parent: None,
            is_polymorphic: false,
            vtable_methods: Vec::new(),
            vtable_method_map: HashMap::new(),
            vtable_offset: 0,
            own_field_names: HashSet::new(),
            is_typed_array: true,
        },
    );
    Ok(())
}
