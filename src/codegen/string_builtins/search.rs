use wasm_encoder::{Function, Instruction, ValType};

use super::{STRING_HEADER_SIZE, mem_load_i32, mem_load8_u};
// ============================================================
// __str_indexOf(haystack: i32, needle: i32) -> i32
// Returns byte offset of first occurrence, or -1
// ============================================================
pub(super) fn build_str_index_of() -> Function {
    // Params: haystack=0, needle=1
    // Locals: h_len=2, n_len=3, limit=4, i=5, j=6, matched=7
    let locals = vec![
        (1, ValType::I32), // h_len
        (1, ValType::I32), // n_len
        (1, ValType::I32), // limit
        (1, ValType::I32), // i
        (1, ValType::I32), // j
        (1, ValType::I32), // matched
    ];
    let mut func = Function::new(locals);
    let (haystack, needle) = (0u32, 1);
    let (h_len, n_len, limit, i, j, matched) = (2u32, 3, 4, 5, 6, 7);

    // h_len = load(haystack), n_len = load(needle)
    func.instruction(&Instruction::LocalGet(haystack));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(h_len));
    func.instruction(&Instruction::LocalGet(needle));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(n_len));

    // if n_len == 0: return 0
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // if n_len > h_len: return -1
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::LocalGet(h_len));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // limit = h_len - n_len
    func.instruction(&Instruction::LocalGet(h_len));
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(limit));

    // i = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    // outer loop
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty)); // outer block
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty)); // outer loop

    // if i > limit: break → return -1
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(limit));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::BrIf(1));

    // j = 0, matched = 1
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(matched));

    // inner loop: compare bytes
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    // if j >= n_len: break inner
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    // h_byte = load8_u(haystack + 4 + i + j)
    func.instruction(&Instruction::LocalGet(haystack));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    // n_byte = load8_u(needle + 4 + j)
    func.instruction(&Instruction::LocalGet(needle));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    // if h_byte != n_byte: matched=0, break inner
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(matched));
    func.instruction(&Instruction::Br(2)); // break inner block
    func.instruction(&Instruction::End);

    // j++
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Br(0)); // continue inner loop
    func.instruction(&Instruction::End); // end inner loop
    func.instruction(&Instruction::End); // end inner block

    // if matched: return i
    func.instruction(&Instruction::LocalGet(matched));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // i++
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0)); // continue outer loop
    func.instruction(&Instruction::End); // end outer loop
    func.instruction(&Instruction::End); // end outer block

    // return -1
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_startsWith(s: i32, prefix: i32) -> i32
// ============================================================
pub(super) fn build_str_starts_with() -> Function {
    // Params: s=0, prefix=1
    // Locals: s_len=2, p_len=3, i=4
    let locals = vec![
        (1, ValType::I32), // s_len
        (1, ValType::I32), // p_len
        (1, ValType::I32), // i
    ];
    let mut func = Function::new(locals);
    let (s, prefix) = (0u32, 1);
    let (s_len, p_len, i) = (2u32, 3, 4);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(s_len));
    func.instruction(&Instruction::LocalGet(prefix));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(p_len));

    // if p_len > s_len: return 0
    func.instruction(&Instruction::LocalGet(p_len));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // Compare first p_len bytes
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(p_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    // Compare bytes
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    func.instruction(&Instruction::LocalGet(prefix));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // end loop
    func.instruction(&Instruction::End); // end block

    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_endsWith(s: i32, suffix: i32) -> i32
// ============================================================
pub(super) fn build_str_ends_with() -> Function {
    // Params: s=0, suffix=1
    // Locals: s_len=2, suf_len=3, offset=4, i=5
    let locals = vec![
        (1, ValType::I32), // s_len
        (1, ValType::I32), // suf_len
        (1, ValType::I32), // offset
        (1, ValType::I32), // i
    ];
    let mut func = Function::new(locals);
    let (s, suffix) = (0u32, 1);
    let (s_len, suf_len, offset, i) = (2u32, 3, 4, 5);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(s_len));
    func.instruction(&Instruction::LocalGet(suffix));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(suf_len));

    // if suf_len > s_len: return 0
    func.instruction(&Instruction::LocalGet(suf_len));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // offset = s_len - suf_len
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(suf_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(offset));

    // Compare last suf_len bytes
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(suf_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    // s byte at offset + i
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(offset));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    // suffix byte at i
    func.instruction(&Instruction::LocalGet(suffix));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_includes(s: i32, search: i32) -> i32
// Delegates to indexOf >= 0
// ============================================================
pub(super) fn build_str_includes() -> Function {
    // This is a simple wrapper — we can't call indexOf from here since we don't know its index.
    // Instead, implement inline (same as indexOf but return 0/1).
    // Params: s=0, search=1
    // Locals: h_len=2, n_len=3, limit=4, i=5, j=6, matched=7
    let locals = vec![
        (1, ValType::I32),
        (1, ValType::I32),
        (1, ValType::I32),
        (1, ValType::I32),
        (1, ValType::I32),
        (1, ValType::I32),
    ];
    let mut func = Function::new(locals);
    let (s, search) = (0u32, 1);
    let (h_len, n_len, limit, i, j, matched) = (2u32, 3, 4, 5, 6, 7);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(h_len));
    func.instruction(&Instruction::LocalGet(search));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(n_len));

    // Empty needle: return 1
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // needle > haystack: return 0
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::LocalGet(h_len));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(h_len));
    func.instruction(&Instruction::LocalGet(n_len));
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

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(matched));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalGet(n_len));
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
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::End);
    func
}
