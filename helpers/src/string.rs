/// String header size in bytes (length field).
const HEADER: u32 = 4;

/// string.slice(start, end) — arena-allocating, accesses global 0 directly.
#[unsafe(no_mangle)]
pub extern "C" fn __str_slice(s: u32, start: i32, end: i32) -> u32 {
    unsafe {
        let len = (s as *const u32).read() as i32;

        // Clamp start (negative → len + start, cap at [0, len])
        let mut st = start;
        if st < 0 {
            st += len;
            if st < 0 {
                st = 0;
            }
        }
        if st > len {
            st = len;
        }

        // Clamp end (negative → len + end, cap at [0, len])
        let mut en = end;
        if en < 0 {
            en += len;
            if en < 0 {
                en = 0;
            }
        }
        if en > len {
            en = len;
        }

        let new_len = if en > st { (en - st) as u32 } else { 0 };
        let total = HEADER + new_len;

        let ptr = crate::arena::alloc(total);
        (ptr as *mut u32).write(new_len);

        let src = (s as *const u8).add(HEADER as usize + st as usize);
        let dst = (ptr as *mut u8).add(HEADER as usize);
        core::ptr::copy_nonoverlapping(src, dst, new_len as usize);

        ptr
    }
}

/// string.lastIndexOf(searchString) -> i32
/// Returns byte offset of the LAST occurrence of needle in haystack, or -1.
/// String layout: [len: u32 (4 bytes)][bytes...]
#[unsafe(no_mangle)]
pub extern "C" fn __str_lastIndexOf(haystack: u32, needle: u32) -> i32 {
    unsafe {
        let h_ptr = haystack as *const u8;
        let n_ptr = needle as *const u8;
        let h_len = (h_ptr as *const u32).read() as usize;
        let n_len = (n_ptr as *const u32).read() as usize;

        if n_len == 0 {
            return h_len as i32;
        }
        if n_len > h_len {
            return -1;
        }

        let h_data = h_ptr.add(HEADER as usize);
        let n_data = n_ptr.add(HEADER as usize);

        let mut i = (h_len - n_len) as isize;
        while i >= 0 {
            let mut j = 0usize;
            while j < n_len {
                if *h_data.offset(i).add(j) != *n_data.add(j) {
                    break;
                }
                j += 1;
            }
            if j == n_len {
                return i as i32;
            }
            i -= 1;
        }
        -1
    }
}
