//! A few built-in sample messages so the search UI has something to show on
//! a fresh install before any real import.

use crate::message::NewMessage;

pub const DEMO_SOURCE: &str = "demo";

/// Sample messages, safe to insert repeatedly (deduped by
/// `(source, external_id)`).
pub fn messages() -> Vec<NewMessage> {
    let mut msgs: Vec<NewMessage> = vec![
        NewMessage::new(
            DEMO_SOURCE,
            "demo-1",
            "Mailchat Vault <demo@mailchat-vault.local>",
            "2026-11-01T09:00:00Z",
            "Welcome to your local mail and chat archive. Everything you import is stored \
             in SQLite and searched with FTS5, entirely on this machine.",
        ),
        NewMessage::new(
            DEMO_SOURCE,
            "demo-2",
            "Ana Sender <ana@example.com>",
            "2026-11-01T10:15:00Z",
            "Quarterly report attached. The vault migration plan was approved by the \
             board yesterday, kickoff is next Monday.",
        ),
        NewMessage::new(
            DEMO_SOURCE,
            "demo-3",
            "marko",
            "2026-11-01T11:42:00Z",
            "deploy finished, the dashboard search now hits the local index. no cloud \
             round trips.",
        ),
        NewMessage::new(
            DEMO_SOURCE,
            "demo-4",
            "jelena",
            "2026-11-01T11:45:00Z",
            "nice! searching \"quoted terms\" and author names works too, try searching \
             for jelena or vault.",
        ),
        NewMessage::new(
            DEMO_SOURCE,
            "demo-5",
            "marko",
            "2026-11-01T11:47:00Z",
            "next up: filters by source, author and date range over the whole archive.",
        ),
        NewMessage::new(
            DEMO_SOURCE,
            "demo-6",
            "jelena",
            "2026-11-01T11:49:00Z",
            "open any hit to read the full message with its thread neighbors. keyboard: \
             arrows to move, enter to open, esc to close.",
        ),
    ];
    // demo-1/2 are mail, the rest a chat: set the per-message grouping.
    for msg in &mut msgs {
        let chat = msg.external_id != "demo-1" && msg.external_id != "demo-2";
        msg.channel = if chat { "chat" } else { "email" }.to_string();
        msg.chat_id = if chat { "project-chat" } else { "welcome" }.to_string();
    }
    // demo-2 says "Quarterly report attached." — record the reference the
    // text promises so the paperclip badge and the files filter show a
    // working feature. The demo vault is an in-memory store seeded with
    // no media pass and no object store behind it, so the honest shape is
    // the unavailable reference (the "0/1" partial badge with its
    // explanatory title), never a pretend-preserved file.
    for msg in &mut msgs {
        if msg.external_id == "demo-2" {
            msg.raw_json = serde_json::json!({
                "attachment": { "kind": "document", "name": "quarterly-report.pdf" }
            })
            .to_string();
        }
    }
    msgs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search2::SearchArchiveRequest;
    use crate::storage::Storage;

    /// The "Quarterly report attached." message must carry real badge
    /// data: the archive records the reference its body promises, so the
    /// paperclip badge and the attachment filter teach a working feature
    /// — honestly, as an unpreserved reference (0/1), because the
    /// in-memory demo vault has no media store behind it.
    #[test]
    fn demo_quarterly_report_carries_an_attachment_reference() {
        let storage = Storage::open_in_memory().unwrap();
        for msg in messages() {
            storage.insert_message(&msg).unwrap();
        }

        let page = storage
            .search_archive(&SearchArchiveRequest {
                text: "Quarterly".into(),
                ..SearchArchiveRequest::default()
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1, "only the quarterly-report message matches");
        let hit = &page.hits[0];
        assert_eq!(hit.author, "Ana Sender <ana@example.com>", "the demo-2 message");
        assert!(
            hit.attachment_count >= 1,
            "badge data: the message that promises a report carries a reference"
        );
        assert_eq!(
            (hit.attachment_count, hit.available_attachment_count),
            (1, 0),
            "an honest unpreserved reference, not a pretend-preserved file"
        );

        // The files filter finds it too — the badge is not decoration.
        let filtered = storage
            .search_archive(&SearchArchiveRequest {
                has_attachment: Some(true),
                ..SearchArchiveRequest::default()
            })
            .unwrap();
        assert_eq!(filtered.hits.len(), 1, "exactly one demo message references a file");
        assert_eq!(
            filtered.hits[0].author, "Ana Sender <ana@example.com>",
            "the attachment filter matches the quarterly-report reference"
        );
    }
}
