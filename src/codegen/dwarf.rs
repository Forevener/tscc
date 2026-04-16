//! Minimal DWARF debug info generation for WASM binaries.
//!
//! Emits `.debug_abbrev`, `.debug_info`, and `.debug_line` custom sections
//! so that wasmtime can map trap addresses back to TypeScript source lines.
//! DWARF version 4, 32-bit format, 4-byte addresses (WASM32).

// --- LEB128 encoding ---

pub fn encode_uleb128(mut value: u64, buf: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            buf.push(byte);
            break;
        }
        buf.push(byte | 0x80);
    }
}

pub fn encode_sleb128(mut value: i64, buf: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        if done {
            buf.push(byte);
            break;
        }
        buf.push(byte | 0x80);
    }
}

/// Compute the encoded size of a ULEB128 value.
pub fn uleb128_size(mut value: u64) -> usize {
    let mut size = 1;
    while value >= 0x80 {
        value >>= 7;
        size += 1;
    }
    size
}

// --- DWARF constants ---

// Tags
const DW_TAG_COMPILE_UNIT: u8 = 0x11;

// Children
const DW_CHILDREN_NO: u8 = 0x00;

// Attributes
const DW_AT_NAME: u8 = 0x03;
const DW_AT_STMT_LIST: u8 = 0x10;
const DW_AT_LOW_PC: u8 = 0x11;
const DW_AT_HIGH_PC: u8 = 0x12;
const DW_AT_LANGUAGE: u8 = 0x13;
const DW_AT_PRODUCER: u8 = 0x25;

// Forms
const DW_FORM_ADDR: u8 = 0x01;
const DW_FORM_DATA4: u8 = 0x06;
const DW_FORM_STRING: u8 = 0x08;
const DW_FORM_DATA2: u8 = 0x05;

// Language (use JavaScript since there's no TypeScript code)
const DW_LANG_JAVASCRIPT: u16 = 0x0020; // Technically "lo_user" range; 0x20 = JS in DWARF5

// Line number standard opcodes
const DW_LNS_COPY: u8 = 0x01;
const DW_LNS_ADVANCE_PC: u8 = 0x02;
const DW_LNS_ADVANCE_LINE: u8 = 0x03;
const DW_LNS_SET_FILE: u8 = 0x04;
const DW_LNS_SET_COLUMN: u8 = 0x05;

// Line number extended opcodes
const DW_LNE_END_SEQUENCE: u8 = 0x01;
const DW_LNE_SET_ADDRESS: u8 = 0x02;

// --- Section builders ---

/// Build a minimal `.debug_abbrev` section.
///
/// Contains one abbreviation (code 1) for DW_TAG_compile_unit with:
/// - DW_AT_name (DW_FORM_string)
/// - DW_AT_producer (DW_FORM_string)
/// - DW_AT_language (DW_FORM_data2)
/// - DW_AT_stmt_list (DW_FORM_data4)
/// - DW_AT_low_pc (DW_FORM_addr)
/// - DW_AT_high_pc (DW_FORM_addr)
pub fn build_debug_abbrev() -> Vec<u8> {
    let mut buf = Vec::new();

    // Abbreviation code 1
    encode_uleb128(1, &mut buf);
    // DW_TAG_compile_unit
    encode_uleb128(DW_TAG_COMPILE_UNIT as u64, &mut buf);
    // No children
    buf.push(DW_CHILDREN_NO);

    // Attribute specs: (attribute, form) pairs
    encode_uleb128(DW_AT_NAME as u64, &mut buf);
    encode_uleb128(DW_FORM_STRING as u64, &mut buf);

    encode_uleb128(DW_AT_PRODUCER as u64, &mut buf);
    encode_uleb128(DW_FORM_STRING as u64, &mut buf);

    encode_uleb128(DW_AT_LANGUAGE as u64, &mut buf);
    encode_uleb128(DW_FORM_DATA2 as u64, &mut buf);

    encode_uleb128(DW_AT_STMT_LIST as u64, &mut buf);
    encode_uleb128(DW_FORM_DATA4 as u64, &mut buf);

    encode_uleb128(DW_AT_LOW_PC as u64, &mut buf);
    encode_uleb128(DW_FORM_ADDR as u64, &mut buf);

    encode_uleb128(DW_AT_HIGH_PC as u64, &mut buf);
    encode_uleb128(DW_FORM_ADDR as u64, &mut buf);

    // End of attribute specs
    buf.push(0x00);
    buf.push(0x00);

    // End of abbreviation table
    buf.push(0x00);

    buf
}

