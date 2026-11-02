//! Hand-crafted edge-case corpus: the deterministic worst cases the
//! structured generators may take many rounds to stumble into. Every
//! entry is a `(description, bytes)` pair; descriptions double as
//! reproduction instructions in a crash report.

/// A realistic Croatian iOS-style export used as the truncation seed.
fn valid_whatsapp() -> Vec<u8> {
    "\
\u{feff}[1.11.2026., 09:00:05] Marko: Zdravo ekipa!\n\
[1.11.2026., 09:00:41] Ana: \u{200e}[Citirana poruka]\n\
[1.11.2026., 08:59:59] Marko: jutro\n\
vidimo se danas\n\
[1.11.2026., 09:05:00] Ana: IMG-20261101-WA0001.jpg (file attached)\n\
[1.11.2026., 09:06:00] Marko: \u{200e}image omitted\n\
[1.11.2026., 09:10:00] Marko je dodao Anu\n\
"
    .as_bytes()
    .to_vec()
}

/// A realistic Telegram Desktop export used as the truncation seed.
fn valid_telegram() -> Vec<u8> {
    br#"{
  "name": "Arhiva",
  "type": "private_group",
  "id": 123456789,
  "messages": [
    {"id": 100, "type": "message", "date": "2026-11-01T10:00:00", "date_unixtime": "1793546400", "from": "Marko", "text": "hello"},
    {"id": 101, "type": "message", "date": "2026-11-01T10:01:00", "date_unixtime": "1793546460", "from": "Ana", "text": ["plain ", {"type": "bold", "text": "bold"}, " tail"]},
    {"id": 102, "type": "service", "action": "phone_call", "date_unixtime": "1793546500"}
  ]
}"#
    .to_vec()
}

/// A realistic two-mail mbox used as the truncation seed.
fn valid_mbox() -> Vec<u8> {
    b"From marko@example.com Mon Nov  1 10:00:00 2026\n\
Message-ID: <a@example.com>\n\
From: Marko <marko@example.com>\n\
To: ana@example.com\n\
Subject: rehearsal dinner\n\
Date: Mon, 1 Nov 2026 10:00:00 +0100\n\
\n\
See you at 19:00.\n\
\n\
From ana@example.com Mon Nov  1 11:00:00 2026\n\
Message-ID: <b@example.com>\n\
From: Ana <ana@example.com>\n\
Subject: Re: rehearsal dinner\n\
Content-Type: multipart/alternative; boundary=\"bound\"\n\
\n\
--bound\n\
Content-Type: text/plain\n\
\n\
On my way!\n\
--bound\n\
Content-Type: text/html\n\
\n\
<p>On my way!</p>\n\
--bound--\n"
        .to_vec()
}

/// `(description, bytes)` corpus cases for one parser family.
type Cases = Vec<(String, Vec<u8>)>;

fn case(desc: &str, bytes: impl Into<Vec<u8>>) -> (String, Vec<u8>) {
    (desc.to_string(), bytes.into())
}

/// Every truncation of `seed` at a spread of offsets, including the two
/// pathological ends (empty, missing final byte) and cuts that land
/// inside multi-byte UTF-8 sequences and JSON strings.
fn truncations(name: &str, seed: &[u8]) -> Cases {
    let mut out = Vec::new();
    let len = seed.len();
    let mut offsets: Vec<usize> = vec![0, 1, 2, 3, 7, 13, 31, len / 2, len.saturating_sub(1), len];
    // Cuts inside every multi-byte sequence and every quote/escape.
    for (i, b) in seed.iter().enumerate() {
        if *b >= 0xc2 || *b == b'"' || *b == b'\\' {
            offsets.push(i + 1);
        }
    }
    offsets.sort_unstable();
    offsets.dedup();
    for o in offsets {
        out.push(case(
            &format!("{name} truncated at byte {o}/{}", len),
            &seed[..o.min(len)],
        ));
    }
    out
}

