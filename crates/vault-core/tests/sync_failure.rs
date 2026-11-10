//! Sync-failure tests (cold review §2.2 / top-5 #5: the first-sync
//! failure loop).
//!
//! Three contracts, each pinned here:
//!
//! - **classification** — every failure shape the connect path can produce
//!   maps to a stable [`SyncFailure`] class with actionable, errno-free
//!   user text: DNS resolution, connection refused, timeout, no route,
//!   auth rejection, TLS, connection-lost ([`classification_*`]);
//! - **durable error state** — a failed first sync leaves the durable
//!   source instance behind *with* the recorded failure (class + message +
//!   stamp), so the accounts panel shows what fell and why across
//!   restarts, with the entered configuration intact
//!   ([`failed_first_sync_records_the_failure_on_the_instance`]);
//! - **clearing** — a later successful sync clears the recorded failure
//!   (the error state describes the *last finished attempt*)
//!   ([`success_clears_the_recorded_failure`]).
//!
//! The failed-first-sync account survives a restart by construction: the
//! failure record lives in the vault database, not process memory (the
//! restart behavior itself is pinned at the product surface by the e2e
//! spec `26-sync-failure-paths`).

use std::io::ErrorKind;
use std::time::Duration;

use vault_core::imap::{self, ImapAuth, ImapConfig, SyncFailure};
use vault_core::imap_coordinator::StoredImapConfig;
use vault_core::storage::Storage;
use vault_core::Error;

/// A config pointed at a local port where nothing listens.
fn refused_config() -> ImapConfig {
    ImapConfig {
        host: "127.0.0.1".into(),
        port: 59993,
        username: "first@h.invalid".into(),
        auth: ImapAuth::Password("not-a-real-password".into()),
        folders: vec![],
    }
}

// ---------------------------------------------------------------------
// Classification: every shape → a stable class with actionable text
// ---------------------------------------------------------------------

#[test]
fn classification_connection_refused() {
    let err = Error::Io(std::io::Error::from_raw_os_error(111)); // ECONNREFUSED
    assert_eq!(SyncFailure::classify(&err), SyncFailure::ConnectionRefused);
    let line = SyncFailure::classify(&err).describe(&err);
    assert!(
        line.contains("nothing answered"),
        "the refused line must be actionable: {line}"
    );
    assert!(
        !line.contains("os error"),
        "no raw errno in user-facing text: {line}"
    );
}

#[test]
fn classification_dns_failure_by_resolver_message() {
    // getaddrinfo failures arrive as uncategorized io errors whose text
    // differs by libc (glibc here; musl, Apple, and Windows variants
    // matched too).
    for shape in [
        "failed to lookup address information: Name or service not known",
        "nodename nor servname provided, or not known",
        "temporary failure in name resolution",
    ] {
        let err = Error::Io(std::io::Error::other(shape));
        assert_eq!(
            SyncFailure::classify(&err),
            SyncFailure::HostNotFound,
            "shape must classify as host-not-found: {shape}"
        );
    }
    // Windows: WSAHOST_NOT_FOUND surfaces as an uncategorized io error
    // whose text embeds the errno ("No such host is known. (os error
    // 11001)") — classified, and the errno stays out of the user line.
    let err = Error::Io(std::io::Error::other(
        "No such host is known. (os error 11001)",
    ));
    assert_eq!(
        SyncFailure::classify(&err),
        SyncFailure::HostNotFound,
        "the Windows host-not-found shape: {err}"
    );
    let line = SyncFailure::classify(&err).describe(&err);
    assert!(line.contains("resolved"), "actionable DNS text: {line}");
    assert!(
        !line.contains("os error"),
        "no raw errno in the user line: {line}"
    );
    let err = Error::Io(std::io::Error::other(
        "failed to lookup address information: Name or service not known",
    ));
    let line = SyncFailure::classify(&err).describe(&err);
    assert!(line.contains("resolved"), "actionable DNS text: {line}");
}

#[test]
fn classification_timeout_and_no_route() {
    let err = Error::Io(std::io::Error::new(
        ErrorKind::TimedOut,
        "connection timed out",
    ));
    assert_eq!(SyncFailure::classify(&err), SyncFailure::Timeout);
    let err = Error::Io(std::io::Error::new(
        ErrorKind::WouldBlock,
        "resource temporarily unavailable",
    ));
    assert_eq!(SyncFailure::classify(&err), SyncFailure::Timeout);
    let err = Error::Io(std::io::Error::new(
        ErrorKind::NetworkUnreachable,
        "network is unreachable",
    ));
    assert_eq!(SyncFailure::classify(&err), SyncFailure::NetworkUnreachable);
}

