//! Structured Discord Data Package channel-file generator: emits raw
//! JSON text (not a `serde_json::Value`) so duplicate keys, malformed
//! numbers, lone escapes, and truncation-prone strings are all
//! expressible, with a controlled rate of damage on top of well-formed
//! exports.

use super::rng::Rng;

/// One generated channel file (raw JSON bytes).
pub fn export(rng: &mut Rng) -> Vec<u8> {
    let mut s = String::new();
    if rng.bool() {
        s.push('\u{feff}');
    }
    s.push('{');
    let mut first = true;
    if rng.below(4) > 0 {
        comma(&mut s, &mut first);
        s.push_str("\"channel\":");
        write_container(rng, &mut s, "channel");
    }
    if rng.below(3) == 0 {
        comma(&mut s, &mut first);
        s.push_str("\"guild\":");
        write_container(rng, &mut s, "guild");
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
    if rng.below(20) == 0 {
        comma(&mut s, &mut first);
        // A Telegram-shaped member mixed in: detection must not be
        // confused by it.
        s.push_str(&format!("\"name\":{}", json_string(rng, "Arhiva")));
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

/// The `channel`/`guild` objects, sometimes malformed.
fn write_container(rng: &mut Rng, s: &mut String, kind: &str) {
    match rng.below(10) {
        0 => s.push_str(rng.pick(&["null", "5", "\"x\"", "[]"])),
        _ => {
            s.push('{');
            let mut first = true;
            if rng.below(6) > 0 {
                comma(s, &mut first);
                s.push_str(&format!("\"id\":{}", id_value(rng)));
            }
            if rng.below(6) > 0 {
                comma(s, &mut first);
                let name = if kind == "guild" {
                    "Ekipa HQ"
                } else {
                    "general"
                };
                s.push_str(&format!("\"name\":{}", json_string(rng, name)));
            }
            s.push('}');
        }
    }
}

fn write_messages(rng: &mut Rng, s: &mut String) {
    s.push('[');
    let n = rng.range(0, 12);
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        write_entry(rng, s);
    }
    s.push(']');
}

fn write_entry(rng: &mut Rng, s: &mut String) {
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
                    let t = *rng.pick(&[
                        "Default",
                        "Reply",
                        "GuildMemberJoin",
                        "ChannelPinnedMessage",
                        "ChatInputCommand",
                        "Call",
                        "",
                    ]);
                    s.push_str(&format!("\"type\":{}", json_string(rng, t)));
                }
            }
            if rng.below(30) == 0 {
                s.push_str(",\"type\":\"Default\""); // duplicate key, last wins
            }
            if rng.below(6) > 0 {
                comma(s, &mut first);
                s.push_str(&format!("\"id\":{}", id_value(rng)));
            }
            if rng.bool() {
                comma(s, &mut first);
                s.push_str("\"author\":");
                write_author(rng, s);
            }
            if rng.below(6) > 0 {
                comma(s, &mut first);
                let ts = timestamp(rng);
                s.push_str(&format!("\"timestamp\":{}", json_string(rng, ts)));
            }
            if rng.bool() {
                comma(s, &mut first);
                s.push_str("\"content\":");
                write_content(rng, s);
            }
            if rng.below(5) == 0 {
                comma(s, &mut first);
                s.push_str("\"reactions\":");
                write_jagged_array(rng, s);
            }
            if rng.below(5) == 0 {
                comma(s, &mut first);
                s.push_str("\"attachments\":");
                write_jagged_array(rng, s);
            }
            s.push('}');
        }
    }
}

/// Author objects: full, partial, or malformed.
fn write_author(rng: &mut Rng, s: &mut String) {
    match rng.below(12) {
        0 => s.push_str(rng.pick(&["null", "5", "\"marko\"", "[]"])),
        _ => {
            s.push('{');
            let mut first = true;
            if rng.below(6) > 0 {
                comma(s, &mut first);
                s.push_str(&format!("\"id\":{}", id_value(rng)));
            }
            if rng.below(6) > 0 {
                comma(s, &mut first);
                s.push_str(&format!("\"name\":{}", json_string(rng, "marko")));
            }
            if rng.below(3) == 0 {
                comma(s, &mut first);
                s.push_str(&format!(
                    "\"nickname\":{}",
                    json_string(rng, "Marko Horvat")
                ));
            }
            s.push('}');
        }
    }
}

fn write_content(rng: &mut Rng, s: &mut String) {
    match rng.below(6) {
        0 => s.push_str(&json_string(rng, "poruka iz opće sobe")),
        1 => s.push_str("null"),
        2 => s.push('5'), // non-string content: treated as empty
        3 => s.push_str(&format!("\"{}\"", "t".repeat(rng.range(0, 300_000)))),
        4 => {
            // Multiline content with unicode edge cases.
            let body = "prvi red\nvidi 🔥 i đ/ć/š\n\u{2028}kraj";
            s.push_str(&json_string(rng, body));
        }
        _ => s.push_str(&format!(
            "\"{}\"",
            json_string(rng, "plain").trim_matches('"')
        )),
    }
}

/// Reactions/attachments arrays, sometimes damaged mid-way.
fn write_jagged_array(rng: &mut Rng, s: &mut String) {
    s.push('[');
    let n = rng.range(0, 4);
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        match rng.below(5) {
            0 => s.push_str("{\"emoji\":{\"name\":\"🔥\"},\"count\":2}"),
            1 => s.push_str("{\"filename\":\"plan.txt\",\"size\":4096}"),
            2 => s.push_str("null"),
            3 => s.push('5'),
            _ => {
                s.push('{');
                if rng.below(4) > 0 {
                    s.push_str("\"nested\":");
                    write_jagged_array(rng, s);
                }
                if rng.below(10) > 0 {
                    s.push('}');
                } // sometimes the close brace is missing
            }
        }
    }
    if rng.below(12) == 0 {
        s.push(','); // trailing comma: invalid JSON
    }
    s.push(']');
}

fn timestamp(rng: &mut Rng) -> &'static str {
    rng.pick(&[
        "2026-10-31 18:00:05.123+00:00", // canonical package form
        "2026-10-31 18:00:05+00:00",     // no milliseconds
        "2026-10-31 18:00:05+0000",      // compact offset
        "2026-10-31T18:00:05Z",          // RFC 3339 spelling
        "2026-10-31 18:00:05.123+02:00", // non-UTC offset
        "2026-13-45 99:99:99.999+99:99", // out-of-range parts
        "not a date",
        "",
        "1793546400",
        " 2026-10-31 18:00:05+00:00",
    ])
}

fn id_value(rng: &mut Rng) -> String {
    rng.pick(&[
        "\"1700000000000000001\"",
        "\"1\"",
        "\"\"",
        "\" \"",
        "1234567890123456789",
        "0",
        "-5",
        "1.5",
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