pub fn whatsapp_cases() -> Cases {
    let mut cases = vec![
        case("empty file", b""),
        case("only BOM", b"\xef\xbb\xbf"),
        case("BOM then newline", b"\xef\xbb\xbf\n"),
        case("only newlines", b"\n\n\n"),
        case("only carriage returns", b"\r\r\r"),
        case("lone NUL", b"\x00"),
        case("text before any header", b"stray intro line\n[1.11.2026., 09:00] A: hi\n"),
        case(
            "invalid UTF-8 in body",
            b"[1.11.2026., 09:00:00] A: \xff\xfe\xc3\x28\n",
        ),
        case(
            "bidi marks in stamp and author",
            "[\u{200e}1\u{200f}.11.2026., 09:00\u{200e}] \u{200f}A\u{200e}: x\n".as_bytes(),
        ),
        case(
            "U+2028/2029 inside a line",
            "[1.11.2026., 09:00] A: a\u{2028}b\u{2029}c\n".as_bytes(),
        ),
        case("day zero", b"[0.0.0, 0:0] A: x\n"),
        case("day 32 month 13", b"[32.13.2026, 25:61] A: x\n"),
        case("feb 30", b"[30.2.2026, 10:00] A: x\n"),
        case("year 26 AD shape", b"[1.1.26, 1:1] A: x\n"),
        case("5-digit year", b"[1.11.99999, 09:00] A: x\n"),
        case("huge date numbers", b"[99999999999999999999.1.1, 99:99] A: x\n"),
        case("negative date parts", b"[-1.-1.-1, -1:-1] A: x\n"),
        case("stamp with tz marker", b"[1.11.2026., 09:00 GMT+3] A: x\n"),
        case("stamp with utc offset", b"[1.11.2026., 09:00:00+02:00] A: x\n"),
        case("narrow nbsp before AM", "[6/28/23, 9:30\u{202f}PM] Mom: hi\n".as_bytes()),
        case("nbsp before AM", "[6/28/23, 9:30\u{a0}PM] Mom: hi\n".as_bytes()),
        case("12h at hour 0/13", b"[6/28/23, 0:30 AM] A: x\n[6/28/23, 13:30 PM] A: x\n"),
        case("bracket never closed", b"[1.11.2026., 09:00:00 A: x\n"),
        case(
            "bracket closed many times",
            b"[1.11.2026., 09:00:00]]]]] A: x\n",
        ),
        case("author colon inside author", b"[1.11.2026., 09:00] A: B: x\n"),
        case("bare trailing colon", b"[1.11.2026., 09:00] A:\n"),
        case("empty author brackets", b"[1.11.2026., 09:00] : x\n"),
        case(
            "separator storm in android line",
            b"1.11.2026., 09:00 - - - - A: x\n",
        ),
        case("android stamp no time", b"1.11.2026. - A: x\n"),
        case("android line with many dashes", b"1.11.2026., 09:00 - a - b - c: d\n"),
        case(
            "quoted marker then eof",
            b"[1.11.2026., 09:00] A: [Quoted Message]\n",
        ),
        case(
            "quoted marker storm",
            {
                let mut s = String::from("[1.11.2026., 09:00] A: [Quoted Message]\n");
                for _ in 0..500 {
                    s.push_str("[1.11.2026., 09:00] B: [Citirana poruka]\n");
                }
                s.into_bytes()
            },
        ),
        case("attachment placeholders", b"[1.11.2026., 09:00] A: image omitted\n[1.11.2026., 09:01] A: <Media omitted>\n[1.11.2026., 09:02] A: document: plan.pdf omitted\n[1.11.2026., 09:03] A: x.pdf (file attached)\n"),
        case("NUL inside message", b"[1.11.2026., 09:00] A\x00: x\x00y\n"),
        case(
            "two-megabyte single line",
            format!("[1.11.2026., 09:00:00] A: {}\n", "x".repeat(2_000_000)).into_bytes(),
        ),
        case(
            "ten thousand headers",
            {
                let mut s = String::new();
                for i in 0..10_000 {
                    s.push_str(&format!(
                        "[1.11.2026., 09:{:02}:{:02}] A: m{}\n",
                        (i / 60) % 60,
                        i % 60,
                        i
                    ));
                }
                s.into_bytes()
            },
        ),
        case(
            "day-first and month-first mixed",
            b"[25.12.26, 19:04] A: hi\n[12/25/26, 7:04 PM] B: hi\n",
        ),
        case("crlf line endings", b"[1.11.2026., 09:00] A: a\r\ncontinuation\r\n[1.11.2026., 09:01] B: b\r\n"),
        case("lone CR inside line", b"[1.11.2026., 09:00] A: a\rb\n"),
        case(
            "rtl message text",
            "[1.11.2026., 09:00] أحم\u{200f}د: مرحبا\n".as_bytes(),
        ),
    ];
    let mut tr = truncations("whatsapp export", &valid_whatsapp());
    cases.append(&mut tr);
    cases
}

