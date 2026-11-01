//! Number and string conversions from the ECMAScript specification.

/// Number::toString, using Rust's shortest round-trip digits.
pub(super) fn number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x.fract() == 0.0 && x.abs() < 1e15 {
        return format!("{}", x as i64);
    }
    let text = format!("{:e}", x.abs());
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let n = exponent.parse::<i32>().unwrap_or(0) + 1;
    let k = digits.len() as i32;
    let sign = if x < 0.0 { "-" } else { "" };
    if k <= n && n <= 21 {
        format!("{sign}{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        let (head, tail) = digits.split_at(n as usize);
        format!("{sign}{head}.{tail}")
    } else if -6 < n && n <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let e_sign = if e < 0 { '-' } else { '+' };
        let (head, tail) = digits.split_at(1);
        let dot = if tail.is_empty() { "" } else { "." };
        format!("{sign}{head}{dot}{tail}e{e_sign}{}", e.abs())
    }
}

fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// ToNumber applied to a string.
pub(super) fn string_to_number(s: &str) -> f64 {
    let t = s.trim_matches(is_js_space);
    if t.is_empty() {
        return 0.0;
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return parse_radix(hex, 16)
            .filter(|(_, n)| *n == hex.len())
            .map_or(f64::NAN, |(v, _)| v);
    }
    let plain = t
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'));
    match t {
        "Infinity" | "+Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        // Rust also accepts "inf" and "nan", which ECMAScript does not.
        _ if plain => t.parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

/// Digits of `s` in `radix` from the start: the value and how many were read.
pub(super) fn parse_radix(s: &str, radix: u32) -> Option<(f64, usize)> {
    let mut value = 0.0;
    let mut count = 0;
    for c in s.chars() {
        let Some(d) = c.to_digit(radix) else { break };
        value = value * f64::from(radix) + f64::from(d);
        count += 1;
    }
    (count > 0).then_some((value, count))
}

pub(super) fn parse_int(s: &str, radix: f64) -> f64 {
    let t = s.trim_start_matches(is_js_space);
    let (negative, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let mut radix = to_int32(radix);
    if radix != 0 && !(2..=36).contains(&radix) {
        return f64::NAN;
    }
    let hex = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"));
    let digits = match hex {
        Some(rest) if radix == 0 || radix == 16 => {
            radix = 16;
            rest
        }
        _ => t,
    };
    if radix == 0 {
        // JScript keeps ES3's leading zero octal.
        radix = if digits.starts_with('0') { 8 } else { 10 };
    }
    match parse_radix(digits, radix as u32) {
        Some((value, _)) if negative => -value,
        Some((value, _)) => value,
        None => f64::NAN,
    }
}

pub(super) fn parse_float(s: &str) -> f64 {
    let t = s.trim_start_matches(is_js_space);
    for (name, value) in [
        ("Infinity", f64::INFINITY),
        ("+Infinity", f64::INFINITY),
        ("-Infinity", f64::NEG_INFINITY),
    ] {
        if t.starts_with(name) {
            return value;
        }
    }
    let b = t.as_bytes();
    let digits_from = |mut i: usize| {
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        i
    };
    let whole = usize::from(matches!(b.first(), Some(b'+' | b'-')));
    let mut i = digits_from(whole);
    let mut any = i > whole;
    if b.get(i) == Some(&b'.') {
        let end = digits_from(i + 1);
        any |= end > i + 1;
        i = end;
    }
    if !any {
        return f64::NAN;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let sign = i + 1 + usize::from(matches!(b.get(i + 1), Some(b'+' | b'-')));
        let end = digits_from(sign);
        if end > sign {
            i = end;
        }
    }
    t.get(..i).and_then(|p| p.parse().ok()).unwrap_or(f64::NAN)
}

pub(super) fn to_int32(x: f64) -> i32 {
    let fast = x as i32;
    if f64::from(fast) == x {
        return fast;
    }
    if !x.is_finite() {
        return 0;
    }
    x.trunc().rem_euclid(4_294_967_296.0) as u32 as i32
}

pub(super) fn to_uint32(x: f64) -> u32 {
    to_int32(x) as u32
}

/// ToInteger, saturated to a range that is safe to do index arithmetic in.
pub(super) fn to_integer(x: f64) -> i64 {
    if x.is_nan() {
        return 0;
    }
    x.clamp(-9e15, 9e15) as i64
}

/// A relative slice bound: negative counts from the end.
pub(super) fn relative(x: f64, len: usize) -> usize {
    let n = to_integer(x);
    let len = len as i64;
    (if n < 0 { (len + n).max(0) } else { n.min(len) }) as usize
}

pub(super) fn to_fixed(x: f64, digits: usize) -> String {
    if !x.is_finite() || x.abs() >= 1e21 {
        return number_to_string(x);
    }
    // Every f64 has a finite decimal expansion, so this text is exact and a
    // tie can round up as the specification asks; `{:.N}` rounds to even.
    let text = format!("{:.1080}", x.abs());
    let dot = text.find('.').unwrap_or(text.len());
    let cut = dot + 1 + digits;
    let mut out: Vec<u8> = text.bytes().take(cut).collect();
    if text.as_bytes().get(cut).is_some_and(|b| *b >= b'5') {
        let mut i = out.len();
        loop {
            if i == 0 {
                out.insert(0, b'1');
                break;
            }
            i -= 1;
            match out.get_mut(i) {
                Some(b'.') => {}
                Some(d @ b'9') => *d = b'0',
                Some(d) => {
                    *d += 1;
                    break;
                }
                None => break,
            }
        }
    }
    if out.last() == Some(&b'.') {
        out.pop();
    }
    let body = String::from_utf8(out).unwrap_or_default();
    if x < 0.0 { format!("-{body}") } else { body }
}

pub(super) fn to_exponential(x: f64, digits: usize) -> String {
    let text = format!("{x:.digits$e}");
    match text.split_once('e') {
        Some((m, e)) if e.starts_with('-') => format!("{m}e{e}"),
        Some((m, e)) => format!("{m}e+{e}"),
        None => text,
    }
}

pub(super) fn to_precision(x: f64, precision: usize) -> String {
    if !x.is_finite() {
        return number_to_string(x);
    }
    let digits = precision.saturating_sub(1);
    let text = format!("{x:.digits$e}");
    let e = match text.split_once('e') {
        Some((_, e)) => e.parse::<i32>().unwrap_or(0),
        None => 0,
    };
    if e < -6 || e >= precision as i32 {
        return to_exponential(x, digits);
    }
    to_fixed(x, (precision as i32 - 1 - e).max(0) as usize)
}

pub(super) fn to_radix(x: f64, radix: u32) -> String {
    if !x.is_finite() {
        return number_to_string(x);
    }
    let base = f64::from(radix);
    let mut whole = x.abs().trunc();
    let mut frac = x.abs().fract();
    let mut digits = Vec::new();
    while whole >= 1.0 {
        digits.push(char::from_digit((whole % base) as u32, radix).unwrap_or('0'));
        whole = (whole / base).trunc();
    }
    if digits.is_empty() {
        digits.push('0');
    }
    if x < 0.0 {
        digits.push('-');
    }
    let mut out: String = digits.iter().rev().collect();
    if frac > 0.0 {
        out.push('.');
        for _ in 0..52 {
            frac *= base;
            let d = frac.trunc();
            out.push(char::from_digit(d as u32, radix).unwrap_or('0'));
            frac -= d;
            if frac == 0.0 {
                break;
            }
        }
    }
    out
}
