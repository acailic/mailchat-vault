//! Structured Telegram `result.json` generator: emits raw JSON text (not
//! a `serde_json::Value`) so duplicate keys, malformed numbers, lone
//! escapes, and truncation-prone strings are all expressible, with a
//! controlled rate of damage on top of well-formed exports.

use super::rng::Rng;

/// Cap on nested `reply_to_message` recursion in generated entries, so
/// the generator itself can never recurse without bound.
const MAX_ENTRY_DEPTH: u8 = 6;

/// One generated export (raw JSON bytes).
pub fn export(rng: &mut Rng) -> Vec<u8> {
    let mut s = String::new();
    if rng.bool() {
        s.push('\u{feff}');
    }
    s.push('{');
    let mut first = true;
    if rng.below(4) > 0 {
        comma(&mut s, &mut first);
        s.push_str(&format!("\"name\":{}", json_string(rng, "Arhiva")));
        if rng.below(20) == 0 {
            s.push_str(&format!(",\"name\":{}", json_string(rng, "Renamed")));
        }
    }
    if rng.below(4) > 0 {
        comma(&mut s, &mut first);
        s.push_str(&format!("\"id\":{}", id_value(rng)));
    }
    if rng.below(4) > 0 {
        comma(&mut s, &mut first);
        s.push_str("\"messages\":");
        if rng.below(30) == 0 {
            // Broken container instead of the array.
            s.push_str(rng.pick(&["null", "{}", "\"x\"", "5"]));
        } else {
            write_messages(rng, &mut s);
        }
    }
    if rng.below(15) == 0 {
        comma(&mut s, &mut first);
        s.push_str(&format!("\"type\":{}", json_string(rng, "private_group")));
    }
    s.push('}');
    if rng.below(25) == 0 {
        s.push_str(rng.pick(&[" trailing", "\x00", " {}", " null"]));
    }
    let mut bytes = s.into_bytes();
    match rng.below(20) {
        0 => {
            // Truncate the document at a random byte.
            let at = rng.below(bytes.len() + 1);
            bytes.truncate(at);
        }
        1 if !bytes.is_empty() => {
            // Break a random byte (often invalid UTF-8/JSON).
            let i = rng.below(bytes.len());
            bytes[i] = *rng.pick(&[0xff, 0x00, b'"', b'\\', b'[', b'}', b':']);
        }
        _ => {}
    }
    bytes
}

fn comma(s: &mut String, first: &mut bool) {
    if !*first {
        s.push(',');
    }
    *first = false;
}

fn write_messages(rng: &mut Rng, s: &mut String) {
    s.push('[');
    let n = rng.range(0, 12);
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        write_entry(rng, s, 0);
    }
    s.push(']');
}

fn write_entry(rng: &mut Rng, s: &mut String, depth: u8) {
    match rng.below(16) {
        0 => s.push_str(rng.pick(&["5", "\"str\"", "null", "true", "[]", "{}"])),
        _ => {
            s.push('{');
            let mut first = true;
            comma(s, &mut first);
            match rng.below(8) {
                0 => {} // no type at all: malformed entry
                1 => s.push_str(&format!("\"type\":{}", id_value(rng))),
                _ => {
                    let t = *rng.pick(&["message", "service", "call", ""]);
                    s.push_str(&format!("\"type\":{}", json_string(rng, t)));
                }
            }
            if rng.below(30) == 0 {
                s.push_str(",\"type\":\"message\""); // duplicate key, last wins
            }
            if rng.below(6) > 0 {
                comma(s, &mut first);
                s.push_str(&format!("\"id\":{}", id_value(rng)));
            }
            if rng.bool() {
                comma(s, &mut first);
                s.push_str(&format!("\"from\":{}", json_string(rng, "Marko")));
            }
            if rng.below(6) == 0 {
                comma(s, &mut first);
                s.push_str(&format!("\"from_id\":{}", json_string(rng, "user#7")));
            }
            let u = unixtime(rng);
            match rng.below(3) {
                0 => {
                    comma(s, &mut first);
                    s.push_str(&format!(
                        "\"date\":{}",
                        json_string(rng, "2026-11-01T10:00:00")
                    ));
                }
                1 => {
                    comma(s, &mut first);
                    s.push_str(&format!("\"date_unixtime\":{}", json_string(rng, u)));
                }
                _ => {
                    comma(s, &mut first);
                    s.push_str(&format!("\"date\":{}", json_string(rng, "garbage-date")));
                    comma(s, &mut first);
                    s.push_str(&format!("\"date_unixtime\":{}", json_string(rng, u)));
                }
            }
            if rng.bool() {
                comma(s, &mut first);
                s.push_str("\"text\":");
                write_text(rng, s);
            }
            if depth < MAX_ENTRY_DEPTH && rng.below(4) == 0 {
                comma(s, &mut first);
                s.push_str("\"reply_to_message\":");
                write_entry(rng, s, depth + 1); // nested original message
            }
            s.push('}');
        }
    }
}

fn write_text(rng: &mut Rng, s: &mut String) {
    match rng.below(6) {
        0 => s.push_str(&json_string(rng, "plain text")),
        1 => s.push_str("null"),
        2 => s.push('['), // unterminated array: deliberate damage
        3 => s.push_str(&format!("\"{}\"", "t".repeat(rng.range(0, 300_000)))),
        4 => s.push_str("[\"a\"]"),
        _ => {
            // Array of strings and entity objects, sometimes damaged.
            s.push('[');
            let n = rng.range(0, 6);
            for i in 0..n {
                if i > 0 {
                    s.push(',');
                }
                match rng.below(5) {
                    0 => s.push_str(&json_string(rng, "part ")),
                    1 => {
                        s.push_str("{\"type\":\"bold\",\"text\":");
                        write_text(rng, s);
                        if rng.below(10) > 0 {
                            s.push('}'); // sometimes the close brace is missing
                        }
                    }
                    2 => s.push_str("{\"text\":null}"),
                    3 => s.push('5'),
                    _ => s.push_str("null"),
                }
            }
            if rng.below(12) == 0 {
                s.push(','); // trailing comma: invalid JSON
            }
            s.push(']');
        }
    }
}

fn unixtime(rng: &mut Rng) -> &'static str {
    rng.pick(&[
        "1793546400",
        "0",
        "-1",
        "9223372036854775807",
        "-9223372036854775808",
        "99999999999999999999",
        "not-a-number",
        "",
        "1793546400+02:00",
        " 42",
    ])
}

fn id_value(rng: &mut Rng) -> String {
    rng.pick(&[
        "1",
        "0",
        "-5",
        "1.5",
        "\"7\"",
        "9223372036854775807",
        "1e999",
        "null",
        "true",
    ])
    .to_string()
}

fn json_string(rng: &mut Rng, value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    match rng.below(20) {
        0 => out.push_str("\\ud800"), // lone high surrogate
        1 => out.push_str("\\udc00"), // lone low surrogate
        2 => out.push_str("\\q"),     // invalid escape
        3 => {}                       // unterminated string: no closing quote
        _ => out.push('"'),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_deterministic_variety() {
        let a = export(&mut Rng::new(1));
        let b = export(&mut Rng::new(1));
        assert_eq!(a, b);
        assert_ne!(a, export(&mut Rng::new(2)));
    }

    #[test]
    fn bounded_size() {
        for seed in 0..500u64 {
            let bytes = export(&mut Rng::new(seed));
            assert!(
                bytes.len() < 5_000_000,
                "seed {seed}: {} bytes",
                bytes.len()
            );
        }
    }
}