pub fn telegram_cases() -> Cases {
    let mut cases = vec![
        case("empty file", b""),
        case("BOM then empty json", b"\xef\xbb\xbf{}"),
        case("bare null", b"null"),
        case("bare number", b"42"),
        case("bare string", b"\"just a string\""),
        case("empty object", b"{}"),
        case("messages not array", br#"{"messages": {"a": 1}}"#),
        case("messages null", br#"{"messages": null}"#),
        case("messages string", br#"{"messages": "x"}"#),
        case(
            "duplicate top-level keys",
            br#"{"id":1,"id":2,"messages":[],"messages":[{"id":1}]}"#,
        ),
        case(
            "duplicate message keys",
            br#"{"messages":[{"type":"message","type":"service","id":1,"id":2}]}"#,
        ),
        case(
            "duplicate name keys",
            br#"{"name":"A","name":"","id":7,"messages":[]}"#,
        ),
        case(
            "entry not an object",
            br#"{"messages":["str", 5, null, true, [], {}]}"#,
        ),
        case("entry without type", br#"{"messages":[{"id":1}]}"#),
        case(
            "type not a string",
            br#"{"messages":[{"type":5,"id":1},{"type":null,"id":2},{"type":[],"id":3}]}"#,
        ),
        case("entry without id", br#"{"messages":[{"type":"message"}]}"#),
        case(
            "id variants",
            br#"{"messages":[{"type":"message","id":0},{"type":"message","id":-5},{"type":"message","id":1.5},{"type":"message","id":"7"},{"type":"message","id":9223372036854775807},{"type":"message","id":null}]}"#,
        ),
        case(
            "huge float id",
            br#"{"id": 1e999, "messages":[]}"#,
        ),
        case(
            "text variants",
            br#"{"messages":[
                {"type":"message","id":1,"text":"plain"},
                {"type":"message","id":2,"text":null},
                {"type":"message","id":3,"text":123},
                {"type":"message","id":4,"text":[]},
                {"type":"message","id":5,"text":["a",{"text":"b"},{},{"text":null},5,null,["nested"]]},
                {"type":"message","id":6,"text":""}
            ]}"#,
        ),
        case(
            "date_unixtime variants",
            br#"{"messages":[
                {"type":"message","id":1,"date_unixtime":"0"},
                {"type":"message","id":2,"date_unixtime":"9223372036854775807"},
                {"type":"message","id":3,"date_unixtime":"-9223372036854775808"},
                {"type":"message","id":4,"date_unixtime":"99999999999999999999"},
                {"type":"message","id":5,"date_unixtime":"not-a-number"},
                {"type":"message","id":6,"date_unixtime":""},
                {"type":"message","id":7,"date_unixtime":42.5},
                {"type":"message","id":8,"date_unixtime":null}
            ]}"#,
        ),
        case(
            "date string fallback garbage",
            br#"{"messages":[
                {"type":"message","id":1,"date":"2026-11-01T10:00:00"},
                {"type":"message","id":2,"date":"1.11.2026."},
                {"type":"message","id":3,"date":"2026-13-45T99:99:99"},
                {"type":"message","id":4,"date":""},
                {"type":"message","id":5,"date":7}
            ]}"#,
        ),
        case(
            "timezone-shaped unixtime",
            br#"{"messages":[{"type":"message","id":1,"date_unixtime":"1793546400+02:00"}]}"#,
        ),
        case(
            "lone surrogate escape",
            b"{\"name\": \"\\ud800\", \"messages\": []}",
        ),
        case(
            "trailing surrogate escape",
            b"{\"name\": \"\\udc00\", \"messages\": []}",
        ),
        case(
            "bad escape",
            b"{\"name\": \"\\q\", \"messages\": []}",
        ),
        case(
            "unterminated string",
            b"{\"name\": \"unterminated",
        ),
        case(
            "nesting beyond serde limit",
            format!("{{\"messages\":{}1{}}}", "[".repeat(400), "]".repeat(400)).into_bytes(),
        ),
        case(
            "deeply nested text array",
            format!(
                "{{\"messages\":[{{\"type\":\"message\",\"id\":1,\"text\":{}1{}}}]}}",
                "[".repeat(120),
                "]".repeat(120)
            )
            .into_bytes(),
        ),
        case("trailing garbage after json", br#"{"messages":[]} trailing"#),
        case("json then null bytes", b"{\"messages\":[]}\x00\x00"),
        case(
            "huge single string value",
            format!("{{\"messages\":[{{\"type\":\"message\",\"id\":1,\"text\":\"{}\"}}]}}", "y".repeat(2_000_000)).into_bytes(),
        ),
        case(
            "unicode escapes and marks",
            b"{\"name\": \"\\u0041\\u200e\\u200f\\u2028\", \"id\": \"\\ud83d\\ude00\", \"messages\": []}",
        ),
        case(
            "chat id variants",
            br#"{"id": -1, "messages":[]}"#,
        ),
        case(
            "id as empty string",
            br#"{"id": "", "name": "", "messages":[{"type":"message","id":1}]}"#,
        ),
    ];
    let mut tr = truncations("telegram export", &valid_telegram());
    cases.append(&mut tr);
    cases
}

