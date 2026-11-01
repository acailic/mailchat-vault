//! Structured Signal conversation export generator: emits raw JSON text
//! (not a `serde_json::Value`) so duplicate keys, malformed numbers, lone
//! escapes, and truncation-prone strings are all expressible, with a
//! controlled rate of damage on top of well-formed exports.

use super::rng::Rng;

/// One generated export (raw JSON bytes): a top-level array of Signal
/// Desktop message objects, as written by `sigtop export-messages -f
/// json`.
pub fn export(rng: &mut Rng) -> Vec<u8> {
    let mut s = String::new();
    if rng.bool() {
        s.push('\u{feff}');
    }
    s.push('[');
    let n = rng.range(0, 12);
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        write_entry(rng, &mut s);
    }
    s.push(']');
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
            bytes[i] = *rng.pick(&[0xff, 0x00, b'"', b'\\', b'{', b']', b':']);
        }
        _ => {}
    }
    bytes
}

fn write_entry(rng: &mut Rng, s: &mut String) {
    match rng.below(16) {
        0 => s.push_str(rng.pick(&["5", "\"str\"", "null", "true", "[]", "{}"])),
        _ => {
            s.push('{');
            let mut first = true;
            match rng.below(8) {
                0 => {} // no type at all: malformed entry
                1 => s.push_str(&format!("\"type\":{}", id_value(rng))),
                _ => {
                    let t = *rng.pick(&[
                        "incoming",
                        "outgoing",
                        "call-history",
                        "keychange",
                        "profile-change",
                        "",
                    ]);
                    s.push_str(&format!("\"type\":{}", json_string(rng, t)));
                }
            }
            if rng.below(30) == 0 {
                s.push_str(",\"type\":\"incoming\""); // duplicate key, last wins
            }
            if rng.below(6) > 0 {
                comma(s, &mut first);
                s.push_str(&format!("\"id\":{}", id_value(rng)));
            }
            if rng.below(4) > 0 {
                comma(s, &mut first);
                s.push_str(&format!(
                    "\"conversationId\":{}",
                    json_string(rng, "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01")
                ));
            }
            if rng.below(6) > 0 {
                comma(s, &mut first);
                s.push_str(&format!(
                    "\"source\":{}",
                    json_string(rng, "+385912345678.2")
                ));
            }
            let ms = unixtime_ms(rng);
            match rng.below(4) {
                0 => {
                    comma(s, &mut first);
                    s.push_str(&format!("\"sent_at\":{ms}"));
                }
                1 => {
                    comma(s, &mut first);
                    s.push_str(&format!("\"timestamp\":{ms}"));
                }
                2 => {
                    comma(s, &mut first);
                    s.push_str(&format!("\"sent_at\":{ms}"));
                    comma(s, &mut first);
                    s.push_str(&format!("\"received_at_ms\":{ms}"));
                }
                _ => {
                    comma(s, &mut first);
                    s.push_str("\"sent_at\":\"not-a-number\"");
                }
            }
            if rng.bool() {
                comma(s, &mut first);
                s.push_str("\"body\":");
                write_body(rng, s);
            }
            if rng.below(4) == 0 {
                comma(s, &mut first);
                s.push_str("\"attachments\":[");
                let k = rng.range(0, 3);
                for i in 0..k {
                    if i > 0 {
                        s.push(',');
                    }
                    s.push_str(&format!(
                        "{{\"fileName\":{},\"contentType\":\"image/jpeg\",\"size\":{}}}",
                        json_string(rng, "signal-2026-11-01-110050.jpg"),
                        rng.below(1_000_000)
                    ));
                }
                s.push(']');
            }
            if rng.below(8) == 0 {
                comma(s, &mut first);
                s.push_str(&format!(
                    "\"quote\":{{\"id\":{ms},\"author\":{},\"text\":{},\"attachments\":[],\"isViewOnce\":false,\"referencedMessageNotFound\":false}}",
                    json_string(rng, "+385912345678"),
                    json_string(rng, "quoted text")
                ));
            }
            s.push('}');
        }
    }
}

fn write_body(rng: &mut Rng, s: &mut String) {
    match rng.below(6) {
        0 => s.push_str(&json_string(rng, "plain text 👋")),
        1 => s.push_str("null"),
        2 => s.push_str(&format!("\"{}\"", "t".repeat(rng.range(0, 300_000)))),
        3 => s.push('5'),
        4 => s.push_str(&json_string(rng, "čevapi i 🎉 arhiva")),
        _ => {
            // Deeply nested unexpected object where a string belongs.
            let depth = rng.range(0, 8);
            for _ in 0..depth {
                s.push_str("{\"body\":");
            }
            s.push_str(&json_string(rng, "nested"));
            for _ in 0..depth {
                s.push('}');
            }
        }
    }
}

fn unixtime_ms(rng: &mut Rng) -> &'static str {
    rng.pick(&[
        "1793525400000",
        "0",
        "-1",
        "9223372036854775807",
        "-9223372036854775808",
        "99999999999999999999",
        "1.5",
        "1e999",
        "null",
        "true",
    ])
}

fn id_value(rng: &mut Rng) -> String {
    rng.pick(&[
        "\"0ea0b5fd-9f2a-4c73-8f8e-2f8ce6f22a11\"",
        "\"7\"",
        "5",
        "null",
        "true",
        "{}",
    ])
    .to_string()
}

fn comma(s: &mut String, first: &mut bool) {
    if !*first {
        s.push(',');
    }
    *first = false;
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
