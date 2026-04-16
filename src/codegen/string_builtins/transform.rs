use wasm_encoder::{Function, Instruction, ValType};

use super::{
    STRING_HEADER_SIZE, emit_is_whitespace, mem_load_i32, mem_load8_u, mem_store_i32, mem_store8,
};
// __str_slice — now implemented via Rust→WASM pipeline (helpers/src/string.rs)
// ============================================================
// __str_toLower(s: i32) -> i32
// ============================================================
pub(super) fn build_str_to_lower(arena_idx: u32) -> Function {
    build_case_convert(arena_idx, true)
}

// ============================================================
// __str_toUpper(s: i32) -> i32
// ============================================================
pub(super) fn build_str_to_upper(arena_idx: u32) -> Function {
    build_case_convert(arena_idx, false)
}

fn build_case_convert(arena_idx: u32, to_lower: bool) -> Function {
    // Params: s=0
    // Locals: len=1, ptr=2, total=3, i=4, byte=5
    let locals = vec![
        (1, ValType::I32), // len
        (1, ValType::I32), // ptr
        (1, ValType::I32), // total
        (1, ValType::I32), // i
        (1, ValType::I32), // byte
    ];
    let mut func = Function::new(locals);
    let s = 0u32;
    let (len, ptr, total, i, byte) = (1u32, 2, 3, 4, 5);

    // Range for conversion
    let (range_start, range_end, offset): (i32, i32, i32) = if to_lower {
        (65, 90, 32) // A-Z → +32
    } else {
        (97, 122, -32) // a-z → -32
    };

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len));

    // total = len + 4
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total));

    // ptr = arena_alloc(total)
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // store length
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&mem_store_i32(0));

    // i = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    // byte = load8_u(s + 4 + i)
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte));

    // if byte >= range_start && byte <= range_end: byte += offset
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(range_start));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(range_end));
    func.instruction(&Instruction::I32LeU);
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(offset));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(byte));
    func.instruction(&Instruction::End);

    // store8(ptr + 4 + i, byte)
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&mem_store8(0));

    // i++
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_trim / __str_trimStart / __str_trimEnd (s: i32) -> i32
// Parameterized by which side(s) to trim.
// ============================================================
pub(super) fn build_str_trim_impl(arena_idx: u32, trim_left: bool, trim_right: bool) -> Function {
    // Params: s=0
    // Locals: len=1, start=2, end=3, byte=4, new_len=5, total=6, ptr=7
    let locals = vec![
        (1, ValType::I32), // len
        (1, ValType::I32), // start
        (1, ValType::I32), // end
        (1, ValType::I32), // byte
        (1, ValType::I32), // new_len
        (1, ValType::I32), // total
        (1, ValType::I32), // ptr
    ];
    let mut func = Function::new(locals);
    let s = 0u32;
    let (len, start, end, byte, new_len, total, ptr) = (1u32, 2, 3, 4, 5, 6, 7);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len));

    // start = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(start));

    // end = len
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::LocalSet(end));

    // Find start: skip whitespace from left
    if trim_left {
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::LocalGet(end));
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));

        func.instruction(&Instruction::LocalGet(s));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::I32Add);
        func.instruction(&mem_load8_u(0));
        func.instruction(&Instruction::LocalSet(byte));

        // Check whitespace: space(32), tab(9), LF(10), CR(13)
        emit_is_whitespace(&mut func, byte);
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1)); // not whitespace → break

        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(start));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    }

    // Find end: skip whitespace from right
    if trim_right {
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(end));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::I32LeU);
        func.instruction(&Instruction::BrIf(1));

        func.instruction(&Instruction::LocalGet(s));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(end));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::I32Add);
        func.instruction(&mem_load8_u(0));
        func.instruction(&Instruction::LocalSet(byte));

        emit_is_whitespace(&mut func, byte);
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1)); // not whitespace → break

        func.instruction(&Instruction::LocalGet(end));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(end));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    }

    // new_len = end - start
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(new_len));

    // total = new_len + 4
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total));

    // ptr = arena_alloc(total)
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&mem_store_i32(0));

    // memory.copy(ptr+4, s+4+start, new_len)
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}
// ============================================================
// __str_replace(s: i32, search: i32, replacement: i32) -> i32
// Replace first occurrence of search with replacement.
// ============================================================
pub(super) fn build_str_replace(arena_idx: u32) -> Function {
    // Strategy: find indexOf(search), if not found return s,
    // otherwise build: s[0..idx] + replacement + s[idx+search.len..]
    // Params: s=0, search=1, replacement=2
    // Locals: s_len=3, search_len=4, repl_len=5, idx=6, i=7, j=8, matched=9,
    //         new_len=10, ptr=11, limit=12
    let locals = vec![
        (1, ValType::I32), // s_len
        (1, ValType::I32), // search_len
        (1, ValType::I32), // repl_len
        (1, ValType::I32), // idx (found position)
        (1, ValType::I32), // i
        (1, ValType::I32), // j
        (1, ValType::I32), // matched
        (1, ValType::I32), // new_len
        (1, ValType::I32), // ptr
        (1, ValType::I32), // limit
    ];
    let mut func = Function::new(locals);
    let (s, search, replacement) = (0u32, 1, 2);
    let (s_len, search_len, repl_len, idx, i, j, matched, new_len, ptr, limit) =
        (3u32, 4, 5, 6, 7, 8, 9, 10, 11, 12);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(s_len));
    func.instruction(&Instruction::LocalGet(search));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(search_len));
    func.instruction(&Instruction::LocalGet(replacement));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(repl_len));

    // Find first occurrence (inline indexOf logic)
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::LocalSet(idx));

    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32LeU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(limit));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(limit));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::BrIf(1));

    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(j));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalGet(search_len));
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
    func.instruction(&Instruction::LocalGet(search));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(matched));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalSet(idx));
    func.instruction(&Instruction::Br(2)); // break outer search loop
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::End); // end if search_len <= s_len

    // If not found, return s
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // Build result: s[0..idx] + replacement + s[idx+search_len..]
    // new_len = idx + repl_len + (s_len - idx - search_len)
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalGet(repl_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(new_len));

    // Allocate
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&mem_store_i32(0));

    // Copy part 1: s[0..idx]
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    // Copy part 2: replacement
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(replacement));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(repl_len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    // Copy part 3: s[idx+search_len..]
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(repl_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}
// ============================================================
// __str_repeat(s: i32, count: i32) -> i32
// ============================================================
pub(super) fn build_str_repeat(arena_idx: u32) -> Function {
    // Params: s=0, count=1
    // Locals: s_len=2, new_len=3, ptr=4, i=5
    let locals = vec![
        (1, ValType::I32), // s_len
        (1, ValType::I32), // new_len
        (1, ValType::I32), // ptr
        (1, ValType::I32), // i
    ];
    let mut func = Function::new(locals);
    let (s, count) = (0u32, 1);
    let (s_len, new_len, ptr, i) = (2u32, 3, 4, 5);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(s_len));

    // new_len = s_len * count
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalSet(new_len));

    // Allocate
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&mem_store_i32(0));

    // Copy s_len bytes `count` times
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_padStart(s: i32, targetLen: i32, fill: i32) -> i32
// ============================================================
pub(super) fn build_str_pad_start(arena_idx: u32) -> Function {
    build_pad(arena_idx, true)
}