/// A realistic Discord Data Package channel file used as the truncation
/// seed.
fn valid_discord() -> Vec<u8> {
    r#"{
  "channel": { "id": "900000000000000101", "name": "general" },
  "guild": { "id": "800000000000000001", "name": "Ekipa HQ" },
  "messages": [
    {"id": "1700000000000000001", "type": "Default", "timestamp": "2026-10-31 18:00:05.123+00:00", "content": "Zdravo ekipa! 🔥", "author": {"id": "111000000000000001", "name": "marko", "nickname": "Marko Horvat"}},
    {"id": "1700000000000000002", "type": "Reply", "timestamp": "2026-10-31 18:00:41.999+00:00", "content": "odgovor", "attachments": [{"filename": "plan.txt", "size": 4096}], "reactions": [{"emoji": {"name": "🔥", "id": null}, "count": 2}], "author": {"id": "111000000000000002", "name": "jelena"}},
    {"id": "1700000000000000003", "type": "GuildMemberJoin", "timestamp": "2026-10-31 18:05:00+00:00", "content": ""}
  ]
}"#
        .as_bytes()
        .to_vec()
}

pub fn discord_cases() -> Cases {
    let mut cases = vec![
        case("empty file", b""),
        case("BOM then empty json", b"\xef\xbb\xbf{}"),
        case("bare null", b"null"),
        case("bare number", b"42"),
        case("bare string", b"\"just a string\""),
        case("empty object", b"{}"),
        case("index.json shape", br#"{"entries": [{"id": "1", "channel_path": "Direct Messages/@x"}]}"#),
        case("messages not array", br#"{"channel": {"id": "1"}, "messages": {"a": 1}}"#),
        case("messages null", br#"{"channel": {"id": "1"}, "messages": null}"#),
        case("messages string", br#"{"channel": {"id": "1"}, "messages": "x"}"#),
        case("channel not object", br#"{"channel": "x", "messages": []}"#),
        case("channel null", br#"{"channel": null, "messages": []}"#),
        case("guild not object", br#"{"guild": 5, "messages": []}"#),
        case(
            "duplicate top-level keys",
            br#"{"channel": {"id": "1"}, "channel": null, "messages": [], "messages": [{"id": "1"}]}"#,
        ),
        case(
            "duplicate message keys",
            br#"{"channel": {"id": "1"}, "messages": [{"type": "Default", "type": "GuildMemberJoin", "id": "1", "id": "2"}]}"#,
        ),
        case(
            "entry not an object",
            br#"{"channel": {"id": "1"}, "messages": ["str", 5, null, true, [], {}]}"#,
        ),
        case("entry without type", br#"{"channel": {"id": "1"}, "messages": [{"id": "1"}]}"#),
        case(
            "type not a string",
            br#"{"channel": {"id": "1"}, "messages": [{"type": 5, "id": "1"}, {"type": null, "id": "2"}, {"type": [], "id": "3"}]}"#,
        ),
        case(
            "type variants",
            br#"{"channel": {"id": "1"}, "messages": [
                {"id": "1", "type": "Default"},
                {"id": "2", "type": "Reply"},
                {"id": "3", "type": "ChatInputCommand"},
                {"id": "4", "type": "ContextMenuCommand"},
                {"id": "5", "type": "GuildMemberJoin"},
                {"id": "6", "type": "ChannelPinnedMessage"},
                {"id": "7", "type": "Call"},
                {"id": "8", "type": ""},
                {"id": "9", "type": "WeirdFutureType"}
            ]}"#,
        ),
        case("entry without id", br#"{"channel": {"id": "1"}, "messages": [{"type": "Default"}]}"#),
        case(
            "id variants",
            br#"{"channel": {"id": "1"}, "messages": [
                {"type": "Default", "id": "1700000000000000001"},
                {"type": "Default", "id": 1234567890123456789},
                {"type": "Default", "id": 0},
                {"type": "Default", "id": -5},
                {"type": "Default", "id": 1.5},
                {"type": "Default", "id": ""},
                {"type": "Default", "id": " "},
                {"type": "Default", "id": 9223372036854775807},
                {"type": "Default", "id": null}
            ]}"#,
        ),
        case(
            "channel id variants",
            br#"{"channel": {"id": 1234567890123456789}, "messages": []}"#,
        ),
        case(
            "timestamp variants",
            br#"{"channel": {"id": "1"}, "messages": [
                {"id": "1", "type": "Default", "timestamp": "2026-10-31 18:00:05.123+00:00"},
                {"id": "2", "type": "Default", "timestamp": "2026-10-31 18:00:05+00:00"},
                {"id": "3", "type": "Default", "timestamp": "2026-10-31 18:00:05+0000"},
                {"id": "4", "type": "Default", "timestamp": "2026-10-31T18:00:05Z"},
                {"id": "5", "type": "Default", "timestamp": "2026-10-31 18:00:05.999999999+02:00"},
                {"id": "6", "type": "Default", "timestamp": "2026-13-45 99:99:99+99:99"},
                {"id": "7", "type": "Default", "timestamp": "not a date"},
                {"id": "8", "type": "Default", "timestamp": ""},
                {"id": "9", "type": "Default", "timestamp": 42},
                {"id": "10", "type": "Default", "timestamp": null}
            ]}"#,
        ),
        case(
            "content variants",
            r#"{"channel": {"id": "1"}, "messages": [
                {"id": "1", "type": "Default", "content": "plain"},
                {"id": "2", "type": "Default", "content": null},
                {"id": "3", "type": "Default", "content": 123},
                {"id": "4", "type": "Default", "content": []},
                {"id": "5", "type": "Default", "content": ""},
                {"id": "6", "type": "Default", "content": "prvi red\nvidi 🔥\r\nkraj"}
            ]}"#
                .as_bytes(),
        ),
        case(
            "author variants",
            br#"{"channel": {"id": "1"}, "messages": [
                {"id": "1", "type": "Default", "author": {"id": "5", "name": "marko", "nickname": "Marko H."}},
                {"id": "2", "type": "Default", "author": {"id": "5", "name": "marko"}},
                {"id": "3", "type": "Default", "author": {"id": "5"}},
                {"id": "4", "type": "Default", "author": {"nickname": ""}, "content": "x"},
                {"id": "5", "type": "Default", "author": {"name": "", "id": 7}},
                {"id": "6", "type": "Default", "author": "not an object"},
                {"id": "7", "type": "Default", "author": null},
                {"id": "8", "type": "Default", "author": []},
                {"id": "9", "type": "Default", "author": {"id": 1.5, "name": 5}}
            ]}"#,
        ),
        case(
            "damaged reactions and attachments",
            r#"{"channel": {"id": "1"}, "messages": [
                {"id": "1", "type": "Default", "reactions": [{"emoji": {"name": "🔥"}, "count": 2}]},
                {"id": "2", "type": "Default", "attachments": [{"filename": "plan.txt", "size": 4096}]},
                {"id": "3", "type": "Default", "reactions": "not an array"},
                {"id": "4", "type": "Default", "attachments": {"a": 1}}
            ]}"#
                .as_bytes(),
        ),
        case(
            "dm without channel object",
            br#"{"messages": [{"id": "1", "type": "Default"}]}"#,
        ),
        case(
            "lone surrogate escape",
            b"{\"channel\": {\"id\": \"1\", \"name\": \"\\ud800\"}, \"messages\": []}",
        ),
        case(
            "bad escape",
            b"{\"channel\": {\"id\": \"1\", \"name\": \"\\q\"}, \"messages\": []}",
        ),
        case(
            "unterminated string",
            b"{\"channel\": {\"id\": \"1\", \"name\": \"unterminated",
        ),
        case(
            "nesting beyond serde limit",
            format!(
                "{{\"channel\": {{\"id\": \"1\"}}, \"messages\":{}1{}}}",
                "[".repeat(400),
                "]".repeat(400)
            )
            .into_bytes(),
        ),
        case("trailing garbage after json", br#"{"channel": {"id": "1"}, "messages": []} trailing"#),
        case("json then null bytes", b"{\"channel\": {\"id\": \"1\"}, \"messages\": []}\x00\x00"),
        case(
            "huge single string value",
            format!(
                "{{\"channel\": {{\"id\": \"1\"}}, \"messages\": [{{\"id\": \"1\", \"type\": \"Default\", \"content\": \"{}\"}}]}}",
                "y".repeat(2_000_000)
            )
            .into_bytes(),
        ),
        case(
            "unicode escapes and marks",
            b"{\"channel\": {\"id\": \"1\", \"name\": \"\\u0041\\u200e\\u200f\\u2028\"}, \"messages\": []}",
        ),
        case(
            "telegram-shaped keys alongside discord shape",
            br#"{"channel": {"id": "1"}, "name": "Arhiva", "id": 42, "type": "private_group", "messages": [{"id": "1", "type": "Default"}]}"#,
        ),
    ];
    let mut tr = truncations("discord channel file", &valid_discord());
    cases.append(&mut tr);
    cases
}

