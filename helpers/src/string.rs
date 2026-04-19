/// String header size in bytes (length field).
const HEADER: u32 = 4;

/// Read a string's length field.
#[inline(always)]
unsafe fn str_len(s: u32) -> usize {
    unsafe { (s as *const u32).read() as usize }
}

/// Get a byte slice over a string's content (skipping the 4-byte header).
#[inline(always)]
unsafe fn str_bytes<'a>(s: u32, len: usize) -> &'a [u8] {
    unsafe { core::slice::from_raw_parts((s + HEADER) as *const u8, len) }
}

/// string.slice(start, end) — arena-allocating.
#[unsafe(no_mangle)]
pub extern "C" fn __str_slice(s: u32, start: i32, end: i32) -> u32 {
    unsafe {
        let len = str_len(s) as i32;

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

/// string == string. Returns 1 if equal, 0 otherwise.
#[unsafe(no_mangle)]
pub extern "C" fn __str_eq(a: u32, b: u32) -> i32 {
    unsafe {
        let al = str_len(a);
        let bl = str_len(b);
        if al != bl {
            return 0;
        }
        if str_bytes(a, al) == str_bytes(b, bl) { 1 } else { 0 }
    }
}

/// Lexicographic byte compare. Returns -1, 0, or 1.
#[unsafe(no_mangle)]
pub extern "C" fn __str_cmp(a: u32, b: u32) -> i32 {
    unsafe {
        let al = str_len(a);
        let bl = str_len(b);
        let ap = str_bytes(a, al);
        let bp = str_bytes(b, bl);
        match ap.cmp(bp) {
            core::cmp::Ordering::Less => -1,
            core::cmp::Ordering::Equal => 0,
            core::cmp::Ordering::Greater => 1,
        }
    }
}

/// string.indexOf(needle) -> i32. Byte offset of first match, or -1.
/// Empty needle returns 0 (matches JS).
#[unsafe(no_mangle)]
pub extern "C" fn __str_indexOf(haystack: u32, needle: u32) -> i32 {
    unsafe {
        let hl = str_len(haystack);
        let nl = str_len(needle);
        if nl == 0 {
            return 0;
        }
        if nl > hl {
            return -1;
        }
        let hp = str_bytes(haystack, hl);
        let np = str_bytes(needle, nl);
        let mut i = 0usize;
        while i + nl <= hl {
            if &hp[i..i + nl] == np {
                return i as i32;
            }
            i += 1;
        }
        -1
    }
}

/// string.lastIndexOf(needle) -> i32.
#[unsafe(no_mangle)]
pub extern "C" fn __str_lastIndexOf(haystack: u32, needle: u32) -> i32 {
    unsafe {
        let hl = str_len(haystack);
        let nl = str_len(needle);
        if nl == 0 {
            return hl as i32;
        }
        if nl > hl {
            return -1;
        }
        let hp = str_bytes(haystack, hl);
        let np = str_bytes(needle, nl);
        let mut i = (hl - nl) as isize;
        while i >= 0 {
            if &hp[i as usize..i as usize + nl] == np {
                return i as i32;
            }
            i -= 1;
        }
        -1
    }
}

/// string.includes(needle) -> i32 (0 or 1). Empty needle returns 1.
#[unsafe(no_mangle)]
pub extern "C" fn __str_includes(haystack: u32, needle: u32) -> i32 {
    unsafe {
        let hl = str_len(haystack);
        let nl = str_len(needle);
        if nl == 0 {
            return 1;
        }
        if nl > hl {
            return 0;
        }
        let hp = str_bytes(haystack, hl);
        let np = str_bytes(needle, nl);
        let mut i = 0usize;
        while i + nl <= hl {
            if &hp[i..i + nl] == np {
                return 1;
            }
            i += 1;
        }
        0
    }
}

/// string.startsWith(prefix) -> i32 (0 or 1).
#[unsafe(no_mangle)]
pub extern "C" fn __str_startsWith(s: u32, prefix: u32) -> i32 {
    unsafe {
        let sl = str_len(s);
        let pl = str_len(prefix);
        if pl > sl {
            return 0;
        }
        if str_bytes(s, pl) == str_bytes(prefix, pl) { 1 } else { 0 }
    }
}

/// Number(n).toString() — JS-spec-compliant f64 → shortest decimal string.
/// Backed by ryu-js. The result is allocated in the arena.
#[unsafe(no_mangle)]
pub extern "C" fn __str_from_f64(n: f64) -> u32 {
    let mut buf = ryu_js::Buffer::new();
    alloc_str(buf.format(n).as_bytes())
}

/// Number.prototype.toFixed(digits) — fixed-point format with `digits`
/// fractional digits. Caller passes `digits` in [0, 100] (the language layer
/// would have rejected anything outside that range; we don't re-validate).
///
/// Edge cases match the JS spec: NaN → "NaN", ±Infinity → "Infinity"/"-Infinity",
/// `(-0).toFixed(d)` → "0[.0...]" (no sign for negative zero per spec), and
/// `|n| ≥ 1e21` falls back to shortest-repr per ES § 21.1.3.3 step 7.
///
/// Rounding follows ES § 21.1.3.3 step 6 ("pick larger n on tie", evaluated on
/// the absolute value, then sign re-attached) — i.e. half-away-from-zero. We
/// achieve this even though Rust's `{:.*}` rounds half-to-even by formatting
/// the abs value at extended precision (`digits + 30`), which puts Rust's own
/// rounding ~30 digits past the position we care about — well past f64's
/// ~17-digit precision, so the digits up to position `digits` reflect the
/// f64's exact decimal value. We then walk the bytes and apply half-up
/// (bump on `>= '5'`) with carry propagation manually.
#[unsafe(no_mangle)]
pub extern "C" fn __str_toFixed(n: f64, digits: i32) -> u32 {
    if n.is_nan() {
        return alloc_str(b"NaN");
    }
    if n.is_infinite() {
        return alloc_str(if n > 0.0 { b"Infinity" } else { b"-Infinity" });
    }
    if n.abs() >= 1e21 {
        return __str_from_f64(n);
    }
    let digits = digits.max(0) as usize;
    let abs = if n < 0.0 { -n } else { n };
    let mut out = StackBuf::<512>::new();
    write_to_fixed_into(&mut out, abs, digits, n < 0.0);
    alloc_str(out.as_bytes())
}

/// Format `abs` (must be ≥ 0) with `digits` fractional digits using
/// half-away-from-zero rounding. Result is appended to `out`. `negative`
/// prepends '-' if true (caller decides — `n < 0.0` is the right test, since
/// it's false for `-0.0`, matching the spec rule that `(-0).toFixed(d)` has
/// no sign).
///
/// Shared by `__str_toFixed` and `__str_toPrecision`'s case B/C paths (both of
/// which are toFixed-style formatting at a derived `digits` value).
fn write_to_fixed_into(out: &mut StackBuf<512>, abs: f64, digits: usize, negative: bool) {
    use core::fmt::Write as _;

    // Format at extended precision so {:.*}'s own banker's rounding lands
    // ~30 digits past the boundary we'll round at. Beyond f64's ~17-digit
    // mantissa, the formatter just emits the deterministic exact decimal
    // expansion of the underlying f64 — no further rounding ambiguity.
    let mut tmp = StackBuf::<512>::new();
    let _ = write!(tmp, "{:.*}", digits + 30, abs);
    let bytes = tmp.as_bytes();

    let dot = bytes.iter().position(|&b| b == b'.');
    let (int_part, frac_part) = match dot {
        Some(d) => (&bytes[..d], &bytes[d + 1..]),
        None => (bytes, &[][..]),
    };

    // First dropped char (if any) tells us whether to bump.
    let round_up = digits < frac_part.len() && frac_part[digits] >= b'5';

    // Build the kept digits in a stack buffer (int_part || frac_part[..digits],
    // padded with zeros if frac_part is short — happens only when input was
    // an integer string).
    let mut kept = [0u8; 512];
    let int_len = int_part.len();
    if int_len > kept.len() {
        // Pathological: integer part bigger than our buffer. Fall back to
        // ryu-js's shortest-repr — loses fixed-precision but at least gives
        // a sensible string. (For |n| < 1e21 this can't actually happen.)
        let mut buf = ryu_js::Buffer::new();
        let s = buf.format(if negative { -abs } else { abs });
        for &b in s.as_bytes() {
            out.push_byte(b);
        }
        return;
    }
    kept[..int_len].copy_from_slice(int_part);
    let frac_take = digits.min(frac_part.len());
    let mut total_len = int_len + frac_take;
    if total_len > kept.len() {
        return;
    }
    kept[int_len..total_len].copy_from_slice(&frac_part[..frac_take]);
    while total_len < int_len + digits && total_len < kept.len() {
        kept[total_len] = b'0';
        total_len += 1;
    }

    let extra_one = round_up && carry_round_up(&mut kept[..total_len]);

    if negative {
        out.push_byte(b'-');
    }
    if extra_one {
        out.push_byte(b'1');
    }
    for &b in &kept[..int_len] {
        out.push_byte(b);
    }
    if digits > 0 {
        out.push_byte(b'.');
        for &b in &kept[int_len..int_len + digits] {
            out.push_byte(b);
        }
    }
}

/// Half-up carry propagation on a digit slice, working right to left:
/// `'9' → '0'` with carry, otherwise `+1` and stop. Returns `true` if the
/// carry overflowed past the leftmost digit (caller prepends a `'1'` and, in
/// the exponential case, bumps the exponent).
fn carry_round_up(digits: &mut [u8]) -> bool {
    let mut i = digits.len();
    while i > 0 {
        i -= 1;
        if digits[i] == b'9' {
            digits[i] = b'0';
        } else {
            digits[i] += 1;
            return false;
        }
    }
    true
}

/// Number.prototype.toExponential(digits) — exponential notation with
/// `digits` fractional digits. Caller passes `digits` in [0, 100] (validated
/// by the JS layer; we don't re-check). Output shape:
/// `<sign?><digit>.<frac_digits>e<+|-><exp>`.
///
/// When `digits < 0` (the sentinel the codegen emits for a no-argument call),
/// we return the shortest round-trippable exponential form — per ES
/// § 21.1.3.4 when `fractionDigits` is undefined.
///
/// Rounding follows JS spec: half-away-from-zero on the absolute value.
/// Same extended-precision-then-manual-round trick as `__str_toFixed`. The
/// twist here: when carry overflows past the leading mantissa digit (e.g.
/// `(9.5).toExponential(0)` rounds `9.5` → `10`), we renormalize by emitting
/// a leading `'1'` and bumping the exponent by `+1` so the mantissa stays in
/// `[1, 10)`. We also patch in the explicit `+` before non-negative
/// exponents (Rust's `{:.*e}` omits it; JS requires it).
#[unsafe(no_mangle)]
pub extern "C" fn __str_toExponential(n: f64, digits: i32) -> u32 {
    use core::fmt::Write as _;
    if n.is_nan() {
        return alloc_str(b"NaN");
    }
    if n.is_infinite() {
        return alloc_str(if n > 0.0 { b"Infinity" } else { b"-Infinity" });
    }

    if digits < 0 {
        return to_exponential_shortest(n);
    }

    let digits = digits as usize;
    let abs = if n < 0.0 { -n } else { n };

    // Extended-precision exponential format, so {:.*e}'s rounding lands ~30
    // digits past our target. The mantissa shape is "X.YYYY" (1 leading digit)
    // for nonzero abs and "0.000...0" for abs == 0.
    let mut tmp = StackBuf::<512>::new();
    let _ = write!(tmp, "{:.*e}", digits + 30, abs);
    let bytes = tmp.as_bytes();

    let e_pos = bytes.iter().position(|&b| b == b'e').unwrap_or(bytes.len());
    let mantissa = &bytes[..e_pos];
    let exp_str = bytes.get(e_pos + 1..).unwrap_or(&[]);

    let dot = mantissa.iter().position(|&b| b == b'.');
    let (m_int, m_frac) = match dot {
        Some(d) => (&mantissa[..d], &mantissa[d + 1..]),
        None => (mantissa, &[][..]),
    };

    let mut exp = parse_signed_int(exp_str);

    // Build kept mantissa digits: m_int (always 1 char in exp form) followed
    // by m_frac[..digits], zero-padded if m_frac is short.
    let mut kept = [0u8; 256];
    let int_len = m_int.len();
    if int_len + digits > kept.len() {
        // Pathological — fall back to shortest-repr.
        return __str_from_f64(n);
    }
    kept[..int_len].copy_from_slice(m_int);
    let frac_take = digits.min(m_frac.len());
    kept[int_len..int_len + frac_take].copy_from_slice(&m_frac[..frac_take]);
    let mut total_len = int_len + frac_take;
    while total_len < int_len + digits {
        kept[total_len] = b'0';
        total_len += 1;
    }

    let round_up = digits < m_frac.len() && m_frac[digits] >= b'5';
    // If carry blows past the leading mantissa digit (e.g. "9.99..." → "10"),
    // the mantissa needs renormalizing into "1.00..." with exp += 1. After the
    // carry, every digit in `kept` is '0', so we don't reuse them — we emit a
    // synthetic `'1'` plus `digits` zeros.
    let leading_one = if round_up && carry_round_up(&mut kept[..total_len]) {
        exp += 1;
        true
    } else {
        false
    };

    let mut out = StackBuf::<256>::new();
    if n < 0.0 {
        out.push_byte(b'-');
    }
    if leading_one {
        out.push_byte(b'1');
        if digits > 0 {
            out.push_byte(b'.');
            for _ in 0..digits {
                out.push_byte(b'0');
            }
        }
    } else {
        for &b in &kept[..int_len] {
            out.push_byte(b);
        }
        if digits > 0 {
            out.push_byte(b'.');
            for &b in &kept[int_len..int_len + digits] {
                out.push_byte(b);
            }
        }
    }
    out.push_byte(b'e');
    if exp >= 0 {
        out.push_byte(b'+');
    } else {
        out.push_byte(b'-');
    }
    let abs_exp = if exp < 0 { -exp } else { exp };
    let _ = write!(out, "{}", abs_exp);

    alloc_str(out.as_bytes())
}

/// Shortest-repr exponential form — the result of `Number.prototype
/// .toExponential()` with no argument per ES § 21.1.3.4 (shortest digits
/// that round-trip to the same f64). We lean on ryu-js for the shortest
/// decimal and reformat it into mantissa-and-exponent shape.
fn to_exponential_shortest(n: f64) -> u32 {
    use core::fmt::Write as _;

    let mut buf = ryu_js::Buffer::new();
    let s = buf.format(n).as_bytes();

    // Sign. ryu-js emits "0" (no sign) for ±0, so `negative` is reliably
    // false when the numeric value is zero.
    let (negative, body) = match s.first() {
        Some(&b'-') => (true, &s[1..]),
        _ => (false, s),
    };

    // Zero: "0e+0" always (spec says no sign even for -0, matching ryu-js).
    if body == b"0" {
        return alloc_str(b"0e+0");
    }

    // Split at 'e' if present. After this, `mantissa` is one of:
    //   "D", "D.D+", "0.D+"   (no 'e'); or "D" / "D.D+" (with separate exp).
    let (mantissa, exp_str) = match body.iter().position(|&b| b == b'e') {
        Some(i) => (&body[..i], &body[i + 1..]),
        None => (body, &[][..]),
    };
    let existing_exp = parse_signed_int(exp_str);

    // Collapse the decimal point, remembering its position so we can place
    // the first significant digit on the correct 10^k power.
    let dot_pos = mantissa
        .iter()
        .position(|&b| b == b'.')
        .unwrap_or(mantissa.len());
    let mut all = [0u8; 32]; // f64 shortest-repr is ≤ 17 digits + header; 32 covers it.
    let mut n_all = 0usize;
    for &b in mantissa {
        if b != b'.' && n_all < all.len() {
            all[n_all] = b;
            n_all += 1;
        }
    }

    // First nonzero in `all` is the mantissa's leading digit. (`body == "0"`
    // was handled above, so a nonzero must exist.)
    let first = all[..n_all]
        .iter()
        .position(|&b| b != b'0')
        .unwrap_or(n_all);

    // The digit at `all[first]` sits at 10^(dot_pos - 1 - first + existing_exp).
    let exp = dot_pos as i32 - 1 - first as i32 + existing_exp;

    // Drop trailing zeros — ryu already minimized them for fixed output, but
    // for inputs like `1.0e+21` we may have "1" + "e21", already minimal.
    let mut end = n_all;
    while end > first + 1 && all[end - 1] == b'0' {
        end -= 1;
    }
    let digits = &all[first..end];

    let mut out = StackBuf::<64>::new();
    if negative {
        out.push_byte(b'-');
    }
    out.push_byte(digits[0]);
    if digits.len() > 1 {
        out.push_byte(b'.');
        for &d in &digits[1..] {
            out.push_byte(d);
        }
    }
    out.push_byte(b'e');
    if exp >= 0 {
        out.push_byte(b'+');
    } else {
        out.push_byte(b'-');
    }
    let abs_exp = if exp < 0 { -exp } else { exp };
    let _ = write!(out, "{}", abs_exp);

    alloc_str(out.as_bytes())
}

/// Parse a decimal integer (with optional leading `+`/`-`) from the start of
/// `bytes`, stopping at the first non-digit. Used to lift the exponent out of
/// `{:.*e}`-formatted output.
fn parse_signed_int(bytes: &[u8]) -> i32 {
    let mut i = 0usize;
    let mut sign = 1i32;
    if i < bytes.len() && bytes[i] == b'-' {
        sign = -1;
        i += 1;
    } else if i < bytes.len() && bytes[i] == b'+' {
        i += 1;
    }
    let mut val = 0i32;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        val = val * 10 + (bytes[i] - b'0') as i32;
        i += 1;
    }
    val * sign
}

/// Number.prototype.toPrecision(precision) — format `n` to `precision`
/// significant digits per ES § 21.1.3.5. Caller passes `precision` in
/// [1, 100] (validated by the JS layer; we don't re-check).
///
/// The spec picks a `p`-digit integer `n` and exponent `e` such that
/// `n × 10^(e - p + 1)` is the closest approximation of `x`, with
/// half-away-from-zero on ties. Then:
///   * if `e < -6` or `e ≥ p`, emit exponential form `d.dde±ee`;
///   * else if `e ≥ 0`, emit fixed form with `e + 1` int digits;
///   * else emit `"0." + "0"×(-e-1) + digits`.
///
/// We reuse the `{:.*e}`-at-extended-precision + manual half-up carry
/// technique from `__str_toExponential`; after the carry dance we dispatch
/// on the resulting `exp`.
#[unsafe(no_mangle)]
pub extern "C" fn __str_toPrecision(n: f64, precision: i32) -> u32 {
    use core::fmt::Write as _;
    if n.is_nan() {
        return alloc_str(b"NaN");
    }
    if n.is_infinite() {
        return alloc_str(if n > 0.0 { b"Infinity" } else { b"-Infinity" });
    }

    let p = precision.max(1) as usize;

    if n == 0.0 {
        // "0" if p == 1, else "0." followed by (p - 1) zeros.
        if p == 1 {
            return alloc_str(b"0");
        }
        let mut buf = StackBuf::<512>::new();
        let _ = buf.write_str("0.");
        for _ in 0..(p - 1) {
            let _ = buf.write_str("0");
        }
        return alloc_str(buf.as_bytes());
    }

    let negative = n < 0.0;
    let abs = if negative { -n } else { n };

    // Extended-precision exponential format: (p - 1) + 30 fractional digits
    // puts {:.*e}'s own rounding ~30 positions past where we round, so the
    // digits up to position (p - 1) are the f64's exact decimal expansion.
    let mut tmp = StackBuf::<512>::new();
    let _ = write!(tmp, "{:.*e}", (p - 1) + 30, abs);
    let bytes = tmp.as_bytes();

    let e_pos = bytes.iter().position(|&b| b == b'e').unwrap_or(bytes.len());
    let mantissa = &bytes[..e_pos];
    let exp_str = bytes.get(e_pos + 1..).unwrap_or(&[]);

    let dot = mantissa.iter().position(|&b| b == b'.');
    let (m_int, m_frac) = match dot {
        Some(d) => (&mantissa[..d], &mantissa[d + 1..]),
        None => (mantissa, &[][..]),
    };

    let mut exp = parse_signed_int(exp_str);

    // Keep p significant digits: m_int (always 1 char for nonzero abs in exp
    // form) followed by m_frac[..p - 1], zero-padded if m_frac is short.
    let mut kept = [0u8; 128];
    let int_len = m_int.len();
    if int_len > p || int_len + (p - int_len) > kept.len() {
        return __str_from_f64(n);
    }
    kept[..int_len].copy_from_slice(m_int);
    let want = p - int_len;
    let take = want.min(m_frac.len());
    kept[int_len..int_len + take].copy_from_slice(&m_frac[..take]);
    let mut total = int_len + take;
    while total < p {
        kept[total] = b'0';
        total += 1;
    }

    // Round-up decision from the first dropped digit. If carry overflows past
    // the leading digit (e.g. "999" rounds up to "1000"), the mantissa
    // renormalizes to "1" followed by p-1 zeros, with exp += 1.
    let round_up = want < m_frac.len() && m_frac[want] >= b'5';
    let leading_one = if round_up && carry_round_up(&mut kept[..p]) {
        exp += 1;
        true
    } else {
        false
    };

    let mut digits = [0u8; 128];
    if leading_one {
        digits[0] = b'1';
        for slot in digits.iter_mut().take(p).skip(1) {
            *slot = b'0';
        }
    } else {
        digits[..p].copy_from_slice(&kept[..p]);
    }

    let mut out = StackBuf::<512>::new();
    if negative {
        out.push_byte(b'-');
    }
    let p_i32 = p as i32;
    if exp < -6 || exp >= p_i32 {
        // Exponential form: d[0].d[1..p]e{sign}{|exp|}
        out.push_byte(digits[0]);
        if p > 1 {
            out.push_byte(b'.');
            for &b in &digits[1..p] {
                out.push_byte(b);
            }
        }
        out.push_byte(b'e');
        if exp >= 0 {
            out.push_byte(b'+');
        } else {
            out.push_byte(b'-');
        }
        let abs_exp = if exp < 0 { -exp } else { exp };
        let _ = write!(out, "{}", abs_exp);
    } else if exp >= 0 {
        // Fixed form with (exp + 1) int digits.
        let e_usize = exp as usize;
        for &b in &digits[..=e_usize] {
            out.push_byte(b);
        }
        if e_usize + 1 < p {
            out.push_byte(b'.');
            for &b in &digits[e_usize + 1..p] {
                out.push_byte(b);
            }
        }
    } else {
        // -6 ≤ exp < 0: "0." + "0"×(-exp - 1) + digits.
        out.push_byte(b'0');
        out.push_byte(b'.');
        for _ in 0..(-exp - 1) {
            out.push_byte(b'0');
        }
        for &b in &digits[..p] {
            out.push_byte(b);
        }
    }

    alloc_str(out.as_bytes())
}

/// Write `bytes` to a fresh arena string; return the [len: u32][bytes...]
/// pointer. Used by the f64-formatting helpers that hand off a borrowed
/// byte slice from a stack buffer or a third-party formatter.
fn alloc_str(bytes: &[u8]) -> u32 {
    let len = bytes.len() as u32;
    let total = HEADER + len;
    unsafe {
        let ptr = crate::arena::alloc(total);
        (ptr as *mut u32).write(len);
        let dst = (ptr as *mut u8).add(HEADER as usize);
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        ptr
    }
}

/// Stack-allocated `core::fmt::Write` sink, so format macros work in `no_std`
/// without `alloc`. Silently truncates on overflow — callers size N for the
/// worst case so this should never fire in practice; on overflow `write!`
/// returns `Err` and the caller falls back to whatever bytes did fit.
struct StackBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackBuf<N> {
    fn new() -> Self {
        Self { buf: [0; N], len: 0 }
    }
    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
    /// Append one byte. Silently no-ops on overflow (callers size N for the
    /// worst case).
    fn push_byte(&mut self, b: u8) {
        if self.len < N {
            self.buf[self.len] = b;
            self.len += 1;
        }
    }
}

impl<const N: usize> core::fmt::Write for StackBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let take = bytes.len().min(N - self.len);
        self.buf[self.len..self.len + take].copy_from_slice(&bytes[..take]);
        self.len += take;
        if take < bytes.len() {
            Err(core::fmt::Error)
        } else {
            Ok(())
        }
    }
}

