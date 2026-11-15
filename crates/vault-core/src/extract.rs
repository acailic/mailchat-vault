//! Local link extraction over archived message bodies: a hand-rolled
//! http(s) scanner, deterministic and dependency-free. Part of the
//! ask-collect-links feature (see
//! docs/superpowers/specs/2026-11-14-ask-collect-links-design.md).

use serde::{Deserialize, Serialize};

use crate::message::Message;

/// Stop scanning once this many unique URLs are collected.
pub const MAX_COLLECTED_LINKS: usize = 200;
/// How far the context snippet reaches around the URL, each side (chars).
const CONTEXT_RADIUS: usize = 60;
/// Longest URL accepted (guards against pathological matches).
const MAX_URL_LEN: usize = 2048;

/// One occurrence of a URL inside one message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkHit {
    pub message_id: i64,
    pub author: String,
    pub sent_at: String,
    /// Up to ~60 chars around the URL, whitespace collapsed.
    pub context: String,
}

/// A URL deduplicated across messages, with every hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectedLink {
    pub url: String,
    pub hits: Vec<LinkHit>,
}

/// Characters that end a URL mid-text (whitespace also ends it).
const END_CHARS: &[char] = &['"', '\'', '<', '>'];
/// Trailing prose punctuation stripped off a scanned URL.
const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', '"', '\''];

/// Next `http://`/`https://` at or after `from` (case-insensitive):
/// byte index of the scheme start and of the `://` end.
///
/// The fixed-width probes compare raw byte slices — `&[u8]` has no
/// char-boundary rule — so a multi-byte char straddling the probe width
/// cannot panic; boundary alignment is not what makes them safe. A match
/// is pure ASCII, so `at + len` is a valid char boundary for callers.
fn find_scheme(body: &str, from: usize) -> Option<(usize, usize)> {
    let bytes = body.as_bytes();
    let mut at = from;
    while at + 7 <= bytes.len() {
        if !body.is_char_boundary(at) {
            at += 1;
            continue;
        }
        let rest = &bytes[at..];
        let len = if rest.len() >= 8 && rest[..8].eq_ignore_ascii_case(b"https://") {
            8
        } else if rest[..7].eq_ignore_ascii_case(b"http://") {
            7
        } else {
            at += 1;
            continue;
        };
        return Some((at, at + len));
    }
    None
}