pub fn mbox_cases() -> Cases {
    let mut cases = vec![
        case("empty file", b""),
        case("only From line", b"From "),
        case("From line no newline", b"From a@b.c\nSubject: hi"),
        case("From line no body", b"From a@b.c\n"),
        case("no headers at all", b"From a@b.c\n\njust body\n"),
        case("header without colon", b"From a@b.c\nNotAHeader\nbody\n"),
        case("header colon no value", b"From a@b.c\nSubject:\n\nx\n"),
        case(
            "folded header bomb",
            format!("From a@b.c\nSubject:{}", "A".repeat(1_000_000)).into_bytes(),
        ),
        case(
            "million-byte header no newline",
            format!("From a@b.c\nSubject: {}", "B".repeat(1_000_000)).into_bytes(),
        ),
        case("base64 garbage", b"From a@b.c\nContent-Transfer-Encoding: base64\n\n!!!!????\n"),
        case("qp garbage", b"From a@b.c\nContent-Transfer-Encoding: quoted-printable\n\n=X\n=Z9\n=\n"),
        case("unknown charset", b"From a@b.c\nContent-Type: text/plain; charset=nosuchcharset\n\nhi\n"),
        case(
            "timezone markers in Date",
            b"From a@b.c\nDate: Mon, 1 Nov 2026 10:00:00 +9999\n\nx\nFrom d@e.f\nDate: Mon, 1 Nov 2026 10:00:00 UT+3\n\nx\nFrom g@h.i\nDate: Mon, 1 Nov 2026 10:00:00 +02:00:00\n\nx\nFrom j@k.l\nDate: Mon, 1 Nov 2026 10:00:00 -0000\n\nx\n",
        ),
        case("date garbage", b"From a@b.c\nDate: not a date at all\n\nx\n"),
        case("empty date", b"From a@b.c\nDate:\n\nx\n"),
        case(
            "multipart without boundary param",
            b"From a@b.c\nContent-Type: multipart/mixed\n\n--x\n\ny\n--x--\n",
        ),
        case(
            "boundary never closed",
            b"From a@b.c\nContent-Type: multipart/mixed; boundary=\"x\"\n\n--x\nContent-Type: text/plain\n\nbody\n",
        ),
        case(
            "unterminated charset quote",
            b"From a@b.c\nContent-Type: text/plain; charset=\"unterminated\n\nx\n",
        ),
        case(
            "escaped From storm",
            ">From >From >From \n".repeat(1000),
        ),
        case("From line storm", "From x\n".repeat(100_000)),
        case("crlf file", b"From a@b.c\r\nSubject: s\r\n\r\nbody\r\n"),
        case("mixed line endings", b"From a@b.c\nSubject: s\r\n\r\nbody\r\nmore\n"),
        case("NUL in header", b"From a@b.c\nSubject: a\x00b\n\nx\n"),
        case("8-bit bytes in header", b"From a@b.c\nSubject: \xff\xfe\n\nx\n"),
        case(
            "hostile 2000-deep multipart",
            nested_multipart(2000),
        ),
        case(
            "hostile 100k-deep multipart",
            nested_multipart(100_000),
        ),
        case(
            "empty-boundary multipart chain",
            b"From a@b.c\n".iter()
                .chain((0..2000).flat_map(|_| b"Content-Type: multipart/mixed; boundary=\n\n--\n".iter()))
                .copied()
                .collect::<Vec<u8>>(),
        ),
        case(
            "reused boundary at depth",
            b"From a@b.c\n".iter()
                .chain((0..3000).flat_map(|_| b"Content-Type: multipart/mixed; boundary=\"x\"\n\n--x\n".iter()))
                .copied()
                .collect::<Vec<u8>>(),
        ),
        case(
            "many sibling parts",
            {
                let mut s = String::from("From a@b.c\nContent-Type: multipart/mixed; boundary=\"b\"\n\n");
                for _ in 0..20_000 {
                    s.push_str("--b\nContent-Type: text/plain\n\np\n");
                }
                s.push_str("--b--\n");
                s.into_bytes()
            },
        ),
    ];
    let mut tr = truncations("mbox file", &valid_mbox());
    cases.append(&mut tr);
    cases
}