#[test]
fn classification_auth_rejection_by_server_response() {
    // RFC 3501 response codes and the common free-text phrasings, with
    // punctuation that would hide a naive substring match.
    for text in [
        "[AUTHENTICATIONFAILED] Invalid credentials (Failure)",
        "AUTHENTICATIONFAILED",
        "invalid credentials for user",
        "LOGIN failed",
        "password error",
    ] {
        let err = Error::Imap(::imap::Error::No(text.to_string()));
        assert_eq!(
            SyncFailure::classify(&err),
            SyncFailure::AuthFailed,
            "server text must classify as auth failure: {text}"
        );
    }
    // The server's own words ride along after the actionable lead.
    let err = Error::Imap(::imap::Error::No(
        "[AUTHENTICATIONFAILED] Invalid credentials (Failure)".into(),
    ));
    let line = SyncFailure::classify(&err).describe(&err);
    assert!(
        line.starts_with("the server rejected the login"),
        "the actionable lead comes first: {line}"
    );
    assert!(
        line.contains("AUTHENTICATIONFAILED"),
        "the server's reason is kept for support: {line}"
    );
}

#[test]
fn classification_non_auth_no_response_is_not_auth() {
    // A NO response that is not an auth rejection (a server-side refusal,
    // a mailbox problem) must not misclassify as bad credentials.
    let err = Error::Imap(::imap::Error::No("no such mailbox".into()));
    assert_eq!(SyncFailure::classify(&err), SyncFailure::Other);
}

#[test]
fn classification_tls_and_connection_lost() {
    let err = Error::Tls(rustls::Error::DecryptError);
    assert_eq!(SyncFailure::classify(&err), SyncFailure::Tls);
    let err = Error::Imap(::imap::Error::ConnectionLost);
    assert_eq!(SyncFailure::classify(&err), SyncFailure::ConnectionLost);
    // An EOF during the handshake surfaces as an imap io error — the
    // plain-TCP-listener-on-993 case from the cold review.
    let err = Error::Imap(::imap::Error::Io(std::io::Error::new(
        ErrorKind::UnexpectedEof,
        "unexpected end of file",
    )));
    let failure = SyncFailure::classify(&err);
    assert_eq!(failure, SyncFailure::ConnectionLost);
    assert!(
        !failure.describe(&err).contains("end of file"),
        "raw EOF vocabulary stays out of the user line"
    );
}

// ---------------------------------------------------------------------
// Durable account-level error state
// ---------------------------------------------------------------------

#[test]
fn failed_first_sync_records_the_failure_on_the_instance() {
    let storage = Storage::open_in_memory().unwrap();
    let err = imap::sync_with(&refused_config(), &storage, None).unwrap_err();
    assert_eq!(SyncFailure::classify(&err), SyncFailure::ConnectionRefused);

    // The account row exists AND carries the failure record: what fell
    // (class), why (message), and when — the state the accounts panel
    // shows, surviving restarts because it lives in the vault database.
    let instances = storage.list_source_instances().unwrap();
    assert_eq!(
        instances.len(),
        1,
        "the failed first sync still registers the account"
    );
    let instance = &instances[0];
    assert_eq!(instance.kind, "imap");
    assert_eq!(instance.state, "disconnected");
    assert_eq!(
        instance.last_error_class.as_deref(),
        Some("connection_refused")
    );
    let message = instance
        .last_error
        .as_deref()
        .expect("the failure is recorded");
    assert!(
        message.contains("nothing answered"),
        "actionable text: {message}"
    );
    assert!(!message.contains("os error"), "no raw errno: {message}");
    assert!(instance.last_error_at.is_some(), "the failure is stamped");
    assert!(instance.last_success_at.is_none(), "nothing was synced");
    // The entered identity is kept on the row — the user retypes nothing.
    let stored = StoredImapConfig::of(instance).expect("imap config stored");
    assert_eq!(stored.host, "127.0.0.1");
    assert_eq!(stored.port, 59993);
    assert_eq!(stored.username, "first@h.invalid");
}

