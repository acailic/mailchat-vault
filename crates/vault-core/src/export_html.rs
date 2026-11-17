//! Readable offline HTML export bundle (A8): static pages a plain file
//! browser opens with no app, no JavaScript, and no network.
//!
//! # Format
//!
//! A bundle is a directory:
//!
//! ```text
//! <dest>/
//!   index.html        scope, count, fidelity label, links to the pages
//!   page-0001.html    up to [`PAGE_MESSAGES`] messages, oldest-relevant
//!   …                 order per scope, relative links only
//!   assets/           copied media objects, assets/<sha256>.<ext>
//!   manifest          provenance + missing-file report, written LAST
//! ```
//!
//! The `manifest`'s presence is the completeness marker: it is streamed
//! into a dot-prefixed `.manifest.part` and renamed into place only
//! after every page and asset is written, so a bundle without it is
//! clearly incomplete by construction.
//!
//! # Invariants
//!
//! - **Escaped content.** Every stored field is HTML-escaped; a body
//!   crafted as markup renders as text. Retained original HTML
//!   (`raw_json.html`, kept by [`crate::mail`]) is never copied raw
//!   into the unrestricted document: it rides only inside a
//!   `sandbox=""` iframe `srcdoc` that carries a
//!   `default-src 'none'` CSP meta and `<base target="_blank">`,
//!   mirroring the reading-pane policy of `src/render2.ts`.
//! - **No scripts, no remote assets.** The bundle contains no
//!   `script` elements and references only relative paths; nothing
//!   fetches over the network when opened offline.
//! - **Safe generated names.** Page files are generated
//!   (`page-0001.html`); assets keep their content-addressed
//!   `<sha256>.<ext>` names ([`crate::object_store::sanitize_ext`]).
//! - **Bounded streaming.** Messages stream one at a time through the
//!   same seams the CSV/JSONL/MBOX exports use
//!   ([`Storage::search_archive_export_stream`],
//!   [`Storage::conversation_thread_stream`]); one page buffer of
//!   [`PAGE_MESSAGES`] messages is in memory at a time.
//! - **Exact scope, stated first.** [`plan_html_export`] returns the
//!   scope label and total count from the same normalized scope
//!   before anything is written; the export summary repeats them.
//! - **Staged publication.** Pages are written into a dot-prefixed
//!   staging directory beside the destination, fsynced, then renamed
//!   into place. Cancellation is honored between messages: a
//!   cancelled run leaves no finished bundle — at most the clearly
//!   incomplete staging directory (no `manifest`) remains.
//! - **Honest fidelity.** [`FIDELITY_LABEL`] distinguishes
//!   reconstructed content from original MIME; in this slice originals
//!   are always labeled not retained (original-retention is its own
//!   package).
//!
//! The entry point takes a cancellation closure so it runs unchanged
//! under the existing `job_submit` mechanism (see [`html_export_job`]);
//! the app-shell registration lands with the frontend slice.

use std::cell::Cell;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::message::Message;
use crate::object_store::{sanitize_ext, ObjectStore};
use crate::storage::Storage;
use crate::{Error, Result};

/// Messages per page file.
pub const PAGE_MESSAGES: usize = 50;

/// The fidelity statement every HTML bundle and summary carries. This
/// slice exports the archive's reconstructed form (parsed fields and
/// decoded text); original MIME bytes are a separate, not-yet-landed
/// retention package, so the label always says they are not retained.
pub const FIDELITY_LABEL: &str =
    "Reconstructed from parsed archive fields; original MIME bytes are not retained.";

/// What a readable HTML bundle exports (plan section 5.6: selected
/// messages, current results, or one conversation; the person timeline
/// waits for its package).
#[derive(Debug, Clone, PartialEq)]
pub enum HtmlExportScope {
    /// An explicit selection of message row ids, in the given order
    /// (exactly what the user picked; unknown ids are skipped and
    /// listed in the manifest).
    Selected { ids: Vec<i64> },
    /// The current search results: a `SearchArchiveRequest` whose
    /// filters and order the UI shows. Pagination fields are ignored —
    /// all matching pages export.
    Search {
        request: crate::search2::SearchArchiveRequest,
    },
    /// The whole conversation containing the anchor message, oldest →
    /// newest (the same canonical identity the detail view walks).
    Conversation { anchor_id: i64 },
}

