//! Person timeline: one chronological thread per person across mail
//! and chat (the backend slice of the unified person view).
//!
//! The query unions the messages matched by **every** identity link of
//! one person and returns them as a single sent-instant-ordered
//! stream, so "everything that ever happened between me and this
//! person" reads as one interleaved thread — mail next to WhatsApp
//! next to Telegram, in the order they happened.
//!
//! - Matching reuses the exact per-anchor semantics of the
//!   link-preview path ([`crate::storage`]): stable-key anchors match
//!   the case-folded address extracted from the verbatim author
//!   ([`author_stable_key`]); author-name anchors match the verbatim
//!   author string within the link's source instance. Exact matching
//!   only, no fuzzy matching, and archived rows are never rewritten
//!   or re-attributed.
//! - Ordering mirrors the search2 contract (A07): resolved instants
//!   ascending (a thread reads oldest → newest), unknown instants
//!   **last** — deterministically by id — and a stable id tiebreak.
//! - Pages continue through an opaque keyset cursor following the
//!   search2 cursor contract: the last row's sort position as
//!   versioned, mode-tagged base64 (a malformed cursor is rejected,
//!   never silently restarted). There is no offset paging at all.
//! - Header facts (first/last contact, totals, per-source counts) and
//!   the identity links with their confirmed-versus-inferred state
//!   derive from the same matched set as the thread, so the header
//!   cannot drift from the rows it summarizes. Every link row is a
//!   user-confirmed association; the *match state* says how strong
//!   that anchor's evidence is — a stable key pins an address, an
//!   author name is inherently ambiguous display-string evidence.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as CURSOR_BASE64;
use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::search2::{browse_snippet, read_archive_revision, DEFAULT_LIMIT, MAX_LIMIT};
use crate::storage::{
    author_stable_key, organization_revision_on, Person, PersonAnchorKind, PersonIdentityLink,
    Storage,
};
use crate::{Error, Result};

// ---------------------------------------------------------------------------
// Request / response shapes (snake_case JSON over IPC in later slices)
// ---------------------------------------------------------------------------

/// One person-timeline request: the person whose thread is read, a page
/// size (default 50, clamped to `1..=200` — the search2 page contract),
/// and the opaque keyset cursor from a previous page's `next_cursor`.
/// There is deliberately no offset: deep threads page at first-page
/// cost through the cursor alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonTimelineRequest {
    pub person_id: String,
    pub limit: Option<u32>,
    /// Opaque keyset cursor from a previous page's `next_cursor`; the
    /// page starts strictly after the cursor row's sort position. A
    /// malformed cursor is rejected with [`Error::InvalidQuery`].
    #[serde(default)]
    pub cursor: Option<String>,
}

impl Default for PersonTimelineRequest {
    fn default() -> Self {
        Self {
            person_id: String::new(),
            limit: Some(DEFAULT_LIMIT),
            cursor: None,
        }
    }
}

/// How strong one identity link's match evidence is — the honesty
/// label the person view renders next to every linked identity. Every
/// link is a user-confirmed association; the state distinguishes what
/// the anchor *pins down*:
///
/// - `confirmed`: a stable-key anchor — the source-scoped stable key
///   (an email address, a provider user id) extracted from the
///   verbatim author. Address-level evidence.
/// - `inferred`: an author-name anchor — the verbatim display string,
///   inherently ambiguous (two humans can share a display name),
///   matched exactly within its source instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonLinkMatchState {
    Confirmed,
    Inferred,
}

impl PersonLinkMatchState {
    /// The state one anchor kind's matches carry.
    fn from_anchor_kind(anchor_kind: PersonAnchorKind) -> Self {
        match anchor_kind {
            PersonAnchorKind::StableKey => Self::Confirmed,
            PersonAnchorKind::AuthorName => Self::Inferred,
        }
    }
}

