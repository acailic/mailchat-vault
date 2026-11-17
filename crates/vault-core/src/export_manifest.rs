//! The tamper-evident verification manifest for HTML export bundles
//! (A9): what a bundle looked like when the vault wrote it, and a
//! verifier that recomputes it from the bundle alone.
//!
//! Every finished HTML bundle carries a `manifest.json` beside the A8
//! pages: one entry per exported message (external id, the stored
//! [`crate::storage`] content digest, the retained original's digest
//! when there is one), the `source_receipts` rows covering each
//! message's source instance, and the SHA-256 of every page and asset
//! file — all chained git-commit-style into one head hash, where each
//! line hashes the previous line's value. [`verify_bundle`]
//! recomputes the chain from the bundle files and reports the first
//! divergence by name (message external id, page, or asset).
//!
//! # Chain
//!
//! The genesis previous-value is the fixed seed [`CHAIN_SEED`]; each
//! line is `sha256(prev ␟ field ␟ …)` with the same U+001F unit
//! separator the stored `content_hash` computation uses:
//!
//! 1. one line per message, in the export's existing stable order —
//!    `sha256(prev ␟ external_id ␟ message_digest ␟ original digest
//!    or empty)`;
//! 2. one line per page file and per asset — `sha256(prev ␟ file
//!    name ␟ sha256 of bytes)`;
//! 3. one line per receipt row — `sha256(prev ␟ "receipt" ␟ <the
//!    row's canonical JSON>)`.
//!
//! The final line's value is the head hash. Message and file entries
//! carry their line's chain value so the verifier can name the first
//! divergent row; receipt rows keep the plain
//! [`crate::storage::SourceReceipt`] shape, so a tampered receipt
//! surfaces as a head-hash divergence.
//!
//! # Honesty
//!
//! The chain is tamper *evidence* that the bundle bytes still match
//! what the archive exported, plus the receipts of what each import
//! and sync run observed. It is not third-party notarization, not a
//! trusted timestamp, and not a completeness claim beyond the
//! receipts — the in-bundle `README.md` says so plainly, and the
//! SHA-256 conventions follow [`docs/release.md`]. No scripts ever
//! ride in the bundle; verification re-derives everything from bytes.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::message::Message;
use crate::storage::{SourceReceipt, Storage};
use crate::{Error, Result};

/// The verification manifest's file name inside a bundle.
pub const MANIFEST_JSON: &str = "manifest.json";

/// The in-bundle human-readable statement of what the manifest does
/// and does not prove.
pub const BUNDLE_README: &str = "README.md";

/// The manifest shape's version. Bump only with a breaking change to
/// the entry layout; [`verify_bundle`] refuses other versions by
/// name.
pub const MANIFEST_VERSION: u32 = 1;

/// The genesis previous-value of the chain: a fixed string, so the
/// first message line (and every empty-scope head) hashes a value no
/// bundle content chose.
pub const CHAIN_SEED: &str = "mailchat-vault-export-manifest-v1";

/// The U+001F unit separator the archive's digest computations join
/// fields with (see the `content_hash` formula in `crate::storage`).
const SEP: char = '\u{1f}';

/// One message line of the manifest: the exported message's identity,
/// digests, page, and the chain value after this line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestMessage {
    pub external_id: String,
    /// The stored `content_hash` digest of the message (sha256 over
    /// external id, author, sent instant, and body).
    pub message_digest: String,
    /// The object-store digest of the retained original bytes —
    /// present only when the archive retained an original for this
    /// message; absent otherwise (the chain hashes the empty string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_digest: Option<String>,
    /// The page file the message renders on (`page-0007.html`).
    pub page: String,
    /// The chain value after this message's line.
    pub chain: String,
}

/// One file line of the manifest: a page or asset file's name, its
/// SHA-256, and the chain value after this file's line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    /// Bundle-relative path (`page-0002.html`, `assets/<sha256>.jpg`).
    pub path: String,
    pub sha256: String,
    /// The chain value after this file's line.
    pub chain: String,
}

/// The verification manifest written into every finished HTML bundle.
/// Field order is the serialization order; nothing here iterates a
/// hash map, so re-exports of the same data are byte-stable and the
/// head hash matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationManifest {
    pub version: u32,
    /// `"mailchat-vault"` — the writer, for future readers.
    pub generator: String,
    pub scope_label: String,
    /// The plan's total for the exported scope.
    pub count: u64,
    /// Messages actually written.
    pub written: u64,
    pub fidelity: String,
    /// One entry per exported message, in the export's stable order.
    pub messages: Vec<ManifestMessage>,
    /// The receipt rows covering the exported messages' source
    /// instances (each instance's rows newest first, instances in
    /// first-appearance order) — serialized with the existing
    /// [`SourceReceipt`] shape.
    pub receipts: Vec<SourceReceipt>,
    /// Every page and asset file, pages in numeric order then assets
    /// sorted by name.
    pub files: Vec<ManifestFile>,
    /// The final chain value: the one hash a re-export of the same
    /// data reproduces and any later tampering breaks.
    pub head_hash: String,
}