/// parseFloat(s) — correctly-rounded decimal string → f64. Follows JS's
/// leading-whitespace trim; otherwise delegates to Rust's `f64::from_str`
/// (IEEE 754-correct rounding via Eisel-Lemire lookup tables in Data).
/// Returns NaN on unparseable input.
#[unsafe(no_mangle)]
pub extern "C" fn __str_parseFloat(s: u32) -> f64 {
    unsafe {
        let len = str_len(s);
        let bytes = str_bytes(s, len);
        match core::str::from_utf8(bytes) {
            Ok(text) => {
                let trimmed = text.trim_start();
                // JS parseFloat scans the longest valid numeric prefix; Rust
                // `from_str` is strict. Implement the prefix scan manually.
                let prefix_end = numeric_prefix_end(trimmed);
                if prefix_end == 0 {
                    return f64::NAN;
                }
                trimmed[..prefix_end].parse::<f64>().unwrap_or(f64::NAN)
            }
            Err(_) => f64::NAN,
        }
    }
}

/// Return the byte length of the longest valid JS numeric prefix of `s`:
/// `[+-]? (digits (. digits?)? | . digits) ([eE][+-]?digits)?`, plus
/// `Infinity`. Partial matches are OK — JS parseFloat accepts "12abc" as 12.
fn numeric_prefix_end(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    // "Infinity"
    if bytes[i..].starts_with(b"Infinity") {
        return i + 8;
    }
    let mut saw_digit = false;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        saw_digit = true;
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            saw_digit = true;
            i += 1;
        }
    }
    if !saw_digit {
        return 0;
    }
    // Optional exponent: only consumed if followed by at least one digit.
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        let exp_digits_start = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > exp_digits_start {
            i = j;
        }
    }
    i
}

/// string.endsWith(suffix) -> i32 (0 or 1).
#[unsafe(no_mangle)]
pub extern "C" fn __str_endsWith(s: u32, suffix: u32) -> i32 {
    unsafe {
        let sl = str_len(s);
        let sufl = str_len(suffix);
        if sufl > sl {
            return 0;
        }
        let offset = sl - sufl;
        let sp = core::slice::from_raw_parts((s + HEADER + offset as u32) as *const u8, sufl);
        let sufp = str_bytes(suffix, sufl);
        if sp == sufp { 1 } else { 0 }
    }
}