/// One linked identity of the person, as the timeline header renders
/// it: the anchor kind and value, the display label, the
/// confirmed-versus-inferred match state, and how many archived
/// messages this one link matches in its source instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonTimelineIdentity {
    pub link_id: String,
    pub source_instance_id: String,
    pub source_label: String,
    pub anchor_kind: PersonAnchorKind,
    pub anchor_value: String,
    pub display_label: String,
    pub match_state: PersonLinkMatchState,
    /// Archived messages this link's anchor matches (preview-path
    /// semantics: per-anchor, before cross-link dedup).
    pub matched_messages: u64,
}

/// Per-source message count for the header facts, one row per source
/// instance the person's matched messages live in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonTimelineSourceCount {
    pub source_instance_id: String,
    pub source_label: String,
    pub messages: u64,
}

/// The thread's header facts. Derived from one aggregate pass over the
/// same matched set the thread pages through, so the facts cannot
/// drift from the rows they summarize. First/last contact come from
/// dated messages only — unknown instants have no position on the
/// timeline (they still count toward the totals and page last).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonTimelineHeader {
    pub first_contact: Option<String>,
    pub last_contact: Option<String>,
    pub total_messages: u64,
    /// One row per source, sorted by label then id (deterministic).
    pub per_source: Vec<PersonTimelineSourceCount>,
}

/// One row of the interleaved thread: a summary-shaped message with
/// the source identity the per-row source badge renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonTimelineMessage {
    pub id: i64,
    pub source_instance_id: String,
    pub source_label: String,
    /// `email` / `chat` — the coarse channel the badge rides on.
    pub channel: String,
    pub conversation_id: Option<String>,
    pub author: String,
    pub subject: Option<String>,
    /// Resolved UTC instant only; `None` when unknown (sorts last).
    pub sent_at: Option<String>,
    pub time_confidence: String,
    /// Bounded body prefix (the search2 browse snippet: HTML-extracted,
    /// whitespace-collapsed — display only).
    pub snippet: String,
}

/// One person-timeline page: the thread rows, the header facts over
/// the whole matched set, the person's linked identities, and the
/// keyset continuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonTimelinePage {
    pub person: Person,
    pub header: PersonTimelineHeader,
    pub identities: Vec<PersonTimelineIdentity>,
    pub messages: Vec<PersonTimelineMessage>,
    pub limit: u32,
    /// Keyset cursor to the next page — the last row's sort position.
    /// `None` when this page reaches the end of the thread.
    pub next_cursor: Option<String>,
    /// Snapshot markers of the read: the archive revision (content)
    /// and the organization revision (links), so later slices can
    /// offer a refresh instead of silently mixing snapshots.
    pub archive_revision: u64,
    pub organization_revision: u64,
}

// ---------------------------------------------------------------------------
// Keyset cursor (the search2 cursor contract, ascending form)
// ---------------------------------------------------------------------------

/// Wire form of a person-timeline cursor: JSON, then URL-safe base64
/// without padding — opaque to clients, version- and mode-guarded like
/// the search2 cursor. The timeline has exactly one order mode; the
/// tag still guards against a cursor from another read path being
/// silently reinterpreted.
#[derive(Serialize, Deserialize)]
struct CursorWire {
    v: u32,
    mode: String,
    sent: Option<String>,
    id: i64,
}

/// Cursor format version this build reads and writes.
const CURSOR_VERSION: u32 = 1;
/// The one order mode a person-timeline cursor is bound to.
const CURSOR_MODE: &str = "person_timeline";

/// The last row's validated sort position.
struct TimelineCursor {
    sent: Option<String>,
    id: i64,
}

fn encode_timeline_cursor(cursor: &TimelineCursor) -> Result<String> {
    let wire = CursorWire {
        v: CURSOR_VERSION,
        mode: CURSOR_MODE.to_string(),
        sent: cursor.sent.clone(),
        id: cursor.id,
    };
    let json = serde_json::to_string(&wire)
        .map_err(|err| Error::InvalidQuery(format!("malformed cursor: {err}")))?;
    Ok(CURSOR_BASE64.encode(json))
}

