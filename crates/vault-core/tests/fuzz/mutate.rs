//! Byte-level mutation operators: the classic AFL-style bit/byte/chunk
//! moves plus targeted "interesting byte" insertions for the formats at
//! hand (JSON structure, UTF-8 edges, line/boundary markers).

use super::rng::Rng;

/// Bytes that stress the parsers structurally: NUL, high/invalid UTF-8
/// lead and continuation bytes, newlines, quotes/braces for JSON and
/// WhatsApp stamps, and boundary/dash characters for mbox.
const INTERESTING: &[u8] = &[
    0x00, 0x01, 0x08, 0x09, 0x0a, 0x0d, 0x1b, 0x22, 0x27, 0x2c, 0x2d, 0x3a, 0x3d, 0x5b, 0x5c, 0x5d,
    0x7b, 0x7d, 0x7f, 0x80, 0xc2, 0xc3, 0xe2, 0xef, 0xf0, 0xfd, 0xfe, 0xff,
];

/// Multi-byte patterns worth injecting verbatim: UTF-8 BOM, bidi marks,
/// line separators Rust treats as plain characters, CRLF, and JSON noise.
const PATTERNS: &[&[u8]] = &[
    b"\xef\xbb\xbf",     // UTF-8 BOM
    b"\xe2\x80\x8e",     // LRM
    b"\xe2\x80\x8f",     // RLM
    b"\xe2\x80\xa8",     // U+2028 LINE SEPARATOR (not a \n for str::lines)
    b"\xe2\x80\xa9",     // U+2029 PARAGRAPH SEPARATOR
    b"\xcc\x80",         // U+0300 combining grave
    b"\xf0\x9f\x98\x80", // emoji
    b"\r\n",
    b"\n\n\n",
    b"null",
    b"NaN",
    b"-Infinity",
    b"\"\":\"\"",
    b"[Quoted Message]",
    b"[Citirana poruka]",
    b" -- ",
    b" - ",
    b"From ",
    b">From ",
    b"boundary=",
    b"--boundary--",
    b" GMT+3 ",
    b" +9999",
];

/// Apply a random number of random mutations to `data` in place.
pub fn apply(data: &mut Vec<u8>, rng: &mut Rng) {
    let rounds = rng.range(1, 6);
    for _ in 0..rounds {
        if data.is_empty() {
            insert_bytes(data, rng);
            continue;
        }
        match rng.below(10) {
            0 => flip_byte(data, rng),
            1 => set_interesting(data, rng),
            2 => insert_bytes(data, rng),
            3 => insert_pattern(data, rng),
            4 => delete_range(data, rng),
            5 => duplicate_range(data, rng),
            6 => *data = truncate(data, rng),
            7 => swap_lines(data, rng),
            8 => flip_ascii_case(data, rng),
            _ => splice(data, rng),
        }
    }
}

fn flip_byte(data: &mut [u8], rng: &mut Rng) {
    let i = rng.below(data.len());
    data[i] ^= 1 << rng.below(8);
}

fn set_interesting(data: &mut [u8], rng: &mut Rng) {
    let i = rng.below(data.len());
    data[i] = *rng.pick(INTERESTING);
}

fn insert_bytes(data: &mut Vec<u8>, rng: &mut Rng) {
    let at = rng.below(data.len() + 1);
    let b = *rng.pick(INTERESTING);
    data.insert(at, b);
}

fn insert_pattern(data: &mut Vec<u8>, rng: &mut Rng) {
    let at = rng.below(data.len() + 1);
    let pat: &[u8] = rng.pick(PATTERNS);
    data.splice(at..at, pat.iter().copied());
}

fn delete_range(data: &mut Vec<u8>, rng: &mut Rng) {
    let (a, b) = small_range(data.len(), rng);
    data.drain(a..b);
}

fn duplicate_range(data: &mut Vec<u8>, rng: &mut Rng) {
    let (a, b) = small_range(data.len(), rng);
    let chunk = data[a..b].to_vec();
    let at = rng.below(data.len() + 1);
    data.splice(at..at, chunk);
}

fn truncate(data: &mut Vec<u8>, rng: &mut Rng) -> Vec<u8> {
    if data.is_empty() {
        return std::mem::take(data);
    }
    let at = rng.below(data.len());
    data.split_off(at)
}

fn swap_lines(data: &mut Vec<u8>, rng: &mut Rng) {
    let mut lines: Vec<&[u8]> = split_lines(data);
    if lines.len() >= 2 {
        let a = rng.below(lines.len());
        let b = rng.below(lines.len());
        lines.swap(a, b);
        let joined: Vec<u8> = lines
            .iter()
            .flat_map(|l| l.iter().chain(std::iter::once(&b'\n')))
            .copied()
            .collect();
        *data = joined;
    }
}

fn flip_ascii_case(data: &mut [u8], rng: &mut Rng) {
    let i = rng.below(data.len());
    if data[i].is_ascii_alphabetic() {
        data[i] ^= 0x20;
    }
}

/// Splice a chunk of one corpus sample into the data (crosses formats).
fn splice(data: &mut Vec<u8>, rng: &mut Rng) {
    let sample: Vec<u8> = match rng.below(3) {
        0 => b"[1.11.2026., 09:00:05] Marko: Zdravo!\n".to_vec(),
        1 => br#"{"name":"A","id":42,"messages":[{"id":1,"type":"message","text":"hi"}]}"#.to_vec(),
        _ => b"From a@b.c\nContent-Type: multipart/mixed; boundary=\"x\"\n\n--x\n\nb\n--x--\n"
            .to_vec(),
    };
    if sample.is_empty() || data.is_empty() {
        return;
    }
    let (sa, sb) = small_range(sample.len(), rng);
    let chunk = sample[sa..sb].to_vec();
    let at = rng.below(data.len() + 1);
    data.splice(at..at, chunk);
}

fn small_range(len: usize, rng: &mut Rng) -> (usize, usize) {
    let a = rng.below(len);
    let b = a + rng.below((len - a).min(64) + 1);
    (a, b.min(len))
}

fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            lines.push(&data[start..i]);
            start = i + 1;
        }
    }
    lines.push(&data[start..]);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutations_stay_bounded_and_deterministic() {
        let mut rng = Rng::new(99);
        let mut data = b"From a@b.c\nSubject: hello\n\nworld\n".to_vec();
        let before = data.clone();
        apply(&mut data, &mut rng);
        let again = {
            let mut rng = Rng::new(99);
            let mut d = before.clone();
            apply(&mut d, &mut rng);
            d
        };
        assert_eq!(data, again, "same seed must give same mutations");
        // Empty input must survive every operator.
        for seed in 0..200u64 {
            let mut rng = Rng::new(seed);
            let mut empty: Vec<u8> = Vec::new();
            apply(&mut empty, &mut rng);
        }
    }
}
