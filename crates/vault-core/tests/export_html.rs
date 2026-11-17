//! HTML export bundle integration tests (A8): the offline content
//! rules (escaped text, sandboxed retained HTML, relative assets, no
//! scripts, no remote URLs), the plan step's scope/count-before-write
//! contract, exact query scope for all three scopes (pagination
//! ignored), staged publication with a clearly-incomplete cancellation
//! artifact, and the `job_submit` body with cooperative cancellation —
//! the crate-level verification pattern `mbox.rs` uses.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use vault_core::export_html::{
    export_html, html_export_job, plan_html_export, HtmlExportOutcome, HtmlExportScope,
    FIDELITY_LABEL,
};
use vault_core::message::NewMessage;
use vault_core::object_store::{Capacity, ObjectStore};
use vault_core::search2::SearchArchiveRequest;
use vault_core::storage::Storage;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// An email-shaped row.
fn email(id: &str, body: &str, sent_at: &str) -> NewMessage {
    NewMessage::new("mbox", id, "Ana Sender <ana@example.com>", sent_at, body)
}

/// A chat-shaped row.
fn chat(id: &str, author: &str, sent_at: &str, body: &str, chat_id: &str) -> NewMessage {
    let mut m = NewMessage::new("whatsapp", id, author, sent_at, body);
    m.channel = "chat".into();
    m.chat_id = chat_id.into();
    m
}

/// Seed `rows`, returning the storage and each row's stored id in
/// insertion order (the id the selection scope takes).
fn seeded_with_ids(rows: Vec<NewMessage>) -> (Storage, Vec<i64>) {
    let s = Storage::open_in_memory().unwrap();
    let mut ids = Vec::new();
    for m in &rows {
        let id = match s.insert_message(m).unwrap() {
            vault_core::message::InsertOutcome::Inserted(id) => id,
            vault_core::message::InsertOutcome::Duplicate(id) => id,
        };
        ids.push(id);
    }
    (s, ids)
}

/// Store one media object into `data_dir`'s object store and return
/// the `raw_json` media entry the archive would link (the shape
/// [`vault_core::media`] writes).
fn stored_media_entry(data_dir: &Path, name: &str, bytes: &[u8]) -> serde_json::Value {
    let src = data_dir.join("incoming");
    fs::write(&src, bytes).unwrap();
    let (entry, _written) =
        vault_core::media::store(data_dir, &src, name, "image", &mut Capacity::default()).unwrap();
    serde_json::json!({
        "sha256": entry.sha256,
        "path": entry.path,
        "name": entry.name,
        "kind": entry.kind,
        "size": entry.size,
    })
}

/// A missing media entry, the shape the import pass records for a
/// reference whose bytes never arrived.
fn missing_media_entry(name: &str, reason: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "kind": "image",
        "missing": true,
        "reason": reason,
    })
}

/// A media entry whose digest names nothing in the store (a dangling
/// reference — corrupted or externally removed objects).
fn dangling_media_entry(name: &str) -> serde_json::Value {
    serde_json::json!({
        "sha256": "1e4e2c66f7d0a0c36a5b1b3e2f8a0c9d".repeat(2),
        "path": "media/dangling.jpg",
        "name": name,
        "kind": "image",
        "size": 10,
    })
}

/// Set (or replaces) the `media` array and `html` of a row's extras.
fn with_extras(mut m: NewMessage, media: serde_json::Value, html: Option<&str>) -> NewMessage {
    let mut obj = serde_json::Map::new();
    obj.insert("media".into(), media);
    if let Some(h) = html {
        obj.insert("html".into(), serde_json::Value::String(h.to_string()));
    }
    m.raw_json = serde_json::Value::Object(obj).to_string();
    m
}

/// Every file under `dir`, recursively, as (relative path, bytes).
fn bundle_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((
                    path.strip_prefix(dir).unwrap().to_path_buf(),
                    fs::read(&path).unwrap(),
                ));
            }
        }
    }
    out
}

/// Every dot-prefixed `.staging` directory beside a would-be bundle.
fn staging_artifacts(parent: &Path) -> Vec<PathBuf> {
    fs::read_dir(parent)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .map(|n| {
                        let n = n.to_string_lossy();
                        n.starts_with('.') && n.ends_with(".staging")
                    })
                    .unwrap_or(false)
        })
        .collect()
}