/// What [`verify_bundle`] found: either the bundle still matches its
/// manifest, or the exact first divergence, by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyReport {
    /// True only when every file hash and every chain line
    /// recomputed to the recorded head hash.
    pub ok: bool,
    /// The recomputed head hash (the empty string when the manifest
    /// could not be read or parsed at all).
    pub head_hash: String,
    /// Message lines checked.
    pub messages: u64,
    /// File lines checked.
    pub files: u64,
    /// The first divergence, phrased for a person: the message
    /// external id, page (with its first message), asset name, or the
    /// manifest field that broke. `None` exactly when `ok`.
    pub first_divergence: Option<String>,
}

// ---------------------------------------------------------------------------
// Chain
// ---------------------------------------------------------------------------

/// The running chain: each `…_line` call hashes the previous value
/// plus the line's fields and returns the new value, git-commit
/// style. Lines are appended in manifest order — messages, then
/// files, then receipts — which the collector's phases enforce.
struct ChainBuilder {
    prev: String,
}

impl ChainBuilder {
    fn new() -> Self {
        Self {
            prev: CHAIN_SEED.to_string(),
        }
    }

    fn step(&mut self, parts: &[&str]) -> String {
        let mut input = String::with_capacity(self.prev.len() + 64);
        input.push_str(&self.prev);
        for part in parts {
            input.push(SEP);
            input.push_str(part);
        }
        self.prev = crate::util::sha256_hex(input.as_bytes());
        self.prev.clone()
    }

    /// One message line: the previous value, external id, message
    /// digest, and original digest (empty when none).
    fn message_line(&mut self, external_id: &str, digest: &str, original: Option<&str>) -> String {
        let original = original.unwrap_or("");
        self.step(&[external_id, digest, original])
    }

    /// One receipt line: the previous value plus the row's canonical
    /// JSON (the serde field order of [`SourceReceipt`], fixed).
    fn receipt_line(&mut self, receipt: &SourceReceipt) -> Result<String> {
        let json = serde_json::to_string(receipt)
            .map_err(|e| Error::InvalidExport(format!("serialize receipt: {e}")))?;
        Ok(self.step(&["receipt", &json]))
    }

    /// One file line: the previous value, file name, and the file's
    /// SHA-256.
    fn file_line(&mut self, name: &str, sha256: &str) -> String {
        self.step(&[name, sha256])
    }

    fn head(&self) -> &str {
        &self.prev
    }
}

// ---------------------------------------------------------------------------
// Export-side collector
// ---------------------------------------------------------------------------

/// Streams the verification facts while the bundle's pages stream:
/// per message the stored digest, the retained original's digest when
/// present, and the covering instance's receipt rows (read from
/// existing tables only — the export never writes). One `message`
/// call per exported message, in the export's stable order; `finish`
/// appends the receipts and file lines and writes `manifest.json`
/// plus the `README.md` into the staged bundle.
pub(crate) struct VerificationCollector<'a> {
    storage: &'a Storage,
    chain: ChainBuilder,
    messages: Vec<ManifestMessage>,
    receipts: Vec<SourceReceipt>,
    instances_seen: HashSet<String>,
}

impl<'a> VerificationCollector<'a> {
    pub(crate) fn new(storage: &'a Storage) -> Self {
        Self {
            storage,
            chain: ChainBuilder::new(),
            messages: Vec::new(),
            receipts: Vec::new(),
            instances_seen: HashSet::new(),
        }
    }

    /// Record one streamed message on its page and advance the chain.
    pub(crate) fn message(&mut self, page: String, m: &Message) -> Result<()> {
        let verification = self.storage.message_verification(m.id)?.ok_or_else(|| {
            Error::InvalidExport(format!(
                "message {} vanished from the archive mid-export",
                m.id
            ))
        })?;
        if self
            .instances_seen
            .insert(verification.source_instance_id.clone())
        {
            // Every receipt row of the covering instance: the manifest
            // is the complete receipt evidence for what the archive's
            // runs observed, newest first (list order).
            self.receipts.extend(
                self.storage
                    .list_source_receipts(&verification.source_instance_id, u32::MAX)?,
            );
        }
        let original_digest = self
            .storage
            .original_for_message(m.id)?
            .map(|original| original.digest);
        let chain = self.chain.message_line(
            &m.external_id,
            &verification.content_hash,
            original_digest.as_deref(),
        );
        self.messages.push(ManifestMessage {
            external_id: m.external_id.clone(),
            message_digest: verification.content_hash,
            original_digest,
            page,
            chain,
        });
        Ok(())
    }

