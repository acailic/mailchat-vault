//! Structured WhatsApp `.txt` export generator: assembles whole exports
//! from realistic header/body/system line parts with a controlled amount
//! of damage, so the fuzzer explores the grammar of the format (not just
//! random bytes).

use super::rng::Rng;

/// One generated export (bytes; may deliberately contain invalid UTF-8).
pub fn export(rng: &mut Rng) -> Vec<u8> {
    let mut out = String::new();
    if rng.bool() {
        out.push('\u{feff}');
    }
    let lines = rng.range(0, 40);
    for _ in 0..lines {
        match rng.below(10) {
            0..=4 => out.push_str(&header_line(rng)),
            5 => out.push_str(&system_line(rng)),
            6 => out.push_str(&continuation(rng)),
            7 => out.push_str(&unicode_soup(rng)),
            8 => out.push_str(&garbage(rng)),
            _ => out.push_str(&attachment_line(rng)),
        }
        match rng.below(10) {
            0 => out.push_str("\r\n"),
            1 if rng.bool() => {} // missing newline: line merges with next
            _ => out.push('\n'),
        }
    }
    if rng.below(50) == 0 {
        // Occasionally a single monstrous line.
        out.push_str(&format!(
            "[{}, {}] {}: {}\n",
            date_token(rng),
            time_token(rng),
            author(rng),
            "m".repeat(rng.range(100_000, 300_000)),
        ));
    }
    let mut bytes = out.into_bytes();
    if rng.below(20) == 0 {
        // Inject stray invalid UTF-8 bytes at random positions.
        for _ in 0..rng.range(1, 8) {
            let at = rng.below(bytes.len() + 1);
            bytes.insert(at, *rng.pick(&[0xff, 0xfe, 0x80, 0xc3, 0xf0]));
        }
    }
    bytes
}

fn header_line(rng: &mut Rng) -> String {
    let bracketed = rng.bool();
    let stamp = format!("{}, {}", date_token(rng), time_token(rng));
    if bracketed {
        format!("[{stamp}] {}: {}", author(rng), body_text(rng))
    } else {
        format!("{stamp} - {}: {}", author(rng), body_text(rng))
    }
}

fn system_line(rng: &mut Rng) -> String {
    match rng.below(4) {
        0 => format!(
            "[{}, {}] {}",
            date_token(rng),
            time_token(rng),
            body_text(rng)
        ),
        1 => format!(
            "[{}, {}] {}:",
            date_token(rng),
            time_token(rng),
            author(rng)
        ),
        2 => format!(
            "{}, {} - {}",
            date_token(rng),
            time_token(rng),
            body_text(rng)
        ),
        _ => format!(
            "[{}, {}] \u{200e}{}",
            date_token(rng),
            time_token(rng),
            body_text(rng)
        ),
    }
}

fn continuation(rng: &mut Rng) -> String {
    match rng.below(6) {
        0..=2 => body_text(rng),
        3 => format!("\u{200e}{}", body_text(rng)),
        4 => format!("{}{}", body_text(rng), separator_junk(rng)),
        _ => format!("{} {}", body_text(rng), date_token(rng)),
    }
}

fn separator_junk(rng: &mut Rng) -> &'static str {
    rng.pick(&[" - ", " -- ", " : ", "]", "[", ":", ", "])
}

fn attachment_line(rng: &mut Rng) -> String {
    let kind = rng.pick(&[
        "image",
        "video",
        "audio",
        "sticker",
        "gif",
        "Contact card",
        "document: notes.txt",
        "<Media omitted>",
        "IMG-20261101-WA0001.jpg (file attached)",
    ]);
    format!(
        "[{}, {}] {}: {}",
        date_token(rng),
        time_token(rng),
        author(rng),
        kind
    )
}

fn date_token(rng: &mut Rng) -> String {
    let d = *rng.pick(&["1", "6", "28", "31", "0", "32", "99", "11"]);
    let m = *rng.pick(&["1", "11", "12", "13", "0", "99"]);
    let y = *rng.pick(&["2026", "26", "999", "9999", "0", "10000"]);
    let sep = *rng.pick(&[".", "/", "-"]);
    match rng.below(8) {
        0 => format!("{y}{sep}{m}{sep}{d}"),
        1 => format!("{d}{sep}{m}{sep}{y}."),
        2 => format!("{d}. {m}. {y}."),
        3 => format!("\u{200e}{d}{sep}{m}{sep}{y}\u{200f}"),
        4 => format!("{d}{sep}{m}"),
        5 => format!("{d}{sep}{m}{sep}{y}{sep}"),
        _ => format!("{d}{sep}{m}{sep}{y}"),
    }
}

fn time_token(rng: &mut Rng) -> String {
    let h = *rng.pick(&["0", "9", "12", "23", "24", "99", "1"]);
    let min = *rng.pick(&["00", "30", "59", "60", "99"]);
    let s = *rng.pick(&["", ":00", ":05", ":60", ":99"]);
    let meridiem = *rng.pick(&[
        "",
        " AM",
        " PM",
        "\u{202f}pm",
        "\u{a0}AM",
        " GMT+3",
        " +02:00",
    ]);
    format!("{h}:{min}{s}{meridiem}")
}

fn author(rng: &mut Rng) -> String {
    rng.pick(&[
        "Marko",
        "Ana",
        "Mom",
        "A",
        "",
        "A: B",
        "привет",
        "أحمد",
        "e\u{200f}moji😀",
        "0",
        "-",
        "]",
        " x ",
    ])
    .to_string()
}

fn body_text(rng: &mut Rng) -> String {
    match rng.below(8) {
        0 => String::new(),
        1 => "ok".into(),
        2 => "[Quoted Message]".into(),
        3 => "[Citirana poruka]".into(),
        4 => format!("{} pic.jpg", "very long ".repeat(rng.range(1, 50))),
        5 => unicode_soup(rng),
        6 => format!(
            "{} - {}",
            "x".repeat(rng.range(1, 200)),
            separator_junk(rng)
        ),
        _ => format!("line with {:?} control", '\u{7}'.to_string()),
    }
}

fn unicode_soup(rng: &mut Rng) -> String {
    let pieces = [
        "\u{200e}",
        "\u{200f}",
        "\u{2028}",
        "\u{2029}",
        "\u{a0}",
        "\u{202f}",
        "😀",
        "équipe",
        "中文",
        "\u{cc80}",
        "\u{1f1ec}\u{1f1e7}",
        "\u{feff}",
        "RTL: مرحبا",
        "\u{301}\u{e9}",
    ];
    (0..rng.range(1, 20)).map(|_| *rng.pick(&pieces)).collect()
}

fn garbage(rng: &mut Rng) -> String {
    (0..rng.range(0, 60))
        .map(|_| {
            *rng.pick(&[
                'x', '7', '[', ']', ':', ',', '.', '/', '-', ' ', '\t', '\u{7}',
            ])
        })
        .collect()
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
}