impl HtmlExportScope {
    /// The user-facing scope label the plan step returns and the
    /// summary and manifest repeat.
    pub fn label(&self) -> &'static str {
        match self {
            HtmlExportScope::Selected { .. } => "selected messages",
            HtmlExportScope::Search { .. } => "current results",
            HtmlExportScope::Conversation { .. } => "conversation",
        }
    }
}

/// The plan step's answer: scope and count, before anything is written
/// (the confirmation dialog itself is UI and lands with the frontend
/// slice).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HtmlExportPlan {
    pub scope_label: String,
    /// Total messages the scope resolves to right now.
    pub count: u64,
}

/// What one HTML bundle export produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HtmlExportSummary {
    /// The scope label, repeated from [`HtmlExportPlan`].
    pub scope_label: String,
    /// The planned total, repeated from [`HtmlExportPlan`].
    pub count: u64,
    /// Messages actually written (may be lower than `count` when
    /// selected ids vanished between plan and export).
    pub written: u64,
    /// Page files written.
    pub pages: u64,
    /// Distinct media objects copied into `assets/`.
    pub assets_copied: u64,
    /// Referenced files not in the bundle (missing at import, dangling
    /// in the store, unreadable) — each is listed in the manifest with
    /// a reason.
    pub assets_missing: u64,
    /// The fidelity wording the manifest also carries.
    pub fidelity: String,
}

/// How an export ended: completed, or cancelled mid-run (no finished
/// bundle; at most the clearly incomplete staging directory remains).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HtmlExportOutcome {
    Complete(HtmlExportSummary),
    Cancelled,
}

/// Resolve the scope to its label and total count without writing
/// anything. Search counts through the same normalized request the
/// stream uses; the conversation scope streams its thread once, read
/// only; the selection counts resolvable ids (the unknown ones are
/// listed in the manifest when the export runs).
pub fn plan_html_export(storage: &Storage, scope: &HtmlExportScope) -> Result<HtmlExportPlan> {
    let count = match scope {
        HtmlExportScope::Selected { ids } => {
            let mut count = 0;
            for id in ids {
                if storage.get(*id)?.is_some() {
                    count += 1;
                }
            }
            count
        }
        HtmlExportScope::Search { request } => {
            // The count of the same normalized scope, one snapshot:
            // a one-row page still reports the exact total.
            let probe = crate::search2::SearchArchiveRequest {
                limit: Some(1),
                offset: Some(0),
                cursor: None,
                ..request.clone()
            };
            storage.search_archive(&probe)?.total
        }
        HtmlExportScope::Conversation { anchor_id } => {
            if storage.get(*anchor_id)?.is_none() {
                return Ok(HtmlExportPlan {
                    scope_label: scope.label().to_string(),
                    count: 0,
                });
            }
            let mut count = 0u64;
            storage.conversation_thread_stream(*anchor_id, &mut |_| {
                count += 1;
                Ok(())
            })?;
            count
        }
    };
    Ok(HtmlExportPlan {
        scope_label: scope.label().to_string(),
        count,
    })
}

