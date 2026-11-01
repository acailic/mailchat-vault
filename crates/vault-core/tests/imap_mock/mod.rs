//! In-process mock IMAP4rev1 server (the hermetic "greenmail equivalent").
//!
//! A minimal scripted server over plain TCP. The production client path
//! wraps the stream in TLS; tests instead use `imap::Client::new` over a
//! raw `TcpStream`, which speaks the same protocol. Only the commands the
//! vault-core sync issues are implemented (LOGIN, EXAMINE/SELECT, UID
//! SEARCH, UID FETCH, LOGOUT); the shared mailbox state can be mutated
//! between connections to simulate new mail and UIDVALIDITY resets.

// Shared between test binaries that use different subsets of the helpers.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// One mock mailbox: UIDVALIDITY, UIDNEXT, and messages by UID.
#[derive(Debug, Clone)]
pub struct MockMailbox {
    pub uidvalidity: u32,
    pub uid_next: u32,
    /// UID -> raw RFC 822 message bytes.
    pub messages: BTreeMap<u32, Vec<u8>>,
}

impl MockMailbox {
    /// A mailbox whose messages are assigned UIDs 1..=n.
    pub fn new(uidvalidity: u32, messages: Vec<Vec<u8>>) -> Self {
        let mut map = BTreeMap::new();
        for (i, raw) in messages.into_iter().enumerate() {
            map.insert((i + 1) as u32, raw);
        }
        Self {
            uidvalidity,
            uid_next: map.len() as u32 + 1,
            messages: map,
        }
    }
}

/// Commands the tests observed, for behavioral assertions.
#[derive(Debug, Default, Clone)]
pub struct ServerLog {
    pub logins: Vec<String>,
    pub searches: Vec<String>,
    pub fetch_sets: Vec<String>,
}

/// Handle to the running mock server.
pub struct MockImap {
    pub addr: SocketAddr,
    mailboxes: Arc<Mutex<BTreeMap<String, MockMailbox>>>,
    log: Arc<Mutex<ServerLog>>,
}

impl MockImap {
    /// Start the server on an ephemeral loopback port.
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock imap");
        let addr = listener.local_addr().expect("mock addr");
        let mailboxes = Arc::new(Mutex::new(BTreeMap::new()));
        let log = Arc::new(Mutex::new(ServerLog::default()));
        {
            let mailboxes = Arc::clone(&mailboxes);
            let log = Arc::clone(&log);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let mailboxes = Arc::clone(&mailboxes);
                    let log = Arc::clone(&log);
                    std::thread::spawn(move || {
                        if let Err(e) = serve(stream, &mailboxes, &log) {
                            // A dropped client mid-command is normal in
                            // tests; anything else is a protocol bug.
                            eprintln!("mock imap connection error: {e}");
                        }
                    });
                }
            });
        }
        Self {
            addr,
            mailboxes,
            log,
        }
    }

    /// Replace a mailbox wholesale (also how UIDVALIDITY resets are
    /// simulated).
    pub fn set_mailbox(&self, name: &str, mailbox: MockMailbox) {
        self.mailboxes
            .lock()
            .unwrap()
            .insert(name.to_string(), mailbox);
    }

    /// Append a message to a mailbox, assigning the next UID. Panics when
    /// the mailbox does not exist.
    pub fn add_message(&self, name: &str, raw: Vec<u8>) -> u32 {
        let mut boxes = self.mailboxes.lock().unwrap();
        let mailbox = boxes.get_mut(name).expect("mailbox exists");
        let uid = mailbox.uid_next;
        mailbox.uid_next += 1;
        mailbox.messages.insert(uid, raw);
        uid
    }

    /// Snapshot of the observed commands.
    pub fn log(&self) -> ServerLog {
        self.log.lock().unwrap().clone()
    }
}

