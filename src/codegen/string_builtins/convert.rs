use wasm_encoder::{Function, Instruction, ValType};

use super::{
    STRING_HEADER_SIZE, emit_is_whitespace, mem_load_i32, mem_load8_u, mem_store_i32, mem_store8,
};
// ============================================================
// __str_from_i32(n: i32) -> i32
// Convert integer to decimal string. Handles negatives.
// Strategy: write digits backwards into a 12-byte scratch area,
// then copy to a properly sized arena string.
// ============================================================
pub(super) fn build_str_from_i32(arena_idx: u32) -> Function {
    // Params: n=0
    // Locals: is_neg=1, abs_val=2, buf_start=3, pos=4, digit=5, len=6, ptr=7, total=8
    let locals = vec![
        (1, ValType::I32), // is_neg
        (1, ValType::I32), // abs_val
        (1, ValType::I32), // buf_start (scratch area in arena for digits)
        (1, ValType::I32), // pos (write position, counts from end)
        (1, ValType::I32), // digit
        (1, ValType::I32), // len
        (1, ValType::I32), // ptr
        (1, ValType::I32), // total
    ];
    let mut func = Function::new(locals);
    let n = 0u32;
    let (is_neg, abs_val, buf_start, pos, digit, len, ptr, total) = (1u32, 2, 3, 4, 5, 6, 7, 8);

    // Allocate 12-byte scratch buffer from arena (enough for -2147483648)
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(buf_start));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::I32Const(12));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // pos = 11 (write backwards from end of buffer)
    func.instruction(&Instruction::I32Const(11));
    func.instruction(&Instruction::LocalSet(pos));

    // Handle 0 specially
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    // Write '0' at pos, len=1
    func.instruction(&Instruction::LocalGet(buf_start));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(48)); // '0'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(len));
    // Skip the digit extraction loop
    func.instruction(&Instruction::Br(0)); // This exits the if block. We need a different flow.
    func.instruction(&Instruction::End);

    // Handle negative
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::LocalSet(is_neg));

    // abs_val = is_neg ? -n : n  (careful: -INT_MIN overflows, but we handle it via unsigned div)
    func.instruction(&Instruction::LocalGet(is_neg));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(abs_val));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::LocalSet(abs_val));
    func.instruction(&Instruction::End);

    // Extract digits: loop while abs_val > 0 (but only if n != 0)
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::I32Eqz); // n != 0
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));

    // if abs_val == 0: break
    func.instruction(&Instruction::LocalGet(abs_val));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));

    // digit = abs_val % 10
    func.instruction(&Instruction::LocalGet(abs_val));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32RemU);
    func.instruction(&Instruction::LocalSet(digit));

    // abs_val = abs_val / 10
    func.instruction(&Instruction::LocalGet(abs_val));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32DivU);
    func.instruction(&Instruction::LocalSet(abs_val));

    // store digit char at buf_start + pos
    func.instruction(&Instruction::LocalGet(buf_start));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(digit));
    func.instruction(&Instruction::I32Const(48)); // '0'
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_store8(0));

    // pos--
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(pos));

    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // end loop
    func.instruction(&Instruction::End); // end block

    // If negative, write '-'
    func.instruction(&Instruction::LocalGet(is_neg));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(buf_start));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(45)); // '-'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::End);

    // len = 11 - pos
    func.instruction(&Instruction::I32Const(11));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(len));

    func.instruction(&Instruction::End); // end if n != 0

    // Now allocate the actual string: ptr = arena_alloc(4 + len)
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total));

    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // Store length
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&mem_store_i32(0));

    // Copy digits from scratch: memory.copy(ptr+4, buf_start+pos+1, len)
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(buf_start));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_parseInt(s: i32) -> i32
// Parse decimal integer from string. Handles leading whitespace, sign.
// Returns 0 on invalid input (matches simplified JS behavior).
// ============================================================
pub(super) fn build_str_parse_int() -> Function {
    // Params: s=0
    // Locals: len=1, i=2, byte=3, sign=4, result=5
    let locals = vec![
        (1, ValType::I32), // len
        (1, ValType::I32), // i
        (1, ValType::I32), // byte
        (1, ValType::I32), // sign
        (1, ValType::I32), // result
    ];
    let mut func = Function::new(locals);
    let s = 0u32;
    let (len, i, byte, sign, result) = (1u32, 2, 3, 4, 5);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(sign));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(result));

    // Skip whitespace
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte));
    emit_is_whitespace(&mut func, byte);
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Check sign
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte));
    // '-'
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(45));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::LocalSet(sign));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Else);
    // '+'
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(43));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Parse digits
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(byte));
    // if byte < '0' || byte > '9': break
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(57));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::BrIf(1));
    // result = result * 10 + (byte - '0')
    func.instruction(&Instruction::LocalGet(result));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(result));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // return result * sign
    func.instruction(&Instruction::LocalGet(result));
    func.instruction(&Instruction::LocalGet(sign));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::End);
    func
}


// ============================================================
// __str_fromCharCode(code: i32) -> i32
// Create a 1-character string from a char code.
// ============================================================
pub(super) fn build_str_from_char_code(arena_idx: u32) -> Function {
    // Params: code=0
    // Locals: ptr=1
    let locals = vec![(1, ValType::I32)]; // ptr
    let mut func = Function::new(locals);
    let (code, ptr) = (0u32, 1);

    // Allocate 5 bytes: 4 (length header) + 1 (byte)
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::I32Const(5));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // Store length = 1
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&mem_store_i32(0));

    // Store byte
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(code));
    func.instruction(&mem_store8(0));

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}
