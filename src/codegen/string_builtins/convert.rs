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
// __str_from_f64(n: f64) -> i32
// Convert f64 to decimal string.
// Strategy: convert integer part via i32 path, append fractional
// digits (up to 6 significant, strip trailing zeros).
// ============================================================
pub(super) fn build_str_from_f64(arena_idx: u32) -> Function {
    // This is complex. We'll use a simpler approach:
    // 1. If the value has no fractional part, convert as i32
    // 2. Otherwise: handle sign, integer part, '.', fractional part (up to 6 digits)
    //
    // We'll write the string character-by-character into arena memory.

    // Params: n=0
    // Locals: is_neg=1, int_part=2, frac_val=3, buf=4, pos=5, digit=6, ptr=7,
    //         abs_val=8, temp=9, frac_digits=10, len=11
    let locals = vec![
        (1, ValType::I32), // is_neg
        (1, ValType::I32), // int_part
        (1, ValType::I32), // frac_val (fractional part * 1000000 as i32)
        (1, ValType::I32), // buf
        (1, ValType::I32), // pos (write position from start)
        (1, ValType::I32), // digit
        (1, ValType::I32), // ptr (result string)
        (1, ValType::I32), // abs_int
        (1, ValType::I32), // temp (for reversing digits)
        (1, ValType::I32), // digit_start
        (1, ValType::I32), // len
        (1, ValType::F64), // abs_f
    ];
    let mut func = Function::new(locals);
    let n = 0u32;
    let (is_neg, int_part, frac_val, buf, pos, digit, ptr, abs_int, temp, digit_start, len, abs_f) =
        (1u32, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12);

    // Allocate 32-byte scratch buffer
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(buf));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::I32Const(32));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // pos = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(pos));

    // Check negative
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::F64Const(0.0f64));
    func.instruction(&Instruction::F64Lt);
    func.instruction(&Instruction::LocalSet(is_neg));

    // abs_f = is_neg ? -n : n
    func.instruction(&Instruction::LocalGet(is_neg));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::F64Neg);
    func.instruction(&Instruction::LocalSet(abs_f));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::LocalSet(abs_f));
    func.instruction(&Instruction::End);

    // Write '-' if negative
    func.instruction(&Instruction::LocalGet(is_neg));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::I32Const(45)); // '-'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::End);

    // int_part = trunc(abs_f) as i32
    func.instruction(&Instruction::LocalGet(abs_f));
    func.instruction(&Instruction::F64Floor);
    func.instruction(&Instruction::I32TruncF64U);
    func.instruction(&Instruction::LocalSet(int_part));

    // Write integer part digits (reverse order then flip)
    // digit_start = pos (remember where digits start for reversing)
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::LocalSet(digit_start));

    // Handle 0 integer part
    func.instruction(&Instruction::LocalGet(int_part));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(48)); // '0'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::Else);

    // Write digits of int_part (forward: extract digits, store in reverse order, then reverse)
    func.instruction(&Instruction::LocalGet(int_part));
    func.instruction(&Instruction::LocalSet(abs_int));
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32RemU);
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(digit));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(digit));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32DivU);
    func.instruction(&Instruction::LocalSet(abs_int));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Reverse the integer digits in place [digit_start..pos)
    // Use temp for swapping. left=digit_start, right=pos-1
    func.instruction(&Instruction::LocalGet(digit_start));
    func.instruction(&Instruction::LocalSet(abs_int)); // reuse as left
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(temp)); // right

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(abs_int)); // left
    func.instruction(&Instruction::LocalGet(temp)); // right
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    // swap buf[left] and buf[right]
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(digit)); // save left char
    // buf[left] = buf[right]
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&mem_store8(0));
    // buf[right] = saved left char
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(digit));
    func.instruction(&mem_store8(0));
    // left++, right--
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(abs_int));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(temp));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::End); // end if int_part == 0 else

    // Check if there's a fractional part
    // frac_val = round((abs_f - floor(abs_f)) * 1000000) as i32
    func.instruction(&Instruction::LocalGet(abs_f));
    func.instruction(&Instruction::LocalGet(abs_f));
    func.instruction(&Instruction::F64Floor);
    func.instruction(&Instruction::F64Sub);
    func.instruction(&Instruction::F64Const(1000000.0f64));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::F64Nearest); // round
    func.instruction(&Instruction::I32TruncF64U);
    func.instruction(&Instruction::LocalSet(frac_val));

    // If frac_val > 0, write '.' and digits
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));

    // Strip trailing zeros from frac_val
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32RemU);
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::I32Eqz); // non-zero remainder? break
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32DivU);
    func.instruction(&Instruction::LocalSet(frac_val));
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1)); // if became 0, break
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Write '.'
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(46)); // '.'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(pos));

    // Write frac digits (backwards then reverse, same pattern)
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::LocalSet(digit_start));

    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32RemU);
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(digit));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(digit));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::LocalGet(frac_val));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32DivU);
    func.instruction(&Instruction::LocalSet(frac_val));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Reverse frac digits
    func.instruction(&Instruction::LocalGet(digit_start));
    func.instruction(&Instruction::LocalSet(abs_int));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(temp));
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(digit));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(digit));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(abs_int));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(abs_int));
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(temp));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::End); // end if frac_val > 0

    // len = pos
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::LocalSet(len));

    // Allocate final string: ptr = arena_alloc(4 + len)
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // Store length
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&mem_store_i32(0));

    // Copy from buf to ptr+4
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(buf));
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
// __str_toFixed(n: f64, digits: i32) -> i32
// Format f64 to fixed-point decimal string with `digits` decimal places.
// Example: (3.14159).toFixed(2) → "3.14"
// Handles: sign, rounding, zero-padding of fractional part.
// Supports digits 0–9. Values scaled beyond i32 range may overflow.
// ============================================================
pub(super) fn build_str_to_fixed(arena_idx: u32) -> Function {
    // Params: n=0(f64), digits=1(i32)
    // Locals: is_neg=2, abs_f=3(f64), scale=4(f64), scaled=5, buf=6, pos=7,
    //         digit=8, int_digits=9, total_digits=10, ptr=11, len=12,
    //         i=13, d_start=14, left=15, right=16, temp=17
    let locals = vec![
        (1, ValType::I32), // is_neg
        (1, ValType::F64), // abs_f
        (1, ValType::F64), // scale
        (1, ValType::I32), // scaled (the integer = abs(n) * 10^digits, rounded)
        (1, ValType::I32), // buf (scratch area)
        (1, ValType::I32), // pos (write position)
        (1, ValType::I32), // digit
        (1, ValType::I32), // int_digits (count of digits before decimal)
        (1, ValType::I32), // total_digits (count of all digits in scaled)
        (1, ValType::I32), // ptr (result string)
        (1, ValType::I32), // len
        (1, ValType::I32), // i (loop counter)
        (1, ValType::I32), // d_start
        (1, ValType::I32), // left (for reversing)
        (1, ValType::I32), // right (for reversing)
        (1, ValType::I32), // temp (swap)
    ];
    let mut func = Function::new(locals);
    let (n, digits) = (0u32, 1);
    let (is_neg, abs_f, scale, scaled, buf, pos, digit, int_digits, total_digits, ptr, len, i, d_start, left, right, temp) =
        (2u32, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17);

    // Allocate 32-byte scratch buffer
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(buf));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::I32Const(32));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // is_neg = n < 0
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::F64Const(0.0));
    func.instruction(&Instruction::F64Lt);
    func.instruction(&Instruction::LocalSet(is_neg));

    // abs_f = abs(n)
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::F64Abs);
    func.instruction(&Instruction::LocalSet(abs_f));

    // Compute scale = 10^digits via loop
    func.instruction(&Instruction::F64Const(1.0));
    func.instruction(&Instruction::LocalSet(scale));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(digits));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(scale));
    func.instruction(&Instruction::F64Const(10.0));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::LocalSet(scale));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // scaled = floor(abs_f * scale + 0.5) as i32  (rounds to nearest)
    func.instruction(&Instruction::LocalGet(abs_f));
    func.instruction(&Instruction::LocalGet(scale));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::F64Const(0.5));
    func.instruction(&Instruction::F64Add);
    func.instruction(&Instruction::F64Floor);
    func.instruction(&Instruction::I32TruncF64S);
    func.instruction(&Instruction::LocalSet(scaled));

    // pos = 0: start writing into buf
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(pos));

    // Write '-' if negative
    func.instruction(&Instruction::LocalGet(is_neg));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::I32Const(45)); // '-'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::End);

    // Extract all digits of `scaled` into buf (reversed), then reverse.
    // If scaled == 0, write a single "0".
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::LocalSet(d_start));

    func.instruction(&Instruction::LocalGet(scaled));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    // scaled is 0: write enough zeros for "0." + digits zeros
    // total_digits = digits + 1 (at minimum 1 digit for the integer "0")
    func.instruction(&Instruction::LocalGet(digits));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total_digits));
    // Write total_digits '0's
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(total_digits));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(48)); // '0'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::Else);
    // Extract digits of scaled (reversed)
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(total_digits));
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(scaled));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(scaled));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32RemU);
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(digit));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(digit));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::LocalGet(total_digits));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total_digits));
    func.instruction(&Instruction::LocalGet(scaled));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32DivU);
    func.instruction(&Instruction::LocalSet(scaled));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Pad with leading zeros if total_digits <= digits
    // (e.g., toFixed(3) on 0.01 → scaled=10, total_digits=2, but we need at least digits+1=4 digits)
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(total_digits));
    func.instruction(&Instruction::LocalGet(digits));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(48)); // '0'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::LocalGet(total_digits));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total_digits));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Reverse the digits we just wrote [d_start..pos)
    func.instruction(&Instruction::LocalGet(d_start));
    func.instruction(&Instruction::LocalSet(left));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(right));
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(left));
    func.instruction(&Instruction::LocalGet(right));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    // swap buf[left] and buf[right]
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(left));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(temp));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(left));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(right));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(right));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(temp));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(left));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(left));
    func.instruction(&Instruction::LocalGet(right));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(right));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::End); // end if scaled == 0 else

    // Now buf contains: ['-']? digits_string
    // int_digits = total_digits - digits (number of integer-part digits)
    func.instruction(&Instruction::LocalGet(total_digits));
    func.instruction(&Instruction::LocalGet(digits));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(int_digits));

    // If digits == 0, no decimal point needed. Output is already complete.
    // Otherwise, we need to insert '.' by building the final string.
    // Final string = sign + integer_digits + '.' + fractional_digits

    func.instruction(&Instruction::LocalGet(digits));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    {
        // digits == 0: result is just the integer part already in buf[0..pos)
        // len = pos
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::LocalSet(len));
    }
    func.instruction(&Instruction::Else);
    {
        // Need to insert decimal point.
        // Final layout: buf[0..d_start] (sign) + buf[d_start..d_start+int_digits] (int) + '.' + buf[d_start+int_digits..pos] (frac)
        // We'll build the final string in the result allocation directly.
        // len = pos + 1 (for the '.')
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(len));
    }
    func.instruction(&Instruction::End);

    // Allocate result string
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&mem_store_i32(0));

    // Build the output
    func.instruction(&Instruction::LocalGet(digits));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    {
        // No decimal: copy buf[0..len] directly
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(len));
        func.instruction(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
    }
    func.instruction(&Instruction::Else);
    {
        // Copy sign + integer part: buf[0..d_start+int_digits]
        let sign_and_int_len = d_start; // reuse local
        func.instruction(&Instruction::LocalGet(d_start));
        func.instruction(&Instruction::LocalGet(int_digits));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(sign_and_int_len));

        // Copy sign + integer digits
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(sign_and_int_len));
        func.instruction(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });

        // Write '.'
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(sign_and_int_len));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(46)); // '.'
        func.instruction(&mem_store8(0));

        // Copy fractional digits: buf[d_start+int_digits .. pos]
        // frac_len = digits
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(sign_and_int_len));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(1)); // past the '.'
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(sign_and_int_len));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(digits));
        func.instruction(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
    }
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