/// Serve one client connection to completion (LOGOUT or disconnect).
fn serve(
    stream: TcpStream,
    mailboxes: &Arc<Mutex<BTreeMap<String, MockMailbox>>>,
    log: &Arc<Mutex<ServerLog>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    // Per-connection state: the mailbox the client last selected.
    let mut selected: Option<String> = None;
    write_line(&mut writer, b"* OK mailchat-vault mock IMAP4rev1 ready")?;

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(()); // client dropped
        }
        let line = line.trim_end();
        let mut parts = line.splitn(3, ' ');
        let (Some(tag), Some(command), rest) =
            (parts.next(), parts.next(), parts.next().unwrap_or(""))
        else {
            continue;
        };
        let rest = rest.trim().to_string();

        match command.to_ascii_uppercase().as_str() {
            "LOGIN" => {
                log.lock().unwrap().logins.push(line.to_string());
                write_line(&mut writer, format!("{tag} OK LOGIN completed").as_bytes())?;
            }
            "CAPABILITY" => {
                write_line(&mut writer, b"* CAPABILITY IMAP4rev1")?;
                write_line(
                    &mut writer,
                    format!("{tag} OK CAPABILITY completed").as_bytes(),
                )?;
            }
            "SELECT" | "EXAMINE" => {
                let mailbox_name = unquote(&rest);
                let snapshot = {
                    let boxes = mailboxes.lock().unwrap();
                    boxes.get(&mailbox_name).cloned()
                };
                match snapshot {
                    Some(mailbox) => {
                        selected = Some(mailbox_name.clone());
                        write_line(
                            &mut writer,
                            format!("* {} EXISTS", mailbox.messages.len()).as_bytes(),
                        )?;
                        write_line(&mut writer, b"* 0 RECENT")?;
                        write_line(&mut writer, b"* FLAGS (\\Answered \\Flagged \\Seen)")?;
                        write_line(
                            &mut writer,
                            format!("* OK [UIDVALIDITY {}] UIDs valid", mailbox.uidvalidity)
                                .as_bytes(),
                        )?;
                        write_line(
                            &mut writer,
                            format!("* OK [UIDNEXT {}] predicted next UID", mailbox.uid_next)
                                .as_bytes(),
                        )?;
                        write_line(
                            &mut writer,
                            format!("{tag} OK [READ-WRITE] {command} completed").as_bytes(),
                        )?;
                    }
                    None => {
                        selected = None;
                        write_line(
                            &mut writer,
                            format!("{tag} NO no such mailbox: {mailbox_name}").as_bytes(),
                        )?;
                    }
                }
            }
            "UID" => {
                let mut sub = rest.splitn(2, ' ');
                let sub_command = sub.next().unwrap_or_default().to_ascii_uppercase();
                let args = sub.next().unwrap_or_default().trim().to_string();
                let Some(mailbox_name) = selected.clone() else {
                    write_line(
                        &mut writer,
                        format!("{tag} BAD no mailbox selected").as_bytes(),
                    )?;
                    continue;
                };
                match sub_command.as_str() {
                    "SEARCH" => {
                        log.lock().unwrap().searches.push(args.clone());
                        let snapshot = {
                            let boxes = mailboxes.lock().unwrap();
                            boxes.get(&mailbox_name).cloned()
                        };
                        let matches = snapshot
                            .map(|mailbox| search_uids(&args, &mailbox))
                            .unwrap_or_default();
                        let joined = matches
                            .iter()
                            .map(|u| u.to_string())
                            .collect::<Vec<_>>()
                            .join(" ");
                        if joined.is_empty() {
                            write_line(&mut writer, b"* SEARCH")?;
                        } else {
                            write_line(&mut writer, format!("* SEARCH {joined}").as_bytes())?;
                        }
                        write_line(&mut writer, format!("{tag} OK SEARCH completed").as_bytes())?;
                    }
                    "FETCH" => {
                        // args: "<uid-set> <query...>"
                        let (set, _query) = args
                            .split_once(' ')
                            .map(|(s, q)| (s.to_string(), q.to_string()))
                            .unwrap_or((args.clone(), String::new()));
                        log.lock().unwrap().fetch_sets.push(set.clone());
                        let snapshot = {
                            let boxes = mailboxes.lock().unwrap();
                            boxes.get(&mailbox_name).cloned()
                        };
                        if let Some(mailbox) = snapshot {
                            for (seq, (uid, raw)) in expand_set(&set)
                                .into_iter()
                                .filter_map(|uid| {
                                    mailbox.messages.get(&uid).map(|r| (uid, r.clone()))
                                })
                                .enumerate()
                            {
                                let prefix = format!(
                                    "* {} FETCH (UID {uid} RFC822 {{{}}}",
                                    seq + 1,
                                    raw.len()
                                );
                                writer.write_all(prefix.as_bytes())?;
                                writer.write_all(b"\r\n")?;
                                writer.write_all(&raw)?;
                                write_line(&mut writer, b")")?;
                            }
                        }
                        write_line(&mut writer, format!("{tag} OK FETCH completed").as_bytes())?;
                    }
                    _ => {
                        write_line(
                            &mut writer,
                            format!("{tag} BAD unknown UID subcommand").as_bytes(),
                        )?;
                    }
                }
            }
            "LOGOUT" => {
                write_line(&mut writer, b"* BYE mock server signing off")?;
                write_line(&mut writer, format!("{tag} OK LOGOUT completed").as_bytes())?;
                return Ok(());
            }
            "NOOP" => {
                write_line(&mut writer, format!("{tag} OK NOOP completed").as_bytes())?;
            }
            _ => {
                write_line(
                    &mut writer,
                    format!("{tag} OK {command} completed (ignored)").as_bytes(),
                )?;
            }
        }
    }
}

