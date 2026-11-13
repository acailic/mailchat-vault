//! Export the archive to files: the current search results (under their
//! filters) or one whole chat thread, as CSV, JSON Lines, or MBOX.
//!
//! Formats:
//! - **CSV** — RFC 4180, one row per message, CRLF line endings, every
//!   field quoted only when it contains a comma, quote, CR or LF; the
//!   header names the columns. Cells beginning with `=`, `+`, `-` or `@`
//!   get an apostrophe prefix so spreadsheets cannot execute them as
//!   formulas.
//! - **JSON Lines** — one full `Message` object per line (the exact serde
//!   shape the app already speaks, so a programmatic
//!   [`crate::storage::Storage::insert_message`] loop re-imports it
//!   losslessly). Export-only today: no app importer accepts this file.
//! - **MBOX** — the classic `From `-separated mailbox, written only for
//!   email-channel rows (chat rows are counted in
//!   [`ExportSummary::skipped`]): key headers are reconstructed from the
//!   row plus its `raw_json` extras, the stored body loses its subject
//!   prefix into the `Subject:` header, and body lines starting with
//!   `From ` are escaped `>From ` the way [`crate::mbox`] expects.
//!
//! Rows are streamed one at a time through a [`std::io::Write`], so even
//! very large result sets never sit fully in memory; the search order is
//! preserved (relevance for text queries, newest-first for filtered
//! browse, oldest→newest for chat threads).

use std::io::Write;

use serde::{Deserialize, Serialize};

use crate::message::Message;
use crate::query::SearchQuery;
use crate::search2::SearchArchiveRequest;
use crate::storage::Storage;
use crate::Result;

/// Column order of the CSV export (stable across versions).
pub const CSV_COLUMNS: [&str; 8] = [
    "source",
    "external_id",
    "channel",
    "chat_id",
    "author",
    "sent_at",
    "body",
    "raw_json",
];

/// One export file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Csv,
    Json,
    Mbox,
}

impl ExportFormat {
    /// Parse the format identifier used on the IPC boundary (`"csv"`,
    /// `"json"`, `"mbox"`). Anything else is a caller bug, not a user
    /// error — the UI only offers these three.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "csv" => Ok(Self::Csv),
            "json" => Ok(Self::Json),
            "mbox" => Ok(Self::Mbox),
            other => Err(crate::Error::InvalidExport(format!(
                "unknown format {other:?} (expected csv, json or mbox)"
            ))),
        }
    }
}

/// What one export produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSummary {
    pub format: ExportFormat,
    /// Rows written to the file.
    pub written: u64,
    /// Rows matching the selection but not representable in the format —
    /// non-email rows in an MBOX export. Always 0 for CSV/JSON.
    pub skipped: u64,
}

/// Export every message matching `query` (its text and filters; the
/// pagination fields are ignored) into `out` as `format`.
///
/// The row order is exactly the one the search UI paginates through:
/// FTS5 relevance for text queries, newest-first otherwise.
pub fn export_search<W: Write>(
    storage: &Storage,
    query: &SearchQuery,
    format: ExportFormat,
    out: W,
) -> Result<ExportSummary> {
    let mut writer = Writer::new(format, out)?;
    storage.search_export_stream(query, &mut |m| writer.write_message(m))?;
    writer.finish()
}

/// Export all v2 search matches using their source-instance scope and
/// complete filter/order contract. Pagination is intentionally ignored.
pub fn export_search_v2<W: Write>(
    storage: &Storage,
    request: &SearchArchiveRequest,
    format: ExportFormat,
    out: W,
) -> Result<ExportSummary> {
    let mut writer = Writer::new(format, out)?;
    storage.search_archive_export_stream(request, &mut |m| writer.write_message(m))?;
    writer.finish()
}

/// Export the whole chat thread containing message `id` into `out` as
/// `format`, oldest → newest. A message with no `chat_id` exports as a
/// single-row file. Returns `Ok(None)` when no message has that id.
pub fn export_chat<W: Write>(
    storage: &Storage,
    id: i64,
    format: ExportFormat,
    out: W,
) -> Result<Option<ExportSummary>> {
    let Some(message) = storage.get(id)? else {
        return Ok(None);
    };
    let rows = if message.chat_id.is_empty() {
        vec![message]
    } else {
        storage.chat_thread_messages(&message.source, &message.chat_id)?
    };
    let mut writer = Writer::new(format, out)?;
    for m in &rows {
        writer.write_message(m)?;
    }
    let summary = writer.finish()?;
    Ok(Some(summary))
}

/// Format-specific row writer: owns the output and the per-format header /
/// escaping state, counts written and skipped rows.
struct Writer<W: Write> {
    format: ExportFormat,
    out: W,
    written: u64,
    skipped: u64,
}

impl<W: Write> Writer<W> {
    fn new(format: ExportFormat, mut out: W) -> Result<Self> {
        if format == ExportFormat::Csv {
            write_csv_row(&mut out, CSV_COLUMNS.iter().copied())?;
        }
        Ok(Self {
            format,
            out,
            written: 0,
            skipped: 0,
        })
    }