// ============================================================
// __str_toPrecision(n: f64, precision: i32) -> i32
// Format f64 to `precision` significant digits.
// Examples: (123.456).toPrecision(5) → "123.46"
//           (0.00456).toPrecision(2) → "0.0046"
//           (123.456).toPrecision(2) → "120"
// Does NOT produce scientific notation (simplified for game use).
// ============================================================
pub(super) fn build_str_to_precision(arena_idx: u32) -> Function {
    // Params: n=0(f64), precision=1(i32)
    // Locals: is_neg=2, abs_f=3(f64), int_digits=4, temp_int=5, scale=6(f64),
    //         scaled=7, buf=8, pos=9, digit=10, ptr=11, len=12,
    //         i=13, d_start=14, left=15, right=16, swap=17,
    //         dec_places=18, total_digits=19, leading_zeros=20, temp_f=21(f64)
    let locals = vec![
        (1, ValType::I32),  // is_neg
        (1, ValType::F64),  // abs_f
        (1, ValType::I32),  // int_digits
        (1, ValType::I32),  // temp_int
        (1, ValType::F64),  // scale
        (1, ValType::I32),  // scaled
        (1, ValType::I32),  // buf
        (1, ValType::I32),  // pos
        (1, ValType::I32),  // digit
        (1, ValType::I32),  // ptr
        (1, ValType::I32),  // len
        (1, ValType::I32),  // i
        (1, ValType::I32),  // d_start
        (1, ValType::I32),  // left
        (1, ValType::I32),  // right
        (1, ValType::I32),  // swap
        (1, ValType::I32),  // dec_places
        (1, ValType::I32),  // total_digits
        (1, ValType::I32),  // leading_zeros
        (1, ValType::F64),  // temp_f
    ];
    let mut func = Function::new(locals);
    let (n, precision) = (0u32, 1);
    let (is_neg, abs_f, int_digits, temp_int, scale, scaled, buf, pos, digit, ptr, len, i, d_start, left, right, swap, dec_places, total_digits, leading_zeros, temp_f) =
        (2u32, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21);

    // Allocate 40-byte scratch buffer
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(buf));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::I32Const(40));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));

    // is_neg = n < 0
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::F64Const(0.0));
    func.instruction(&Instruction::F64Lt);
    func.instruction(&Instruction::LocalSet(is_neg));

    // abs_f = abs(n)
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::F64Abs);
    func.instruction(&Instruction::LocalSet(abs_f));

    // pos = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(pos));

    // Write '-' if negative
    func.instruction(&Instruction::LocalGet(is_neg));
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::I32Const(45)); // '-'
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::End);

    // Count integer digits: temp_int = trunc(abs_f) as i32, count digits
    func.instruction(&Instruction::LocalGet(abs_f));
    func.instruction(&Instruction::F64Floor);
    func.instruction(&Instruction::I32TruncF64S);
    func.instruction(&Instruction::LocalSet(temp_int));

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(int_digits));

    // If temp_int == 0 and abs_f < 1, int_digits stays 0
    // Otherwise count digits
    func.instruction(&Instruction::LocalGet(temp_int));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    {
        // Count digits of temp_int
        func.instruction(&Instruction::LocalGet(temp_int));
        func.instruction(&Instruction::LocalSet(scaled)); // reuse as temp
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(scaled));
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(int_digits));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(int_digits));
        func.instruction(&Instruction::LocalGet(scaled));
        func.instruction(&Instruction::I32Const(10));
        func.instruction(&Instruction::I32DivU);
        func.instruction(&Instruction::LocalSet(scaled));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    }
    func.instruction(&Instruction::End);

    // Now branch on three cases:
    // Case 1: abs_f == 0 → output "0.000..." with precision-1 zeros
    // Case 2: int_digits >= precision → round, no decimal point
    // Case 3: int_digits > 0 && int_digits < precision → like toFixed(precision - int_digits)
    // Case 4: int_digits == 0 (abs_f < 1) → "0." + leading zeros + significant digits

    func.instruction(&Instruction::LocalGet(abs_f));
    func.instruction(&Instruction::F64Const(0.0));
    func.instruction(&Instruction::F64Eq);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    {
        // === Case 1: abs_f == 0 ===
        // Write "0"
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(48)); // '0'
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));

        // If precision > 1, write '.' and (precision-1) zeros
        func.instruction(&Instruction::LocalGet(precision));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32GtS);
        func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(46)); // '.'
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(precision));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(48)); // '0'
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    }
    func.instruction(&Instruction::Else);

    func.instruction(&Instruction::LocalGet(int_digits));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    {
        // === Case 4: abs_f < 1 (int_digits == 0) ===
        // Write "0."
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(48)); // '0'
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(46)); // '.'
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));

        // Count leading zeros: multiply abs_f by 10 until >= 1
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(leading_zeros));
        func.instruction(&Instruction::LocalGet(abs_f));
        func.instruction(&Instruction::LocalSet(temp_f));
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(temp_f));
        func.instruction(&Instruction::F64Const(1.0));
        func.instruction(&Instruction::F64Ge);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(temp_f));
        func.instruction(&Instruction::F64Const(10.0));
        func.instruction(&Instruction::F64Mul);
        func.instruction(&Instruction::LocalSet(temp_f));
        func.instruction(&Instruction::LocalGet(leading_zeros));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(leading_zeros));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        // Write leading_zeros - 1 zeros (the first one is already the "0." prefix)
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(leading_zeros));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(48)); // '0'
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        // Now emit `precision` significant digits:
        // scale = 10^precision, scaled = round(abs_f * 10^(leading_zeros + precision - 1) ... )
        // Actually: scale = 10^(leading_zeros + precision), scaled = round(abs_f * scale)
        func.instruction(&Instruction::F64Const(1.0));
        func.instruction(&Instruction::LocalSet(scale));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(i));
        // target_exp = leading_zeros + precision - 1
        // (leading_zeros counts multiplications to reach >=1; we want p sig digits)
        func.instruction(&Instruction::LocalGet(leading_zeros));
        func.instruction(&Instruction::LocalGet(precision));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(dec_places)); // reuse
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(dec_places));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(scale));
        func.instruction(&Instruction::F64Const(10.0));
        func.instruction(&Instruction::F64Mul);
        func.instruction(&Instruction::LocalSet(scale));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        func.instruction(&Instruction::LocalGet(abs_f));
        func.instruction(&Instruction::LocalGet(scale));
        func.instruction(&Instruction::F64Mul);
        func.instruction(&Instruction::F64Const(0.5));
        func.instruction(&Instruction::F64Add);
        func.instruction(&Instruction::F64Floor);
        func.instruction(&Instruction::I32TruncF64S);
        func.instruction(&Instruction::LocalSet(scaled));

        // Extract `precision` digits of scaled (reversed)
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::LocalSet(d_start));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(total_digits));
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(scaled));
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(scaled));
        func.instruction(&Instruction::I32Const(10));
        func.instruction(&Instruction::I32RemU);
        func.instruction(&Instruction::I32Const(48));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(digit));
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(digit));
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::LocalGet(total_digits));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(total_digits));
        func.instruction(&Instruction::LocalGet(scaled));
        func.instruction(&Instruction::I32Const(10));
        func.instruction(&Instruction::I32DivU);
        func.instruction(&Instruction::LocalSet(scaled));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        // Pad with leading zeros if total_digits < precision
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(total_digits));
        func.instruction(&Instruction::LocalGet(precision));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(48));
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::LocalGet(total_digits));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(total_digits));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        // Reverse digits [d_start..pos)
        emit_reverse_range(&mut func, buf, d_start, pos, left, right, swap);
    }
    func.instruction(&Instruction::Else);

    func.instruction(&Instruction::LocalGet(int_digits));
    func.instruction(&Instruction::LocalGet(precision));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    {
        // === Case 2: int_digits >= precision → round, no decimal ===
        // scale = 10^(int_digits - precision)
        func.instruction(&Instruction::F64Const(1.0));
        func.instruction(&Instruction::LocalSet(scale));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::LocalGet(int_digits));
        func.instruction(&Instruction::LocalGet(precision));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(dec_places)); // reuse as exp
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(dec_places));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(scale));
        func.instruction(&Instruction::F64Const(10.0));
        func.instruction(&Instruction::F64Mul);
        func.instruction(&Instruction::LocalSet(scale));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        // rounded = round(abs_f / scale) * scale → as i32
        func.instruction(&Instruction::LocalGet(abs_f));
        func.instruction(&Instruction::LocalGet(scale));
        func.instruction(&Instruction::F64Div);
        func.instruction(&Instruction::F64Const(0.5));
        func.instruction(&Instruction::F64Add);
        func.instruction(&Instruction::F64Floor);
        func.instruction(&Instruction::LocalGet(scale));
        func.instruction(&Instruction::F64Mul);
        func.instruction(&Instruction::F64Const(0.5));
        func.instruction(&Instruction::F64Add);
        func.instruction(&Instruction::F64Floor);
        func.instruction(&Instruction::I32TruncF64S);
        func.instruction(&Instruction::LocalSet(scaled));

        // Extract digits of scaled (reversed)
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::LocalSet(d_start));
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(scaled));
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(scaled));
        func.instruction(&Instruction::I32Const(10));
        func.instruction(&Instruction::I32RemU);
        func.instruction(&Instruction::I32Const(48));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(digit));
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(digit));
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::LocalGet(scaled));
        func.instruction(&Instruction::I32Const(10));
        func.instruction(&Instruction::I32DivU);
        func.instruction(&Instruction::LocalSet(scaled));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        // Reverse
        emit_reverse_range(&mut func, buf, d_start, pos, left, right, swap);
    }
    func.instruction(&Instruction::Else);
    {
        // === Case 3: 0 < int_digits < precision → like toFixed(precision - int_digits) ===
        func.instruction(&Instruction::LocalGet(precision));
        func.instruction(&Instruction::LocalGet(int_digits));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(dec_places));

        // scale = 10^dec_places
        func.instruction(&Instruction::F64Const(1.0));
        func.instruction(&Instruction::LocalSet(scale));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(dec_places));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(scale));
        func.instruction(&Instruction::F64Const(10.0));
        func.instruction(&Instruction::F64Mul);
        func.instruction(&Instruction::LocalSet(scale));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        // scaled = round(abs_f * scale)
        func.instruction(&Instruction::LocalGet(abs_f));
        func.instruction(&Instruction::LocalGet(scale));
        func.instruction(&Instruction::F64Mul);
        func.instruction(&Instruction::F64Const(0.5));
        func.instruction(&Instruction::F64Add);
        func.instruction(&Instruction::F64Floor);
        func.instruction(&Instruction::I32TruncF64S);
        func.instruction(&Instruction::LocalSet(scaled));

        // Extract all digits (reversed)
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::LocalSet(d_start));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(total_digits));
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(scaled));
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(scaled));
        func.instruction(&Instruction::I32Const(10));
        func.instruction(&Instruction::I32RemU);
        func.instruction(&Instruction::I32Const(48));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(digit));
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(digit));
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::LocalGet(total_digits));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(total_digits));
        func.instruction(&Instruction::LocalGet(scaled));
        func.instruction(&Instruction::I32Const(10));
        func.instruction(&Instruction::I32DivU);
        func.instruction(&Instruction::LocalSet(scaled));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        // Pad if total_digits < precision
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(total_digits));
        func.instruction(&Instruction::LocalGet(precision));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(48));
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::LocalGet(total_digits));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(total_digits));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        // Reverse digits [d_start..pos)
        emit_reverse_range(&mut func, buf, d_start, pos, left, right, swap);

        // Now insert decimal point: digits are at buf[d_start..pos]
        // We need int_digits before the '.', then the rest after.
        // Shift fractional digits right by 1 to make room for '.'
        // Move buf[d_start+int_digits .. pos] → buf[d_start+int_digits+1 .. pos+1]
        // Then write '.' at buf[d_start+int_digits]

        // Copy backwards to avoid overlap issues
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::LocalSet(i)); // i = pos (source end)
        func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(d_start));
        func.instruction(&Instruction::LocalGet(int_digits));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32LeS);
        func.instruction(&Instruction::BrIf(1));
        // buf[i] = buf[i-1]
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::I32Add);
        func.instruction(&mem_load8_u(0));
        func.instruction(&mem_store8(0));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        // Write '.' at d_start + int_digits
        func.instruction(&Instruction::LocalGet(buf));
        func.instruction(&Instruction::LocalGet(d_start));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(int_digits));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(46)); // '.'
        func.instruction(&mem_store8(0));

        // pos += 1 (for the inserted '.')
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));
    }
    func.instruction(&Instruction::End); // end case 2 vs 3
    func.instruction(&Instruction::End); // end case 4 vs (2|3)
    func.instruction(&Instruction::End); // end case 1 vs rest

    // len = pos
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::LocalSet(len));

    // Allocate result string
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::GlobalGet(arena_idx));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(arena_idx));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&mem_store_i32(0));

    // Copy buf[0..len] to ptr+4
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Const(STRING_HEADER_SIZE));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::End);
    func
}

