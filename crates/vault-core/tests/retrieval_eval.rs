//! BEIR-style retrieval evaluation harness: a fixed synthetic corpus, a
//! fixed set of labeled queries, and quality floors for the three ranking
//! legs the Ask pipeline can use — FTS5 ([`llm::run_plan`]), the local
//! vector scan ([`Storage::vector_scan`] + [`HashEmbedder`]), and their
//! RRF fusion ([`vector::rrf_fuse`]).
//!
//! This is the measurement Task 2's BM25F weights are tuned against. The
//! `const` floors below gate ranking changes: a regression in any leg (or
//! in fusion) fails `cargo test -p vault-core --test retrieval_eval`
//! instead of merely printing a worse score.
//!
//! The corpus mirrors production shape: email rows go through
//! [`Storage::insert_mail_message`] with the stored body composed as
//! `"{subject}\n\n{text}"` (exactly what `mail.rs` stores) and the subject
//! passed as the separate subject argument; chat rows go through
//! [`Storage::insert_message`] with no subject. Everything is offline and
//! deterministic — `HashEmbedder` stays the embedding model.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, Once};

use vault_core::llm::{run_plan, PlanSource, PlannedQuery, QueryPlan};
use vault_core::message::{InsertOutcome, NewMessage};
use vault_core::storage::{
    ConnectorIdentity, MailInsertOutcome, NewSourceInstance, Occurrence, Storage,
};
use vault_core::vector::{rrf_fuse, run_embedding_job, Embedder, HashEmbedder};

// The date-bounded legs translate local-day bounds, so pin the process TZ
// exactly once under exclusive access (same approach as vector_search.rs).
static TZ_LOCK: Mutex<()> = Mutex::new(());
static TZ_PIN: Once = Once::new();

fn pin_utc_tz() {
    let _guard = TZ_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    TZ_PIN.call_once(|| {
        std::env::set_var("TZ", "UTC");
        let _ = chrono::Local::now(); // resolve with the pinned TZ
    });
}

/// Metric depth: every floor and every assertion below is at k = 10.
const K: usize = 10;
/// Candidate depth per leg before fusion.
const CANDIDATES: u32 = 50;
/// RRF fusion output depth (production fuses down to the caller's page
/// limit; the harness judges the same top-10 window the metrics score).
const FUSED_LIMIT: usize = 10;

// Measured baseline (2026-11-13, base 531b044 plus the mixed-channel
// queries this commit adds, HashEmbedder, 40 fixtures / 16 queries,
// macro-averaged):
//
//   fts     recall@10 = 0.859   ndcg@10 = 0.879
//   vector  recall@10 = 0.839   ndcg@10 = 0.827
//   fused   recall@10 = 0.943   ndcg@10 = 0.941
//
// History: equal-weight baseline (af286c5, 14 queries): fts 0.857/0.886.
// Shipped BM25F weights (c7ece20, subject 3.0), still 14 queries at
// 531b044: fts 0.857/0.874, vector 0.833/0.819, fused 0.952/0.945 — the
// vector and fused baselines stayed valid under the shipped weights.
// Adding the two mixed-channel queries re-derived the macros: fused moved
// DOWN to 0.943/0.941 (the mixed "release and rollback news" query
// measures 0.750/0.832 — its paraphrased chat is FTS-invisible and the
// vector leg reaches only three of four), so FUSED_RECALL_FLOOR honestly
// dropped 0.90 -> 0.89; the vector macro rose and its floors follow
// (0.75 -> 0.76, 0.74 -> 0.75). Floors hold for the combined 16-query
// corpus.
//
// Floors sit ~5-10% below the measurements: far enough to absorb
// tie-order noise, close enough to catch a real ranking regression.
const FTS_RECALL_FLOOR: f64 = 0.78;
const FTS_NDCG_FLOOR: f64 = 0.80;
const VECTOR_RECALL_FLOOR: f64 = 0.76;
const VECTOR_NDCG_FLOOR: f64 = 0.75;
const FUSED_RECALL_FLOOR: f64 = 0.89;
const FUSED_NDCG_FLOOR: f64 = 0.89;

// ---------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------