/// Run an export into `dest` and unwrap the completed summary.
fn export_ok(
    s: &Storage,
    scope: &HtmlExportScope,
    dest: &Path,
    data_dir: &Path,
) -> vault_core::export_html::HtmlExportSummary {
    let objects = ObjectStore::new(data_dir);
    match export_html(s, scope, dest, &objects, &|| false).unwrap() {
        Some(HtmlExportOutcome::Complete(summary)) => summary,
        other => panic!("expected a completed export, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The plan step: scope and count before anything is written
// ---------------------------------------------------------------------------

#[test]
fn plan_states_scope_and_count_without_writing() {
    let (s, ids) = seeded_with_ids(vec![
        email("a@e.com", "first", "2026-11-01T09:00:00Z"),
        email("b@e.com", "second", "2026-11-02T09:00:00Z"),
        chat("w-1", "marko", "2026-11-03T09:00:00Z", "third", "ekipa"),
        chat("w-2", "ana", "2026-11-04T09:00:00Z", "fourth", "ekipa"),
    ]);

    // Search scope: the count comes from the same normalized request
    // (a text query's full match set, not one page).
    let plan = plan_html_export(
        &s,
        &HtmlExportScope::Search {
            request: SearchArchiveRequest {
                text: "".into(),
                limit: Some(1), // pagination must not bound the count
                ..Default::default()
            },
        },
    )
    .unwrap();
    assert_eq!(plan.scope_label, "current results");
    assert_eq!(plan.count, 4);

    // Selection scope: only resolvable ids count; the unknown one is
    // the manifest's business, not the count's.
    let plan = plan_html_export(
        &s,
        &HtmlExportScope::Selected {
            ids: vec![ids[0], 999, ids[2]],
        },
    )
    .unwrap();
    assert_eq!(plan.scope_label, "selected messages");
    assert_eq!(plan.count, 2);

    // Conversation scope: the whole thread, not just the anchor.
    let plan = plan_html_export(&s, &HtmlExportScope::Conversation { anchor_id: ids[2] }).unwrap();
    assert_eq!(plan.scope_label, "conversation");
    assert_eq!(plan.count, 2);

    // An unknown anchor plans as an empty conversation, and the export
    // declines (Ok(None)) without creating anything.
    let plan = plan_html_export(&s, &HtmlExportScope::Conversation { anchor_id: 424242 }).unwrap();
    assert_eq!(plan.count, 0);
}

// ---------------------------------------------------------------------------
// Offline content rules
// ---------------------------------------------------------------------------

/// The full offline contract: escaping, sandboxed retained HTML,
/// relative-only references, copied assets, and the manifest's
/// missing-file report.
#[test]
fn bundle_is_static_escaped_and_offline() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();

    let stored = stored_media_entry(&data_dir, "photo.jpg", b"jpeg bytes here");
    let sha = stored["sha256"].as_str().unwrap().to_string();
    let rows = vec![
        with_extras(
            email(
                "html@e.com",
                "<script>alert('x')</script> & <b>markup</b> \"quoted\"",
                "2026-11-01T09:00:00Z",
            ),
            serde_json::json!([stored, missing_media_entry("gone.jpg", "not-in-export")]),
            Some("<p>Original <em>HTML</em></p><script>evil()</script>"),
        ),
        with_extras(
            chat(
                "w-1",
                "marko",
                "2026-11-02T09:00:00Z",
                "plain chat body",
                "ekipa",
            ),
            serde_json::json!([dangling_media_entry("vanished.mp4")]),
            None,
        ),
    ];
    let (s, ids) = seeded_with_ids(rows);
    let dest = dir.path().join("bundle");

    let summary = export_ok(
        &s,
        &HtmlExportScope::Search {
            request: SearchArchiveRequest::default(),
        },
        &dest,
        &data_dir,
    );

    // The summary repeats the plan's scope and count, and carries the
    // fidelity wording.
    assert_eq!(summary.scope_label, "current results");
    assert_eq!(summary.count, 2);
    assert_eq!(summary.written, 2);
    assert_eq!(summary.pages, 1);
    assert_eq!(summary.assets_copied, 1);
    assert_eq!(summary.assets_missing, 2);
    assert_eq!(summary.fidelity, FIDELITY_LABEL);
    assert!(summary
        .fidelity
        .contains("original MIME bytes are not retained"));

    // One page plus index, the copied asset, and the manifest.
    let files = bundle_files(&dest);
    let names: Vec<String> = files
        .iter()
        .map(|(p, _)| p.to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"index.html".to_string()));
    assert!(names.contains(&"page-0001.html".to_string()));
    assert!(names.contains(&"manifest".to_string()));
    assert!(names.contains(&format!("assets/{sha}.jpg")));
    assert!(
        staging_artifacts(dir.path()).is_empty(),
        "no staging litter"
    );

    // Offline rules over every file: no scripts, no remote URLs, and
    // (for the html pages) no absolute or scheme-bearing reference.
    for (path, bytes) in &files {
        let text = String::from_utf8_lossy(bytes).to_lowercase();
        assert!(
            !text.contains("<script"),
            "{} must contain no script element",
            path.display()
        );
        assert!(
            !text.contains("http://") && !text.contains("https://"),
            "{} must reference nothing remote",
            path.display()
        );
        if path.extension().map(|e| e == "html").unwrap_or(false) {
            for href in hrefs(&text) {
                assert!(
                    !href.starts_with('/') && !href.contains(':'),
                    "every reference must be relative: {href}"
                );
            }
        }
    }

    let page = fs::read_to_string(dest.join("page-0001.html")).unwrap();
    // Stored text is escaped: crafted markup renders as text.
    assert!(page.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;"));
    assert!(page.contains("&lt;b&gt;markup&lt;/b&gt;"));
    assert!(page.contains("&amp;"));
    // The retained HTML rides only inside the escaped srcdoc of a
    // sandboxed iframe, behind the reading-pane CSP/base preamble
    // (single quotes escape as &#39; inside the attribute value).
    assert!(page.contains("<iframe sandbox=\"\""));
    assert!(page.contains("srcdoc=\"&lt;!doctype html&gt;"));
    assert!(page
        .contains("default-src &#39;none&#39;; style-src &#39;unsafe-inline&#39;; img-src data:"));
    assert!(page.contains("&lt;base target=&quot;_blank&quot;&gt;"));
    assert!(page.contains("Original &lt;em&gt;HTML&lt;/em&gt;"));
    // The asset link is relative and content-addressed.
    assert!(page.contains(&format!("href=\"assets/{sha}.jpg\"")));
    // Missing references stay visible, linked to nothing.
    assert!(page.contains("gone.jpg (image) — not preserved at import"));
    assert!(page.contains("vanished.mp4 (image) — object missing from the archive"));

    // The copied asset is byte-identical to the stored object.
    assert_eq!(
        fs::read(dest.join(format!("assets/{sha}.jpg"))).unwrap(),
        b"jpeg bytes here"
    );

    // The manifest reports scope, provenance, fidelity, and every
    // skipped or missing file with a reason.
    let manifest = fs::read_to_string(dest.join("manifest")).unwrap();
    assert!(manifest.contains("scope: current results"));
    assert!(manifest.contains("messages: 2"));
    assert!(manifest.contains(&format!("fidelity: {FIDELITY_LABEL}")));
    assert!(manifest.contains("external_id=html@e.com"));
    assert!(manifest.contains(&format!(
        "missing: message {} (gone.jpg): not preserved at import (not-in-export)",
        ids[0]
    )));
    assert!(manifest.contains(&format!(
        "missing: message {} (vanished.mp4): referenced object missing from the media store",
        ids[1]
    )));
    assert!(manifest.contains("1 assets copied; 2 files missing or skipped"));

    let index = fs::read_to_string(dest.join("index.html")).unwrap();
    assert!(index.contains("scope: current results"));
    assert!(index.contains("messages: 2"));
    assert!(index.contains(&format!("fidelity: {FIDELITY_LABEL}")));
    assert!(index.contains("href=\"page-0001.html\""));
}

/// Every `href="…"` value of an (already lowercased) html document.
fn hrefs(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("href=\"") {
        rest = &rest[start + 6..];
        if let Some(end) = rest.find('"') {
            out.push(&rest[..end]);
            rest = &rest[end..];
        } else {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Exact query scope
// ---------------------------------------------------------------------------

/// The search scope exports every matching page: pagination fields are
/// ignored, the plan count equals the stream, and pages chunk at
/// PAGE_MESSAGES.
#[test]
fn search_scope_exports_all_matching_pages() {
    let rows: Vec<NewMessage> = (0..120)
        .map(|i| {
            email(
                &format!("m{i}@e.com"),
                &format!("zebra body number {i}"),
                &format!("2026-11-{:02}T09:00:00Z", i % 28 + 1),
            )
        })
        .collect();
    let (s, _) = seeded_with_ids(rows);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("all-results");

    // A visible page of 50 at offset 10: the export must ignore it.
    let scope = HtmlExportScope::Search {
        request: SearchArchiveRequest {
            text: "zebra".into(),
            limit: Some(50),
            offset: Some(10),
            ..Default::default()
        },
    };
    let plan = plan_html_export(&s, &scope).unwrap();
    assert_eq!(plan.count, 120);

    let summary = export_ok(&s, &scope, &dest, dir.path());
    assert_eq!(summary.count, 120);
    assert_eq!(summary.written, 120);
    assert_eq!(summary.pages, 3); // 50 + 50 + 20
    for page in 1..=3 {
        assert!(dest.join(format!("page-{page:04}.html")).is_file());
    }
    let index = fs::read_to_string(dest.join("index.html")).unwrap();
    for page in 1..=3 {
        assert!(index.contains(&format!("href=\"page-{page:04}.html\"")));
    }
    // Every body made it into some page.
    let all_pages = (1..=3)
        .map(|p| fs::read_to_string(dest.join(format!("page-{p:04}.html"))).unwrap())
        .collect::<String>();
    for i in 0..120 {
        assert!(all_pages.contains(&format!("zebra body number {i}")));
    }
}

/// The conversation scope exports the anchor's whole canonical thread,
/// oldest → newest, and nothing else.
#[test]
fn conversation_scope_exports_the_whole_thread_oldest_first() {
    let mut rows = Vec::new();
    for i in 0..30 {
        rows.push(chat(
            &format!("w-{i}"),
            &format!("member-{i}"),
            &format!("2026-11-01T{:02}:00:00Z", i),
            &format!("thread message {i}"),
            "ekipa",
        ));
    }
    rows.push(chat(
        "x-1",
        "outsider",
        "2026-11-01T15:00:00Z",
        "other thread entirely",
        "drugi",
    ));
    let (s, ids) = seeded_with_ids(rows);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("thread");

    let summary = export_ok(
        &s,
        &HtmlExportScope::Conversation { anchor_id: ids[15] },
        &dest,
        dir.path(),
    );
    assert_eq!(summary.scope_label, "conversation");
    assert_eq!(summary.count, 30);
    assert_eq!(summary.written, 30);
    assert_eq!(summary.assets_copied, 0);

    let page = fs::read_to_string(dest.join("page-0001.html")).unwrap();
    assert!(page.contains("thread message 0"));
    assert!(page.contains("thread message 29"));
    // Oldest first: message 0 renders before message 1.
    let first = page.find("thread message 0").unwrap();
    let second = page.find("thread message 1").unwrap();
    assert!(first < second);
    assert!(!page.contains("other thread entirely"));
}

/// The selection scope exports exactly what was picked, in that order,
/// and lists vanished ids in the manifest instead of fabricating them.
#[test]
fn selected_scope_is_exact_and_reports_missing_ids() {
    let (s, ids) = seeded_with_ids(vec![
        email("a@e.com", "alpha body", "2026-11-01T09:00:00Z"),
        email("b@e.com", "beta body", "2026-11-02T09:00:00Z"),
        chat(
            "w-1",
            "marko",
            "2026-11-03T09:00:00Z",
            "gamma body",
            "ekipa",
        ),
        email("d@e.com", "delta body", "2026-11-04T09:00:00Z"),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("picks");

    let summary = export_ok(
        &s,
        &HtmlExportScope::Selected {
            ids: vec![ids[2], 999, ids[0]],
        },
        &dest,
        dir.path(),
    );
    assert_eq!(summary.scope_label, "selected messages");
    assert_eq!(summary.count, 2);
    assert_eq!(summary.written, 2);

    let page = fs::read_to_string(dest.join("page-0001.html")).unwrap();
    assert!(page.contains("gamma body"));
    assert!(page.contains("alpha body"));
    assert!(!page.contains("beta body"));
    assert!(!page.contains("delta body"));
    let manifest = fs::read_to_string(dest.join("manifest")).unwrap();
    assert!(manifest.contains("skipped: selected id 999: no such message"));
}

/// An unknown conversation anchor declines with `Ok(None)` and writes
/// nothing — no destination, no staging.
#[test]
fn unknown_conversation_anchor_writes_nothing() {
    let (s, _) = seeded_with_ids(vec![email("a@e.com", "body", "2026-11-01T09:00:00Z")]);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("never");
    let objects = ObjectStore::new(dir.path());
    let outcome = export_html(
        &s,
        &HtmlExportScope::Conversation { anchor_id: 424242 },
        &dest,
        &objects,
        &|| false,
    )
    .unwrap();
    assert!(outcome.is_none());
    assert!(!dest.exists());
    assert!(staging_artifacts(dir.path()).is_empty());
}

// ---------------------------------------------------------------------------
// Cancellation and staged publication
// ---------------------------------------------------------------------------

/// Cancelling mid-export leaves no finished bundle: no destination,
/// and at most a dot-prefixed staging directory whose missing
/// `manifest` says incomplete by construction.
#[test]
fn cancelled_export_leaves_no_finished_bundle() {
    let (s, _) = seeded_with_ids(vec![
        email("a@e.com", "first body", "2026-11-01T09:00:00Z"),
        email("b@e.com", "second body", "2026-11-02T09:00:00Z"),
        email("c@e.com", "third body", "2026-11-03T09:00:00Z"),
        email("d@e.com", "fourth body", "2026-11-04T09:00:00Z"),
        email("e@e.com", "fifth body", "2026-11-05T09:00:00Z"),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("cancelled");
    let objects = ObjectStore::new(dir.path());

    // Cancellation flips after a few checks: the entry check plus two
    // messages pass, the third message observes it.
    let checks = Cell::new(0u32);
    let is_cancelled = || {
        checks.set(checks.get() + 1);
        checks.get() > 3
    };
    let outcome = export_html(
        &s,
        &HtmlExportScope::Search {
            request: SearchArchiveRequest::default(),
        },
        &dest,
        &objects,
        &is_cancelled,
    )
    .unwrap()
    .expect("an in-scope export always answers");
    assert_eq!(outcome, HtmlExportOutcome::Cancelled);

    assert!(!dest.exists(), "no finished-looking bundle");
    let artifacts = staging_artifacts(dir.path());
    for staging in &artifacts {
        assert!(
            !staging.join("manifest").exists(),
            "a cancelled staging directory carries no completeness marker"
        );
        assert!(
            !staging.join("index.html").exists(),
            "the index is a finished-bundle page"
        );
    }
}

/// An error mid-export (not cancellation) leaves nothing at all beside
/// the destination — the staged_write discipline for directories.
#[test]
fn failed_export_cleans_up_after_itself() {
    let (s, _) = seeded_with_ids(vec![email("a@e.com", "body", "2026-11-01T09:00:00Z")]);
    let dir = tempfile::tempdir().unwrap();
    // The destination's parent is a FILE: staging creation fails.
    let parent = dir.path().join("not-a-dir");
    fs::write(&parent, b"file").unwrap();
    let dest = parent.join("bundle");
    let objects = ObjectStore::new(dir.path());
    assert!(export_html(
        &s,
        &HtmlExportScope::Search {
            request: SearchArchiveRequest::default()
        },
        &dest,
        &objects,
        &|| false,
    )
    .is_err());
    assert!(!dest.exists());
}

// ---------------------------------------------------------------------------
// The job path (the way mbox.rs pins its job body)
// ---------------------------------------------------------------------------

/// Poll until the job reaches a terminal state (real runner thread
/// over a real SQLite file), the `wait_terminal` of the mbox job
/// tests.
fn wait_terminal(
    pool: &Arc<vault_core::jobs::DbPool>,
    id: vault_core::jobs::JobId,
) -> vault_core::jobs::JobRecord {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(record) = pool.job(id) {
            if record.state.is_terminal() {
                return record;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "job never became terminal: {:?}",
            pool.job(id)
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The happy job path: the export runs on the pool's read connection
/// and its result carries scope, count, and fidelity.
#[test]
fn html_export_job_completes_under_submit_job() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = vault_core::jobs::DbPool::spawn(
        tmp.path().join("vault.db"),
        vault_core::jobs::PoolConfig::default(),
        |_| {},
    )
    .unwrap();
    let mut ids = Vec::new();
    for i in 0..3 {
        let m = chat(
            &format!("w-{i}"),
            "marko",
            &format!("2026-11-0{}T09:00:00Z", i + 1),
            &format!("job body {i}"),
            "ekipa",
        );
        let id = match pool.submit_write(move |s| s.insert_message(&m)).unwrap() {
            vault_core::message::InsertOutcome::Inserted(id) => id,
            vault_core::message::InsertOutcome::Duplicate(id) => id,
        };
        ids.push(id);
    }

    let dest = tmp.path().join("job-bundle");
    let data_dir = tmp.path().join("data");
    let scope = HtmlExportScope::Conversation { anchor_id: ids[1] };
    let id = pool
        .submit_job("export_html", move |ctx| {
            html_export_job(ctx, &scope, &dest, &data_dir)
        })
        .unwrap();
    let record = wait_terminal(&pool, id);
    assert_eq!(record.kind, "export_html");
    let vault_core::jobs::JobState::Succeeded { result } = record.state else {
        panic!("the job-path export must succeed: {record:?}");
    };
    assert_eq!(result["scope_label"], serde_json::json!("conversation"));
    assert_eq!(result["count"], serde_json::json!(3));
    assert_eq!(result["written"], serde_json::json!(3));
    assert_eq!(result["fidelity"], serde_json::json!(FIDELITY_LABEL));
    assert!(
        tmp.path().join("job-bundle").join("manifest").is_file(),
        "a finished bundle exists"
    );
    pool.shutdown();
}

/// Cancelling the job before the runner reaches it (deterministically:
/// a blocker job holds the runner) produces a Cancelled job and no
/// bundle at all — the cooperative-cancellation contract of
/// `job_submit`, verified at crate level.
#[test]
fn html_export_job_honours_cancellation() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = vault_core::jobs::DbPool::spawn(
        tmp.path().join("vault.db"),
        vault_core::jobs::PoolConfig::default(),
        |_| {},
    )
    .unwrap();
    pool.submit_write(|s| {
        s.insert_message(&email("a@e.com", "queued body", "2026-11-01T09:00:00Z"))
    })
    .unwrap();

    // Hold the runner on a blocker job so the export sits in the
    // queue while it is cancelled — no race with a fast export.
    let release = Arc::new(AtomicBool::new(false));
    let release_for_blocker = Arc::clone(&release);
    pool.submit_job("blocker", move |_ctx| {
        while !release_for_blocker.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Ok(serde_json::Value::Null)
    })
    .unwrap();

    let dest = tmp.path().join("cancelled-job-bundle");
    let data_dir = tmp.path().join("data");
    let scope = HtmlExportScope::Search {
        request: SearchArchiveRequest::default(),
    };
    let id = pool
        .submit_job("export_html", move |ctx| {
            html_export_job(ctx, &scope, &dest, &data_dir)
        })
        .unwrap();
    assert!(pool.cancel_job(id), "the queued job accepts cancellation");
    release.store(true, Ordering::Relaxed);

    let record = wait_terminal(&pool, id);
    assert_eq!(
        record.state,
        vault_core::jobs::JobState::Cancelled,
        "a cancelled export job must end Cancelled"
    );
    assert!(
        !tmp.path().join("cancelled-job-bundle").exists(),
        "no bundle may exist after cancellation"
    );
    assert!(staging_artifacts(tmp.path()).is_empty());
    pool.shutdown();
}