/// Build a minimal `.debug_info` section.
///
/// Contains one compilation unit DIE using abbreviation code 1.
pub fn build_debug_info(
    filename: &str,
    stmt_list_offset: u32,
    low_pc: u32,
    high_pc: u32,
) -> Vec<u8> {
    let producer = "tscc 0.1.0";

    // Build the DIE content first (after the header) to compute unit_length
    let mut die = Vec::new();

    // Abbreviation code 1
    encode_uleb128(1, &mut die);

    // DW_AT_name: null-terminated string
    die.extend_from_slice(filename.as_bytes());
    die.push(0x00);

    // DW_AT_producer: null-terminated string
    die.extend_from_slice(producer.as_bytes());
    die.push(0x00);

    // DW_AT_language: u16
    die.extend_from_slice(&DW_LANG_JAVASCRIPT.to_le_bytes());

    // DW_AT_stmt_list: u32 offset into .debug_line
    die.extend_from_slice(&stmt_list_offset.to_le_bytes());

    // DW_AT_low_pc: 4-byte address
    die.extend_from_slice(&low_pc.to_le_bytes());

    // DW_AT_high_pc: 4-byte address
    die.extend_from_slice(&high_pc.to_le_bytes());

    // Build the full section
    let mut buf = Vec::new();

    // Compilation unit header:
    // unit_length (4 bytes) = size of everything after this field
    let header_rest = 2 + 4 + 1; // version(2) + debug_abbrev_offset(4) + address_size(1)
    let unit_length = (header_rest + die.len()) as u32;
    buf.extend_from_slice(&unit_length.to_le_bytes());

    // version: 4
    buf.extend_from_slice(&4u16.to_le_bytes());

    // debug_abbrev_offset: 0 (we have one abbrev table at offset 0)
    buf.extend_from_slice(&0u32.to_le_bytes());

    // address_size: 4 (WASM32)
    buf.push(4);

    // The DIE itself
    buf.extend_from_slice(&die);

    buf
}