/// One email fixture: `(external_id, subject, author, sent_at, body text)`.
/// The stored body is `"{subject}\n\n{text}"` — the shape `mail.rs`
/// composes — with the subject also passed as the dedicated argument.
const EMAILS: &[(&str, &str, &str, &str, &str)] = &[
    // The date-clustered Kestrel migration thread (Oct 20-22).
    (
        "em-kestrel-01",
        "Kestrel migration plan",
        "Mara Lindqvist",
        "2026-10-20T09:00:00Z",
        "Phase one of the Kestrel migration moves the billing service to \
         the new cluster. Cutover steps: freeze deploys at noon, drain the \
         queue, flip DNS, then watch the dashboards for an hour.",
    ),
    (
        "em-kestrel-02",
        "Re: Kestrel migration plan",
        "Dev Patel",
        "2026-10-20T11:30:00Z",
        "The cutover checklist looks solid. One gap: we need a documented \
         recovery path before we flip DNS on the Kestrel cluster.",
    ),
    (
        "em-kestrel-03",
        "Re: Kestrel migration plan",
        "Sofia Reyes",
        "2026-10-21T08:15:00Z",
        "Recovery plan attached: snapshot the database, keep the old \
         billing pods warm for 24 hours after the cutover, and rehearse \
         the switch twice.",
    ),
    (
        "em-kestrel-04",
        "Re: Kestrel migration plan",
        "Mara Lindqvist",
        "2026-10-22T16:45:00Z",
        "Go for the Kestrel cutover on Friday. Dev owns the checklist, \
         Sofia owns recovery, I will watch the dashboards with support.",
    ),
    // Four messages sharing the distinctive "Aurora" subject term, each
    // about something different.
    (
        "em-aurora-01",
        "Aurora billing outage postmortem",
        "Sofia Reyes",
        "2026-10-05T10:00:00Z",
        "The Aurora billing pipeline failed for two hours on October 3rd: \
         an invoice retry loop exhausted the database pool. Full timeline \
         and action items attached.",
    ),
    (
        "em-aurora-02",
        "Aurora team offsite agenda",
        "Mara Lindqvist",
        "2026-09-18T14:20:00Z",
        "Aurora offsite on November 7th at the lakeside venue: roadmap \
         review in the morning, reliability workshop after lunch, dinner \
         downtown at eight.",
    ),
    (
        "em-aurora-03",
        "Aurora v2.4 release notes",
        "Dev Patel",
        "2026-10-28T12:00:00Z",
        "Aurora v2.4 ships the new invoice export, faster search, and a \
         fix for the timezone bug in weekly reports. Rollout begins \
         Thursday.",
    ),
    (
        "em-aurora-04",
        "Aurora on-call rotation for November",
        "Tom Okafor",
        "2026-10-30T09:10:00Z",
        "November on-call for Aurora: Priya week one, Tom week two, Dev \
         week three, Sofia week four. Escalation path unchanged.",
    ),
    // Mara Lindqvist as a repeated author.
    (
        "em-mara-budget",
        "Quarterly budget review notes",
        "Mara Lindqvist",
        "2026-11-01T17:00:00Z",
        "Hosting spend dropped 12 percent after the Kestrel cluster \
         consolidation. Licenses and headcount are flat; travel is under \
         budget.",
    ),
    (
        "em-mara-hiring",
        "Platform hiring update",
        "Mara Lindqvist",
        "2026-11-03T10:30:00Z",
        "Two senior platform engineers start in January. The hiring \
         committee shortlisted five candidates for the on-site loop next \
         week.",
    ),
    (
        "em-mara-allhands",
        "All-hands deck and recap",
        "Mara Lindqvist",
        "2026-10-15T18:00:00Z",
        "Slides from the all-hands are attached: Kestrel timeline, revenue \
         numbers, and the support load trends. Questions welcome in the \
         thread.",
    ),
    // Near-duplicate paraphrased bodies: one refund story worded three
    // ways (for the vector leg).
    (
        "em-refund-01",
        "Refund request #4182",
        "Lena Fischer",
        "2026-10-25T09:40:00Z",
        "A customer is asking for a refund of the annual subscription they \
         paid in March. Their card was charged twice by mistake, the \
         second charge landing on April 1st.",
    ),
    (
        "em-refund-02",
        "Duplicate charge reimbursement",
        "Lena Fischer",
        "2026-10-26T11:05:00Z",
        "The subscriber confirmed the double payment on the yearly \
         subscription; finance approved the reimbursement yesterday and \
         the money should land within five days.",
    ),
    (
        "em-refund-03",
        "Refund policy exception",
        "Raj Chopra",
        "2026-11-02T13:25:00Z",
        "Support wants a policy exception: refund the yearly subscription \
         even though the window closed, because the outage made the \
         product unusable for them.",
    ),
    // The invoice retry incident and its follow-up.
    (
        "em-invoice-01",
        "Invoice batch failed overnight",
        "Dev Patel",
        "2026-10-03T07:55:00Z",
        "The nightly invoice batch failed at 03:00 when a retry storm \
         saturated the database pool. The manual retry succeeded and all \
         invoices went out.",
    ),
    (
        "em-invoice-02",
        "Invoice export timing change",
        "Dev Patel",
        "2026-10-29T09:00:00Z",
        "Starting next week the invoice export runs at 04:00 instead of \
         02:00 to avoid colliding with the backup window.",
    ),
    // Cutover preparation directly before the thread (outside its window).
    (
        "em-dns-01",
        "DNS TTL change for the cutover",
        "Tom Okafor",
        "2026-10-19T15:30:00Z",
        "Lowered the DNS TTL to 60 seconds ahead of the Kestrel cutover so \
         the flip propagates quickly. Reverting to 3600 once we are \
         stable.",
    ),
    (
        "em-dns-02",
        "Re: DNS TTL change for the cutover",
        "Mara Lindqvist",
        "2026-10-19T16:10:00Z",
        "Good call on the TTL. Let's confirm the dashboard alerts fire \
         before Friday so the cutover night is boring.",
    ),
    // Clearly unrelated filler.
    (
        "em-fill-garden",
        "Community garden schedule",
        "Nadia Haddad",
        "2026-09-12T12:00:00Z",
        "The garden workday moves to Saturday morning. Bring gloves; we \
         are turning the beds and planting garlic before the rain comes \
         back.",
    ),
    (
        "em-fill-book",
        "Book club pick for December",
        "Lena Fischer",
        "2026-11-05T19:45:00Z",
        "December's book club pick is the new solarpunk anthology. \
         Discussion on the 14th at the usual cafe, snacks provided.",
    ),
    (
        "em-fill-insurance",
        "Home insurance renewal",
        "Tom Okafor",
        "2026-10-08T08:20:00Z",
        "Our home insurance renewal quote arrived: a six percent increase. \
         Reviewing coverage limits before the deadline at the end of the \
         month.",
    ),
    (
        "em-fill-benefits",
        "Benefits enrollment window",
        "Raj Chopra",
        "2026-10-10T10:00:00Z",
        "The benefits enrollment window closes October 31st. The dental \
         plan is unchanged and the vision provider switches to \
         Northview.",
    ),
    (
        "em-fill-lunch",
        "Team lunch on Friday",
        "Priya Nair",
        "2026-10-16T11:15:00Z",
        "Team lunch Friday at the noodle place near the office. Vegetarian \
         options available; RSVP by Thursday noon.",
    ),
    (
        "em-fill-storm",
        "Storm warning for the weekend",
        "Nadia Haddad",
        "2026-10-17T21:30:00Z",
        "The weather service issued a storm warning for Saturday: heavy \
         rain and gusts up to 80 km/h. Secure the balcony furniture.",
    ),
];