/// Write the readable HTML bundle for `scope` into the directory
/// `dest` (which must not already exist as a non-empty directory — a
/// finished bundle is published by rename, never merged into a user
/// directory). Media objects copy out of `objects` by digest.
///
/// `Ok(None)` mirrors [`crate::export::export_chat`]: the conversation
/// anchor names no message, so nothing is written at all.
/// `Ok(Some(HtmlExportOutcome::Cancelled))` honors a cancellation
/// request observed between messages: no bundle exists at `dest`, and
/// at most the dot-prefixed staging directory — always without its
/// `manifest` — remains beside it.
pub fn export_html(
    storage: &Storage,
    scope: &HtmlExportScope,
    dest: &Path,
    objects: &ObjectStore,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<HtmlExportOutcome>> {
    if is_cancelled() {
        return Ok(Some(HtmlExportOutcome::Cancelled));
    }
    // The search scope normalizes (and can reject) before anything is
    // created; the conversation scope needs its anchor to exist.
    if let HtmlExportScope::Search { request } = scope {
        request.normalize()?;
    }
    if let HtmlExportScope::Conversation { anchor_id } = scope {
        if storage.get(*anchor_id)?.is_none() {
            return Ok(None);
        }
    }
    let plan = plan_html_export(storage, scope)?;

    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let stem = dest
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "html-export".to_string());
    let staging = parent.join(format!(".{stem}-{}.staging", std::process::id()));
    // A previous cancelled run may have left its clearly-incomplete
    // staging directory; start fresh rather than merging into it.
    if staging.is_dir() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(staging.join("assets"))?;

    match write_bundle(storage, scope, &plan, dest, &staging, objects, is_cancelled) {
        // Cancelled: leave the staging directory as the clearly
        // incomplete artifact it is (no manifest), never a
        // finished-looking bundle.
        Ok(HtmlExportOutcome::Cancelled) => Ok(Some(HtmlExportOutcome::Cancelled)),
        Ok(other) => Ok(Some(other)),
        // Any failure cleans up after itself, like staged_write's
        // temp guard: no misleading partial bundle beside the dest.
        Err(e) => {
            let _ = fs::remove_dir_all(&staging);
            Err(e)
        }
    }
}

/// The cancellation marker that aborts a streaming seam mid-row: the
/// sink returns it, the stream propagates it, and the caller
/// translates it back into [`HtmlExportOutcome::Cancelled`] (a plain
/// `Cell` flag keeps the translation honest even if the marker string
/// were ever reworded).
const CANCEL_MARKER: &str = "html export cancelled";