/// Hand-crafted Signal conversation export edge cases.
pub fn signal_cases() -> Cases {
    let mut cases = vec![
        case(
            "well-formed incoming and outgoing",
            r#"[{"id": "a", "type": "incoming", "conversationId": "c", "source": "+385912345678.2", "sent_at": 1793525400000, "body": "hello"},
{"id": "b", "type": "outgoing", "conversationId": "c", "sent_at": 1793525460000, "body": "hi"}]"#.as_bytes(),
        ),
        case("empty array", b"[]"),
        case("array of non-objects", b"[1, 2, 3]"),
        case(
            "entries with only service types",
            b"[{\"type\": \"call-history\"}, {\"type\": \"keychange\"}]",
        ),
        case(
            "type is not a string",
            b"[{\"type\": 7, \"sent_at\": 1, \"body\": \"x\"}]",
        ),
        case(
            "timestamps as strings, floats, and null",
            b"[{\"type\": \"incoming\", \"sent_at\": \"1793525400000\"}, {\"type\": \"incoming\", \"sent_at\": 1.5}, {\"type\": \"incoming\", \"sent_at\": null}, {\"type\": \"incoming\", \"sent_at\": \"garbage\"}]",
        ),
        case(
            "timestamp beyond chrono range",
            b"[{\"type\": \"incoming\", \"sent_at\": 9223372036854775807, \"body\": \"x\"}]",
        ),
        case(
            "negative timestamp",
            b"[{\"type\": \"incoming\", \"sent_at\": -1793525400000, \"body\": \"x\"}]",
        ),
        case(
            "unicode body and lone surrogates",
            r#"[{"type": "incoming", "sent_at": 1, "body": "čevapi 🎉 \ud800 مركبا"}]"#.as_bytes(),
        ),
        case(
            "bidi marks in body and source",
            "[{\"type\": \"incoming\", \"sent_at\": 1, \"source\": \"\\u200e+385912345678\\u200f\", \"body\": \"\\u200ehello\\u200f\"}]"
                .as_bytes(),
        ),
        case(
            "quote with null id and missing author",
            b"[{\"type\": \"incoming\", \"sent_at\": 1, \"quote\": {\"id\": null, \"text\": \"t\"}}]",
        ),
        case(
            "deeply nested attachments",
            {
                let mut s = String::from("[{\"type\": \"incoming\", \"sent_at\": 1, \"attachments\": [");
                for _ in 0..2000 {
                    s.push_str("{\"fileName\": \"f.jpg\", \"contentType\": \"image/jpeg\"},");
                }
                s.push_str("null]}");
                s.into_bytes()
            },
        ),
        case(
            "huge body string",
            format!(
                "[{{\"type\": \"incoming\", \"sent_at\": 1, \"body\": \"{}\"}}]",
                "x".repeat(2_000_000)
            )
            .into_bytes(),
        ),
        case(
            "duplicate keys, last wins",
            b"[{\"type\": \"call-history\", \"type\": \"incoming\", \"sent_at\": 1}]",
        ),
        case(
            "array with trailing comma",
            b"[{\"type\": \"incoming\", \"sent_at\": 1},]",
        ),
        case(
            "device suffix shapes in source",
            b"[{\"type\": \"incoming\", \"source\": \"+385912345678.2\", \"sent_at\": 1}, {\"type\": \"incoming\", \"source\": \"a.b.c\", \"sent_at\": 1}, {\"type\": \"incoming\", \"source\": \"\", \"sent_at\": 1}]",
        ),
        case("only a BOM", "\u{feff}".as_bytes()),
    ];
    let mut tr = truncations("signal export", &valid_signal());
    cases.append(&mut tr);
    cases
}

