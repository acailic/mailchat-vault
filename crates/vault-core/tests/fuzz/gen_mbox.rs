//! Structured `.mbox` generator: whole files assembled from From-lines,
//! header blocks (well-formed, folded, or damaged), and bodies (plain,
//! transfer-encoded garbage, nested MIME), including the hostile deeply
//! nested multipart shape the parser's depth guard exists for.

use super::rng::Rng;

/// One generated mbox file (bytes).
pub fn file(rng: &mut Rng) -> Vec<u8> {
    let mut s = String::new();
    let mails = rng.range(1, 5);
    for _ in 0..mails {
        from_line(rng, &mut s);
        write_mail(rng, &mut s);
    }
    if rng.below(20) == 0 {
        // File truncated mid-mail.
        let bytes = s.into_bytes();
        let at = rng.below(bytes.len() + 1);
        return bytes[..at].to_vec();
    }
    s.into_bytes()
}

fn from_line(rng: &mut Rng, s: &mut String) {
    let date = *rng.pick(&[
        "Mon Nov  1 10:00:00 2026",
        "Mon, 1 Nov 2026 10:00:00 +0100",
        "Mon, 1 Nov 2026 10:00:00 +9999",
        "Mon, 1 Nov 2026 10:00:00 UT+3",
        "Mon, 1 Nov 2026 10:00:00 +02:00:00",
        "not a date",
        "",
        "Tue Feb 30 25:61:61 99999",
    ]);
    let addr = *rng.pick(&[
        "a@b.c",
        "",
        "spaces in it@x",
        "\u{fffd}",
        "a@b.c extra tokens",
    ]);
    match rng.below(10) {
        0 => s.push_str(&format!("From {addr} {date}")),
        1 => s.push_str("From "),
        _ => s.push_str(&format!("From {addr} {date}\n")),
    }
}

fn write_mail(rng: &mut Rng, s: &mut String) {
    let kind = rng.below(10);
    let depth = match kind {
        0..=4 => return simple_mail(rng, s),
        5..=7 => rng.range(1, 6),
        _ => rng.range(60, 3000), // past the depth guard: hostile mail
    };
    multipart_mail(rng, s, depth);
}

fn simple_mail(rng: &mut Rng, s: &mut String) {
    if rng.below(8) == 0 {
        // No headers at all: body-only mail.
        s.push('\n');
        body(rng, s);
        return;
    }
    if rng.bool() {
        s.push_str(&format!("Message-ID: <{}@b.c>\n", rng.range(0, 1_000_000)));
    }
    if rng.below(10) == 0 {
        s.push_str("Message-ID: <dup@b.c>\n"); // duplicate header
    }
    if rng.bool() {
        s.push_str(&format!(
            "From: {}\n",
            *rng.pick(&[
                "Marko <marko@ex.com>",
                "bare@ex.com",
                "",
                "8-bit \u{fffd} name <x@y.z>",
            ])
        ));
    }
    if rng.below(3) > 0 {
        let subject = match rng.below(8) {
            0 => String::new(),
            1 => "A".repeat(rng.range(1, 20_000)),
            2 => "folded\n continuation".into(),
            3 => "no colon subject".into(),
            _ => "rehearsal dinner".into(),
        };
        match rng.below(12) {
            0 => s.push_str(&format!("Subject:{subject}")), // no space, no newline
            1 => s.push_str(&format!("Subject {subject}\n")), // missing colon
            2 => s.push_str(&format!("Subject: {subject}\n subject-tail\n")), // fold
            _ => s.push_str(&format!("Subject: {subject}\n")),
        }
    }
    if rng.bool() {
        s.push_str(&format!(
            "To: {}\n",
            *rng.pick(&["ana@example.com", "", "undisclosed"])
        ));
    }
    if rng.below(3) > 0 {
        s.push_str(&format!(
            "Date: {}\n",
            *rng.pick(&[
                "Mon, 1 Nov 2026 10:00:00 +0100",
                "Mon, 1 Nov 2026 10:00:00 +9999",
                "Mon, 1 Nov 2026 10:00:00 UT+3",
                "Mon, 1 Nov 2026 10:00:00 -0000",
                "1 Nov 26",
                "garbage",
                "",
            ])
        ));
    }
    match rng.below(10) {
        0 => s.push_str("Content-Transfer-Encoding: base64\n"),
        1 => s.push_str("Content-Transfer-Encoding: quoted-printable\n"),
        2 => s.push_str("Content-Type: text/plain; charset=nosuchcharset\n"),
        3 => s.push_str("Content-Type: text/plain; charset=\"unterminated\n"),
        _ => {}
    }
    s.push('\n');
    body(rng, s);
}

fn multipart_mail(rng: &mut Rng, s: &mut String, depth: usize) {
    // Nested multipart chain with per-level boundary damage.
    for d in 0..depth {
        let b = boundary_name(rng, d);
        match rng.below(8) {
            0 => s.push_str(&format!(
                "Content-Type: multipart/mixed;\n boundary={b}\n\n"
            )),
            1 => s.push_str(&format!("Content-Type: MULTIPART/MIXED; BOUNDARY={b}\n\n")),
            2 => s.push_str("Content-Type: multipart/mixed\n\n"), // no boundary param
            3 => s.push_str("Content-Type: multipart/mixed; boundary=\n\n"),
            _ => s.push_str(&format!(
                "Content-Type: multipart/mixed; boundary=\"{b}\"\n\n"
            )),
        }
        s.push_str(&format!("--{b}\n"));
    }
    s.push_str("Content-Type: text/plain\n\n");
    body(rng, s);
    for d in (0..depth).rev() {
        let b = boundary_name(rng, d);
        match rng.below(8) {
            0 => {} // boundary never closed: damage
            _ => s.push_str(&format!("--{b}--\n")),
        }
    }
}

fn boundary_name(rng: &mut Rng, d: usize) -> String {
    match rng.below(6) {
        0 => "x".to_string(), // same boundary at every level
        1 => format!("b{d}"),
        2 => format!("=_{}", "b".repeat(40)), // RFC 2231-ish shape
        3 => String::new(),
        4 => format!("{}\"", d), // stray quote
        _ => format!("sec{d}"),
    }
}

fn body(rng: &mut Rng, s: &mut String) {
    match rng.below(10) {
        0 => {}
        1 => s.push_str("plain body line\n"),
        2 => s.push_str("!!!!????====\n"), // base64 garbage
        3 => s.push_str("=X\n=Z9\n=\n"),   // quoted-printable garbage
        4 => s.push_str(">From escaped\n"),
        5 => s.push_str(&format!("{}\n", "m".repeat(rng.range(1, 100_000)))),
        6 => s.push_str("From embedded unescaped\n"),
        7 => s.push_str("\r\nmixed\r\nendings\n"),
        8 => s.push_str("body\x00with\x00nuls\n"),
        _ => {
            let n = rng.range(1, 30);
            for _ in 0..n {
                s.push_str(&format!(
                    "{}\n",
                    *rng.pick(&["line", ">From x", "--boundary--", ""])
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_deterministic_variety() {
        let a = file(&mut Rng::new(1));
        let b = file(&mut Rng::new(1));
        assert_eq!(a, b);
        assert_ne!(a, file(&mut Rng::new(2)));
    }
}