/// The staged bundle writer: pages and assets stream in, `index.html`
/// and the renamed `manifest` close the bundle, then the staging
/// directory is fsynced and renamed into place.
fn write_bundle(
    storage: &Storage,
    scope: &HtmlExportScope,
    plan: &HtmlExportPlan,
    dest: &Path,
    staging: &Path,
    objects: &ObjectStore,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<HtmlExportOutcome> {
    let mut pages = PageWriter::new();
    let mut manifest = ManifestWriter::new(staging, plan)?;
    let cancelled_seen = Cell::new(false);
    let mut copied: HashSet<String> = HashSet::new();
    let written = match scope {
        HtmlExportScope::Selected { ids } => {
            let mut written = 0u64;
            for id in ids {
                if is_cancelled() {
                    return Ok(HtmlExportOutcome::Cancelled);
                }
                match storage.get(*id)? {
                    Some(m) => {
                        pages.message(&m, &mut manifest, staging, objects, &mut copied)?;
                        written += 1;
                    }
                    None => manifest.selection_missing(*id),
                }
            }
            Some(written)
        }
        HtmlExportScope::Search { request } => {
            let mut sink = |m: &Message| -> Result<()> {
                if is_cancelled() {
                    cancelled_seen.set(true);
                    // Aborts the stream at this row: no further page is
                    // built.
                    return Err(Error::Jobs(CANCEL_MARKER.into()));
                }
                pages.message(m, &mut manifest, staging, objects, &mut copied)
            };
            match storage.search_archive_export_stream(request, &mut sink) {
                Ok(n) => Some(n),
                Err(_) if cancelled_seen.get() => return Ok(HtmlExportOutcome::Cancelled),
                Err(e) => return Err(e),
            }
        }
        HtmlExportScope::Conversation { anchor_id } => {
            let mut sink = |m: &Message| -> Result<()> {
                if is_cancelled() {
                    cancelled_seen.set(true);
                    return Err(Error::Jobs(CANCEL_MARKER.into()));
                }
                pages.message(m, &mut manifest, staging, objects, &mut copied)
            };
            match storage.conversation_thread_stream(*anchor_id, &mut sink) {
                Ok(()) => None,
                Err(_) if cancelled_seen.get() => return Ok(HtmlExportOutcome::Cancelled),
                Err(e) => return Err(e),
            }
        }
    };
    let written = match written {
        Some(n) => n,
        None => pages.written,
    };

    let page_count = pages.finish(staging)?;
    let summary = HtmlExportSummary {
        scope_label: plan.scope_label.clone(),
        count: plan.count,
        written,
        pages: page_count,
        assets_copied: copied.len() as u64,
        assets_missing: manifest.missing,
        fidelity: FIDELITY_LABEL.to_string(),
    };
    manifest.finish(staging, &summary)?;
    write_index(staging, &summary)?;
    publish(staging, dest)?;
    Ok(HtmlExportOutcome::Complete(summary))
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// One page file under construction: buffers up to [`PAGE_MESSAGES`]
/// message bodies, tracking provenance counts, and opens the next page
/// when this one is full. Only one page buffer is alive at a time.
struct PageWriter {
    page_no: u64,
    in_page: usize,
    buf: String,
    written: u64,
}

impl PageWriter {
    fn new() -> Self {
        let mut me = Self {
            page_no: 0,
            in_page: 0,
            buf: String::new(),
            written: 0,
        };
        me.open_page();
        me
    }

    fn open_page(&mut self) {
        self.page_no += 1;
        self.in_page = 0;
        self.buf.clear();
        let _ = write!(
            self.buf,
            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
             <title>Mailchat Vault export — page {n}</title>\n</head>\n<body>\n\
             <h1>Mailchat Vault export</h1>\n<p>page {n}</p>\n",
            n = self.page_no
        );
    }

    /// Render one message into the current page, copying its media
    /// objects and recording provenance/missing lines. Bounded: the
    /// message fields are read one at a time; the page flushes to disk
    /// every [`PAGE_MESSAGES`] messages.
    fn message(
        &mut self,
        m: &Message,
        manifest: &mut ManifestWriter,
        staging: &Path,
        objects: &ObjectStore,
        copied: &mut HashSet<String>,
    ) -> Result<()> {
        let _ = write!(
            self.buf,
            "<section class=\"message\" id=\"m-{id}\">\n\
             <p class=\"meta\">source: {source} · author: {author}</p>\n\
             <p class=\"meta\">sent: {sent} · channel: {channel}</p>\n\
             <pre class=\"body\">{body}</pre>\n",
            id = m.id,
            source = escape(&m.source),
            author = escape(&m.author),
            sent = escape(&m.sent_at),
            channel = escape(&m.channel),
            body = escape(&m.body),
        );
        self.media(m, manifest, staging, objects, copied)?;
        if let Some(html) = retained_html(&m.raw_json) {
            let _ = write!(
                self.buf,
                "<details class=\"rendered\">\
                 <summary>Rendered original HTML (sandboxed)</summary>\n\
                 <iframe sandbox=\"\" title=\"Rendered message body\" \
                 srcdoc=\"{srcdoc}\"></iframe>\n\
                 </details>\n",
                // The retained HTML rides ONLY inside this escaped
                // srcdoc attribute: the outer document never receives
                // a raw markup byte, and the sandbox + CSP keep the
                // inner document inert (the reading-pane policy).
                srcdoc = escape(&sandboxed_srcdoc(&html)),
            );
        }
        self.buf.push_str("</section>\n");
        self.written += 1;
        manifest.provenance(self.page_no, m)?;

        self.in_page += 1;
        if self.in_page >= PAGE_MESSAGES {
            self.flush_page(staging)?;
            self.open_page();
        }
        Ok(())
    }

    /// Copy the message's referenced media into `assets/` (deduplicated
    /// by digest) and list every reference that cannot travel.
    fn media(
        &mut self,
        m: &Message,
        manifest: &mut ManifestWriter,
        staging: &Path,
        objects: &ObjectStore,
        copied: &mut HashSet<String>,
    ) -> Result<()> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.raw_json) else {
            return Ok(());
        };
        let Some(entries) = value.get("media").and_then(|v| v.as_array()) else {
            return Ok(());
        };
        if entries.is_empty() {
            return Ok(());
        }
        self.buf.push_str("<ul class=\"attachments\">\n");
        for entry in entries {
            let name = entry
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed");
            let kind = entry.get("kind").and_then(|v| v.as_str()).unwrap_or("file");
            if entry
                .get("missing")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                let reason = entry
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                manifest.file_missing(m.id, name, &format!("not preserved at import ({reason})"));
                let _ = write!(
                    self.buf,
                    "<li>{name} ({kind}) — not preserved at import</li>\n",
                    name = escape(name),
                    kind = escape(kind),
                );
                continue;
            }
            let Some(sha256) = entry.get("sha256").and_then(|v| v.as_str()) else {
                manifest.file_missing(m.id, name, "media entry carries no digest");
                let _ = write!(
                    self.buf,
                    "<li>{name} ({kind}) — no digest recorded</li>\n",
                    name = escape(name),
                    kind = escape(kind),
                );
                continue;
            };
            let Some(store_name) = objects.object_name(sha256) else {
                manifest.file_missing(m.id, name, "referenced object missing from the media store");
                let _ = write!(
                    self.buf,
                    "<li>{name} ({kind}) — object missing from the archive</li>\n",
                    name = escape(name),
                    kind = escape(kind),
                );
                continue;
            };
            let asset = asset_name(&store_name);
            if copied.insert(asset.clone()) {
                let from = objects.store_dir().join(&store_name);
                let to = staging.join("assets").join(&asset);
                if let Err(e) = fs::copy(&from, &to) {
                    copied.remove(&asset);
                    manifest.file_missing(m.id, name, &format!("copy failed: {e}"));
                    let _ = write!(
                        self.buf,
                        "<li>{name} ({kind}) — could not be copied</li>\n",
                        name = escape(name),
                        kind = escape(kind),
                    );
                    continue;
                }
            }
            let _ = write!(
                self.buf,
                "<li><a href=\"assets/{asset}\">{name}</a> ({kind})</li>\n",
                asset = escape(&asset),
                name = escape(name),
                kind = escape(kind),
            );
        }
        self.buf.push_str("</ul>\n");
        Ok(())
    }

    /// Write the buffered page to `page-NNNN.html`.
    fn flush_page(&mut self, staging: &Path) -> Result<()> {
        if self.in_page == 0 {
            return Ok(());
        }
        self.buf.push_str("</body>\n</html>\n");
        write_synced(&staging.join(page_name(self.page_no)), self.buf.as_bytes())
    }

    /// Flush the trailing partial page; returns the page count. An
    /// untouched trailing page (an exact multiple of
    /// [`PAGE_MESSAGES`], or zero messages) is not written.
    fn finish(&mut self, staging: &Path) -> Result<u64> {
        self.flush_page(staging)?;
        if self.in_page == 0 && self.page_no > 0 {
            self.page_no -= 1;
        }
        Ok(self.page_no)
    }
}

