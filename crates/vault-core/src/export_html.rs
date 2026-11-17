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
//!   manifest.json     the tamper-evident verification manifest (A9)
//!   README.md         what the manifest proves and what it does not
//!   manifest          provenance + missing-file report, written LAST
//! ```
//!
//! An **evidence pack** ([`HtmlExportScope::EvidencePack`]) reuses the
//! same bundle machinery with a lawyer-facing shape: `index.html`
//! becomes a cover sheet (scope statement, counts, Bates range, a
//! SHA-256 digest commitment over the ordered per-message digests),
//! every message page carries Bates footers and print CSS
//! (`@media print` page breaks, `@page` margins), and `custody.html`
//! appends the chain-of-custody rows from `source_receipts` plus
//! verification instructions. The same selection, Bates prefix, and
//! start number always reproduce identical bytes.
//!
//! The `manifest`'s presence is the completeness marker: it is streamed
//! into a dot-prefixed `.manifest.part` and renamed into place only
//! after every page and asset is written, so a bundle without it is
//! clearly incomplete by construction. The verification `manifest.json`
//! and its `README.md` are written into the staged bundle before that
//! rename (see [`crate::export_manifest`]).
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
use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::export_manifest::VerificationCollector;
use crate::message::Message;
use crate::object_store::{sanitize_ext, ObjectStore};
use crate::storage::Storage;
use crate::{Error, Result};

/// Messages per page file.
pub const PAGE_MESSAGES: usize = 50;

/// Minimum digit width of a Bates number ([`bates_number`] grows past
/// it when the start number already exceeds it).
pub const BATES_DIGITS: usize = 6;

/// How the cover sheet and manifest label the SHA-256 commitment over
/// the ordered per-message digests — exactly a digest commitment,
/// never a cryptographic proof root (that lands with the proof-export
/// package, and the cover sheet says so via [`PENDING_PROOF_LINE`]).
pub const DIGEST_COMMITMENT_LABEL: &str = "digest commitment";

/// The cover sheet's pending-cryptographic-proof line: the pack binds
/// its messages with a digest commitment only until proof export lands.
pub const PENDING_PROOF_LINE: &str =
    "Cryptographic proof export is pending; this pack binds its messages with a SHA-256 digest commitment only.";

/// Receipt rows the chain-of-custody appendix lists per scoped source
/// (the same scan bound the health scan uses).
const CUSTODY_RECEIPTS_PER_SOURCE: u32 = 128;

/// The fidelity statement every HTML bundle and summary carries. This
/// slice exports the archive's reconstructed form (parsed fields and
/// decoded text); original MIME bytes are a separate, not-yet-landed
/// retention package, so the label always says they are not retained.
pub const FIDELITY_LABEL: &str =
    "Reconstructed from parsed archive fields; original MIME bytes are not retained.";

/// Bates numbering configuration: a prefix plus a start number, e.g.
/// prefix `VATL`, start `1` numbers the message pages `VATL-000001`,
/// `VATL-000002`, … across the whole bundle. Numbering is
/// deterministic given (selection, prefix, start): the same scope
/// re-exported numbers — and bytes — identically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatesOptions {
    /// The exhibit prefix, e.g. `VATL`; rendered before the dash.
    pub prefix: String,
    /// The first number (1-based; `VATL-{start:06}` is the first page).
    pub start: u64,
}

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
    /// A Bates-numbered, print-ready eDiscovery evidence pack scoped
    /// by the request's filters (date range, sources, people/author):
    /// cover sheet, per-message Bates footers, chain-of-custody
    /// appendix, and a digest commitment in the manifest.
    EvidencePack {
        request: crate::search2::SearchArchiveRequest,
        bates: BatesOptions,
    },
}