/// Cross-format mixups for format detection: each format's bytes glued to
/// or wrapped inside another's.
pub fn mixed_format_cases() -> Cases {
    let cases = vec![
        case(
            "whatsapp header inside json string",
            br#"{"messages": ["[1.11.2026., 09:00:05] Marko: Zdravo!"]}"#,
        ),
        case(
            "json inside whatsapp export",
            b"[1.11.2026., 09:00:05] A: {\"messages\": [{\"id\": 1}]}\n{\"messages\": [{\"id\": 2}]}\n",
        ),
        case(
            "mbox From line before whatsapp headers",
            b"From a@b.c\n[1.11.2026., 09:00] A: hi\n",
        ),
        case(
            "json with BOM then whatsapp",
            b"\xef\xbb\xbf{\"messages\":[]}\n[1.11.2026., 09:00] A: hi\n",
        ),
        case(
            "whatsapp stamp inside mbox body",
            b"From a@b.c\nSubject: [1.11.2026., 09:00] A: hi\n\n[6/28/23, 9:30 AM] B: yo\n",
        ),
        case(
            "empty json object then whatsapp",
            b"{}\n[1.11.2026., 09:00] A: hi\n",
        ),
        case(
            "truncated json that opens like telegram",
            b"{\"name\": \"Arhiva\", \"messages\": [",
        ),
        case("all three glued", {
            let mut v = valid_whatsapp();
            v.extend_from_slice(&valid_telegram());
            v.extend_from_slice(&valid_discord());
            v.extend_from_slice(&valid_mbox());
            v
        }),
        case(
            "signal array with telegram-shaped element inside",
            b"[{\"messages\": [{\"id\": 1, \"type\": \"message\", \"text\": \"x\"}], \"type\": \"incoming\", \"sent_at\": 1}]",
        ),
        case(
            "telegram export with signal-shaped message fields",
            b"{\"name\": \"A\", \"messages\": [{\"id\": 1, \"type\": \"incoming\", \"sent_at\": 1000, \"body\": \"x\"}]}",
        ),
        case(
            "whatsapp stamp inside signal body",
            b"[{\"type\": \"incoming\", \"sent_at\": 1000, \"body\": \"[1.11.2026., 09:00] A: hi\"}]",
        ),
        case(
            "signal array after json object glued",
            b"{}[{\"type\": \"incoming\", \"sent_at\": 1000, \"body\": \"x\"}]",
        ),
        case(
            "discord channel object inside telegram export",
            br#"{"channel": {"id": "1"}, "name": "Arhiva", "messages": [{"id": "1", "type": "message"}]}"#,
        ),
        case(
            "telegram messages array with non-object channel member",
            br#"{"messages": [{"id": 1, "type": "message"}], "channel": "not an object"}"#,
        ),
        case("all five glued", {
            let mut v = valid_whatsapp();
            v.extend_from_slice(&valid_telegram());
            v.extend_from_slice(&valid_signal());
            v.extend_from_slice(&valid_discord());
            v.extend_from_slice(&valid_mbox());
            v
        }),
    ];
    cases
}