/// Parse and validate an incoming cursor string. Every failure is
/// [`Error::InvalidQuery`] — a malformed cursor is never silently
/// ignored (that would restart pagination mid-thread).
fn decode_timeline_cursor(raw: &str) -> Result<TimelineCursor> {
    let malformed = |detail: &str| Error::InvalidQuery(format!("malformed cursor: {detail}"));
    let json = CURSOR_BASE64
        .decode(raw.trim())
        .map_err(|_| malformed("not valid cursor text"))?;
    let wire: CursorWire =
        serde_json::from_slice(&json).map_err(|_| malformed("not valid cursor text"))?;
    if wire.v != CURSOR_VERSION {
        return Err(malformed("unknown cursor version"));
    }
    if wire.mode != CURSOR_MODE {
        return Err(malformed(&format!(
            "cursor was issued for {} order, not person_timeline order",
            wire.mode
        )));
    }
    if let Some(sent) = &wire.sent {
        if sent.is_empty() {
            return Err(malformed("cursor instant must be non-empty when present"));
        }
    }
    Ok(TimelineCursor {
        sent: wire.sent,
        id: wire.id,
    })
}

/// The lightweight matched-row form the aggregate pass sorts: a row id,
/// the source instance it lives in, and its resolved instant (`None` =
/// unknown). The sort key is `(sent IS NULL) ASC, sent ASC, id ASC` —
/// chronological with unknown instants last and a stable id tiebreak.
#[derive(Debug, Clone)]
struct MatchedRow {
    id: i64,
    source_instance_id: String,
    sent: Option<String>,
}

impl MatchedRow {
    fn sort_key(&self) -> (bool, &Option<String>, i64) {
        (self.sent.is_none(), &self.sent, self.id)
    }