/// Chat ingest source for the chat fixtures below (short handles, like the
/// telegram/discord paths use).
const CHAT_SOURCE: &str = "team-chat";

/// One chat fixture: `(external_id, author, sent_at, body)`. Chat rows
/// carry no subject, exactly like the chat ingest path.
const CHATS: &[(&str, &str, &str, &str)] = &[
    // The failed deploy of October 21 — paraphrased across three messages
    // (for the vector leg).
    (
        "c-deploy-01",
        "Priya",
        "2026-10-21T13:05:00Z",
        "the release train derailed again, rolling back to the previous \
         build before anyone notices",
    ),
    (
        "c-deploy-02",
        "Tom",
        "2026-10-21T13:20:00Z",
        "yeah the rollout failed halfway, reverting to the last known good \
         build now",
    ),
    (
        "c-deploy-03",
        "Dev",
        "2026-10-21T14:00:00Z",
        "rollback finished, the previous build is live again, incident \
         channel has the full timeline",
    ),
    // Chat slang: lgtm review verdicts.
    (
        "c-lgtm-01",
        "Sofia",
        "2026-10-14T10:02:00Z",
        "lgtm on the pricing patch, ship it once ci is green",
    ),
    (
        "c-lgtm-02",
        "Dev",
        "2026-10-14T10:31:00Z",
        "lgtm too, merge whenever, i will watch the deploy",
    ),
    (
        "c-lgtm-03",
        "Mara",
        "2026-10-15T09:12:00Z",
        "not lgtm yet, the pricing patch needs a migration test first",
    ),
    // A date-clustered standup mini-thread (Oct 12-14) naming the
    // search-cache work three different ways.
    (
        "c-standup-01",
        "Nadia",
        "2026-10-12T08:00:00Z",
        "standup notes: priya on the search cache, tom on the migration \
         docs, blocker none",
    ),
    (
        "c-standup-02",
        "Nadia",
        "2026-10-13T08:02:00Z",
        "standup notes: priya still on the cache, tom finished the docs, \
         raj started the audit",
    ),
    (
        "c-standup-03",
        "Nadia",
        "2026-10-14T08:01:00Z",
        "standup notes: cache merged, audit ongoing, demo prep friday",
    ),
    // Clearly unrelated filler.
    (
        "c-fill-plants",
        "Nadia",
        "2026-10-09T16:40:00Z",
        "reminder: office plants watering is my turn this week, someone \
         take next week?",
    ),
    (
        "c-fill-cat",
        "Tom",
        "2026-10-10T22:15:00Z",
        "this cat video is the best thing on the internet today, no \
         contest",
    ),
    (
        "c-fill-coffee",
        "Lena",
        "2026-10-11T09:03:00Z",
        "the new espresso beans are in the kitchen, actually drinkable for \
         once",
    ),
    (
        "c-fill-gamenight",
        "Raj",
        "2026-10-18T20:30:00Z",
        "board game night at my place saturday, bring snacks, we start at \
         seven",
    ),
    (
        "c-fill-traffic",
        "Priya",
        "2026-10-23T08:44:00Z",
        "stuck in traffic, start the meeting without me, i will hop on in \
         ten",
    ),
    (
        "c-fill-joke",
        "Tom",
        "2026-10-24T17:12:00Z",
        "a database walks into a bar, approaches two tables and asks to \
         join them",
    ),
    (
        "c-fill-hike",
        "Sofia",
        "2026-10-25T11:55:00Z",
        "hiking the ridge trail this weekend if the storm holds off, who \
         is in?",
    ),
];

