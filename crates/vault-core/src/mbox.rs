//! `.mbox` file source: a manual/demo import that keeps the ingest line
//! executable without any IMAP access.
//!
//! Supports the common `mboxo`/`mboxrd` shape: messages separated by a line
//! starting with `From ` (sender + date), with bodies escaping embedded
//! `From ` lines as `>From ` (the escaped form does not split a message).
//! A file that does not lead with `From ` is a bare RFC 5322 message
//! (`.eml`): the whole file is one message.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use crate::bounded::{EntryOutcome, Flow, StreamItem, StreamSource};
use crate::ingest::{Collected, Source};
use crate::mail::parse_raw_mail;
use crate::message::NewMessage;
use crate::{Error, Result};

/// Source name and dedup namespace for mbox imports.
pub const MBOX_SOURCE: &str = "mbox";

/// Reads one `.mbox` file and yields its messages.
#[derive(Debug, Clone)]
pub struct MboxSource {
    path: PathBuf,
}

impl MboxSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Source for MboxSource {
    fn name(&self) -> &str {
        MBOX_SOURCE
    }

    fn collect(&self) -> Result<Collected> {
        let bytes = fs::read(&self.path)?;
        let chat_id = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        parse_mbox(&bytes, &chat_id)
    }
}

/// The bounded streaming path (M2.2): the file is walked line by line,
/// one message's bytes alive at a time — a `From ` separator line flushes
/// the pending message to the consumer. Peak memory is one message,
/// whatever the file's size. (A single oversized message is a capacity
/// question the parser surfaces as-is, never truncation.)
impl StreamSource for MboxSource {
    fn name(&self) -> &str {
        MBOX_SOURCE
    }

    fn stream(&self, skip: u64, emit: &mut dyn FnMut(StreamItem) -> Result<Flow>) -> Result<()> {
        let chat_id = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        stream_mbox(
            BufReader::new(fs::File::open(&self.path)?),
            &chat_id,
            skip,
            emit,
        )
    }
}

/// A picked directory of `.eml` files (e.g. messages dragged out of
/// Outlook): every `*.eml` imports as one mail, sorted by file name.
/// Unreadable or unparseable files count as failed; the run continues.
#[derive(Debug, Clone)]
pub struct EmlDirSource {
    path: PathBuf,
}

impl EmlDirSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The `*.eml` files of the directory, sorted by name (single level —
    /// the same flat-listing contract as the Discord channel files).
    fn files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.path)? {
            let path = entry?.path();
            let is_eml = path.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("eml"));
            if is_eml {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }
}

/// Parse one `.eml` file's bytes. The RFC 5322 parser is lenient — it
/// tolerates a headerless blob as "a mail with nothing in it" — so a
/// result with no recognizable mail content at all (no author, no
/// subject, no body: binary junk wearing the `.eml` extension) counts
/// as failed, exactly like a hard parse error.
fn parse_eml(bytes: &[u8]) -> Option<NewMessage> {
    parse_raw_mail(bytes, MBOX_SOURCE, "")
        .filter(|msg| !msg.author.is_empty() || !msg.body.is_empty())
}

impl Source for EmlDirSource {
    fn name(&self) -> &str {
        MBOX_SOURCE
    }

    fn collect(&self) -> Result<Collected> {
        let mut messages = Vec::new();
        let mut failed: u64 = 0;
        for file in self.files()? {
            let bytes = fs::read(&file)?;
            match parse_eml(&bytes) {
                Some(msg) => messages.push(msg),
                None => failed += 1,
            }
        }
        if messages.is_empty() && failed == 0 {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "no .eml files found under {} — pick the folder that holds them",
                    self.path.display()
                ),
            )));
        }
        Ok(Collected { messages, failed })
    }
}

/// The bounded streaming path: files are read one at a time (sorted, as
/// the collect path is), one file's bytes alive at a time. An unreadable
/// or unparseable file is a `Failed` item naming the file; the run
/// continues with the next one.
impl StreamSource for EmlDirSource {
    fn name(&self) -> &str {
        MBOX_SOURCE
    }

    fn stream(&self, skip: u64, emit: &mut dyn FnMut(StreamItem) -> Result<Flow>) -> Result<()> {
        let files = self.files()?;
        let mut failure: Option<crate::Error> = None;
        {
            let mut state = crate::bounded::ConsumerState {
                skip,
                emit,
                stopped: false,
                failure: &mut failure,
            };
            for file in &files {
                let outcome = match fs::read(file) {
                    Ok(bytes) => match parse_eml(&bytes) {
                        Some(msg) => EntryOutcome::Message(msg),
                        None => EntryOutcome::Failed {
                            external_ref: String::new(),
                            reason: "mail part failed to parse".to_string(),
                        },
                    },
                    Err(e) => EntryOutcome::Failed {
                        external_ref: file.display().to_string(),
                        reason: format!("unreadable file: {e}"),
                    },
                };
                if !state.offer(outcome) {
                    break;
                }
            }
        }
        if let Some(e) = failure {
            return Err(e);
        }
        Ok(())
    }
}