/// Build a `.debug_line` section with a line number program.
///
/// `filename` is the source file name.
/// `line_mappings` is a sorted list of `(wasm_address, source_line, source_column)` triples.
/// `end_address` is one past the last code byte (for end_sequence).
pub fn build_debug_line(
    filename: &str,
    line_mappings: &[(u32, u32, u32)],
    end_address: u32,
) -> Vec<u8> {
    // Line program parameters
    let min_instruction_length: u8 = 1;
    let max_ops_per_insn: u8 = 1; // DWARF v4
    let default_is_stmt: u8 = 1;
    let line_base: i8 = -5;
    let line_range: u8 = 14;
    let opcode_base: u8 = 13; // Standard opcodes 1..12

    // Standard opcode argument counts (opcodes 1..12)
    let std_opcode_lengths: [u8; 12] = [
        0, // DW_LNS_copy
        1, // DW_LNS_advance_pc
        1, // DW_LNS_advance_line
        1, // DW_LNS_set_file
        1, // DW_LNS_set_column
        0, // DW_LNS_negate_stmt
        0, // DW_LNS_set_basic_block
        0, // DW_LNS_const_add_pc
        1, // DW_LNS_fixed_advance_pc
        0, // DW_LNS_set_prologue_end
        0, // DW_LNS_set_epilogue_begin
        1, // DW_LNS_set_isa
    ];

    // Build the header content (everything between header_length and the line program)
    let mut header_content = Vec::new();

    // Directory table: empty (just null terminator)
    header_content.push(0x00);

    // File table: one entry then null terminator
    // Entry: filename (null-terminated), dir_index (ULEB), mod_time (ULEB), file_size (ULEB)
    header_content.extend_from_slice(filename.as_bytes());
    header_content.push(0x00); // null-terminate filename
    encode_uleb128(0, &mut header_content); // dir_index = 0 (current dir)
    encode_uleb128(0, &mut header_content); // mod_time = 0
    encode_uleb128(0, &mut header_content); // file_size = 0

    // End of file table
    header_content.push(0x00);

    // Build the line number program
    let mut program = Vec::new();

    // Set file to 1 (the single file we registered)
    program.push(DW_LNS_SET_FILE);
    encode_uleb128(1, &mut program);

    let mut current_address: u32 = 0;
    let mut current_line: u32 = 1;
    let mut current_column: u32 = 0;

    for &(addr, line, column) in line_mappings {
        if line == 0 {
            continue; // Skip invalid line numbers
        }

        // Set address (extended opcode)
        if addr != current_address {
            if current_address == 0 && addr > 0 {
                // Use DW_LNE_set_address for the first entry
                program.push(0x00); // Extended opcode marker
                encode_uleb128(5, &mut program); // length: 1 (opcode) + 4 (address)
                program.push(DW_LNE_SET_ADDRESS);
                program.extend_from_slice(&addr.to_le_bytes());
            } else {
                // Use DW_LNS_advance_pc for subsequent entries
                let delta = addr.wrapping_sub(current_address);
                program.push(DW_LNS_ADVANCE_PC);
                encode_uleb128(delta as u64, &mut program);
            }
            current_address = addr;
        }

        // Advance line
        if line != current_line {
            let line_delta = line as i64 - current_line as i64;
            program.push(DW_LNS_ADVANCE_LINE);
            encode_sleb128(line_delta, &mut program);
            current_line = line;
        }

        // Set column
        if column != current_column {
            program.push(DW_LNS_SET_COLUMN);
            encode_uleb128(column as u64, &mut program);
            current_column = column;
        }

        // Copy: emit a line table row
        program.push(DW_LNS_COPY);
    }

    // End sequence
    if end_address > current_address {
        program.push(DW_LNS_ADVANCE_PC);
        encode_uleb128((end_address - current_address) as u64, &mut program);
    }
    program.push(0x00); // Extended opcode marker
    encode_uleb128(1, &mut program); // length: 1 (just the opcode)
    program.push(DW_LNE_END_SEQUENCE);

    // Now assemble the full section
    let mut buf = Vec::new();

    // Compute header_length: everything from after header_length to start of program
    let header_fixed = 1 + 1 + 1 + 1 + 1 + 1 + std_opcode_lengths.len();
    let header_length = (header_fixed + header_content.len()) as u32;

    // Compute unit_length: everything after the initial 4-byte length field
    // = version(2) + header_length(4) + header_fixed + header_content + program
    let unit_length = 2 + 4 + header_length as usize + program.len();

    // unit_length (4 bytes, DWARF32)
    buf.extend_from_slice(&(unit_length as u32).to_le_bytes());

    // version: 4
    buf.extend_from_slice(&4u16.to_le_bytes());

    // header_length (4 bytes)
    buf.extend_from_slice(&header_length.to_le_bytes());

    // Fixed header fields
    buf.push(min_instruction_length);
    buf.push(max_ops_per_insn);
    buf.push(default_is_stmt);
    buf.push(line_base as u8);
    buf.push(line_range);
    buf.push(opcode_base);
    buf.extend_from_slice(&std_opcode_lengths);

    // Header content (directory + file tables)
    buf.extend_from_slice(&header_content);

    // Line number program
    buf.extend_from_slice(&program);

    buf
}

// --- WASM binary scanning ---

/// Decode a ULEB128 value from a byte slice, returning (value, bytes_consumed).
pub fn decode_uleb128(bytes: &[u8]) -> (u64, usize) {
    let mut result: u64 = 0;
    let mut shift = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return (result, i + 1);
        }
        shift += 7;
    }
    (result, bytes.len())
}

