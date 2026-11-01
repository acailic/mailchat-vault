//! Scenario: Gmail sign-in (OAuth2 → XOAUTH2) syncs the archive
//! end-to-end.
//!
//! The whole chain runs hermetically: the mock OAuth server
//! (`../oauth_mock`) issues real tokens through the real loopback-redirect
//! login flow, and the mock IMAP server (`../imap_mock`) is switched into
//! XOAUTH2-only mode — `LOGIN` refused, `AUTHENTICATE XOAUTH2` accepted
//! for exactly the SASL string the issued token produces. Scenarios cover
//! the first sync, refresh-then-resync (token rotation), stale-token
//! rejection, and the no-persist guarantee for the refresh token.

#[path = "../oauth_mock/mod.rs"]
mod oauth_mock;

use std::net::TcpStream;

use oauth_mock::{browser_visit, MockOAuth};
use vault_core::imap::{self as vimap, ImapAuth, ImapConfig};
use vault_core::oauth::{self, run_login, LOGIN_TIMEOUT};
use vault_core::storage::Storage;

use crate::imap_mock::MockImap;
use crate::{mails, USER};

/// The client credentials the mock OAuth server accepts.
const CLIENT_ID: &str = "test-client.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "test-fake-secret";

/// Start both halves of the chain: the mock OAuth server and an
/// XOAUTH2-only mock IMAP server that currently accepts `access_token`.
fn xoauth2_stack(access_token: &str) -> (MockOAuth, MockImap) {
    let oauth_server = MockOAuth::start(CLIENT_ID, CLIENT_SECRET);
    let imap_server = MockImap::start();
    imap_server.set_xoauth2(USER, access_token);
    (oauth_server, imap_server)
}

/// Run the real sign-in flow against the mock OAuth server (browser
/// callback included) and return the granted tokens.
fn sign_in(oauth_server: &MockOAuth) -> oauth::TokenSet {
    run_login(
        &oauth_server.config(),
        Some(USER),
        |url| browser_visit(url).map(|_| ()),
        LOGIN_TIMEOUT,
    )
    .expect("sign-in completes")
}

/// Open an authenticated XOAUTH2 session against the mock IMAP server —
/// the plain-TCP twin of the production `connect` (TLS + authenticate).
fn session_xoauth2(server: &MockImap, access_token: &str) -> imap::Session<TcpStream> {
    let stream = TcpStream::connect(server.addr).unwrap();
    let mut client = imap::Client::new(stream);
    client.read_greeting().unwrap();
    vimap::authenticate(
        client,
        USER,
        &ImapAuth::Xoauth2 {
            access_token: access_token.into(),
        },
    )
    .expect("XOAUTH2 session opens")
}

/// One sync round: authenticate with `access_token`, sync `folders`,
/// log out.
fn sync_round(
    server: &MockImap,
    access_token: &str,
    storage: &Storage,
    folders: &[&str],
) -> Vec<vimap::FolderSyncSummary> {
    let folders: Vec<String> = folders.iter().map(|f| f.to_string()).collect();
    let mut session = session_xoauth2(server, access_token);
    let source = vimap::source_name(USER);
    let summaries = vimap::sync_session(&mut session, storage, &source, &folders);
    let _ = session.logout();
    summaries
}

/// The full real-world chain: Google sign-in → tokens → XOAUTH2 IMAP sync
/// → searchable archive. No LOGIN ever happens; the SASL string on the
/// wire is exactly the XOAUTH2 format for the issued token.
#[test]
fn a_full_google_sign_in_syncs_mail_over_xoauth2() {
    let (oauth_server, imap_server) = xoauth2_stack("access-1");
    imap_server.set_mailbox(
        "INBOX",
        crate::imap_mock::MockMailbox::new(11, mails(3, "oa")),
    );
    let storage = Storage::open_in_memory().unwrap();

    let tokens = sign_in(&oauth_server);
    assert_eq!(tokens.access_token, "access-1");
    let refresh = tokens.refresh_token.clone().expect("refresh token granted");

    let summaries = sync_round(&imap_server, &tokens.access_token, &storage, &["INBOX"]);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].inserted, 3);
    assert!(summaries[0].error.is_none());

    // The archive holds the mail under the same source/dedup namespace as
    // password syncs, and it is searchable.
    assert_eq!(storage.stats().unwrap().total, 3);
    let hits = crate::find(&storage, "oabody2");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source, format!("imap:{USER}"));

    // On the wire: no LOGIN (refused anyway), exactly one AUTHENTICATE
    // whose decoded SASL string is the XOAUTH2 format for this user+token.
    let log = imap_server.log();
    assert!(
        log.logins.is_empty(),
        "password login must not happen: {:?}",
        log.logins
    );
    assert_eq!(
        log.authentications,
        vec![oauth::xoauth2_sasl(USER, "access-1")]
    );

    // And the refresh token is the only long-lived secret in play.
    assert_eq!(refresh, "refresh-1");
}