/// A realistic Signal conversation export used as the truncation seed:
/// a top-level array of raw Signal Desktop message objects.
fn valid_signal() -> Vec<u8> {
    r#"[
  {"id": "0ea0b5fd-9f2a-4c73-8f8e-2f8ce6f22a11", "type": "incoming", "conversationId": "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01", "source": "+385912345678.2", "sent_at": 1793525400000, "timestamp": 1793525400000, "body": "Zdravo ekipa! 👋"},
  {"id": "3c7b5d3a-2f1e-4a6b-8c9d-0e1f2a3b4c5d", "type": "outgoing", "conversationId": "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01", "sent_at": 1793525460000, "body": "vidimo se"},
  {"type": "call-history", "conversationId": "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01", "callId": "4f6e8a2b", "sent_at": 1793525500000},
  {"id": "5d9c1e4f-6a7b-4c8d-9e0f-1a2b3c4d5e6f", "type": "incoming", "conversationId": "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01", "source": "+385912345678", "sent_at": 1793525550000, "attachments": [{"fileName": "signal-2026-11-01-110050.jpg", "contentType": "image/jpeg", "size": 284119}]}
]"#
    .as_bytes()
    .to_vec()
}

/// A properly closed `depth`-deep nested multipart mail behind a From
/// line: the hostile shape that used to overflow mailparse's recursion.
pub fn nested_multipart(depth: usize) -> Vec<u8> {
    let mut s = String::from("From a@b.c\n");
    for d in 0..depth {
        s.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"b{d}\"\n\n--b{d}\n"
        ));
    }
    s.push_str("Content-Type: text/plain\n\nhi\n");
    for d in (0..depth).rev() {
        s.push_str(&format!("--b{d}--\n"));
    }
    s.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_is_nonempty_and_unique() {
        for (name, cases) in [
            ("whatsapp", whatsapp_cases()),
            ("telegram", telegram_cases()),
            ("signal", signal_cases()),
            ("discord", discord_cases()),
            ("mbox", mbox_cases()),
            ("mixed", mixed_format_cases()),
        ] {
            assert!(!cases.is_empty(), "{name} corpus must not be empty");
            let mut descs: Vec<&str> = cases.iter().map(|(d, _)| d.as_str()).collect();
            descs.sort_unstable();
            let n = descs.len();
            descs.dedup();
            assert_eq!(descs.len(), n, "{name} corpus has duplicate descriptions");
            for (desc, bytes) in &cases {
                // The 100k-deep hostile multipart (~6.4 MB, generated at
                // runtime) sets the ceiling; everything else stays far
                // below it.
                assert!(bytes.len() <= 8_000_000, "{desc} absurdly large");
            }
        }
    }
}
