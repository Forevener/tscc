use wasm_encoder::{Function, Instruction, MemArg, ValType};

use super::{STRING_HEADER_SIZE, mem_load_i32, mem_load8_u, mem_store_i32};
// ============================================================
// __str_split(s: i32, delim: i32) -> i32 (Array<string>)
// Returns arena-allocated array of string pointers.
// Array layout: [length:i32][capacity:i32][elements:i32...]
// ============================================================
pub(super) fn build_str_split(arena_idx: u32) -> Function {
    // Params: s=0, delim=1
    // Locals: s_len=2, d_len=3, arr=4, count=5, start=6, i=7, j=8, matched=9,
    //         seg_len=10, seg_ptr=11, cap=12
    let locals = vec![
        (1, ValType::I32), // s_len
        (1, ValType::I32), // d_len
        (1, ValType::I32), // arr (array pointer)
        (1, ValType::I32), // count (number of segments found)
        (1, ValType::I32), // start (start of current segment)
        (1, ValType::I32), // i (scan position)
        (1, ValType::I32), // j (inner loop)
        (1, ValType::I32), // matched
        (1, ValType::I32), // seg_len
        (1, ValType::I32), // seg_ptr
        (1, ValType::I32), // cap (initial capacity)
    ];
    let mut func = Function::new(locals);
    let (s, delim) = (0u32, 1);
    let (s_len, d_len, arr, count, start, i, j, matched, seg_len, seg_ptr, cap) =
        (2u32, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(s_len));
    func.instruction(&Instruction::LocalGet(delim));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(d_len));

    // Allocate array with initial capacity 8. Array header = 8 bytes.
    func.instruction(&Instruction::I32Const(8));
    func.instruction(&Instruction::LocalSet(cap));
    // arr = arena_alloc(8 + cap * 4)
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(arr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::I32Const(8)); // header
    func.instruction(&Instruction::LocalGet(cap));
    func.instruction(&Instruction::I32Const(4)); // element size
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    // arr.length = 0, arr.capacity = cap
    func.instruction(&Instruction::LocalGet(arr));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&mem_store_i32(0));
    func.instruction(&Instruction::LocalGet(arr));
    func.instruction(&Instruction::LocalGet(cap));
    func.instruction(&Instruction::I32Store(MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));

    // count = 0, start = 0, i = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(count));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(start));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    // Scan loop
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    // if i > s_len - d_len: break (but handle d_len=0 / i >= s_len)
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(d_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::BrIf(1));

    // Check if delimiter matches at position i
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(j));

    // Empty delimiter: don't match (prevent infinite loop)
    func.instruction(&Instruction::LocalGet(d_len));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::Else);

    // Inner compare loop
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalGet(d_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    func.instruction(&Instruction::LocalGet(delim));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::Br(2)); // break inner
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::End); // end if d_len == 0

    // If matched: emit segment [start..i), advance i past delimiter
    func.instruction(&Instruction::LocalGet(matched));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));

    // seg_len = i - start
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(seg_len));

    // Allocate segment string
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(seg_ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(seg_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(seg_ptr));
    func.instruction(&Instruction::LocalGet(seg_len));
    func.instruction(&mem_store_i32(0));
    // Copy bytes
    func.instruction(&Instruction::LocalGet(seg_ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(seg_len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    // Store seg_ptr in arr[count]
    func.instruction(&Instruction::LocalGet(arr));
    func.instruction(&Instruction::I32Const(8)); // array header
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(seg_ptr));
    func.instruction(&mem_store_i32(0));

    // count++
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(count));

    // start = i + d_len
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(d_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(start));
    // i = start
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::LocalSet(i));

    func.instruction(&Instruction::Br(1)); // continue outer loop
    func.instruction(&Instruction::End); // end if matched

    // Not matched: i++
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // end loop
    func.instruction(&Instruction::End); // end block

    // Emit final segment [start..s_len)
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(seg_len));

    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(seg_ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(seg_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(seg_ptr));
    func.instruction(&Instruction::LocalGet(seg_len));
    func.instruction(&mem_store_i32(0));
    func.instruction(&Instruction::LocalGet(seg_ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(seg_len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    func.instruction(&Instruction::LocalGet(arr));
    func.instruction(&Instruction::I32Const(8));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(seg_ptr));
    func.instruction(&mem_store_i32(0));

    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(count));

    // Update array length
    func.instruction(&Instruction::LocalGet(arr));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&mem_store_i32(0));

    func.instruction(&Instruction::LocalGet(arr));
    func.instruction(&Instruction::End);
    func
}