#[test]
fn failed_then_rejected_again_updates_the_record_not_duplicates() {
    let storage = Storage::open_in_memory().unwrap();
    imap::sync_with(&refused_config(), &storage, None).unwrap_err();
    imap::sync_with(&refused_config(), &storage, None).unwrap_err();
    let instances = storage.list_source_instances().unwrap();
    assert_eq!(
        instances.len(),
        1,
        "the connector key maps retries to one instance"
    );
    assert!(instances[0].last_error.is_some());
}

#[test]
fn success_clears_the_recorded_failure() {
    let storage = Storage::open_in_memory().unwrap();
    let id = imap::ensure_source_instance(&storage, &refused_config()).unwrap();
    storage
        .record_source_failure(&id, "connection_refused", "nothing answered")
        .unwrap();
    let recorded = storage.list_source_instances().unwrap();
    assert!(recorded[0].last_error.is_some());

    storage.record_source_success(&id).unwrap();
    let cleared = storage.list_source_instances().unwrap();
    assert_eq!(cleared[0].state, "connected");
    assert_eq!(cleared[0].last_error, None);
    assert_eq!(cleared[0].last_error_class, None);
    assert_eq!(cleared[0].last_error_at, None);
}

#[test]
fn failure_record_survives_a_storage_reopen() {
    // A "restart": close the connection, reopen the same database file —
    // the error state is durable, not process memory.
    let dir = std::env::temp_dir().join(format!("mc-sync-fail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("vault.db");
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(db.with_extension("db-wal"));
    let _ = std::fs::remove_file(db.with_extension("db-shm"));

    {
        let storage = Storage::open(&db).unwrap();
        imap::sync_with(&refused_config(), &storage, None).unwrap_err();
    }
    let reopened = Storage::open(&db).unwrap();
    let instances = reopened.list_source_instances().unwrap();
    assert_eq!(instances.len(), 1, "the account survives the reopen");
    assert_eq!(
        instances[0].last_error_class.as_deref(),
        Some("connection_refused"),
        "the failure record survives the reopen"
    );
    assert!(instances[0].last_error_at.is_some());

    std::fs::remove_dir_all(&dir).ok();
}

/// A bounded connect must bound the blackholed-host path too (the class
/// recorded is timeout or no-route depending on the host's network).
#[test]
fn blackholed_host_fails_bounded_with_a_recorded_network_failure() {
    let storage = Storage::open_in_memory().unwrap();
    let mut config = refused_config();
    config.host = "10.255.255.1".into();
    let start = std::time::Instant::now();
    let err = imap::sync_with(&config, &storage, None).unwrap_err();
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(90),
        "connect stays bounded: {elapsed:?}"
    );
    let failure = SyncFailure::classify(&err);
    assert!(
        failure == SyncFailure::Timeout || failure == SyncFailure::NetworkUnreachable,
        "network-bound failure class: {failure:?}"
    );
    let instances = storage.list_source_instances().unwrap();
    assert_eq!(
        instances.len(),
        1,
        "the timed-out first sync still registers the account"
    );
}

// ---------------------------------------------------------------------
// Bad auth at the protocol level (mock server; the TLS-trusted-server
// precondition cannot exist in a sandbox, so the product-surface spec
// covers refused/dns/timeout and the auth class is pinned here)
// ---------------------------------------------------------------------

#[path = "imap_mock/mod.rs"]
mod imap_mock;

#[test]
fn bad_auth_sync_records_auth_failure() {
    use imap_mock::MockImap;

    // The mock's XOAUTH2 path rejects a wrong token with a genuine
    // `[AUTHENTICATIONFAILED]` NO — the same server rejection a real
    // server gives bad credentials, through the same `authenticate` the
    // TLS path funnels both auth modes through.
    let server = MockImap::start();
    server.set_xoauth2("alice@example.com", "the-right-token");
    let mut stream = std::net::TcpStream::connect(server.addr).unwrap();
    let mut client = ::imap::Client::new(&mut stream);
    client.read_greeting().unwrap();
    let err = imap::authenticate(
        client,
        "alice@example.com",
        &ImapAuth::Xoauth2 {
            access_token: "wrong-token".into(),
        },
    )
    .unwrap_err();
    assert_eq!(SyncFailure::classify(&err), SyncFailure::AuthFailed);
    let line = SyncFailure::classify(&err).describe(&err);
    assert!(line.contains("rejected the login"), "actionable: {line}");
    assert!(
        line.contains("app password"),
        "the Gmail hint is in the line: {line}"
    );
}