/// The `page-NNNN.html` file name of a 1-based page number.
fn page_name(page_no: u64) -> String {
    format!("page-{page_no:04}.html")
}

/// The `assets/<sha256>.<ext>` bundle name of a stored object file
/// name: the digest stays, the extension is re-sanitized so nothing
/// but `[a-z0-9]{0,16}` can follow the dot.
fn asset_name(store_name: &str) -> String {
    let ext = store_name
        .rsplit_once('.')
        .map(|(_, ext)| sanitize_ext(ext))
        .unwrap_or_else(|| "bin".to_string());
    let digest = store_name.split('.').next().unwrap_or(store_name);
    format!("{digest}.{ext}")
}

/// The sandboxed srcdoc document for retained original HTML: the CSP
/// blocks every remote subresource (tracking pixels included) and
/// `<base target="_blank">` makes link clicks target a new window the
/// popup-less sandbox refuses to open — the exact reading-pane
/// preamble of `src/render2.ts`.
fn sandboxed_srcdoc(html: &str) -> String {
    let preamble = "<!doctype html><meta http-equiv=\"Content-Security-Policy\" \
                    content=\"default-src 'none'; style-src 'unsafe-inline'; \
                    img-src data:'><base target=\"_blank\">";
    format!("{preamble}{html}")
}

