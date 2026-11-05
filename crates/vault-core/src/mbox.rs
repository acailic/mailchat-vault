//! `.mbox` file source: a manual/demo import that keeps the ingest line
//! executable without any IMAP access.
//!
//! Supports the common `mboxo`/`mboxrd` shape: messages separated by a line
//! starting with `From ` (sender + date), with bodies escaping embedded
//! `From ` lines as `>From ` (the escaped form does not split a message).

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::bounded::{
    import_id_for, BoundedOutcome, BoundedRun, EntryOutcome, Flow, IngestSink, MediaKind,
    MediaPass, StreamItem, StreamSource,
};
use crate::ingest::{Collected, Source};
use crate::mail::parse_raw_mail;
use crate::Result;

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

/// The bounded production import for `.mbox` files (M4.2 adds the mail
/// media pass): streams the file one message at a time, and — with a
/// `data_dir` — stores each mail's MIME attachments/inline parts into
/// the content-addressed object store and links them from `raw_json`
/// before every batch commit. Without a `data_dir` the import is
/// text-only (attachment references stay recorded in `raw_json` only).
/// Dedup and resume semantics are the shared bounded contract
/// (see [`crate::bounded`]).
pub fn import_mbox_bounded(
    sink: &mut dyn IngestSink,
    path: &Path,
    data_dir: Option<&Path>,
    is_cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64, u64, &str),
) -> Result<BoundedOutcome> {
    let source = MboxSource::new(path);
    let mut run = BoundedRun::new(import_id_for("mbox-import", path));
    if let Some(data_dir) = data_dir {
        run.media = Some(MediaPass {
            kind: MediaKind::Mail,
            export_root: Path::new(""),
            export_file: None,
            data_dir,
            unlinked: false,
        });
    }
    run.run(&source, sink, is_cancelled, progress)
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
/// streaming source: `From ` separator lines split the stream into
/// messages (escaped `>From ` lines do not), one message buffered at a
/// time. Parts before the first separator are dropped, and a separator
/// line with no content is skipped silently — exactly the whole-file
/// splitter's semantics, item for item.
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
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        let at_line_start = part.is_empty() || *part.last().unwrap() == b'\n';
        if at_line_start && line.starts_with(b"From ") {
            if started && !offer_part(&part, chat_id, &mut skip, emit, &mut failure, &mut stopped) {
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
        offer_part(&part, chat_id, &mut skip, emit, &mut failure, &mut stopped);
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
) -> bool {
    // Drop the leading "From sender date" separator line before parsing.
    let raw = match part.iter().position(|&b| b == b'\n') {
        Some(nl) => &part[nl + 1..],
        None => return true, // separator line with no content: skipped
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
}