    /// Append the file and receipt lines, seal the head hash, and
    /// write `manifest.json` and the `README.md` into the staging
    /// directory — before the completeness rename, so an incomplete
    /// bundle never carries either.
    pub(crate) fn finish(
        mut self,
        staging: &Path,
        pages: u64,
        assets: &BTreeSet<String>,
        scope_label: &str,
        count: u64,
        written: u64,
        fidelity: &str,
    ) -> Result<VerificationManifest> {
        let mut files = Vec::new();
        for page in 1..=pages {
            let name = crate::export_html::page_name(page);
            let sha = file_sha256(&staging.join(&name))?;
            let chain = self.chain.file_line(&name, &sha);
            files.push(ManifestFile {
                path: name,
                sha256: sha,
                chain,
            });
        }
        for asset in assets {
            let name = format!("assets/{asset}");
            let sha = file_sha256(&staging.join(&name))?;
            let chain = self.chain.file_line(&name, &sha);
            files.push(ManifestFile {
                path: name,
                sha256: sha,
                chain,
            });
        }
        for receipt in &self.receipts {
            self.chain.receipt_line(receipt)?;
        }
        let manifest = VerificationManifest {
            version: MANIFEST_VERSION,
            generator: "mailchat-vault".to_string(),
            scope_label: scope_label.to_string(),
            count,
            written,
            fidelity: fidelity.to_string(),
            messages: self.messages,
            receipts: self.receipts,
            files,
            head_hash: self.chain.head().to_string(),
        };
        let json = serde_json::to_string_pretty(&manifest)?;
        crate::export_html::write_synced(&staging.join(MANIFEST_JSON), json.as_bytes())?;
        let readme = readme_bytes();
        crate::export_html::write_synced(&staging.join(BUNDLE_README), &readme)?;
        Ok(manifest)
    }
}

/// SHA-256 of a staged file's bytes (the digest form every checksum
/// surface in this repo uses — see `docs/release.md`).
fn file_sha256(path: &Path) -> Result<String> {
    Ok(crate::util::sha256_hex(&fs::read(path)?))
}

// ---------------------------------------------------------------------------
// README
// ---------------------------------------------------------------------------

/// The in-bundle `README.md`: what the manifest proves and what it
/// does not, stated plainly. No scripts — a plain file a browser or
/// file manager renders as-is.
fn readme_bytes() -> Vec<u8> {
    let text = "\
# About this export's verification manifest

This bundle carries a `manifest.json` beside its pages. It records, for
every exported message, the archive's stored digest of that message and
the digest of its retained original bytes (when the archive retained
one); the run receipts (`source_receipts`) of the import and sync runs
that covered each message's source; and the SHA-256 of every page and
asset file. Those lines are chained git-commit-style — each line hashes
the previous one — into a single head hash.

## What that proves

If any exported page, asset, or recorded digest changes after the
export, the chain no longer reproduces the recorded head hash, and the
bundle's Verify function names the first divergent message, page, or
file. Re-exporting the same unchanged data reproduces the same head
hash.

## What it does not

- This is tamper **evidence** about the bundle from the moment the
  archive exported it. It is **not** third-party notarization and
  carries no trusted timestamp: the clock is the exporting machine's.
- The receipts are evidence of what each completed run observed. They
  are not a completeness claim — an observed date range says nothing
  about coverage between its endpoints.
- The chain covers the pages and assets listed in `manifest.json`.
  Files outside it (this README, `index.html`, the manifest itself)
  are not chained.
- Digests are SHA-256, the same convention the release checksums use.

To verify, open this bundle with the vault and choose \"Verify an
export\", pointing it at this directory.
";
    text.as_bytes().to_vec()
}

// ---------------------------------------------------------------------------
// Verify
// ---------------------------------------------------------------------------