    fn write_message(&mut self, m: &Message) -> Result<()> {
        match self.format {
            ExportFormat::Csv => {
                write_csv_row(
                    &mut self.out,
                    [
                        &m.source,
                        &m.external_id,
                        &m.channel,
                        &m.chat_id,
                        &m.author,
                        &m.sent_at,
                        &m.body,
                        &m.raw_json,
                    ]
                    .into_iter()
                    .map(|s| s.as_str()),
                )?;
            }
            ExportFormat::Json => {
                serde_json::to_writer(&mut self.out, m)?;
                self.out.write_all(b"\n")?;
            }
            ExportFormat::Mbox => {
                if m.channel != "email" {
                    self.skipped += 1;
                    return Ok(());
                }
                write_mbox_message(&mut self.out, m)?;
            }
        }
        self.written += 1;
        Ok(())
    }

    fn finish(mut self) -> Result<ExportSummary> {
        self.out.flush()?;
        Ok(ExportSummary {
            format: self.format,
            written: self.written,
            skipped: self.skipped,
        })
    }
}

/// Write one CSV record (RFC 4180): fields quoted iff they contain a
/// comma, double quote, CR or LF; embedded quotes doubled; CRLF endings.
fn write_csv_row<I: IntoIterator<Item = S>, S: AsRef<str>>(
    out: &mut impl Write,
    fields: I,
) -> Result<()> {
    let mut record = String::new();
    for (i, field) in fields.into_iter().enumerate() {
        if i > 0 {
            record.push(',');
        }
        let field = field.as_ref();
        // Spreadsheet formula-injection mitigation: a cell beginning with
        // `=`, `+`, `-` or `@` would be executed as a formula by Excel /
        // LibreOffice; the standard mitigation prefixes such cells with an
        // apostrophe so they open as inert text.
        if matches!(field.as_bytes().first(), Some(b'=' | b'+' | b'-' | b'@')) {
            record.push('\'');
        }
        if field.chars().any(|c| matches!(c, ',' | '"' | '\r' | '\n')) {
            record.push('"');
            record.push_str(&field.replace('"', "\"\""));
            record.push('"');
        } else {
            record.push_str(field);
        }
    }
    record.push_str("\r\n");
    out.write_all(record.as_bytes())?;
    Ok(())
}