/// Find the code section in a WASM binary.
/// Returns (code_section_content_offset, function_body_offsets).
/// Each function_body_offset is the offset from the start of the WASM binary
/// to the first byte of the function body (after the body size LEB128).
pub fn find_code_section(wasm: &[u8]) -> Option<CodeSectionInfo> {
    let mut offset = 8; // Skip WASM magic + version

    while offset < wasm.len() {
        let section_id = wasm[offset];
        offset += 1;

        let (section_size, leb_len) = decode_uleb128(&wasm[offset..]);
        offset += leb_len;

        let section_content_start = offset;

        if section_id == 10 {
            // Code section found
            let (num_funcs, func_count_len) = decode_uleb128(&wasm[offset..]);
            offset += func_count_len;

            let mut func_body_offsets = Vec::with_capacity(num_funcs as usize);
            for _ in 0..num_funcs {
                let (body_size, body_size_len) = decode_uleb128(&wasm[offset..]);
                let body_start = offset + body_size_len;
                func_body_offsets.push(body_start);
                offset = body_start + body_size as usize;
            }

            return Some(CodeSectionInfo {
                func_body_offsets,
                section_end: section_content_start + section_size as usize,
            });
        }

        offset += section_size as usize;
    }

    None
}

pub struct CodeSectionInfo {
    /// Offset from start of WASM binary to each function body (after body size LEB128)
    pub func_body_offsets: Vec<usize>,
    /// Offset from start of WASM binary to the end of the code section
    pub section_end: usize,
}

// --- Custom section encoding ---

/// Encode a WASM custom section (section ID 0) and append it to the binary.
pub fn append_custom_section(wasm: &mut Vec<u8>, name: &str, data: &[u8]) {
    let name_len = uleb128_size(name.len() as u64) + name.len();
    let section_size = name_len + data.len();

    wasm.push(0); // Custom section ID
    encode_uleb128(section_size as u64, wasm);
    encode_uleb128(name.len() as u64, wasm);
    wasm.extend_from_slice(name.as_bytes());
    wasm.extend_from_slice(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uleb128_encode_decode_roundtrip() {
        for &val in &[0u64, 1, 127, 128, 255, 624, 16384, 0xFFFFFFFF] {
            let mut buf = Vec::new();
            encode_uleb128(val, &mut buf);
            let (decoded, len) = decode_uleb128(&buf);
            assert_eq!(decoded, val);
            assert_eq!(len, buf.len());
        }
    }

    #[test]
    fn sleb128_encode_basic() {
        let mut buf = Vec::new();
        encode_sleb128(0, &mut buf);
        assert_eq!(buf, [0x00]);

        buf.clear();
        encode_sleb128(-1, &mut buf);
        assert_eq!(buf, [0x7f]);

        buf.clear();
        encode_sleb128(1, &mut buf);
        assert_eq!(buf, [0x01]);
    }

    #[test]
    fn debug_abbrev_is_valid() {
        let abbrev = build_debug_abbrev();
        // Must start with abbrev code 1, tag 0x11, no children
        assert_eq!(abbrev[0], 1); // code
        assert_eq!(abbrev[1], DW_TAG_COMPILE_UNIT);
        assert_eq!(abbrev[2], DW_CHILDREN_NO);
        // Must end with 0x00 (end of table)
        assert_eq!(*abbrev.last().unwrap(), 0x00);
    }

    #[test]
    fn debug_info_has_correct_version() {
        let info = build_debug_info("test.ts", 0, 0, 100);
        // After 4-byte unit_length, version is bytes 4..6
        let version = u16::from_le_bytes([info[4], info[5]]);
        assert_eq!(version, 4);
        // Address size is byte 10
        assert_eq!(info[10], 4);
    }

    #[test]
    fn debug_line_has_correct_structure() {
        let mappings = vec![(10, 1, 1), (20, 5, 5), (30, 10, 3)];
        let line = build_debug_line("test.ts", &mappings, 40);
        // After 4-byte unit_length, version is bytes 4..6
        let version = u16::from_le_bytes([line[4], line[5]]);
        assert_eq!(version, 4);
        // Section should be non-trivial
        assert!(line.len() > 20);
    }
}
