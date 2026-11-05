//! Manual real-server sync test.
//!
//! This test is `#[ignore]`d and NEVER runs in CI: it needs real
//! credentials, which CI does not have. Run it locally against your own
//! account (e.g. Gmail with an app password):
//!
//! ```sh
//! MAILCHAT_IMAP_USER=you@gmail.com \
//! MAILCHAT_IMAP_PASS=abcd-efgh-ijkl-mnop \
//! cargo test --test imap_gmail_manual -- --ignored --nocapture
//! ```
//!
//! Optional overrides: `MAILCHAT_IMAP_HOST` (default `imap.gmail.com`),
//! `MAILCHAT_IMAP_PORT` (default 993), `MAILCHAT_IMAP_FOLDERS`
//! (comma-separated, default `INBOX`). The archive is written to a
//! throwaway temp directory — nothing touches your real vault.

use vault_core::imap::{self, ImapAuth, ImapConfig};
use vault_core::storage::Storage;

#[test]
#[ignore = "hits a real IMAP server; needs MAILCHAT_IMAP_USER/PASS credentials"]
fn syncs_a_real_account_into_a_temp_archive() {
    let user = std::env::var("MAILCHAT_IMAP_USER").expect("MAILCHAT_IMAP_USER");
    let pass = std::env::var("MAILCHAT_IMAP_PASS").expect("MAILCHAT_IMAP_PASS");
    let host =
        std::env::var("MAILCHAT_IMAP_HOST").unwrap_or_else(|_| imap::GMAIL_IMAP_HOST.to_string());
    let port: u16 = std::env::var("MAILCHAT_IMAP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(imap::GMAIL_IMAP_PORT);
    let folders: Vec<String> = std::env::var("MAILCHAT_IMAP_FOLDERS")
        .map(|f| {
            f.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let config = ImapConfig {
        host,
        port,
        username: user,
        auth: ImapAuth::Password(pass),
        folders,
    };

    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("vault.db")).unwrap();
    let summaries = imap::sync(&config, &storage, None).unwrap();
    for s in &summaries {
        println!(
            "{s:?}: uidvalidity={}, reset={}, inserted={} skipped={} failed={}",
            s.uidvalidity, s.reset, s.inserted, s.skipped, s.failed
        );
    }
    println!("archive stats: {:?}", storage.stats().unwrap());
}