/// Total corpus size (compile-time guard: at least 40 fixtures).
const TOTAL: usize = EMAILS.len() + CHATS.len();
const _: () = assert!(TOTAL >= 40, "eval corpus must have at least 40 messages");

// ---------------------------------------------------------------------
// Labeled queries
// ---------------------------------------------------------------------

/// One labeled query: natural-language text (what the vector leg embeds),
/// the FTS5 MATCH expression a good translation of that text would carry,
/// optional inclusive local-day date bounds (both legs), and the
/// `external_id`s of every relevant message (binary relevance — the lists
/// are complete per query).
struct LabeledQuery {
    text: &'static str,
    match_expr: &'static str,
    date_from: Option<&'static str>,
    date_to: Option<&'static str>,
    relevant: &'static [&'static str],
}

const QUERIES: &[LabeledQuery] = &[
    // Subject-term family: "Aurora" appears in four different subjects.
    LabeledQuery {
        text: "Aurora",
        match_expr: "\"aurora\"",
        date_from: None,
        date_to: None,
        relevant: &[
            "em-aurora-01",
            "em-aurora-02",
            "em-aurora-03",
            "em-aurora-04",
        ],
    },
    // Subject-term family: the Kestrel migration plan thread.
    LabeledQuery {
        text: "Kestrel migration plan",
        match_expr: "\"kestrel\" \"migration\" \"plan\"",
        date_from: None,
        date_to: None,
        relevant: &[
            "em-kestrel-01",
            "em-kestrel-02",
            "em-kestrel-03",
            "em-kestrel-04",
        ],
    },
    // Subject-term family: a single offsite-planning mail.
    LabeledQuery {
        text: "offsite agenda",
        match_expr: "\"offsite\"",
        date_from: None,
        date_to: None,
        relevant: &["em-aurora-02"],
    },
    // Author-name family: every email Lena Fischer sent.
    LabeledQuery {
        text: "messages from Lena Fischer",
        match_expr: "author:\"lena fischer\"",
        date_from: None,
        date_to: None,
        relevant: &["em-refund-01", "em-refund-02", "em-fill-book"],
    },
    // Author-name family: every email Tom Okafor sent.
    LabeledQuery {
        text: "Tom Okafor emails",
        match_expr: "author:\"tom okafor\"",
        date_from: None,
        date_to: None,
        relevant: &["em-aurora-04", "em-dns-01", "em-fill-insurance"],
    },
    // Author-name + term: Dev Patel messages mentioning invoices.
    LabeledQuery {
        text: "Dev Patel invoice",
        match_expr: "author:\"dev patel\" \"invoice\"",
        date_from: None,
        date_to: None,
        relevant: &["em-aurora-03", "em-invoice-01", "em-invoice-02"],
    },
    // Term + date-range family: the Kestrel cutover window (the Oct 19 DNS
    // prep mail mentions the cutover too and must stay excluded by date).
    LabeledQuery {
        text: "Kestrel cutover between October 20 and October 22 2026",
        match_expr: "\"kestrel\" \"cutover\"",
        date_from: Some("2026-10-20"),
        date_to: Some("2026-10-22"),
        relevant: &[
            "em-kestrel-01",
            "em-kestrel-02",
            "em-kestrel-03",
            "em-kestrel-04",
        ],
    },
    // Term + date-range family (chat): the day of the failed deploy; only
    // one message uses the word "rollback" — the paraphrases need the
    // vector leg.
    LabeledQuery {
        text: "deploy rollback on October 21",
        match_expr: "\"rollback\"",
        date_from: Some("2026-10-21"),
        date_to: Some("2026-10-21"),
        relevant: &["c-deploy-01", "c-deploy-02", "c-deploy-03"],
    },
    // Paraphrase/synonym family: one refund story worded three ways.
    LabeledQuery {
        text: "customer wants a refund for the duplicate subscription charge",
        match_expr: "\"refund\"",
        date_from: None,
        date_to: None,
        relevant: &["em-refund-01", "em-refund-02", "em-refund-03"],
    },
    // Paraphrase/synonym family (chat): "went back to the old build".
    LabeledQuery {
        text: "the release failed and we went back to the old build",
        match_expr: "\"release\" OR \"rollback\"",
        date_from: None,
        date_to: None,
        relevant: &["c-deploy-01", "c-deploy-02", "c-deploy-03"],
    },
    // Paraphrase family (chat): the search-cache work named three ways
    // across the standups.
    LabeledQuery {
        text: "search cache demo prep",
        match_expr: "\"search\" \"cache\"",
        date_from: None,
        date_to: None,
        relevant: &["c-standup-01", "c-standup-02", "c-standup-03"],
    },
    // Chat-slang family: lgtm review verdicts.
    LabeledQuery {
        text: "lgtm ship it",
        match_expr: "\"lgtm\"",
        date_from: None,
        date_to: None,
        relevant: &["c-lgtm-01", "c-lgtm-02", "c-lgtm-03"],
    },
    // Multi-term AND family: the invoice retry incident (two emails).
    LabeledQuery {
        text: "invoice retry database pool",
        match_expr: "\"invoice\" \"retry\" \"pool\"",
        date_from: None,
        date_to: None,
        relevant: &["em-aurora-01", "em-invoice-01"],
    },
    // Term + date-range family: three consecutive standup notes.
    LabeledQuery {
        text: "standup notes from October 12 to October 14",
        match_expr: "\"standup\"",
        date_from: Some("2026-10-12"),
        date_to: Some("2026-10-14"),
        relevant: &["c-standup-01", "c-standup-02", "c-standup-03"],
    },
    // Mixed-channel family: "migration" lives in the four Kestrel email
    // subjects AND in two chat bodies (the migration test, the migration
    // docs). Every earlier query's relevant set is single-channel, so the
    // floors never saw a term compete across channels — exactly the axis
    // where the subject weight (3.0 over the body's 1.0, with mail bodies
    // also carrying the subject as their prefix) systematically favors the
    // email leg. The relevant set is complete for the term: all six
    // "migration" mentions in the corpus.
    LabeledQuery {
        text: "migration updates",
        match_expr: "\"migration\"",
        date_from: None,
        date_to: None,
        relevant: &[
            "em-kestrel-01",
            "em-kestrel-02",
            "em-kestrel-03",
            "em-kestrel-04",
            "c-lgtm-03",
            "c-standup-01",
        ],
    },
    // Mixed-channel family: "release" appears in an email subject (the
    // Aurora release notes) and in the chat deploy saga; "rollback" is the
    // chat-side verdict. The relevant set mixes the one release email with
    // all three deploy chats — c-deploy-02 says "rollout … reverting" and
    // matches neither term, so the FTS translation alone cannot reach it
    // (the vector leg's job, same paraphrase gap as the all-chat release
    // query above).
    LabeledQuery {
        text: "release and rollback news",
        match_expr: "\"release\" OR \"rollback\"",
        date_from: None,
        date_to: None,
        relevant: &["em-aurora-03", "c-deploy-01", "c-deploy-02", "c-deploy-03"],
    },
];