/// Write one message as a complete MBOX block: the `From `-separator line,
/// reconstructed headers, blank line, escaped body, trailing newline.
///
/// Headers come from the row plus its `raw_json` extras (written by
/// [`crate::mail`]): `Subject`/`To`/`Date` use the original header values
/// when present, `From` falls back to the row author, `Message-ID` is the
/// row's `external_id` re-bracketed. The stored email body carries the
/// subject as a `subject\n\n` prefix; that prefix moves back into the
/// `Subject:` header so a re-import reproduces the stored row.
fn write_mbox_message(out: &mut impl Write, m: &Message) -> Result<()> {
    let (subject, to, date) = mail_headers(m);
    let from = {
        let raw_from = raw_json_str(m, "from");
        if raw_from.is_empty() {
            m.author.clone()
        } else {
            raw_from
        }
    };
    let date = if date.is_empty() {
        rfc2822(&m.sent_at).unwrap_or_default()
    } else {
        date
    };

    let separator_date = if date.is_empty() {
        String::from("Thu, 01 Jan 1970 00:00:00 +0000")
    } else {
        date.clone()
    };
    writeln!(out, "From {} {}", envelope_from(&from), separator_date)?;
    let message_id = if m.external_id.starts_with('<') {
        m.external_id.clone()
    } else {
        format!("<{}>", m.external_id)
    };
    writeln!(out, "Message-ID: {message_id}")?;
    if !from.is_empty() {
        writeln!(out, "From: {from}")?;
    }
    if !to.is_empty() {
        writeln!(out, "To: {to}")?;
    }
    if !subject.is_empty() {
        writeln!(out, "Subject: {subject}")?;
    }
    if !date.is_empty() {
        writeln!(out, "Date: {date}")?;
    }
    writeln!(out, "Content-Type: text/plain; charset=utf-8")?;
    writeln!(out)?;

    let body = export_body(m, &subject);
    for line in body.split('\n') {
        if line.starts_with("From ") {
            out.write_all(b">")?;
        }
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// The `(subject, to, date)` header values recorded in `raw_json`, empty
/// strings when absent.
fn mail_headers(m: &Message) -> (String, String, String) {
    (
        raw_json_str(m, "subject"),
        raw_json_str(m, "to"),
        raw_json_str(m, "date"),
    )
}

/// Read one string field out of the row's `raw_json` extras; `""` for
/// missing keys, non-strings or unparseable JSON.
fn raw_json_str(m: &Message, key: &str) -> String {
    serde_json::from_str::<serde_json::Value>(&m.raw_json)
        .ok()
        .and_then(|v| {
            v.get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// The body to write for an MBOX export: the stored body minus the
/// `subject\n\n` prefix (which the `Subject:` header now carries). A body
/// equal to just the subject means the mail had an empty text part, so it
/// exports as an empty body and re-imports to the same stored row.
fn export_body(m: &Message, subject: &str) -> String {
    if !subject.is_empty() {
        if let Some(rest) = m.body.strip_prefix(subject) {
            if let Some(text) = rest.strip_prefix("\n\n") {
                return text.to_string();
            }
            if rest.is_empty() {
                return String::new();
            }
        }
    }
    m.body.clone()
}

/// Extract the bare address for the MBOX `From ` separator line: the
/// `<addr>` part of a display-name address when present, the whole value
/// when it contains no spaces (already bare), and a placeholder otherwise.
fn envelope_from(from: &str) -> String {
    if let (Some(open), Some(close)) = (from.find('<'), from.find('>')) {
        if open < close {
            return from[open + 1..close].to_string();
        }
    }
    if !from.is_empty() && !from.contains(char::is_whitespace) {
        return from.to_string();
    }
    "archive@localhost".to_string()
}

/// Render a stored RFC 3339 `sent_at` as an RFC 2822 date header.
fn rfc2822(sent_at: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(sent_at)
        .ok()
        .map(|dt| dt.to_rfc2822())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(body: &str) -> Message {
        Message {
            id: 1,
            source: "mbox".into(),
            external_id: "a@b.c".into(),
            channel: "email".into(),
            chat_id: "".into(),
            author: "Ana <ana@example.com>".into(),
            sent_at: "2026-11-01T09:00:00Z".into(),
            body: body.into(),
            raw_json: "{}".into(),
        }
    }

    #[test]
    fn format_parse() {
        assert_eq!(ExportFormat::parse("csv").unwrap(), ExportFormat::Csv);
        assert_eq!(ExportFormat::parse("JSON").unwrap(), ExportFormat::Json);
        assert_eq!(ExportFormat::parse(" mbox ").unwrap(), ExportFormat::Mbox);
        assert!(ExportFormat::parse("pdf").is_err());
    }

    #[test]
    fn csv_neutralizes_formula_prefixes() {
        let mut buf = Vec::new();
        write_csv_row(
            &mut buf,
            [
                "=cmd|' /C calc'!A0 probe",
                "+SUM(A1)",
                "-2+3",
                "@x",
                "plain text",
                "normal -dash and +plus",
            ],
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "'=cmd|' /C calc'!A0 probe,'+SUM(A1),'-2+3,'@x,plain text,normal -dash and +plus\r\n",
        );
    }

    #[test]
    fn csv_quotes_only_when_needed() {
        let mut buf = Vec::new();
        write_csv_row(
            &mut buf,
            [
                "plain",
                "with,comma",
                "say \"hi\"",
                "line\nbreak",
                "čćžšđ 👋",
            ],
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "plain,\"with,comma\",\"say \"\"hi\"\"\",\"line\nbreak\",čćžšđ 👋\r\n"
        );
    }

    #[test]
    fn mbox_escapes_from_lines_and_rebuilds_headers() {
        let mut m = row("Report\n\nFrom the desk of Ana\nbye");
        m.raw_json = r#"{"subject":"Report","from":"Ana <ana@example.com>","to":"Bob <bob@example.com>","date":"Sun, 1 Nov 2026 10:00:00 +0100"}"#
            .to_string();
        let mut buf = Vec::new();
        write_mbox_message(&mut buf, &m).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.starts_with("From ana@example.com Sun, 1 Nov 2026 10:00:00 +0100\n"));
        assert!(text.contains("Message-ID: <a@b.c>\n"));
        assert!(text.contains("From: Ana <ana@example.com>\n"));
        assert!(text.contains("To: Bob <bob@example.com>\n"));
        assert!(text.contains("Subject: Report\n"));
        assert!(text.contains("Date: Sun, 1 Nov 2026 10:00:00 +0100\n"));
        // Body lost the subject prefix; the embedded From line is escaped.
        assert!(text.contains(
            "\n\nFrom the desk of Ana"
                .replace("From the", ">From the")
                .as_str()
        ));
        assert!(text.ends_with("bye\n"));
    }

    #[test]
    fn body_equal_to_subject_exports_empty() {
        let mut m = row("Report");
        m.raw_json = r#"{"subject":"Report"}"#.to_string();
        assert_eq!(export_body(&m, "Report"), "");
        // And the prefix is stripped from a full body.
        let m2 = row("Report\n\nthe text");
        assert_eq!(export_body(&m2, "Report"), "the text");
        // No subject metadata: body stays as-is.
        assert_eq!(export_body(&row("just a body"), "Report"), "just a body");
    }

    #[test]
    fn envelope_from_extraction() {
        assert_eq!(envelope_from("Ana <ana@example.com>"), "ana@example.com");
        assert_eq!(envelope_from("ana@example.com"), "ana@example.com");
        assert_eq!(envelope_from("Ana Sender"), "archive@localhost");
        assert_eq!(envelope_from(""), "archive@localhost");
    }

    #[test]
    fn rfc2822_rendering() {
        assert_eq!(
            rfc2822("2026-11-01T09:00:00Z").unwrap(),
            "Sun, 1 Nov 2026 09:00:00 +0000"
        );
        assert!(rfc2822("").is_none());
        assert!(rfc2822("not a date").is_none());
    }
}
