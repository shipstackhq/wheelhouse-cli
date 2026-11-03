//! Canonical JSON: byte-for-byte what `canonicalJson` in `daemon/src/crypto.mjs` produces.
//!
//! Keys sorted at every level (by UTF-16 code units, as JS `Array.prototype.sort` does), no spaces, strings escaped as
//! `JSON.stringify` does, numbers formatted as `Number.prototype.toString` does.
//!
//! `undefined` cannot exist in a parsed `serde_json::Value`: a JS object key holding `undefined` is simply absent on the wire, so
//! the Rust side never sees it. (The fixtures cover this: the generator builds objects with `undefined` values and the expected
//! bytes have no such key; the Rust input has the key missing.)

use serde_json::Value;

pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write(v, &mut out);
    out
}

fn write(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        // JS parses every number as a double, so a Rust integer past 2^53 must round the same way.
        Value::Number(n) => out.push_str(&js_number(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => write_string(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(x, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write(&m[k.as_str()], out);
            }
            out.push('}');
        }
    }
}

/// `JSON.stringify` for a string. (Lone surrogates, which JS escapes as `\udXXX`, cannot occur in a Rust `str`.)
pub fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// ECMAScript `Number::toString(10)` for a finite double (`-0` is `0`).
pub fn js_number(f: f64) -> String {
    if f == 0.0 || !f.is_finite() {
        return "0".into(); // JSON.stringify prints non-finite numbers as null, but JSON text cannot hold them
    }
    let neg = f < 0.0;
    // Rust's `{:e}` gives the shortest digits that round-trip, like JS: "1.2345e-7", "5e0".
    let sci = format!("{:e}", f.abs());
    let (mant, exp) = sci.split_once('e').expect("exponent");
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp.parse::<i32>().expect("exponent number") + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let sign = if e < 0 { '-' } else { '+' };
        if k == 1 {
            format!("{digits}e{sign}{}", e.abs())
        } else {
            format!("{}.{}e{sign}{}", &digits[..1], &digits[1..], e.abs())
        }
    };
    if neg {
        format!("-{body}")
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_like_js() {
        for (f, s) in [
            (1.0, "1"),
            (-0.0, "0"),
            (0.5, "0.5"),
            (100.0, "100"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (1.5e-7, "1.5e-7"),
            (0.000001, "0.000001"),
            (123456789.125, "123456789.125"),
            (-2.5e25, "-2.5e+25"),
            (0.1 + 0.2, "0.30000000000000004"),
        ] {
            assert_eq!(js_number(f), s);
        }
    }

    #[test]
    fn keys_sorted_by_utf16_and_strings_escaped() {
        let v = json!({"b": 1, "a": {"z": [1, null], "y": "\u{1}\"\\\n"}, "\u{10000}": 1, "\u{ffff}": 2});
        assert_eq!(
            canonical_json(&v),
            "{\"a\":{\"y\":\"\\u0001\\\"\\\\\\n\",\"z\":[1,null]},\"b\":1,\"\u{10000}\":1,\"\u{ffff}\":2}"
        );
    }
}
