//! M4.3: the `mcmedia://` read-only media protocol and its pure logic.
//!
//! The reading pane renders preserved media (`media/<sha256>.<ext>`
//! objects of the M4.1 content-addressed store) through this custom
//! URI scheme — never through the file system, a webview file mount,
//! or a remote URL. The handler is deliberately narrow:
//!
//! * **one object per URL** — the only accepted path shape is
//!   `mcmedia://local/<64-hex>.<ext>`, where the component is validated
//!   exactly like an object-store file name ([`object_name_of`]):
//!   traversal segments, nested paths, and any other shape are a 404
//!   before the file system is ever touched;
//! * **byte-range requests** ([`parse_range`]) answer `206 Partial
//!   Content` with an exact slice, so `<video>`/`<audio>` can seek and
//!   stream arbitrarily large files, and a range read never pulls more
//!   than the requested bytes through memory;
//! * **hard response CSP** ([`MEDIA_RESPONSE_CSP`]) — every response,
//!   whatever the served bytes contain, carries
//!   `Content-Security-Policy: default-src 'none'`, and HTML is never
//!   a servable content type ([`content_type_for`]), so active content
//!   smuggled into a stored object cannot execute even if something
//!   managed to navigate to it;
//! * **asynchronous serving** — the handler parses and immediately
//!   parks the read on the blocking pool
//!   (`register_asynchronous_uri_scheme_protocol` +
//!   `spawn_blocking`), so no media read can freeze the UI thread.

use std::path::{Path, PathBuf};

/// The CSP stamped on every `mcmedia://` response: the media bytes are
/// data, never a document — no origin may run scripts, load
/// subresources, or frame the response.
pub const MEDIA_RESPONSE_CSP: &str = "default-src 'none'; sandbox";

/// The accepted `<64-hex>.<ext>` object file name — validated exactly
/// like the store's own file names (`object_store::object_digest`
/// shape), and additionally required to be a single path component.
/// Anything else (`..`, nested paths, empty, odd shapes) is `None`.
pub fn object_name_of(uri_path: &str) -> Option<String> {
    let path = uri_path.strip_prefix('/')?;
    // Exactly one component — a nested path is never an object name.
    if path.contains('/') || path.contains('\\') || path.is_empty() {
        return None;
    }
    // Percent-decoding is deliberately NOT applied: the store only has
    // literal hex names, so an encoded traversal (`%2e%2e`) simply does
    // not name any object.
    let (stem, ext) = path.rsplit_once('.')?;
    let stem_ok = stem.len() == 64 && stem.chars().all(|c| c.is_ascii_hexdigit());
    let ext_ok =
        !ext.is_empty() && ext.len() <= 16 && ext.chars().all(|c| c.is_ascii_alphanumeric());
    (stem_ok && ext_ok).then(|| path.to_ascii_lowercase())
}

/// A parsed single-range `Range: bytes=…` header against a body of
/// `len` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// The header is absent or unusable (`bytes=` with neither bound) —
    /// serve the whole body with `200`.
    Full,
    /// `bytes=start-end` inclusive, or the open `bytes=start-`.
    Partial { start: u64, end: u64 },
    /// The start is at or past the end of the body — `416`.
    Unsatisfiable,
}

/// Parse a single-range `Range` header (`bytes=first-last`,
/// `bytes=first-`, `bytes=-suffix`). Multi-range requests are treated
/// as [`ByteRange::Full`]: engines that need byte ranges issue
/// single-range requests, and a full response is always a correct —
/// if less efficient — answer.
pub fn parse_range(header: Option<&str>, len: u64) -> ByteRange {
    let Some(header) = header else {
        return ByteRange::Full;
    };
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return ByteRange::Full;
    };
    if spec.contains(',') {
        return ByteRange::Full;
    }
    let Some((first, last)) = spec.split_once('-') else {
        return ByteRange::Full;
    };
    match (first.trim().is_empty(), last.trim().is_empty()) {
        // `bytes=-N`: the final N bytes (0 when N ≥ len).
        (true, false) => {
            let Ok(n) = last.trim().parse::<u64>() else {
                return ByteRange::Full;
            };
            if len == 0 || n == 0 {
                return ByteRange::Unsatisfiable;
            }
            let n = n.min(len);
            ByteRange::Partial {
                start: len - n,
                end: len - 1,
            }
        }
        // `bytes=N-`: from N to the end.
        (false, true) => {
            let Ok(start) = first.trim().parse::<u64>() else {
                return ByteRange::Full;
            };
            if len == 0 || start >= len {
                return ByteRange::Unsatisfiable;
            }
            ByteRange::Partial {
                start,
                end: len - 1,
            }
        }
        // `bytes=N-M`: inclusive bounds; end is clamped to the last
        // byte (RFC 9110 lets a server shorten an over-long end).
        (false, false) => {
            let (Ok(start), Ok(end)) = (first.trim().parse::<u64>(), last.trim().parse::<u64>())
            else {
                return ByteRange::Full;
            };
            if len == 0 || start >= len || start > end {
                return ByteRange::Unsatisfiable;
            }
            ByteRange::Partial {
                start,
                end: end.min(len - 1),
            }
        }
        // `bytes=-`: neither bound — unusable.
        (true, true) => ByteRange::Full,
    }
}

