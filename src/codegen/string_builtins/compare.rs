use wasm_encoder::{Function, Instruction, ValType};

use super::{STRING_HEADER_SIZE, mem_load_i32, mem_load8_u};
// ============================================================
// __str_eq(a: i32, b: i32) -> i32
// Returns 1 if equal, 0 otherwise
// ============================================================
pub(super) fn build_str_eq() -> Function {
    // Params: a=0, b=1
    // Locals: len_a=2, i=3, byte_a=4, byte_b=5
    let locals = vec![
        (1, ValType::I32), // len_a
        (1, ValType::I32), // i
        (1, ValType::I32), // byte_a
        (1, ValType::I32), // byte_b
    ];
    let mut func = Function::new(locals);
    let (a, b) = (0u32, 1);
    let (len_a, i) = (2u32, 3);

    // len_a = load(a)
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len_a));

    // if len_a != load(b): return 0
    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // i = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    // loop
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty)); // block (break target)
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty)); // loop

    // if i >= len_a: break → return 1
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1)); // break to outer block

    // byte_a = load8_u(a + 4 + i)
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    // byte_b = load8_u(b + 4 + i)
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));

    // if byte_a != byte_b: return 0
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // i++
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));

    // br loop
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // end loop
    func.instruction(&Instruction::End); // end block

    // return 1
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_cmp(a: i32, b: i32) -> i32
// Lexicographic compare: returns -1, 0, or 1
// ============================================================
pub(super) fn build_str_cmp() -> Function {
    // Params: a=0, b=1
    // Locals: len_a=2, len_b=3, min_len=4, i=5, byte_a=6, byte_b=7
    let locals = vec![
        (1, ValType::I32), // len_a
        (1, ValType::I32), // len_b
        (1, ValType::I32), // min_len
        (1, ValType::I32), // i
        (1, ValType::I32), // byte_a
        (1, ValType::I32), // byte_b
    ];
    let mut func = Function::new(locals);
    let (a, b) = (0u32, 1);
    let (len_a, len_b, min_len, i, byte_a, byte_b) = (2u32, 3, 4, 5, 6, 7);

    // len_a = load(a), len_b = load(b)
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len_a));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len_b));

    // min_len = min(len_a, len_b)
    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::LocalGet(len_b));
    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::LocalGet(len_b));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(min_len));

    // i = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    // loop: compare bytes
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    // if i >= min_len: break
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(min_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    // byte_a = load8_u(a+4+i)
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte_a));

    // byte_b = load8_u(b+4+i)
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte_b));

    // if byte_a < byte_b: return -1
    func.instruction(&Instruction::LocalGet(byte_a));
    func.instruction(&Instruction::LocalGet(byte_b));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // if byte_a > byte_b: return 1
    func.instruction(&Instruction::LocalGet(byte_a));
    func.instruction(&Instruction::LocalGet(byte_b));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // i++
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // end loop
    func.instruction(&Instruction::End); // end block

    // Compare lengths: if len_a < len_b return -1, if > return 1, else 0
    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::LocalGet(len_b));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::LocalGet(len_b));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::End);
    func
}