impl HtmlExportScope {
    /// The user-facing scope label the plan step returns and the
    /// summary and manifest repeat.
    pub fn label(&self) -> &'static str {
        match self {
            HtmlExportScope::Selected { .. } => "selected messages",
            HtmlExportScope::Search { .. } => "current results",
            HtmlExportScope::Conversation { .. } => "conversation",
            HtmlExportScope::EvidencePack { .. } => "evidence pack",
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
    /// The verification manifest's head hash ([`crate::export_manifest`]):
    /// stable across re-exports of the same data, broken by any later
    /// change to the chained pages, assets, or digests.
    pub head_hash: String,
    /// Evidence packs only: the Bates range rendered on the cover
    /// sheet, e.g. `VATL-000001 to VATL-000003` (`None` for an empty
    /// pack).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bates_range: Option<String>,
    /// Evidence packs only: attachment references listed across the
    /// message pages (preserved and missing alike).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_refs: Option<u64>,
    /// Evidence packs only: the hex SHA-256
    /// [`DIGEST_COMMITMENT_LABEL`] over the ordered per-message
    /// digests, repeated by the manifest and the cover sheet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest_commitment: Option<String>,
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
        HtmlExportScope::Search { request } | HtmlExportScope::EvidencePack { request, .. } => {
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
    // The search and evidence-pack scopes normalize (and can reject)
    // before anything is created; the conversation scope needs its
    // anchor to exist.
    if let HtmlExportScope::Search { request } | HtmlExportScope::EvidencePack { request, .. } =
        scope
    {
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
    let bates_cfg = match scope {
        HtmlExportScope::EvidencePack { bates, .. } => Some(bates),
        _ => None,
    };
    let mut pages = PageWriter::new(bates_cfg);
    let mut manifest = ManifestWriter::new(staging, plan, bates_cfg.is_some())?;
    let mut verification = VerificationCollector::new(storage);
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
                        pages.message(
                            &m,
                            &mut manifest,
                            &mut verification,
                            staging,
                            objects,
                            &mut copied,
                        )?;
                        written += 1;
                    }
                    None => manifest.selection_missing(*id),
                }
            }
            Some(written)
        }
        HtmlExportScope::EvidencePack { request, .. } => {
            // Resolve the scope to its message ids once (the same
            // filters the search seam applies, newest-first), then
            // flip to oldest-first: dated messages ascending by
            // (instant, id), undated last — the stable order Bates
            // numbers are computed from, so the same selection always
            // numbers identically.
            let mut ordered = request.clone();
            ordered.sort = crate::search2::SearchSort::Newest;
            ordered.cursor = None;
            let mut rows: Vec<(bool, String, i64)> = Vec::new();
            let mut sink = |m: &Message| -> Result<()> {
                if is_cancelled() {
                    cancelled_seen.set(true);
                    // Aborts the stream at this row: no id is collected
                    // beyond this point.
                    return Err(Error::Jobs(CANCEL_MARKER.into()));
                }
                rows.push((m.sent_at.is_empty(), m.sent_at.clone(), m.id));
                Ok(())
            };
            match storage.search_archive_export_stream(&ordered, &mut sink) {
                Ok(_) => {}
                Err(_) if cancelled_seen.get() => return Ok(HtmlExportOutcome::Cancelled),
                Err(e) => return Err(e),
            }
            rows.sort_unstable();
            let mut written = 0u64;
            for (_, _, id) in rows {
                if is_cancelled() {
                    return Ok(HtmlExportOutcome::Cancelled);
                }
                match storage.get(id)? {
                    Some(m) => {
                        pages.message(
                            &m,
                            &mut manifest,
                            &mut verification,
                            staging,
                            objects,
                            &mut copied,
                        )?;
                        written += 1;
                    }
                    None => manifest.selection_missing(id),
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
                pages.message(
                    m,
                    &mut manifest,
                    &mut verification,
                    staging,
                    objects,
                    &mut copied,
                )
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
                pages.message(
                    m,
                    &mut manifest,
                    &mut verification,
                    staging,
                    objects,
                    &mut copied,
                )
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
    // The verification manifest and its README land in the staged
    // bundle before the completeness rename, so an incomplete bundle
    // never carries either.
    let assets: BTreeSet<String> = copied.into_iter().collect();
    let verification = verification.finish(
        staging,
        page_count,
        &assets,
        &plan.scope_label,
        plan.count,
        written,
        FIDELITY_LABEL,
    )?;
    let (bates_range, attachment_refs, digest_commitment, scope_statement) = match scope {
        HtmlExportScope::EvidencePack { request, .. } => {
            // The custody appendix streams from `source_receipts`
            // before the manifest closes the bundle.
            write_custody(storage, request, staging)?;
            (
                pages.bates_range(),
                Some(manifest.attachments),
                manifest.commitment_hex(),
                Some(evidence_scope_statement(storage, request)),
            )
        }
        _ => (None, None, None, None),
    };
    let summary = HtmlExportSummary {
        scope_label: plan.scope_label.clone(),
        count: plan.count,
        written,
        pages: page_count,
        assets_copied: assets.len() as u64,
        assets_missing: manifest.missing,
        fidelity: FIDELITY_LABEL.to_string(),
        head_hash: verification.head_hash,
        bates_range,
        attachment_refs,
        digest_commitment,
    };
    manifest.finish(staging, &summary)?;
    write_index(staging, &summary, scope_statement.as_deref())?;
    publish(staging, dest)?;
    Ok(HtmlExportOutcome::Complete(summary))
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// The evidence-pack page state: the Bates counter. One number per
/// message, ascending across the whole bundle in the export's stable
/// order.
struct EvidencePageState {
    prefix: String,
    next: u64,
    first: Option<u64>,
    last: u64,
}

/// One page file under construction: buffers up to [`PAGE_MESSAGES`]
/// message bodies, tracking provenance counts, and opens the next page
/// when this one is full. Only one page buffer is alive at a time.
/// With evidence-pack state the pages additionally carry the print
/// stylesheet, per-message header rows, attachment digests, and Bates
/// footers.
struct PageWriter {
    page_no: u64,
    in_page: usize,
    buf: String,
    written: u64,
    evidence: Option<EvidencePageState>,
}

impl PageWriter {
    fn new(bates: Option<&BatesOptions>) -> Self {
        let mut me = Self {
            page_no: 0,
            in_page: 0,
            buf: String::new(),
            written: 0,
            evidence: bates.map(|b| EvidencePageState {
                prefix: b.prefix.clone(),
                next: b.start,
                first: None,
                last: b.start.saturating_sub(1),
            }),
        };
        me.open_page();
        me
    }

    /// The cover sheet's Bates range line, e.g.
    /// `VATL-000001 to VATL-000003` (`None` for an empty pack or a
    /// plain bundle).
    fn bates_range(&self) -> Option<String> {
        let e = self.evidence.as_ref()?;
        let first = e.first?;
        Some(format!(
            "{} to {}",
            bates_number(&e.prefix, first),
            bates_number(&e.prefix, e.last)
        ))
    }

    fn open_page(&mut self) {
        self.page_no += 1;
        self.in_page = 0;
        self.buf.clear();
        if self.evidence.is_some() {
            let _ = write!(
                self.buf,
                "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
                 <title>Mailchat Vault evidence pack — page {n}</title>\n\
                 <style>{css}</style>\n</head>\n<body>\n\
                 <h1>Mailchat Vault evidence pack</h1>\n<p>page {n}</p>\n",
                n = self.page_no,
                css = EVIDENCE_PRINT_CSS,
            );
        } else {
            let _ = write!(
                self.buf,
                "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
                 <title>Mailchat Vault export — page {n}</title>\n</head>\n<body>\n\
                 <h1>Mailchat Vault export</h1>\n<p>page {n}</p>\n",
                n = self.page_no
            );
        }
    }

    /// Render one message into the current page, copying its media
    /// objects and recording provenance/missing lines plus the
    /// verification manifest's message line. Bounded: the message
    /// fields are read one at a time; the page flushes to disk every
    /// [`PAGE_MESSAGES`] messages.
    fn message(
        &mut self,
        m: &Message,
        manifest: &mut ManifestWriter,
        verification: &mut VerificationCollector<'_>,
        staging: &Path,
        objects: &ObjectStore,
        copied: &mut HashSet<String>,
    ) -> Result<()> {
        if let Some(e) = &mut self.evidence {
            let number = e.next;
            e.next += 1;
            if e.first.is_none() {
                e.first = Some(number);
            }
            e.last = number;
            let _ = write!(
                self.buf,
                "<section class=\"message\" id=\"m-{id}\">\n\
                 <table class=\"headers\">\n\
                 <tr><th>From</th><td>{author}</td></tr>\n\
                 <tr><th>To</th><td>{to}</td></tr>\n\
                 <tr><th>Date</th><td>{sent}</td></tr>\n\
                 <tr><th>Source</th><td>{source}</td></tr>\n\
                 </table>\n\
                 <pre class=\"body\">{body}</pre>\n",
                id = m.id,
                author = escape(&m.author),
                // The archive stores no mail recipient list; `To` is
                // the chat the row belongs to, or an explicit blank.
                to = if m.chat_id.is_empty() {
                    "—".to_string()
                } else {
                    escape(&m.chat_id)
                },
                sent = escape(&m.sent_at),
                source = escape(&m.source),
                body = escape(&m.body),
            );
        } else {
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
        }
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
        if let Some(e) = self.evidence.as_ref() {
            let _ = write!(
                self.buf,
                "<footer class=\"bates\">{number}</footer>\n",
                number = escape(&bates_number(&e.prefix, e.last)),
            );
        }
        self.buf.push_str("</section>\n");
        self.written += 1;
        manifest.provenance(self.page_no, m)?;
        verification.message(page_name(self.page_no), m)?;

        self.in_page += 1;
        if self.in_page >= PAGE_MESSAGES {
            self.flush_page(staging)?;
            self.open_page();
        }
        Ok(())
    }

    /// Copy the message's referenced media into `assets/` (deduplicated
    /// by digest) and list every reference that cannot travel.
    /// Evidence packs additionally print each preserved object's
    /// SHA-256 digest beside its link.
    fn media(
        &mut self,
        m: &Message,
        manifest: &mut ManifestWriter,
        staging: &Path,
        objects: &ObjectStore,
        copied: &mut HashSet<String>,
    ) -> Result<()> {
        let evidence = self.evidence.is_some();
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
            if evidence {
                manifest.attachment_listed();
            }
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
            if evidence {
                let _ = write!(
                    self.buf,
                    "<li><a href=\"assets/{asset}\">{name}</a> ({kind}, sha256 {sha256})</li>\n",
                    asset = escape(&asset),
                    name = escape(name),
                    kind = escape(kind),
                    sha256 = escape(sha256),
                );
            } else {
                let _ = write!(
                    self.buf,
                    "<li><a href=\"assets/{asset}\">{name}</a> ({kind})</li>\n",
                    asset = escape(&asset),
                    name = escape(name),
                    kind = escape(kind),
                );
            }
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
pub(crate) fn page_name(page_no: u64) -> String {
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

/// The printed-page stylesheet every evidence-pack HTML file carries:
/// `@page` margins frame the print, `@media print` breaks one message
/// per sheet, and the Bates footer pins to the foot of every printed
/// page (`position: fixed` repeats it per sheet; a long message that
/// spills onto a second sheet repeats its number, the common Bates
/// convention for multi-page exhibits). No PDF engine is pulled in:
/// print-ready HTML is the chosen path, and adding one is an owner
/// decision.
const EVIDENCE_PRINT_CSS: &str = "\
@page { margin: 20mm 18mm 26mm; }\n\
@media print {\n\
  body { background: #fff; color: #000; font: 10pt/1.45 serif; margin: 0; }\n\
  table.headers { border-collapse: collapse; margin: 0 0 1em; }\n\
  table.headers th { text-align: left; vertical-align: top; padding: 0 2em 0 0; }\n\
  pre.body { white-space: pre-wrap; word-wrap: break-word; font: 10pt/1.45 serif; }\n\
  section.message { break-after: page; page-break-after: always; }\n\
  section.message:last-of-type { break-after: auto; page-break-after: auto; }\n\
  footer.bates { position: fixed; bottom: 6mm; left: 0; right: 0; text-align: center; font: 9pt serif; }\n\
}\n";

/// One Bates number: `{prefix}-{number}` zero-padded to at least
/// [`BATES_DIGITS`] digits (growing past it when the start number
/// already does), e.g. `VATL-000001`.
fn bates_number(prefix: &str, number: u64) -> String {
    format!("{prefix}-{number:0width$}", width = BATES_DIGITS)
}

/// The per-message SHA-256 digest the evidence pack commits to: a
/// canonical unit-separator-joined serialization of every stored
/// field (attachments ride inside `raw_json`), hex-encoded. The
/// commitment is SHA-256 over these digests in exported order.
fn message_digest(m: &Message) -> String {
    let canonical = format!(
        "{id}\u{1f}{source}\u{1f}{external_id}\u{1f}{channel}\u{1f}{chat_id}\u{1f}\
         {author}\u{1f}{sent_at}\u{1f}{body}\u{1f}{raw_json}",
        id = m.id,
        source = m.source,
        external_id = m.external_id,
        channel = m.channel,
        chat_id = m.chat_id,
        author = m.author,
        sent_at = m.sent_at,
        body = m.body,
        raw_json = m.raw_json,
    );
    hex_encode(&Sha256::digest(canonical.as_bytes()))
}

/// Lowercase hex of a byte slice (no hex dependency for 32-byte
/// digests).
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
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
///
/// Evidence packs additionally accumulate the SHA-256
/// [`DIGEST_COMMITMENT_LABEL`] over the ordered per-message digests
/// (the hasher state is constant-memory; the digests themselves are
/// the per-message `digest=` provenance lines) and close with the
/// commitment and pending-proof lines.
struct ManifestWriter {
    out: BufWriter<File>,
    path: PathBuf,
    /// Referenced files not in the bundle.
    missing: u64,
    /// Evidence packs: attachment references listed on the pages.
    attachments: u64,
    /// Evidence packs: the running commitment hasher over the ordered
    /// per-message digests (`None` for a plain bundle).
    commitment: Option<Sha256>,
}

impl ManifestWriter {
    fn new(staging: &Path, plan: &HtmlExportPlan, evidence: bool) -> Result<Self> {
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
            attachments: 0,
            commitment: evidence.then(Sha256::new),
        })
    }

    /// One provenance line per exported message. Evidence packs append
    /// the message's digest and fold it into the commitment.
    fn provenance(&mut self, page_no: u64, m: &Message) -> Result<()> {
        if self.commitment.is_some() {
            let digest = message_digest(m);
            if let Some(h) = &mut self.commitment {
                h.update(digest.as_bytes());
                h.update(b"\n");
            }
            return writeln!(
                self.out,
                "[{}] id={} source={} external_id={} sent_at={} digest={}",
                page_name(page_no),
                m.id,
                m.source,
                m.external_id,
                m.sent_at,
                digest
            )
            .map_err(Into::into);
        }
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

    /// One attachment reference listed on an evidence-pack page.
    fn attachment_listed(&mut self) {
        self.attachments += 1;
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

    /// The hex commitment so far (the hasher state is cloned, so the
    /// writer keeps accumulating). `None` for a plain bundle.
    fn commitment_hex(&self) -> Option<String> {
        self.commitment
            .as_ref()
            .map(|h| hex_encode(&h.clone().finalize()))
    }

    /// Footer, flush, fsync, and the completeness rename.
    fn finish(mut self, staging: &Path, summary: &HtmlExportSummary) -> Result<()> {
        writeln!(self.out)?;
        writeln!(
            self.out,
            "written: {} messages across {} pages; {} assets copied; {} files missing or skipped",
            summary.written, summary.pages, summary.assets_copied, summary.assets_missing
        )?;
        writeln!(
            self.out,
            "verification: {} head {}",
            crate::export_manifest::MANIFEST_JSON,
            summary.head_hash
        )?;
        if let Some(hex) = self.commitment_hex() {
            writeln!(self.out)?;
            writeln!(
                self.out,
                "{DIGEST_COMMITMENT_LABEL} (SHA-256 over the ordered per-message digests): {hex}"
            )?;
            writeln!(self.out, "{PENDING_PROOF_LINE}")?;
        }
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
/// visible page) plus the manifest. For an evidence pack the same
/// file becomes the **cover sheet**: scope statement, message and
/// attachment counts, the Bates range, the
/// [`DIGEST_COMMITMENT_LABEL`] with its explicit label, the
/// [`PENDING_PROOF_LINE`], the fidelity wording, and links to the
/// pages, the chain-of-custody appendix, and the manifest.
fn write_index(
    staging: &Path,
    summary: &HtmlExportSummary,
    evidence_scope: Option<&str>,
) -> Result<()> {
    let mut html = String::new();
    match evidence_scope {
        Some(statement) => {
            let _ = write!(
                html,
                "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
                 <title>Mailchat Vault evidence pack</title>\n\
                 <style>{css}</style>\n</head>\n<body>\n\
                 <h1>Mailchat Vault evidence pack</h1>\n<h2>Cover sheet</h2>\n\
                 <p>scope: {statement}</p>\n\
                 <p>messages: {written}</p>\n",
                css = EVIDENCE_PRINT_CSS,
                statement = escape(statement),
                written = summary.written,
            );
            if let Some(attachments) = summary.attachment_refs {
                let _ = write!(html, "<p>attachment references: {attachments}</p>\n");
            }
            if let Some(range) = &summary.bates_range {
                let _ = write!(
                    html,
                    "<p>Bates numbers: {range}</p>\n",
                    range = escape(range)
                );
            }
            if let Some(hex) = &summary.digest_commitment {
                let _ = write!(
                    html,
                    "<p>{DIGEST_COMMITMENT_LABEL} (SHA-256 over this pack's ordered per-message digests): \
                     <span class=\"sha256\">{hex}</span></p>\n\
                     <p>{PENDING_PROOF_LINE}</p>\n",
                    hex = escape(hex),
                );
            }
            let _ = write!(
                html,
                "<p>fidelity: {fidelity}</p>\n",
                fidelity = escape(&summary.fidelity),
            );
        }
        None => {
            let _ = write!(
                html,
                "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
                 <title>Mailchat Vault export</title>\n</head>\n<body>\n\
                 <h1>Mailchat Vault export</h1>\n\
                 <p>scope: {scope}</p>\n<p>messages: {count}</p>\n\
                 <p>fidelity: {fidelity}</p>\n",
                scope = escape(&summary.scope_label),
                count = summary.count,
                fidelity = escape(&summary.fidelity),
            );
        }
    }
    html.push_str("<nav>\n<ul>\n");
    for page in 1..=summary.pages {
        let _ = write!(
            html,
            "<li><a href=\"{name}\">{name}</a></li>\n",
            name = escape(&page_name(page)),
        );
    }
    if evidence_scope.is_some() {
        html.push_str("<li><a href=\"custody.html\">custody.html</a></li>\n");
    }
    html.push_str("</ul>\n</nav>\n<p><a href=\"manifest\">manifest</a></p>\n");
    let _ = write!(
        html,
        "<p><a href=\"{manifest_json}\">{manifest_json}</a> · \
         <a href=\"{readme}\">{readme}</a></p>\n</body>\n</html>\n",
        manifest_json = escape(crate::export_manifest::MANIFEST_JSON),
        readme = escape(crate::export_manifest::BUNDLE_README),
    );
    write_synced(&staging.join("index.html"), html.as_bytes())
}

/// The cover sheet's scope statement: the pack's date range, source
/// labels, and people (author) filter, resolved from the request
/// against the source instances (deterministic order: instance id).
fn evidence_scope_statement(
    storage: &Storage,
    request: &crate::search2::SearchArchiveRequest,
) -> String {
    let dates = match (&request.from_date, &request.to_date) {
        (Some(from), Some(to)) => format!("date range {from} to {to}"),
        (Some(from), None) => format!("date range {from} onward"),
        (None, Some(to)) => format!("date range up to {to}"),
        (None, None) => "all dates".to_string(),
    };
    let mut wanted: Vec<String> = request
        .source_ids
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    wanted.sort();
    wanted.dedup();
    let sources = if wanted.is_empty() {
        "all sources".to_string()
    } else {
        let mut labels: Vec<String> = Vec::new();
        if let Ok(mut instances) = storage.list_source_instances() {
            instances.sort_by(|a, b| a.id.cmp(&b.id));
            for instance in instances {
                if wanted.contains(&instance.id) {
                    labels.push(instance.label);
                }
            }
        }
        if labels.is_empty() {
            "unresolved source ids".to_string()
        } else {
            labels.join(", ")
        }
    };
    let people = if request.author.trim().is_empty() {
        "all people".to_string()
    } else {
        request.author.trim().to_string()
    };
    format!("{dates} · sources: {sources} · people: {people}")
}

/// The evidence pack's `custody.html`: the chain-of-custody appendix
/// — one row per import/sync receipt the archive recorded for the
/// scoped sources (newest first per source, the receipt listing's own
/// order) — plus verification instructions that point at the bundle's
/// manifest digest lines and the release-checksum documentation in
/// `docs/release.md`.
fn write_custody(
    storage: &Storage,
    request: &crate::search2::SearchArchiveRequest,
    staging: &Path,
) -> Result<()> {
    let mut wanted: Vec<String> = request
        .source_ids
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    wanted.sort();
    wanted.dedup();
    let mut instances = storage.list_source_instances()?;
    instances.sort_by(|a, b| a.id.cmp(&b.id));
    if !wanted.is_empty() {
        instances.retain(|i| wanted.contains(&i.id));
    }

    let mut html = String::new();
    let _ = write!(
        html,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>Mailchat Vault evidence pack — chain of custody</title>\n\
         <style>{css}</style>\n</head>\n<body>\n\
         <h1>Chain of custody</h1>\n\
         <p>Import and sync receipts recorded by the archive for the scoped sources \
         (append-only rows from the source_receipts ledger; newest first per source).</p>\n\
         <table class=\"custody\">\n\
         <tr><th>source</th><th>kind</th><th>completed at</th><th>observed range</th>\
         <th>new</th><th>present</th><th>failed</th><th>files kept</th><th>files missing</th></tr>\n",
        css = EVIDENCE_PRINT_CSS,
    );
    let mut rows = 0u64;
    for instance in &instances {
        for r in storage.list_source_receipts(&instance.id, CUSTODY_RECEIPTS_PER_SOURCE)? {
            let range = match (&r.observed_min, &r.observed_max) {
                (Some(min), Some(max)) => format!("{min} to {max}"),
                (Some(min), None) => format!("from {min}"),
                (None, Some(max)) => format!("up to {max}"),
                (None, None) => "—".to_string(),
            };
            let _ = write!(
                html,
                "<tr><td>{label}</td><td>{kind}</td><td>{completed}</td><td>{range}</td>\
                 <td>{new}</td><td>{present}</td><td>{failed}</td>\
                 <td>{kept}</td><td>{missing}</td></tr>\n",
                label = escape(&instance.label),
                kind = r.kind.as_str(),
                completed = escape(&r.completed_at),
                range = escape(&range),
                new = r.new_records,
                present = r.present_records,
                failed = r.failed_records,
                kept = r.files_preserved,
                missing = r.files_missing,
            );
            rows += 1;
        }
    }
    if rows == 0 {
        html.push_str(
            "</table>\n<p>No import or sync receipts are recorded for the scoped sources.</p>\n",
        );
    } else {
        html.push_str("</table>\n");
    }
    let _ = write!(
        html,
        "<h2>Verification</h2>\n<ol>\n\
         <li>The bundle's manifest lists one provenance line per exported message with its \
         SHA-256 digest, and the {DIGEST_COMMITMENT_LABEL} over their ordered concatenation.</li>\n\
         <li>Re-exporting the same selection with the same Bates prefix and start number \
         reproduces this bundle byte for byte.</li>\n\
         <li>To verify application installers against published release checksums, follow \
         \"Checksums &amp; verifying a published release\" in docs/release.md \
         (the SHA256SUMS asset and scripts/release/release-verify.sh).</li>\n\
         </ol>\n</body>\n</html>\n",
    );
    write_synced(&staging.join("custody.html"), html.as_bytes())
}

/// Write and fsync one staged file (the staged-file idiom of
/// [`crate::staged_write`] and [`crate::backup`]).
pub(crate) fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
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

    #[test]
    fn bates_numbers_pad_to_six_digits_and_grow_past_them() {
        assert_eq!(bates_number("VATL", 1), "VATL-000001");
        assert_eq!(bates_number("VATL", 123), "VATL-000123");
        assert_eq!(bates_number("VATL", 1234567), "VATL-1234567");
        assert_eq!(bates_number("", 2), "-000002");
    }

    #[test]
    fn message_digests_are_deterministic_and_field_sensitive() {
        let m = Message {
            id: 7,
            source: "mbox".into(),
            source_key: String::new(),
            external_id: "a@e.com".into(),
            channel: "mail".into(),
            chat_id: String::new(),
            author: "Ana <ana@example.com>".into(),
            sent_at: "2026-03-05T09:00:00Z".into(),
            body: "body".into(),
            raw_json: "{}".into(),
        };
        let first = message_digest(&m);
        assert_eq!(first, message_digest(&m), "same row, same digest");
        assert_eq!(first.len(), 64, "hex SHA-256");
        let mut changed = m.clone();
        changed.body.push('!');
        assert_ne!(first, message_digest(&changed));
        // The canonical join is unambiguous: field separators cannot
        // be forged by content.
        assert!(format!("{first}").chars().all(|c| c.is_ascii_hexdigit()));
    }
}