/// Recompute a bundle's verification chain from its files alone and
/// report the exact first divergence. A bundle without `manifest.json`
/// reports clearly incomplete (it is either not a finished export or
/// was stripped); an unreadable or altered manifest reports as a
/// divergence, never as success.
pub fn verify_bundle(dir: &Path) -> Result<VerifyReport> {
    let manifest_path = dir.join(MANIFEST_JSON);
    if !manifest_path.is_file() {
        return Ok(VerifyReport {
            ok: false,
            head_hash: String::new(),
            messages: 0,
            files: 0,
            first_divergence: Some(format!(
                "{MANIFEST_JSON} is missing: the bundle is incomplete (a finished export always carries one)"
            )),
        });
    }
    let manifest: VerificationManifest =
        match serde_json::from_str(&fs::read_to_string(&manifest_path)?) {
            Ok(manifest) => manifest,
            Err(e) => {
                return Ok(VerifyReport {
                    ok: false,
                    head_hash: String::new(),
                    messages: 0,
                    files: 0,
                    first_divergence: Some(format!("{MANIFEST_JSON} cannot be parsed: {e}")),
                });
            }
        };
    let divergence = |detail: String| {
        Ok(VerifyReport {
            ok: false,
            head_hash: manifest.head_hash.clone(),
            messages: manifest.messages.len() as u64,
            files: manifest.files.len() as u64,
            first_divergence: Some(detail),
        })
    };

    if manifest.version != MANIFEST_VERSION {
        return divergence(format!(
            "{MANIFEST_JSON} carries version {}, this build verifies version {MANIFEST_VERSION}",
            manifest.version
        ));
    }

    // Phase 1 — every listed file still hashes to its recorded digest.
    // A page's divergence names the page's first message; an asset's
    // names the asset.
    let mut recomputed: Vec<(String, String)> = Vec::with_capacity(manifest.files.len());
    for file in &manifest.files {
        let path = dir.join(&file.path);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                return divergence(format!(
                    "{}: {} ({})",
                    describe_file(&manifest, &file.path),
                    file.path,
                    e
                ))
            }
        };
        let sha = crate::util::sha256_hex(&bytes);
        if sha != file.sha256 {
            return divergence(format!(
                "{} does not match its recorded digest ({})",
                describe_file(&manifest, &file.path),
                file.path
            ));
        }
        recomputed.push((file.path.clone(), sha));
    }

    // Phase 2 — the chain, line by line, in manifest order: messages,
    // files, receipts, head.
    let mut chain = ChainBuilder::new();
    for message in &manifest.messages {
        let line = chain.message_line(
            &message.external_id,
            &message.message_digest,
            message.original_digest.as_deref(),
        );
        if line != message.chain {
            return divergence(format!(
                "the chain diverges at message {} (recorded {}, recomputed {line})",
                message.external_id, message.chain
            ));
        }
    }
    for (file, (path, sha)) in manifest.files.iter().zip(recomputed.iter()) {
        let line = chain.file_line(path, sha);
        if line != file.chain {
            return divergence(format!(
                "the chain diverges at file {} (recorded {}, recomputed {line})",
                file.path, file.chain
            ));
        }
    }
    for receipt in &manifest.receipts {
        chain.receipt_line(receipt)?;
    }
    if chain.head() != manifest.head_hash {
        return divergence(format!(
            "the head hash does not match: {MANIFEST_JSON} records {}, the bundle recomputes {}",
            manifest.head_hash,
            chain.head()
        ));
    }

    Ok(VerifyReport {
        ok: true,
        head_hash: manifest.head_hash.clone(),
        messages: manifest.messages.len() as u64,
        files: manifest.files.len() as u64,
        first_divergence: None,
    })
}

/// How a divergent file is named: a page by its first message, an
/// asset (or anything else) by its path.
fn describe_file(manifest: &VerificationManifest, path: &str) -> String {
    if let Some(first) = manifest.messages.iter().find(|m| m.page == path) {
        format!("page {path} (first message: {})", first.external_id)
    } else {
        format!("file {path}")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_genesis_seed_heads_an_empty_chain() {
        let chain = ChainBuilder::new();
        assert_eq!(chain.head(), CHAIN_SEED);
    }

    #[test]
    fn each_line_hashes_the_previous() {
        let mut chain = ChainBuilder::new();
        let first = chain.message_line("a@e.com", "digest-a", None);
        let second = chain.message_line("b@e.com", "digest-b", Some("original-b"));
        let file = chain.file_line("page-0001.html", "file-sha");
        assert_ne!(first, second);
        assert_ne!(second, file);
        // Manual recomputation of the second line over the first.
        let input = format!("{first}{SEP}b@e.com{SEP}digest-b{SEP}original-b");
        assert_eq!(second, crate::util::sha256_hex(input.as_bytes()));
        assert_eq!(chain.head(), file);
    }

    #[test]
    fn a_missing_original_and_an_empty_original_hash_identically() {
        let mut with_none = ChainBuilder::new();
        let mut with_empty = ChainBuilder::new();
        assert_eq!(
            with_none.message_line("x", "d", None),
            with_empty.message_line("x", "d", Some(""))
        );
    }

    #[test]
    fn the_readme_states_both_directions_honestly() {
        let text = String::from_utf8(readme_bytes()).unwrap();
        assert!(text.contains("tamper **evidence**"));
        assert!(text.contains("third-party notarization"));
        assert!(text.contains("not a completeness claim"));
        assert!(text.contains("SHA-256"));
    }
}