/// Parse raw `.mbox` file contents into messages.
///
/// `chat_id` groups the messages (typically the file name stem). Message
/// parsing itself lives in [`crate::mail`] and is shared with the IMAP
/// source. Mails that fail to parse are counted in `Collected::failed`
/// rather than silently dropped.
pub fn parse_mbox(bytes: &[u8], chat_id: &str) -> Result<Collected> {
    let mut messages = Vec::new();
    let mut failed: u64 = 0;
    stream_mbox(std::io::Cursor::new(bytes), chat_id, 0, &mut |item| {
        match item {
            StreamItem::Message(msg) => messages.push(msg),
            StreamItem::Failed { .. } => failed += 1,
        }
        Ok(Flow::Continue)
    })?;
    Ok(Collected { messages, failed })
}

/// The shared streaming engine behind the whole-file parser and the
/// streaming source. A file leading with a `From ` separator line is
/// mbox: separator lines split the stream into messages (escaped
/// `>From ` lines do not), one message buffered at a time; parts
/// before the first separator and separator lines with no content are
/// skipped silently — exactly the whole-file splitter's semantics,
/// item for item. Any other file is a bare RFC 5322 message (.eml):
/// the whole file is one message, `From ` lines in its body included
/// as body text, not separators.
pub fn stream_mbox<R: BufRead>(
    reader: R,
    chat_id: &str,
    skip: u64,
    emit: &mut dyn FnMut(StreamItem) -> Result<Flow>,
) -> Result<()> {
    let mut skip = skip;
    let mut reader = BufReader::new(reader);
    let mut part: Vec<u8> = Vec::new();
    let mut started = false;
    let mut failure: Option<crate::Error> = None;
    let mut stopped = false;
    let mut line = Vec::new();
    // Pre-read the first line: a file that does not lead with a `From `
    // separator line is a bare RFC 5322 message (.eml) — the whole file
    // is one message, and `From ` lines inside its body are body text,
    // not separators (legal unescaped in .eml; an .mbox always leads
    // with `From `).
    let mut first: Vec<u8> = Vec::new();
    if reader.read_until(b'\n', &mut first)? == 0 {
        return Ok(()); // empty file: nothing to import
    }
    if !first.starts_with(b"From ") {
        let mut whole = first;
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            whole.extend_from_slice(&line);
        }
        offer_part(
            &whole,
            chat_id,
            &mut skip,
            emit,
            &mut failure,
            &mut stopped,
            false,
        );
        if let Some(e) = failure {
            return Err(e);
        }
        return Ok(());
    }
    let mut pending = Some(first);
    loop {
        line.clear();
        if let Some(first) = pending.take() {
            line = first;
        } else if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        let at_line_start = part.is_empty() || *part.last().unwrap() == b'\n';
        if at_line_start && line.starts_with(b"From ") {
            if started
                && !offer_part(
                    &part,
                    chat_id,
                    &mut skip,
                    emit,
                    &mut failure,
                    &mut stopped,
                    true,
                )
            {
                break;
            }
            started = true;
            part = std::mem::take(&mut line);
            continue;
        }
        if started {
            part.extend_from_slice(&line);
        }
        // Bytes before the first separator line are dropped (the
        // whole-file splitter never emitted them either).
    }
    if started && failure.is_none() && !stopped {
        offer_part(
            &part,
            chat_id,
            &mut skip,
            emit,
            &mut failure,
            &mut stopped,
            true,
        );
    }
    if let Some(e) = failure {
        return Err(e);
    }
    Ok(())
}