/// Resolve the UID SEARCH criteria "<start>:*" (or a single "<uid>") against
/// the given mailbox. An inverted range (start above the highest existing
/// UID) is empty, per RFC 3501 §6.4.8.
fn search_uids(criteria: &str, mailbox: &MockMailbox) -> Vec<u32> {
    let (start, end) = match criteria.split_once(':') {
        Some((a, b)) => (
            a.parse::<u32>().unwrap_or(1),
            if b == "*" {
                u32::MAX
            } else {
                b.parse::<u32>().unwrap_or(u32::MAX)
            },
        ),
        None => {
            let uid = criteria.parse::<u32>().unwrap_or(0);
            return if mailbox.messages.contains_key(&uid) {
                vec![uid]
            } else {
                vec![]
            };
        }
    };
    mailbox
        .messages
        .keys()
        .copied()
        .filter(|uid| *uid >= start && *uid <= end)
        .collect()
}

/// Expand an IMAP UID set ("1", "1:3", "5:*", "1,3:4") to a sorted list of
/// UIDs.
fn expand_set(set: &str) -> Vec<u32> {
    let mut uids = Vec::new();
    for item in set.split(',') {
        match item.split_once(':') {
            Some((a, b)) => {
                let lo = a.parse::<u32>().unwrap_or(1);
                let hi = if b == "*" {
                    u32::MAX
                } else {
                    b.parse::<u32>().unwrap_or(u32::MAX)
                };
                // Guard against absurd expansions (1:*): callers only use
                // this for explicit small sets.
                for uid in lo..=hi.min(lo.saturating_add(10_000)) {
                    uids.push(uid);
                }
            }
            None => {
                if let Ok(uid) = item.parse::<u32>() {
                    uids.push(uid);
                }
            }
        }
    }
    uids.sort_unstable();
    uids.dedup();
    uids
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

fn write_line<W: Write>(writer: &mut W, bytes: &[u8]) -> std::io::Result<()> {
    writer.write_all(bytes)?;
    writer.write_all(b"\r\n")?;
    writer.flush()
}