/// The steady-state loop: each sync refreshes the (rotating) access token
/// from the stored refresh token, and only new mail is fetched.
#[test]
fn resyncs_refresh_the_access_token_and_fetch_only_new_mail() {
    let (oauth_server, imap_server) = xoauth2_stack("access-1");
    imap_server.set_mailbox(
        "INBOX",
        crate::imap_mock::MockMailbox::new(5, mails(2, "rf")),
    );
    let storage = Storage::open_in_memory().unwrap();

    let tokens = sign_in(&oauth_server);
    let refresh = tokens.refresh_token.clone().unwrap();
    sync_round(&imap_server, &tokens.access_token, &storage, &["INBOX"]);

    // New mail arrives; the access token expired, so the next round
    // refreshes first — and the mock now only accepts the new token.
    imap_server.add_message(
        "INBOX",
        crate::mail(Some("rf-3@scenario.test"), "rf mail 3", "rfbody3"),
    );
    let refreshed = oauth::refresh_access_token(&oauth_server.config(), &refresh).unwrap();
    assert_eq!(refreshed.access_token, "access-2");
    assert_eq!(
        refreshed.refresh_token.as_deref(),
        Some(refresh.as_str()),
        "refreshing must keep the stored refresh token"
    );
    imap_server.set_xoauth2(USER, "access-2");

    let summaries = sync_round(&imap_server, &refreshed.access_token, &storage, &["INBOX"]);
    assert_eq!(summaries[0].inserted, 1, "only the new mail is fetched");
    assert_eq!(storage.stats().unwrap().total, 3);

    // The second round's grant on the wire was a refresh grant.
    let requests = oauth_server.token_requests();
    assert_eq!(requests.len(), 2, "sign-in exchange + one refresh");
    let refresh_request = &requests[1];
    let grant = refresh_request
        .iter()
        .find(|(k, _)| k == "grant_type")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    assert_eq!(grant, "refresh_token");
    // And the second IMAP login used the new token's SASL string.
    let log = imap_server.log();
    assert_eq!(
        log.authentications.last().map(String::as_str),
        Some(oauth::xoauth2_sasl(USER, "access-2").as_str())
    );
}

/// A stale or wrong access token is refused by the server with a clear
/// authentication failure — the sync never stores anything.
#[test]
fn a_wrong_access_token_is_refused_by_the_imap_server() {
    let (_oauth_server, imap_server) = xoauth2_stack("access-1");
    imap_server.set_mailbox(
        "INBOX",
        crate::imap_mock::MockMailbox::new(5, mails(2, "st")),
    );
    let storage = Storage::open_in_memory().unwrap();

    // The server rotated to access-9; our token is stale.
    imap_server.set_xoauth2(USER, "access-9");
    let stream = TcpStream::connect(imap_server.addr).unwrap();
    let mut client = imap::Client::new(stream);
    client.read_greeting().unwrap();
    let error = vimap::authenticate(
        client,
        USER,
        &ImapAuth::Xoauth2 {
            access_token: "access-1".into(),
        },
    )
    .expect_err("a stale token must be refused");
    assert!(
        error
            .to_string()
            .to_lowercase()
            .contains("authenticationfailed"),
        "the server's AUTHENTICATIONFAILED must surface: {error}"
    );
    assert_eq!(storage.stats().unwrap().total, 0, "nothing was stored");
}

/// The config presets: `gmail_oauth` builds the XOAUTH2 shape, and Debug
/// output never prints the token.
#[test]
fn gmail_oauth_config_shape_and_redaction() {
    let config = ImapConfig::gmail_oauth(USER, "access-1");
    assert_eq!(config.host, "imap.gmail.com");
    assert_eq!(config.port, 993);
    assert!(
        matches!(&config.auth, ImapAuth::Xoauth2 { access_token } if access_token == "access-1")
    );
    let printed = format!("{config:?}");
    assert!(
        !printed.contains("access-1"),
        "Debug leaked the token: {printed}"
    );
    assert!(
        printed.contains("xoauth2"),
        "the auth mode stays readable: {printed}"
    );
}

/// The no-persist guarantee extends to the OAuth secrets: after a full
/// sign-in + sync lifecycle (live WAL included, and again after close),
/// no file under the storage directory holds the refresh token's bytes.
#[test]
fn no_trace_of_the_refresh_token_in_any_file_after_oauth_syncs() {
    use std::path::Path;

    fn all_files_under(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(current) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    found.push(path);
                }
            }
        }
        found
    }

    let (oauth_server, imap_server) = xoauth2_stack("access-1");
    imap_server.set_mailbox(
        "INBOX",
        crate::imap_mock::MockMailbox::new(9, mails(4, "np")),
    );
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path().join("vault.db")).unwrap();

    let tokens = sign_in(&oauth_server);
    let refresh = tokens.refresh_token.clone().unwrap();
    sync_round(&imap_server, &tokens.access_token, &storage, &["INBOX"]);
    // A second (incremental) round, plus a failed folder, so error paths
    // write to the database too.
    imap_server.add_message(
        "INBOX",
        crate::mail(Some("np-5@scenario.test"), "np mail 5", "npbody5"),
    );
    sync_round(
        &imap_server,
        &tokens.access_token,
        &storage,
        &["INBOX", "NoSuchBox"],
    );
    assert!(storage.stats().unwrap().total >= 5);

    let scan = |phase: &str| {
        for path in all_files_under(dir.path()) {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            assert!(
                !bytes
                    .windows(refresh.len())
                    .any(|w| w == refresh.as_bytes()),
                "{phase}: refresh token leaked into {}",
                path.display()
            );
        }
    };
    scan("live");
    drop(storage);
    scan("after close");
}