/// Parse one buffered part and offer it to the consumer; `false` = stop
/// reading (the stop/failure slots record why).
fn offer_part(
    part: &[u8],
    chat_id: &str,
    skip: &mut u64,
    emit: &mut dyn FnMut(StreamItem) -> Result<Flow>,
    failure: &mut Option<crate::Error>,
    stopped: &mut bool,
    separator: bool,
) -> bool {
    // A `From sender date` separator line heads the part; drop it. A
    // bare .eml part (no separator) parses whole.
    let raw = if separator {
        match part.iter().position(|&b| b == b'\n') {
            Some(nl) => &part[nl + 1..],
            None => return true, // separator line with no content: skipped
        }
    } else {
        part
    };
    if raw.is_empty() {
        return true;
    }
    let outcome = match parse_raw_mail(raw, MBOX_SOURCE, chat_id) {
        Some(msg) => EntryOutcome::Message(msg),
        None => EntryOutcome::Failed {
            external_ref: String::new(),
            reason: "mail part failed to parse".to_string(),
        },
    };
    match crate::bounded::emit_entry(outcome, skip, emit) {
        Ok(Flow::Continue) => true,
        Ok(Flow::Stop) => {
            *stopped = true;
            false
        }
        Err(e) => {
            *failure = Some(e);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `depth`-deep nested multipart mail, properly closed.
    fn nested_multipart(depth: usize) -> Vec<u8> {
        let mut s = String::new();
        for d in 0..depth {
            s.push_str(&format!(
                "Content-Type: multipart/mixed; boundary=\"b{d}\"\n\n"
            ));
            s.push_str(&format!("--b{d}\n"));
        }
        s.push_str("Content-Type: text/plain\n\nhi\n");
        for d in (0..depth).rev() {
            s.push_str(&format!("--b{d}--\n"));
        }
        s.into_bytes()
    }

    #[test]
    fn hostile_deep_multipart_is_failed_not_a_crash() {
        // Regression: before the depth guard this overflowed the stack in
        // mailparse's recursive multipart descent (SIGABRT, not an error).
        for depth in [2000usize, 20_000, 100_000] {
            let mut mail = nested_multipart(depth);
            let bytes = [b"From a@b.c\n".as_slice(), &mail].concat();
            mail.clear();
            let collected = parse_mbox(&bytes, "f").unwrap();
            assert_eq!(collected.messages.len(), 0, "depth {depth}");
            assert_eq!(collected.failed, 1, "depth {depth}");
        }
    }

    #[test]
    fn moderate_nesting_still_parses() {
        let mail = nested_multipart(8);
        let bytes = [b"From a@b.c\n".as_slice(), &mail].concat();
        let collected = parse_mbox(&bytes, "f").unwrap();
        assert_eq!(collected.failed, 0);
        assert_eq!(collected.messages.len(), 1);
        assert_eq!(collected.messages[0].body, "hi");
    }

    const EML: &[u8] = b"From: ana@example.com\nTo: bane@example.com\nSubject: Zdravo\nMessage-ID: <eml-1@example.com>\nDate: Fri, 13 Nov 2026 10:00:00 +0100\n\nPozdrav iz .eml datoteke.\n";

    #[test]
    fn bare_rfc822_message_parses_as_one_email() {
        // No leading `From ` separator: the file is a single .eml.
        let collected = parse_mbox(EML, "outlook").unwrap();
        assert_eq!(collected.failed, 0);
        assert_eq!(collected.messages.len(), 1);
        assert!(collected.messages[0].body.contains("Pozdrav iz"));
        assert!(collected.messages[0].author.contains("ana@example.com"));
    }

    #[test]
    fn unescaped_from_line_in_bare_message_is_body_text() {
        // In .eml a body line may start with `From ` — it must not split
        // the message.
        let mail = b"Subject: eml\nMessage-ID: <e2@example.com>\n\nline one\nFrom the desk of Ana\nline two\n";
        let collected = parse_mbox(mail, "f").unwrap();
        assert_eq!(collected.messages.len(), 1);
        assert!(collected.messages[0].body.contains("From the desk of Ana"));
        assert!(collected.messages[0].body.contains("line two"));
    }

    #[test]
    fn eml_dir_imports_each_file_as_one_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("b.eml"),
            b"Subject: drugi\nMessage-ID: <b@x>\n\nBody B\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("a.eml"),
            b"Subject: prvi\nMessage-ID: <a@x>\n\nBody A\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"ignored").unwrap();

        let source = EmlDirSource::new(dir.path());
        let collected = source.collect().unwrap();
        // Both `Source` and `StreamSource` are in scope here; pin the
        // collect-side name.
        assert_eq!(Source::name(&source), MBOX_SOURCE);
        assert_eq!(collected.messages.len(), 2);
        assert_eq!(collected.failed, 0);
        // Sorted by file name: a.eml first.
        assert!(collected.messages[0].body.contains("Body A"));
        assert!(collected.messages[1].body.contains("Body B"));
    }

    #[test]
    fn eml_dir_counts_broken_files_and_continues() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("good.eml"),
            b"Subject: ok\nMessage-ID: <g@x>\n\nOK\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("bad.eml"), b"\x80\x81 not a mail").unwrap();
        let collected = EmlDirSource::new(dir.path()).collect().unwrap();
        assert_eq!(collected.messages.len(), 1);
        assert_eq!(collected.failed, 1);
    }

    #[test]
    fn eml_dir_without_eml_files_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(EmlDirSource::new(dir.path()).collect().is_err());
    }
}