/// The content type served for an object. Deliberately a closed list
/// of passive media/container types: HTML and every other
/// document/active type is `application/octet-stream`, which a webview
/// renders as a download, never as a page — active content cannot be
/// served, by omission rather than by policy alone.
pub fn content_type_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml", // images only; CSP default-src 'none' keeps it inert
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "3gp" => "video/3gpp",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "amr" => "audio/amr",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Resolve a validated object name to its store path (the caller
/// guarantees the name shape; this only joins).
pub fn store_path(data_dir: &Path, name: &str) -> PathBuf {
    data_dir.join("media").join(name)
}

/// A single safe file-name component for an export/external-open
/// suggestion: the last path component of the display name, minus
/// separators and NUL, kept short. Falls back to the object's own
/// store name when the display name has no usable component.
pub fn safe_display_name(display: &str, fallback: &str) -> String {
    let last = display.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control() && *c != '\0')
        .take(120)
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn object_name_accepts_only_store_shapes() {
        assert_eq!(
            object_name_of(&format!("/{SHA}.jpg")),
            Some(format!("{SHA}.jpg"))
        );
        // Uppercase hex normalizes to the store's lowercase names.
        let upper = format!("/{}.PNG", SHA.to_ascii_uppercase());
        assert_eq!(object_name_of(&upper), Some(format!("{SHA}.png")));
        // Traversal and nesting never name an object.
        assert_eq!(object_name_of(&format!("/../{}.jpg", SHA)), None);
        assert_eq!(object_name_of(&format!("/media/{}.jpg", SHA)), None);
        assert_eq!(object_name_of(&format!("/{}", SHA)), None); // no extension
        assert_eq!(object_name_of(&format!("/{}.jpg/", SHA)), None);
        assert_eq!(object_name_of(&format!("/{}.a.very/long", SHA)), None);
        assert_eq!(object_name_of("/notes.txt"), None);
        assert_eq!(object_name_of(""), None);
        assert_eq!(object_name_of("/"), None);
        assert_eq!(
            object_name_of(&format!("/%2e%2e/{}.jpg", SHA)),
            None,
            "encoded traversal is not decoded into a name"
        );
    }

    #[test]
    fn parse_range_covers_rfc_shapes() {
        use ByteRange::*;
        let len = 100;
        assert_eq!(parse_range(None, len), Full);
        assert_eq!(parse_range(Some("items=0-9"), len), Full);
        assert_eq!(
            parse_range(Some("bytes=0-9"), len),
            Partial { start: 0, end: 9 }
        );
        assert_eq!(
            parse_range(Some("bytes=90-"), len),
            Partial { start: 90, end: 99 }
        );
        assert_eq!(
            parse_range(Some("bytes=90-500"), len),
            Partial { start: 90, end: 99 },
            "end clamps to len-1"
        );
        assert_eq!(
            parse_range(Some("bytes=-5"), len),
            Partial { start: 95, end: 99 }
        );
        assert_eq!(
            parse_range(Some("bytes=-500"), len),
            Partial { start: 0, end: 99 },
            "oversized suffix clamps to whole body"
        );
        assert_eq!(
            parse_range(Some("bytes=-0"), len),
            Unsatisfiable,
            "empty suffix range names no bytes"
        );
        assert_eq!(parse_range(Some("bytes=100-"), len), Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=100-150"), len), Unsatisfiable);
        assert_eq!(
            parse_range(Some("bytes=50-10"), len),
            Unsatisfiable,
            "reversed bounds"
        );
        assert_eq!(
            parse_range(Some("bytes=0-9,20-29"), len),
            Full,
            "multi-range falls back to full"
        );
        assert_eq!(parse_range(Some("bytes=-"), len), Full);
        assert_eq!(
            parse_range(Some("bytes=abc"), len),
            Full,
            "garbage is treated as absent"
        );
        // Empty body: any bounded range is unsatisfiable.
        assert_eq!(parse_range(Some("bytes=0-"), 0), Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-1"), 0), Unsatisfiable);
        assert_eq!(
            parse_range(None, 0),
            Full,
            "absent header still serves a (zero-byte) 200"
        );
    }

    #[test]
    fn content_types_are_passive_only() {
        assert_eq!(content_type_for(&format!("{SHA}.jpg")), "image/jpeg");
        assert_eq!(content_type_for(&format!("{SHA}.mp4")), "video/mp4");
        assert_eq!(content_type_for(&format!("{SHA}.opus")), "audio/ogg");
        assert_eq!(content_type_for(&format!("{SHA}.pdf")), "application/pdf");
        assert_eq!(
            content_type_for(&format!("{SHA}.html")),
            "application/octet-stream",
            "HTML is never served as a page"
        );
        assert_eq!(content_type_for(&format!("{SHA}.svg")), "image/svg+xml");
    }

    #[test]
    fn display_names_collapse_to_one_component() {
        assert_eq!(safe_display_name("photo.jpg", "x.jpg"), "photo.jpg");
        assert_eq!(safe_display_name("../../etc/passwd", "x"), "passwd");
        assert_eq!(safe_display_name("a/b\\c.png", "x.png"), "c.png");
        assert_eq!(safe_display_name("  ..hidden..  ", "x.bin"), "hidden..");
        assert_eq!(
            safe_display_name("", &format!("{SHA}.bin")),
            format!("{SHA}.bin")
        );
        assert_eq!(
            safe_display_name("///", &format!("{SHA}.bin")),
            format!("{SHA}.bin")
        );
    }
}