// ---------------------------------------------------------------------
// Metrics (no external crates)
// ---------------------------------------------------------------------

/// Fraction of the relevant set present in the first `k` ranked ids.
fn recall_at(relevant: &HashSet<i64>, ranked: &[i64], k: usize) -> f64 {
    if relevant.is_empty() {
        return 1.0; // nothing to find; vacuously complete
    }
    let hits = ranked
        .iter()
        .take(k)
        .filter(|id| relevant.contains(*id))
        .count();
    hits as f64 / relevant.len() as f64
}

/// nDCG with binary gains and a log2 position discount, normalized by the
/// ideal DCG (all relevant documents ranked first).
fn ndcg_at(relevant: &HashSet<i64>, ranked: &[i64], k: usize) -> f64 {
    let dcg: f64 = ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, id)| {
            if relevant.contains(id) {
                1.0 / (i as f64 + 2.0).log2()
            } else {
                0.0
            }
        })
        .sum();
    let ideal: f64 = (1..=relevant.len().min(k))
        .map(|i| 1.0 / ((i + 1) as f64).log2())
        .sum();
    if ideal > 0.0 {
        dcg / ideal
    } else {
        1.0
    }
}

// ---------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------

/// Seed the whole corpus in an in-memory `Storage` and return the storage
/// plus the fixture `external_id` -> row-id map the labels correlate
/// through.
fn seeded_corpus() -> (Storage, HashMap<&'static str, i64>) {
    pin_utc_tz();
    let storage = Storage::open_in_memory().unwrap();

    // A durable source instance the mail connector path can scope to.
    let instance = storage
        .ensure_source_instance(&NewSourceInstance {
            kind: "imap",
            label: "Eval Mail",
            connector_key: "imap|mail.eval.test|993|implicit-tls|eval@mail.test",
            config_json: "{}",
        })
        .unwrap();

    let mut ids = HashMap::new();
    for (n, &(id, subject, author, sent_at, text)) in EMAILS.iter().enumerate() {
        // Production mail shape: body = "{subject}\n\n{text}" with the
        // subject stored separately as well (mail.rs composition).
        let msg = NewMessage::new(
            instance.clone(),
            id,
            author,
            sent_at,
            format!("{subject}\n\n{text}"),
        );
        let identity = ConnectorIdentity {
            namespace: "imap",
            external_key: id,
            revision_hash: "eval-1",
        };
        let item = format!("m{n}");
        let occurrence = Occurrence {
            scope: "INBOX",
            provider_item_id: &item,
            uidvalidity: Some(1),
        };
        let row = match storage
            .insert_mail_message(&instance, &msg, subject, &identity, &occurrence)
            .unwrap()
        {
            MailInsertOutcome::Inserted { id, .. } | MailInsertOutcome::Duplicate { id } => id,
        };
        ids.insert(id, row);
    }

    for &(id, author, sent_at, body) in CHATS {
        let mut msg = NewMessage::new(CHAT_SOURCE, id, author, sent_at, body);
        msg.channel = "chat".into();
        msg.chat_id = "team".into();
        let row = match storage.insert_message(&msg).unwrap() {
            InsertOutcome::Inserted(row) | InsertOutcome::Duplicate(row) => row,
        };
        ids.insert(id, row);
    }

    (storage, ids)
}