/// Emit inline: reverse buf[start_local..end_local) in place.
fn emit_reverse_range(
    func: &mut Function,
    buf: u32,
    start_local: u32,
    end_local: u32,
    left: u32,
    right: u32,
    swap: u32,
) {
    func.instruction(&Instruction::LocalGet(start_local));
    func.instruction(&Instruction::LocalSet(left));
    func.instruction(&Instruction::LocalGet(end_local));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(right));
    func.instruction(&Instruction::Block(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::Loop(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(left));
    func.instruction(&Instruction::LocalGet(right));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    // swap
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(left));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&Instruction::LocalSet(swap));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(left));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(right));
    func.instruction(&Instruction::I32Add);
    func.instruction(&mem_load8_u(0));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(buf));
    func.instruction(&Instruction::LocalGet(right));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(swap));
    func.instruction(&mem_store8(0));
    func.instruction(&Instruction::LocalGet(left));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(left));
    func.instruction(&Instruction::LocalGet(right));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(right));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
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
// __str_parseFloat(s: i32) -> f64
// Parse decimal float from string. Handles sign, integer, '.', fractional.
// ============================================================
pub(super) fn build_str_parse_float() -> Function {
    // Params: s=0
    // Locals: len=1, i=2, byte=3, sign=4, int_part=5, frac_part=6, frac_div=7
    let locals = vec![
        (1, ValType::I32), // len
        (1, ValType::I32), // i
        (1, ValType::I32), // byte
        (1, ValType::F64), // sign
        (1, ValType::F64), // int_part
        (1, ValType::F64), // frac_part
        (1, ValType::F64), // frac_div
    ];
    let mut func = Function::new(locals);
    let s = 0u32;
    let (len, i, byte, sign, int_part, frac_part, frac_div) = (1u32, 2, 3, 4, 5, 6, 7);

    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&mem_load_i32(0));
    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::F64Const(1.0f64));
    func.instruction(&Instruction::LocalSet(sign));
    func.instruction(&Instruction::F64Const(0.0f64));
    func.instruction(&Instruction::LocalSet(int_part));
    func.instruction(&Instruction::F64Const(0.0f64));
    func.instruction(&Instruction::LocalSet(frac_part));
    func.instruction(&Instruction::F64Const(1.0f64));
    func.instruction(&Instruction::LocalSet(frac_div));

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
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(45)); // '-'
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::F64Const(-1.0f64));
    func.instruction(&Instruction::LocalSet(sign));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(43)); // '+'
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Parse integer part
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
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(57));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(int_part));
    func.instruction(&Instruction::F64Const(10.0f64));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::F64ConvertI32U);
    func.instruction(&Instruction::F64Add);
    func.instruction(&Instruction::LocalSet(int_part));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Check for '.'
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
    func.instruction(&Instruction::I32Const(46)); // '.'
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));

    // Parse fractional digits
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
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(57));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(frac_div));
    func.instruction(&Instruction::F64Const(10.0f64));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::LocalSet(frac_div));
    func.instruction(&Instruction::LocalGet(frac_part));
    func.instruction(&Instruction::F64Const(10.0f64));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Const(48));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::F64ConvertI32U);
    func.instruction(&Instruction::F64Add);
    func.instruction(&Instruction::LocalSet(frac_part));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::End); // end if '.'
    func.instruction(&Instruction::End); // end if i < len

    // result = sign * (int_part + frac_part / frac_div)
    func.instruction(&Instruction::LocalGet(sign));
    func.instruction(&Instruction::LocalGet(int_part));
    func.instruction(&Instruction::LocalGet(frac_part));
    func.instruction(&Instruction::LocalGet(frac_div));
    func.instruction(&Instruction::F64Div);
    func.instruction(&Instruction::F64Add);
    func.instruction(&Instruction::F64Mul);
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
