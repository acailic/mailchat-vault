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
    msgs
}