    /// Whether this row sits strictly after the cursor's sort position
    /// (the ascending keyset predicate — unknowns sort last, so every
    /// unknown row follows a dated cursor, and only lower-id unknowns
    /// follow an unknown cursor).
    fn is_after(&self, cursor: &TimelineCursor) -> bool {
        match &cursor.sent {
            None => self.sent.is_none() && self.id > cursor.id,
            Some(bound) => match &self.sent {
                None => true,
                Some(instant) => instant > bound || (instant == bound && self.id > cursor.id),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Storage read path
// ---------------------------------------------------------------------------

impl Storage {
    /// One person's timeline: the union of the messages matched by
    /// every identity link of `person_id`, ordered chronologically
    /// (unknown instants last, id tiebreak), paged by an opaque keyset
    /// cursor. The header facts and the linked identities derive from
    /// the same matched set as the rows.
    ///
    /// Matching is exactly the link-preview path's per-anchor
    /// semantics: author-name anchors match the verbatim author string
    /// within the link's source instance; stable-key anchors match the
    /// case-folded address extracted from the verbatim author
    /// (`Name <address>` and bare-address forms — display-only chat
    /// authors and lookalike addresses never match). A message matched
    /// by several of the person's links appears once.
    ///
    /// Unknown persons are refused with [`Error::PersonNotFound`];
    /// malformed cursors with [`Error::InvalidQuery`].
    pub fn person_timeline(&self, request: &PersonTimelineRequest) -> Result<PersonTimelinePage> {
        let person_id = request.person_id.trim();
        let limit = request.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let cursor = match &request.cursor {
            None => None,
            Some(raw) => Some(decode_timeline_cursor(raw)?),
        };

        let tx = self.conn.unchecked_transaction()?;

        let person: Option<Person> = tx
            .query_row(
                "SELECT id, name, created_at, updated_at FROM people WHERE id = ?1",
                params![person_id],
                |row| {
                    Ok(Person {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        created_at: row.get(2)?,
                        updated_at: row.get(3)?,
                    })
                },
            )
            .optional()?;
        let Some(person) = person else {
            return Err(Error::PersonNotFound(person_id.to_string()));
        };

        // The person's links, oldest first (same read as
        // `person_identity_links`, inside this page's snapshot).
        let links = self.person_identity_links(&person.id)?;
        let labels = source_labels(&tx)?;

        // One aggregate pass over the person's whole matched set:
        // per-link matching (preview semantics), cross-link dedup,
        // then the chronological merge the page reads.
        let mut seen: HashSet<i64> = HashSet::new();
        let mut rows: Vec<MatchedRow> = Vec::new();
        let mut per_link_matches: Vec<u64> = Vec::with_capacity(links.len());
        for link in &links {
            let (matched, linked_rows) = anchor_matched_rows(&tx, link)?;
            per_link_matches.push(matched);
            for row in linked_rows {
                if seen.insert(row.id) {
                    rows.push(row);
                }
            }
        }
        rows.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));

        let header = PersonTimelineHeader {
            first_contact: rows
                .iter()
                .filter_map(|row| row.sent.as_deref())
                .min()
                .map(str::to_string),
            last_contact: rows
                .iter()
                .filter_map(|row| row.sent.as_deref())
                .max()
                .map(str::to_string),
            total_messages: rows.len() as u64,
            per_source: per_source_counts(&rows, &labels),
        };

        let identities: Vec<PersonTimelineIdentity> = links
            .iter()
            .zip(per_link_matches)
            .map(|(link, matched_messages)| PersonTimelineIdentity {
                link_id: link.id.clone(),
                source_label: labels
                    .get(&link.source_instance_id)
                    .cloned()
                    .unwrap_or_else(|| link.source_instance_id.clone()),
                source_instance_id: link.source_instance_id.clone(),
                anchor_kind: link.anchor_kind,
                anchor_value: link.anchor_value.clone(),
                display_label: link.display_label.clone(),
                match_state: PersonLinkMatchState::from_anchor_kind(link.anchor_kind),
                matched_messages,
            })
            .collect();

        // The keyset page: rows strictly after the cursor's sort
        // position, one probe row past the page size.
        let fetch = limit as usize + 1;
        let mut page_rows: Vec<&MatchedRow> = rows
            .iter()
            .filter(|row| match &cursor {
                Some(cursor) => row.is_after(cursor),
                None => true,
            })
            .take(fetch)
            .collect();
        let has_more = page_rows.len() == fetch;
        if has_more {
            page_rows.truncate(limit as usize);
        }
        let messages = fetch_page_messages(&tx, &page_rows)?;
        let next_cursor = if has_more {
            page_rows
                .last()
                .map(|last| {
                    encode_timeline_cursor(&TimelineCursor {
                        sent: last.sent.clone(),
                        id: last.id,
                    })
                })
                .transpose()?
        } else {
            None
        };

        let archive_revision = read_archive_revision(&tx)?;
        let organization_revision = organization_revision_on(&tx)?;
        tx.commit()?;

        Ok(PersonTimelinePage {
            person,
            header,
            identities,
            messages,
            limit,
            next_cursor,
            archive_revision,
            organization_revision,
        })
    }
}

/// Source-instance id → label map for the per-source header facts and
/// the per-row source badge.
fn source_labels(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT id, label FROM source_instances")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
}

/// The messages one identity link's anchor matches in its source
/// instance (the link-preview path's exact per-anchor semantics),
/// as lightweight rows plus the total match count.
fn anchor_matched_rows(
    conn: &Connection,
    link: &PersonIdentityLink,
) -> Result<(u64, Vec<MatchedRow>)> {
    const NAME_ROW_SQL: &str = "SELECT id, sent_instant FROM archive_messages
             WHERE source_instance_id = ?1";
    match link.anchor_kind {
        PersonAnchorKind::AuthorName => {
            let mut stmt = conn.prepare(&format!("{NAME_ROW_SQL} AND author = ?2"))?;
            let rows =
                stmt.query_map(params![link.source_instance_id, link.anchor_value], |row| {
                    Ok(MatchedRow {
                        id: row.get(0)?,
                        source_instance_id: link.source_instance_id.clone(),
                        sent: row.get(1)?,
                    })
                })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok((out.len() as u64, out))
        }
        PersonAnchorKind::StableKey => {
            // The `instr` prefilter (case-folded both sides — stable
            // keys are always stored lowercased) keeps the scan bounded
            // to authors that even mention the address; the exact check
            // extracts and case-folds each author's address in Rust.
            let mut stmt = conn.prepare(
                "SELECT id, author, sent_instant FROM archive_messages
                 WHERE source_instance_id = ?1 AND instr(lower(author), ?2) > 0",
            )?;
            let candidates =
                stmt.query_map(params![link.source_instance_id, link.anchor_value], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?;
            let mut out = Vec::new();
            for entry in candidates {
                let (id, author, sent) = entry?;
                if author_stable_key(&author).as_deref() != Some(link.anchor_value.as_str()) {
                    continue;
                }
                out.push(MatchedRow {
                    id,
                    source_instance_id: link.source_instance_id.clone(),
                    sent,
                });
            }
            Ok((out.len() as u64, out))
        }
    }
}

/// Per-source counts over the deduped matched rows, sorted by label
/// then id — deterministic, one row per source the thread touches.
fn per_source_counts(
    rows: &[MatchedRow],
    labels: &HashMap<String, String>,
) -> Vec<PersonTimelineSourceCount> {
    let mut counts: HashMap<&str, u64> = HashMap::new();
    for row in rows {
        *counts.entry(row.source_instance_id.as_str()).or_default() += 1;
    }
    let mut out: Vec<PersonTimelineSourceCount> = counts
        .into_iter()
        .map(|(source_instance_id, messages)| PersonTimelineSourceCount {
            source_label: labels
                .get(source_instance_id)
                .cloned()
                .unwrap_or_else(|| source_instance_id.to_string()),
            source_instance_id: source_instance_id.to_string(),
            messages,
        })
        .collect();
    out.sort_by(|a, b| {
        a.source_label
            .cmp(&b.source_label)
            .then(a.source_instance_id.cmp(&b.source_instance_id))
    });
    out
}

/// Fetch the full timeline rows for one page's matched ids, preserving
/// the page's order (the sort key travels with the ids; the fetch
/// itself is unordered).
fn fetch_page_messages(
    conn: &Connection,
    page_rows: &[&MatchedRow],
) -> Result<Vec<PersonTimelineMessage>> {
    if page_rows.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; page_rows.len()].join(", ");
    let sql = format!(
        "SELECT m.id, m.source_instance_id, si.label, m.channel, m.conversation_id,
                m.author, NULLIF(m.subject, ''), m.sent_instant, m.time_confidence,
                substr(m.body, 1, 4000)
         FROM archive_messages AS m
         JOIN source_instances AS si ON si.id = m.source_instance_id
         WHERE m.id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let ids: Vec<i64> = page_rows.iter().map(|row| row.id).collect();
    let fetched = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |row| {
        let body: String = row.get(9)?;
        Ok((
            row.get::<_, i64>(0)?,
            PersonTimelineMessage {
                id: row.get(0)?,
                source_instance_id: row.get(1)?,
                source_label: row.get(2)?,
                channel: row.get(3)?,
                conversation_id: row.get(4)?,
                author: row.get(5)?,
                subject: row.get(6)?,
                sent_at: row.get(7)?,
                time_confidence: row.get(8)?,
                snippet: browse_snippet(&body),
            },
        ))
    })?;
    let mut by_id: HashMap<i64, PersonTimelineMessage> = HashMap::new();
    for entry in fetched {
        let (id, message) = entry?;
        by_id.insert(id, message);
    }
    Ok(page_rows
        .iter()
        .map(|row| {
            by_id
                .remove(&row.id)
                .expect("page row fetched by its own id")
        })
        .collect())
}
