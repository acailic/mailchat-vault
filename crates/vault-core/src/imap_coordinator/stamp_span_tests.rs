//! The success stamp sits outside the failure-recorded span: a failed
//! stamp write (transient sqlite pressure) must propagate without
//! durably labeling an account whose mail actually synced as failed.

use super::*;
use crate::imap::SyncFailure;
use crate::storage::Storage;
use std::sync::{Arc, Mutex};

/// A [`SyncStorage`] sink whose final success stamp always fails —
/// every other call delegates to the real [`Storage`].
struct StampFails<'a>(&'a Storage);

impl SyncStorage for StampFails<'_> {
    fn ensure_instance(&self, config: &ImapConfig) -> crate::Result<String> {
        self.0.ensure_instance(config)
    }
    fn record_attempt(&self, instance: &str) -> crate::Result<()> {
        self.0.record_attempt(instance)
    }
    fn record_success(&self, _instance: &str) -> crate::Result<()> {
        Err(crate::Error::Io(std::io::Error::other(
            "stamp write failed",
        )))
    }
    fn record_failure(
        &self,
        instance: &str,
        failure: &SyncFailure,
        message: &str,
    ) -> crate::Result<()> {
        self.0.record_failure(instance, failure, message)
    }
    fn checkpoint(
        &self,
        file: &str,
        folder: &str,
    ) -> crate::Result<Option<crate::message::ImapSyncState>> {
        self.0.checkpoint(file, folder)
    }
    fn commit_batch(
        &self,
        instance: &str,
        folder: &str,
        uidvalidity: u32,
        last_uid: u32,
        mails: &[crate::storage::MailBatchItem<'_>],
        failures: &[crate::storage::BatchFailure<'_>],
        resolved: &[u32],
    ) -> crate::Result<crate::storage::MailBatchOutcome> {
        self.0.commit_batch(
            instance,
            folder,
            uidvalidity,
            last_uid,
            mails,
            failures,
            resolved,
        )
    }
    fn failed_uids(
        &self,
        instance: &str,
        folder: &str,
        uidvalidity: u32,
    ) -> crate::Result<Vec<u32>> {
        self.0.failed_uids(instance, folder, uidvalidity)
    }
}

/// Plain-TCP logged-in session builder (the no-TLS equivalent of the
/// production implicit-TLS connect, as in the integration tests).
fn mock_connect(config: &ImapConfig) -> crate::Result<Session<std::net::TcpStream>> {
    let stream = std::net::TcpStream::connect(("127.0.0.1", config.port))?;
    let mut client = ::imap::Client::new(stream);
    client.read_greeting()?;
    Ok(client
        .login(config.username.clone(), "stamp-test-password")
        .map_err(|(e, _client)| e)?)
}

/// A minimal scripted IMAP server: a greeting, then `OK` to every
/// command with the command's own tag — enough for LOGIN, EXAMINE,
/// UID SEARCH (empty), and LOGOUT over an empty INBOX. The sync span
/// therefore completes fully; only the final stamp write fails.
struct StampServer {
    addr: std::net::SocketAddr,
    log: Arc<Mutex<Vec<String>>>,
}

impl StampServer {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log_thread = Arc::clone(&log);
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                let mut line = String::new();
                use std::io::BufRead;
                let _ = std::io::Write::write_all(&mut writer, b"* OK stamp server ready\r\n");
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    log_thread.lock().unwrap().push(line.trim_end().to_string());
                    let upper = line.to_ascii_uppercase();
                    let tag = line.split_whitespace().next().unwrap_or("*").to_string();
                    let ok = format!("{tag} OK completed\r\n");
                    if upper.contains("EXAMINE") || upper.contains("SELECT") {
                        for untagged in [
                            "* 0 EXISTS\r\n",
                            "* 0 RECENT\r\n",
                            "* FLAGS (\\Answered \\Flagged \\Seen)\r\n",
                            "* OK [UIDVALIDITY 1] UIDs valid\r\n",
                            "* OK [UIDNEXT 1] predicted next UID\r\n",
                        ] {
                            let _ = std::io::Write::write_all(&mut writer, untagged.as_bytes());
                        }
                    } else if upper.contains("UID SEARCH") {
                        let _ = std::io::Write::write_all(&mut writer, b"* SEARCH\r\n");
                    }
                    let _ = std::io::Write::write_all(&mut writer, ok.as_bytes());
                    if upper.contains("LOGOUT") {
                        break;
                    }
                    line.clear();
                }
            }
        });
        Self { addr, log }
    }
}

#[test]
fn failed_success_stamp_propagates_without_recording_a_failure() {
    let storage = Storage::open_in_memory().unwrap();
    let server = StampServer::start();
    let config = ImapConfig {
        host: "127.0.0.1".into(),
        port: server.addr.port(),
        username: "alice@example.com".into(),
        auth: ImapAuth::Password("stamp-test-password".into()),
        folders: Vec::new(),
    };

    let sink = StampFails(&storage);
    let instance = sink.ensure_instance(&config).unwrap();
    let err = sync_one(&config, &sink, &instance, &mock_connect, None).unwrap_err();

    // The span reached the server and completed: the session commands
    // were all served, so only the stamp write fell.
    assert!(
        server
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.to_ascii_uppercase().contains("LOGOUT")),
        "the session ran to logout: {:?}",
        server.log.lock().unwrap()
    );
    // The error propagates (the caller still sees the stamp failure)…
    assert!(
        err.to_string().contains("stamp write failed"),
        "the stamp error propagates: {err}"
    );
    // …but the account whose mail did sync carries no failure record:
    // the span itself succeeded, so only the stamp error fell.
    let instances = storage.list_source_instances().unwrap();
    assert_eq!(instances.len(), 1);
    assert_eq!(
        instances[0].last_error_class, None,
        "no sync failure recorded for a successful session"
    );
    assert_eq!(instances[0].last_error, None);
    assert_eq!(instances[0].last_error_at, None);
}