/// Context around `start..end` in `body`: ±60 chars on char boundaries,
/// whitespace runs collapsed to single spaces.
fn context_around(body: &str, start: usize, end: usize) -> String {
    let chars: Vec<(usize, char)> = body.char_indices().collect();
    let pos = |byte| chars.partition_point(|(b, _)| *b < byte);
    let (lo, hi) = (pos(start), pos(end));
    let from = lo.saturating_sub(CONTEXT_RADIUS);
    let to = (hi + CONTEXT_RADIUS).min(chars.len());
    chars[from..to]
        .iter()
        .map(|(_, c)| *c)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Scan one body for URLs: `(url, start, end)` triples in order.
fn scan_urls(body: &str) -> Vec<(String, usize, usize)> {
    let mut found = Vec::new();
    let mut search_from = 0;
    while let Some((scheme_at, scheme_end)) = find_scheme(body, search_from) {
        search_from = scheme_end;
        let rest = &body[scheme_end..];
        let take = rest
            .find(|c: char| c.is_whitespace() || END_CHARS.contains(&c))
            .unwrap_or(rest.len());
        let mut url_end = scheme_end + take;
        let mut url = body[scheme_at..url_end].to_string();
        // Strip trailing prose punctuation; a closing paren only when the
        // URL opened none (unbalanced = prose, balanced = part of the URL).
        loop {
            let trimmed = url.trim_end_matches(|c| TRAILING.contains(&c));
            let trimmed = trimmed
                .strip_suffix(')')
                .filter(|_| !trimmed.contains('('))
                .map(str::to_string)
                .unwrap_or_else(|| trimmed.to_string());
            if trimmed.len() == url.len() {
                break;
            }
            url = trimmed;
        }
        url_end = scheme_at + url.len();
        // The scan only yields URLs that start with the scheme, so `find`
        // always succeeds; the host is whatever follows `://`.
        let has_host = url.len() > url.find("://").expect("url starts with scheme") + 3;
        if has_host && url.len() <= MAX_URL_LEN {
            found.push((url, scheme_at, url_end));
            search_from = url_end;
        }
    }
    found
}

/// Collect http(s) links from `messages` (rank order), deduplicated by
/// exact URL string. Stops entirely once [`MAX_COLLECTED_LINKS`] unique
/// URLs are collected.
pub fn extract_links(messages: &[Message]) -> Vec<CollectedLink> {
    let mut collected: Vec<CollectedLink> = Vec::new();
    'messages: for msg in messages {
        for (url, start, end) in scan_urls(&msg.body) {
            let hit = LinkHit {
                message_id: msg.id,
                author: msg.author.clone(),
                sent_at: msg.sent_at.clone(),
                context: context_around(&msg.body, start, end),
            };
            if let Some(existing) = collected.iter_mut().find(|l| l.url == url) {
                existing.hits.push(hit);
            } else {
                if collected.len() >= MAX_COLLECTED_LINKS {
                    break 'messages;
                }
                collected.push(CollectedLink {
                    url,
                    hits: vec![hit],
                });
            }
        }
    }
    collected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: i64, author: &str, sent_at: &str, body: &str) -> Message {
        Message {
            id,
            source: "test".into(),
            source_key: String::new(),
            external_id: format!("e{id}"),
            channel: "email".into(),
            chat_id: String::new(),
            author: author.into(),
            sent_at: sent_at.into(),
            body: body.into(),
            raw_json: "{}".into(),
        }
    }

    #[test]
    fn extracts_plain_urls_with_sender_and_context() {
        let out = extract_links(&[msg(
            1,
            "Ana <ana@example.com>",
            "2026-10-01T10:00:00Z",
            "see https://example.com/a for the draft",
        )]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "https://example.com/a");
        assert_eq!(out[0].hits.len(), 1);
        assert_eq!(out[0].hits[0].message_id, 1);
        assert_eq!(out[0].hits[0].author, "Ana <ana@example.com>");
        assert_eq!(out[0].hits[0].sent_at, "2026-10-01T10:00:00Z");
        assert!(
            out[0].hits[0].context.contains("see"),
            "{}",
            out[0].hits[0].context
        );
        assert!(out[0].hits[0].context.contains("for the draft"));
    }

    #[test]
    fn scheme_match_is_case_insensitive() {
        let out = extract_links(&[msg(
            1,
            "a",
            "2026-01-01T00:00:00Z",
            "go to HTTPS://Example.COM/x now",
        )]);
        assert_eq!(out.len(), 1);
        // The URL is preserved verbatim beyond the scheme.
        assert_eq!(out[0].url, "HTTPS://Example.COM/x");
    }

    #[test]
    fn dedups_by_exact_url_across_messages() {
        let out = extract_links(&[
            msg(1, "ana", "2026-01-01T00:00:00Z", "https://example.com/a"),
            msg(
                2,
                "bo",
                "2026-01-02T00:00:00Z",
                "first https://example.com/a again",
            ),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].hits.len(), 2);
        assert_eq!(out[0].hits[0].message_id, 1);
        assert_eq!(out[0].hits[1].message_id, 2);
    }

    #[test]
    fn strips_trailing_punctuation_and_unbalanced_paren() {
        let out = extract_links(&[msg(
            1,
            "a",
            "2026-01-01T00:00:00Z",
            "one https://example.com/a, two https://example.com/b).",
        )]);
        assert_eq!(out[0].url, "https://example.com/a");
        assert_eq!(out[1].url, "https://example.com/b");
    }

    #[test]
    fn keeps_balanced_parens_inside_the_url() {
        let out = extract_links(&[msg(
            1,
            "a",
            "2026-01-01T00:00:00Z",
            "https://en.wikipedia.org/wiki/Foo_(bar) end",
        )]);
        assert_eq!(out[0].url, "https://en.wikipedia.org/wiki/Foo_(bar)");
    }

    #[test]
    fn stops_at_whitespace_and_quotes() {
        let out = extract_links(&[msg(
            1,
            "a",
            "2026-01-01T00:00:00Z",
            "<a href=\"https://example.com/x\">link</a>",
        )]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "https://example.com/x");
    }

    #[test]
    fn context_is_bounded_and_whitespace_collapsed() {
        let out = extract_links(&[msg(
            1,
            "a",
            "2026-01-01T00:00:00Z",
            &format!(
                "{} https://example.com/a {}",
                "w".repeat(200),
                "n".repeat(200)
            ),
        )]);
        let ctx = &out[0].hits[0].context;
        assert!(ctx.chars().count() <= 141, "{ctx}"); // 60 + url + 60 (+ellipses)
        assert!(!ctx.contains("\n"));
    }

    #[test]
    fn caps_unique_urls_at_max() {
        let body = (0..MAX_COLLECTED_LINKS + 10)
            .map(|i| format!("https://example.com/{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let out = extract_links(&[msg(1, "a", "2026-01-01T00:00:00Z", &body)]);
        assert_eq!(out.len(), MAX_COLLECTED_LINKS);
    }

    #[test]
    fn requires_a_host_character() {
        let out = extract_links(&[msg(
            1,
            "a",
            "2026-01-01T00:00:00Z",
            "empty https:// and https://x ok",
        )]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "https://x");
    }

    #[test]
    fn multibyte_char_before_url_does_not_panic() {
        let out = extract_links(&[msg(
            1,
            "a",
            "2026-01-01T00:00:00Z",
            "Café vrati https://example.com/x pdf",
        )]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "https://example.com/x");
    }

    #[test]
    fn multibyte_char_after_url_does_not_panic() {
        let out = extract_links(&[msg(
            1,
            "a",
            "2026-01-01T00:00:00Z",
            "see https://example.com/x café und noch mehr text danach",
        )]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "https://example.com/x");
    }

    #[test]
    fn multibyte_char_straddling_probe_width_no_panic() {
        // '九' is 3 bytes: byte 8 of the probe window lands mid-character.
        let out = extract_links(&[msg(
            1,
            "a",
            "2026-01-01T00:00:00Z",
            "a九九九九 https://example.com/x",
        )]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "https://example.com/x");
    }

    #[test]
    fn emoji_straddling_probe_width_no_panic() {
        // '😀' is 4 bytes: byte 7 of the probe window lands mid-character.
        let out = extract_links(&[msg(
            1,
            "a",
            "2026-01-01T00:00:00Z",
            "😀😀😀 https://example.com/x",
        )]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "https://example.com/x");
    }
}