// ---------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------

/// Macro-averaged metrics of one leg.
struct LegMetrics {
    recall: f64,
    ndcg: f64,
}

/// The full harness run: both legs plus fusion over every labeled query.
struct Evaluation {
    fts: LegMetrics,
    vector: LegMetrics,
    fused: LegMetrics,
    /// Aggregate nDCG@10 of the deliberately reversed fused rankings (the
    /// sanity metric: it must score strictly worse than `fused.ndcg`).
    fused_reversed_ndcg: f64,
}

fn evaluate() -> Evaluation {
    let (storage, ids) = seeded_corpus();
    let embedder = HashEmbedder::new();

    // Both legs measured only over a fully embedded corpus.
    run_embedding_job(&storage, &embedder, None).unwrap();
    let counts = storage.embedding_counts(embedder.id()).unwrap();
    assert_eq!(
        (counts.embedded, counts.messages),
        (TOTAL as u64, TOTAL as u64),
        "every fixture must be embedded before measuring"
    );

    let (mut fts_r, mut fts_n, mut vec_r, mut vec_n, mut fus_r, mut fus_n, mut rev_n) =
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for q in QUERIES {
        let relevant: HashSet<i64> = q
            .relevant
            .iter()
            .map(|id| {
                *ids.get(*id)
                    .unwrap_or_else(|| panic!("label {id:?} resolves to no fixture"))
            })
            .collect();
        assert!(
            storage.is_valid_match(q.match_expr),
            "query {:?} has an invalid MATCH expression {:?}",
            q.text,
            q.match_expr
        );

        // FTS leg: the plan a good translation of `q.text` would produce.
        let planned = PlannedQuery {
            plan: QueryPlan {
                match_expr: q.match_expr.to_string(),
                date_from: q.date_from.map(str::to_string),
                date_to: q.date_to.map(str::to_string),
                from: None,
                has_link: None,
                extract_links: false,
            },
            source: PlanSource::NoLlm,
            notes: Vec::new(),
        };
        let fts_ids: Vec<i64> = run_plan(&storage, &planned, CANDIDATES)
            .unwrap()
            .iter()
            .map(|m| m.id)
            .collect();

        // Vector leg: embed the raw question, scan under the same bounds.
        let vec_ids: Vec<i64> = storage
            .vector_scan(
                embedder.id(),
                embedder.dim(),
                &embedder.embed(q.text),
                q.date_from,
                q.date_to,
                None,
                false,
                CANDIDATES,
            )
            .unwrap()
            .iter()
            .map(|h| h.message_id)
            .collect();

        // Fusion, exactly as the hybrid pipeline composes it.
        let fused = rrf_fuse(&fts_ids, &vec_ids, FUSED_LIMIT);
        let reversed: Vec<i64> = fused.iter().rev().copied().collect();

        fts_r += recall_at(&relevant, &fts_ids, K);
        fts_n += ndcg_at(&relevant, &fts_ids, K);
        vec_r += recall_at(&relevant, &vec_ids, K);
        vec_n += ndcg_at(&relevant, &vec_ids, K);
        fus_r += recall_at(&relevant, &fused, K);
        fus_n += ndcg_at(&relevant, &fused, K);
        rev_n += ndcg_at(&relevant, &reversed, K);
    }

    let n = QUERIES.len() as f64;
    Evaluation {
        fts: LegMetrics {
            recall: fts_r / n,
            ndcg: fts_n / n,
        },
        vector: LegMetrics {
            recall: vec_r / n,
            ndcg: vec_n / n,
        },
        fused: LegMetrics {
            recall: fus_r / n,
            ndcg: fus_n / n,
        },
        fused_reversed_ndcg: rev_n / n,
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

/// The corpus and the labels are internally consistent: every fixture
/// landed as its own row, every label resolves, every query is labeled.
#[test]
fn corpus_and_labels_are_wellformed() {
    let (storage, ids) = seeded_corpus();
    assert_eq!(ids.len(), TOTAL, "fixture external ids are unique");
    assert_eq!(
        storage.stats().unwrap().total,
        TOTAL as u64,
        "no fixture silently deduped away"
    );
    for q in QUERIES {
        assert!(!q.relevant.is_empty(), "query {:?} has no labels", q.text);
        for id in q.relevant {
            assert!(ids.contains_key(*id), "label {id:?} resolves to no fixture");
        }
    }
}

/// Sanity of the metrics themselves: a deliberately reversed ranking must
/// score strictly worse than the real fused ranking on aggregate nDCG@10,
/// proving the harness detects bad orderings (and thus real regressions).
#[test]
fn reversed_ranking_scores_strictly_worse() {
    let ev = evaluate();
    assert!(
        ev.fused_reversed_ndcg < ev.fused.ndcg,
        "reversed fused nDCG@10 = {:.3} must be strictly below the real {:.3}",
        ev.fused_reversed_ndcg,
        ev.fused.ndcg
    );
}

/// The FTS leg alone meets its baseline floors.
#[test]
fn fts_leg_meets_baseline_floors() {
    let ev = evaluate();
    assert!(
        ev.fts.recall >= FTS_RECALL_FLOOR,
        "fts recall@10 = {:.3} below floor {:.3}",
        ev.fts.recall,
        FTS_RECALL_FLOOR
    );
    assert!(
        ev.fts.ndcg >= FTS_NDCG_FLOOR,
        "fts nDCG@10 = {:.3} below floor {:.3}",
        ev.fts.ndcg,
        FTS_NDCG_FLOOR
    );
}

/// The vector leg alone meets its baseline floors.
#[test]
fn vector_leg_meets_baseline_floors() {
    let ev = evaluate();
    assert!(
        ev.vector.recall >= VECTOR_RECALL_FLOOR,
        "vector recall@10 = {:.3} below floor {:.3}",
        ev.vector.recall,
        VECTOR_RECALL_FLOOR
    );
    assert!(
        ev.vector.ndcg >= VECTOR_NDCG_FLOOR,
        "vector nDCG@10 = {:.3} below floor {:.3}",
        ev.vector.ndcg,
        VECTOR_NDCG_FLOOR
    );
}

/// The fused leg meets its baseline floors.
#[test]
fn fused_leg_meets_baseline_floors() {
    let ev = evaluate();
    // The exact macro-averaged measurements (visible with --nocapture;
    // failure messages always carry them): the AFTER side of the
    // BEFORE/AFTER comparison against the recorded baseline above.
    println!(
        "recall@10 / nDCG@10 — fts {:.3}/{:.3}, vector {:.3}/{:.3}, \
         fused {:.3}/{:.3} (fused-reversed nDCG@10 {:.3})",
        ev.fts.recall,
        ev.fts.ndcg,
        ev.vector.recall,
        ev.vector.ndcg,
        ev.fused.recall,
        ev.fused.ndcg,
        ev.fused_reversed_ndcg
    );
    assert!(
        ev.fused.recall >= FUSED_RECALL_FLOOR,
        "fused recall@10 = {:.3} below floor {:.3}",
        ev.fused.recall,
        FUSED_RECALL_FLOOR
    );
    assert!(
        ev.fused.ndcg >= FUSED_NDCG_FLOOR,
        "fused nDCG@10 = {:.3} below floor {:.3}",
        ev.fused.ndcg,
        FUSED_NDCG_FLOOR
    );
}

/// Fusion helps or at least never hurts: macro recall@10 of the fused leg
/// must not fall below the FTS-only leg on this corpus.
#[test]
fn fusion_never_hurts_recall() {
    let ev = evaluate();
    assert!(
        ev.fused.recall >= ev.fts.recall,
        "fused recall@10 = {:.3} below FTS-only {:.3}",
        ev.fused.recall,
        ev.fts.recall
    );
}