/// The retained original HTML of a message, when present
/// (`raw_json.html`, bounded at ingest by `crate::mail`'s
/// `MAX_HTML_BYTES`).
fn retained_html(raw_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(raw_json)
        .ok()?
        .get("html")?
        .as_str()
        .filter(|h| !h.trim().is_empty())
        .map(str::to_string)
}

/// HTML-escape text content and attribute values: `&`, `<`, `>`, `"`,
/// `'`. Every stored field and every generated attribute value passes
/// through here; nothing from the archive reaches a page raw.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// The streaming manifest writer: provenance lines and missing-file
/// lines append as messages stream (bounded — the file holds them, not
/// memory), and the finished report is fsynced and renamed from
/// `.manifest.part` to `manifest` only after the bundle is complete,
/// so the name itself says "finished".
struct ManifestWriter {
    out: BufWriter<File>,
    path: PathBuf,
    /// Referenced files not in the bundle.
    missing: u64,
}

impl ManifestWriter {
    fn new(staging: &Path, plan: &HtmlExportPlan) -> Result<Self> {
        let path = staging.join(".manifest.part");
        let mut out = BufWriter::new(File::create(&path)?);
        writeln!(out, "Mailchat Vault HTML export manifest")?;
        writeln!(out, "scope: {}", plan.scope_label)?;
        writeln!(out, "messages: {}", plan.count)?;
        writeln!(out, "fidelity: {FIDELITY_LABEL}")?;
        writeln!(out)?;
        writeln!(out, "message provenance:")?;
        Ok(Self {
            out,
            path,
            missing: 0,
        })
    }

    /// One provenance line per exported message.
    fn provenance(&mut self, page_no: u64, m: &Message) -> Result<()> {
        writeln!(
            self.out,
            "[{}] id={} source={} external_id={} sent_at={}",
            page_name(page_no),
            m.id,
            m.source,
            m.external_id,
            m.sent_at
        )
        .map_err(Into::into)
    }

    /// One missing/skipped file line with its reason.
    fn file_missing(&mut self, message_id: i64, name: &str, reason: &str) {
        self.missing += 1;
        let _ = writeln!(self.out, "missing: message {message_id} ({name}): {reason}");
    }

    /// One selected id that names no message.
    fn selection_missing(&mut self, id: i64) {
        self.missing += 1;
        let _ = writeln!(self.out, "skipped: selected id {id}: no such message");
    }