// ============================================================
// __str_padEnd(s: i32, targetLen: i32, fill: i32) -> i32
// ============================================================
pub(super) fn build_str_pad_end(arena_idx: u32) -> Function {
    build_pad(arena_idx, false)
}

fn build_pad(arena_idx: u32, pad_start: bool) -> Function {
    // Params: s=0, targetLen=1, fill=2
    // Locals: s_len=3, fill_len=4, pad_needed=5, ptr=6, i=7, fill_byte=8
    let locals = vec![
        (1, ValType::I32), // s_len
        (1, ValType::I32), // fill_len
        (1, ValType::I32), // pad_needed
        (1, ValType::I32), // ptr
        (1, ValType::I32), // i
        (1, ValType::I32), // fill_byte
    ];
    let mut func = Function::new(locals);
    let (s, target_len, fill) = (0u32, 1, 2);
    let (s_len, fill_len, pad_needed, ptr, i, fill_byte) = (3u32, 4, 5, 6, 7, 8);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(s_len));
    func.instruction(&Instruction::LocalGet(fill));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(fill_len));

    // If s_len >= targetLen, return s unchanged
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(target_len));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // pad_needed = targetLen - s_len
    func.instruction(&Instruction::LocalGet(target_len));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(pad_needed));

    // Allocate result: targetLen + 4
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(target_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(target_len));
    func.instruction(&mem_store_i32(0));

    if pad_start {
        // Write padding bytes first (cycling through fill string)
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(pad_needed));
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));
        // fill_byte = load8_u(fill + 4 + (i % fill_len))
        func.instruction(&Instruction::LocalGet(fill));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(fill_len));
        func.instruction(&Instruction::I32RemU);
        func.instruction(&Instruction::I32Add);
        func.instruction(&mem_load8_u(0));
        func.instruction(&Instruction::LocalSet(fill_byte));
        // store8(ptr + 4 + i, fill_byte)
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(fill_byte));
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        // Copy original string after padding
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(pad_needed));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(s));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(s_len));
        func.instruction(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
    } else {
        // Copy original string first
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(s));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(s_len));
        func.instruction(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        // Write padding bytes after (cycling through fill string)
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(pad_needed));
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(fill));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(fill_len));
        func.instruction(&Instruction::I32RemU);
        func.instruction(&Instruction::I32Add);
        func.instruction(&mem_load8_u(0));
        func.instruction(&Instruction::LocalSet(fill_byte));
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(s_len));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(fill_byte));
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    }

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_replaceAll(s: i32, search: i32, replacement: i32) -> i32
// Replace ALL non-overlapping occurrences of search with replacement.
// Two-pass: (1) count occurrences to compute result length,
//           (2) build the output by copying segments + replacements.
// ============================================================
pub(super) fn build_str_replace_all(arena_idx: u32) -> Function {
    // Params: s=0, search=1, replacement=2
    // Locals: s_len=3, search_len=4, repl_len=5, count=6, i=7, j=8, matched=9,
    //         new_len=10, ptr=11, src_pos=12, dst_pos=13, limit=14
    let locals = vec![
        (1, ValType::I32), // s_len
        (1, ValType::I32), // search_len
        (1, ValType::I32), // repl_len
        (1, ValType::I32), // count
        (1, ValType::I32), // i
        (1, ValType::I32), // j
        (1, ValType::I32), // matched
        (1, ValType::I32), // new_len
        (1, ValType::I32), // ptr
        (1, ValType::I32), // src_pos
        (1, ValType::I32), // dst_pos
        (1, ValType::I32), // limit
    ];
    let mut func = Function::new(locals);
    let (s, search, replacement) = (0u32, 1, 2);
    let (s_len, search_len, repl_len, count, i, j, matched, new_len, ptr, src_pos, dst_pos, limit) =
        (3u32, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14);

    // Load lengths
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(s_len));
    func.instruction(&Instruction::LocalGet(search));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(search_len));
    func.instruction(&Instruction::LocalGet(replacement));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(repl_len));

    // If search_len == 0, return s (avoid infinite loop)
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // If search_len > s_len, no match possible
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // limit = s_len - search_len
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(limit));

    // === Pass 1: count occurrences ===
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(count));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty)); // $count_break
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty)); // $count_loop
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(limit));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::BrIf(1));

    // Check if search matches at position i
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(j));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty)); // $match_break
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty)); // $match_loop
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    // Compare s[i+j] vs search[j]
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalGet(search));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::Br(2)); // break match
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // end match loop
    func.instruction(&Instruction::End); // end match block

    // If matched, count++ and skip past the match (non-overlapping)
    func.instruction(&Instruction::LocalGet(matched));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(count));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(1)); // continue count_loop
    func.instruction(&Instruction::End);

    // Not matched: i++
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // end count loop
    func.instruction(&Instruction::End); // end count block

    // If count == 0, return s unchanged
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // === Compute result length ===
    // new_len = s_len - count * search_len + count * repl_len
    //         = s_len + count * (repl_len - search_len)
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::LocalGet(repl_len));
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(new_len));

    // === Allocate result ===
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(new_len));
    func.instruction(&mem_store_i32(0));

    // === Pass 2: build result ===
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(src_pos));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(dst_pos));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty)); // $build_break
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty)); // $build_loop
    func.instruction(&Instruction::LocalGet(src_pos));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    // Check if search matches at src_pos (and there's room)
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(matched));

    // Only try matching if src_pos + search_len <= s_len
    func.instruction(&Instruction::LocalGet(src_pos));
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32LeU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(j));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty)); // $m2_break
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty)); // $m2_loop
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(src_pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalGet(search));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::Br(2)); // break m2
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // end m2 loop
    func.instruction(&Instruction::End); // end m2 block

    func.instruction(&Instruction::End); // end "if room" check

    // If matched: copy replacement, advance src_pos by search_len
    func.instruction(&Instruction::LocalGet(matched));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));

    // memory.copy(ptr + 4 + dst_pos, replacement + 4, repl_len)
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(dst_pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(replacement));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(repl_len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    func.instruction(&Instruction::LocalGet(dst_pos));
    func.instruction(&Instruction::LocalGet(repl_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(dst_pos));
    func.instruction(&Instruction::LocalGet(src_pos));
    func.instruction(&Instruction::LocalGet(search_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(src_pos));

    func.instruction(&Instruction::Else);

    // Not matched: copy 1 byte from s[src_pos] to result[dst_pos]
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(dst_pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(src_pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(dst_pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(dst_pos));
    func.instruction(&Instruction::LocalGet(src_pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(src_pos));

    func.instruction(&Instruction::End); // end if matched

    func.instruction(&Instruction::Br(0)); // continue build loop
    func.instruction(&Instruction::End); // end build loop
    func.instruction(&Instruction::End); // end build block

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

/// `__str_concat(a, b) -> c` — allocate a new string with bytes of `a` then
/// `b`. Used by `Array.join` at runtime where the number of concatenations is
/// only known at runtime. Compile-time chains go through `emit_fused_string_chain`.
pub(super) fn build_str_concat(arena_idx: u32) -> Function {
    let locals = vec![(4, ValType::I32)];
    let mut func = Function::new(locals);
    let (a, b, a_len, b_len, total, ptr) = (0u32, 1, 2, 3, 4, 5);

    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(a_len));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(b_len));

    func.instruction(&Instruction::LocalGet(a_len));
    func.instruction(&Instruction::LocalGet(b_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total));

    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&mem_store_i32(0));

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(a_len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(a_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(b_len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}
