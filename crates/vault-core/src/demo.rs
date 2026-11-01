//! A few built-in sample messages so the search UI has something to show on
//! a fresh install before any real import.

use crate::message::NewMessage;

pub const DEMO_SOURCE: &str = "demo";

/// Sample messages, safe to insert repeatedly (deduped by
/// `(source, external_id)`).
pub fn messages() -> Vec<NewMessage> {
    vec![
        NewMessage {
            source: DEMO_SOURCE.into(),
            external_id: "demo-1".into(),
            channel: "email".into(),
            chat_id: "welcome".into(),
            author: "Mailchat Vault <demo@mailchat-vault.local>".into(),
            sent_at: "2026-11-01T09:00:00Z".into(),
            body: "Welcome to your local mail and chat archive. \
                   Everything you import is stored in SQLite and searched with FTS5, \
                   entirely on this machine."
                .into(),
            raw_json: "{}".into(),
        },
        NewMessage {
            source: DEMO_SOURCE.into(),
            external_id: "demo-2".into(),
            channel: "email".into(),
            chat_id: "welcome".into(),
            author: "Ana Sender <ana@example.com>".into(),
            sent_at: "2026-11-01T10:15:00Z".into(),
            body: "Quarterly report attached. The vault migration plan was approved \
                   by the board yesterday, kickoff is next Monday."
                .into(),
            raw_json: "{}".into(),
        },
        NewMessage {
            source: DEMO_SOURCE.into(),
            external_id: "demo-3".into(),
            channel: "chat".into(),
            chat_id: "project-chat".into(),
            author: "marko".into(),
            sent_at: "2026-11-01T11:42:00Z".into(),
            body: "deploy finished, the dashboard search now hits the local index. \
                   no cloud round trips."
                .into(),
            raw_json: "{}".into(),
        },
        NewMessage {
            source: DEMO_SOURCE.into(),
            external_id: "demo-4".into(),
            channel: "chat".into(),
            chat_id: "project-chat".into(),
            author: "jelena".into(),
            sent_at: "2026-11-01T11:45:00Z".into(),
            body: "nice! searching \"quoted terms\" and author names works too, \
                   try searching for jelena or vault."
                .into(),
            raw_json: "{}".into(),
        },
    ]
}