    /// Footer, flush, fsync, and the completeness rename.
    fn finish(mut self, staging: &Path, summary: &HtmlExportSummary) -> Result<()> {
        writeln!(self.out)?;
        writeln!(
            self.out,
            "written: {} messages across {} pages; {} assets copied; {} files missing or skipped",
            summary.written, summary.pages, summary.assets_copied, summary.assets_missing
        )?;
        self.out.flush()?;
        File::open(&self.path)?.sync_all()?;
        let manifest = staging.join("manifest");
        fs::rename(&self.path, manifest)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Index and publication
// ---------------------------------------------------------------------------

/// The bundle's `index.html`: scope, count, fidelity label, and
/// relative links to every page (the exact query scope, not the
/// visible page) plus the manifest.
fn write_index(staging: &Path, summary: &HtmlExportSummary) -> Result<()> {
    let mut html = String::new();
    let _ = write!(
        html,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>Mailchat Vault export</title>\n</head>\n<body>\n\
         <h1>Mailchat Vault export</h1>\n\
         <p>scope: {scope}</p>\n<p>messages: {count}</p>\n\
         <p>fidelity: {fidelity}</p>\n<nav>\n<ul>\n",
        scope = escape(&summary.scope_label),
        count = summary.count,
        fidelity = escape(&summary.fidelity),
    );
    for page in 1..=summary.pages {
        let _ = write!(
            html,
            "<li><a href=\"{name}\">{name}</a></li>\n",
            name = escape(&page_name(page)),
        );
    }
    html.push_str("</ul>\n</nav>\n<p><a href=\"manifest\">manifest</a></p>\n</body>\n</html>\n");
    write_synced(&staging.join("index.html"), html.as_bytes())
}

/// Write and fsync one staged file (the staged-file idiom of
/// [`crate::staged_write`] and [`crate::backup`]).
fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Publish the finished bundle: fsync the staging directory (so its
/// entries are durable), rename it to `dest` (which must be absent or
/// an empty directory — a non-empty destination is refused, never
/// merged into), then fsync the parent so the rename itself survives.
fn publish(staging: &Path, dest: &Path) -> Result<()> {
    File::open(staging)?.sync_all()?;
    fs::rename(staging, dest)?;
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        File::open(parent)?.sync_all().ok();
    }
    Ok(())
}

/// The `job_submit` body for an HTML bundle export: the shared
/// [`export_html`] over a pool read connection, cancellation and the
/// summary through the [`crate::jobs::JobContext`] — the same shape
/// `import_mbox` uses, verified at crate level until the app-shell
/// kind registration lands with the frontend slice.
pub fn html_export_job(
    ctx: &crate::jobs::JobContext<'_>,
    scope: &HtmlExportScope,
    dest: &Path,
    data_dir: &Path,
) -> std::result::Result<serde_json::Value, crate::jobs::JobError> {
    let objects = ObjectStore::new(data_dir);
    let cancelled = || ctx.is_cancelled();
    match ctx
        .read(|s| export_html(s, scope, dest, &objects, &cancelled))
        .map_err(crate::jobs::JobError::from)?
    {
        None => Ok(serde_json::Value::Null),
        Some(HtmlExportOutcome::Cancelled) => Err(crate::jobs::JobError::Cancelled),
        Some(HtmlExportOutcome::Complete(summary)) => {
            Ok(serde_json::to_value(&summary).unwrap_or(serde_json::Value::Null))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_covers_markup_and_quotes() {
        assert_eq!(
            escape(r#"<script>a & "b" 'c'</script>"#),
            "&lt;script&gt;a &amp; &quot;b&quot; &#39;c&#39;&lt;/script&gt;",
        );
    }

    #[test]
    fn asset_names_reuse_the_digest_and_sanitize_the_extension() {
        assert_eq!(
            asset_name("ab12cd34ef56.jpg"),
            "ab12cd34ef56.jpg",
            "a clean store name passes through"
        );
        assert_eq!(
            asset_name("ab12cd34ef56.UPPER"),
            "ab12cd34ef56.upper",
            "extensions lowercase like the store"
        );
        assert_eq!(
            asset_name("ab12cd34ef56.odd/ext"),
            "ab12cd34ef56.bin",
            "a hostile extension degrades to bin"
        );
        assert_eq!(asset_name("digest-only"), "digest-only.bin");
    }

    #[test]
    fn srcdoc_preamble_carries_the_reading_pane_policy() {
        let doc = sandboxed_srcdoc("<p>hi</p>");
        assert!(doc.starts_with("<!doctype html><meta http-equiv=\"Content-Security-Policy\""));
        assert!(doc.contains("default-src 'none'; style-src 'unsafe-inline'; img-src data:"));
        assert!(doc.contains("<base target=\"_blank\">"));
        assert!(doc.ends_with("<p>hi</p>"));
    }

    #[test]
    fn retained_html_reads_raw_json_and_ignores_other_shapes() {
        assert_eq!(
            retained_html(r#"{"html":"<p>x</p>"}"#).as_deref(),
            Some("<p>x</p>")
        );
        assert_eq!(retained_html(r#"{"html":"   "}"#), None);
        assert_eq!(retained_html("{}"), None);
        assert_eq!(retained_html("not json"), None);
    }

    #[test]
    fn page_names_are_generated_and_collision_free() {
        assert_eq!(page_name(1), "page-0001.html");
        assert_eq!(page_name(12345), "page-12345.html");
    }
}
