//! In-process mock IMAP4rev1 server (the hermetic "greenmail equivalent").
//!
//! A minimal scripted server over plain TCP. The production client path
//! wraps the stream in TLS; tests instead use `imap::Client::new` over a
//! raw `TcpStream`, which speaks the same protocol. Only the commands the
//! vault-core sync issues are implemented (LOGIN, AUTHENTICATE XOAUTH2,
//! EXAMINE/SELECT, UID SEARCH, UID FETCH, LOGOUT); the shared mailbox
//! state can be mutated between connections to simulate new mail and
//! UIDVALIDITY resets.

// Shared between test binaries that use different subsets of the helpers.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};

use base64::Engine;

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
    /// Decoded SASL strings of `AUTHENTICATE XOAUTH2` attempts.
    pub authentications: Vec<String>,
    pub searches: Vec<String>,
    pub fetch_sets: Vec<String>,
}

/// The server's shared fault-injection and observation knobs (one struct
/// so the connection threads take a single argument).
struct Knobs {
    /// When `Some(n)`, exactly the nth UID FETCH command seen on this
    /// server is answered with a tagged `BAD` instead of message data —
    /// simulates a server that breaks mid-folder. Every other FETCH is
    /// served normally.
    fail_fetch_once: Mutex<Option<usize>>,
    /// When `Some(n)`, the connection is closed right after the nth UID
    /// FETCH completes successfully (one-shot: the setting clears itself
    /// when it fires). Simulates the network — or the whole client
    /// process — dying between two fetch batches.
    drop_after_fetch_once: Mutex<Option<usize>>,
    /// 1-based ordinal of each UID FETCH command seen.
    fetch_ordinal: Mutex<usize>,
    /// When `Some(n)`, the nth UID FETCH stalls: its handler thread parks
    /// on `stall_gate` (closed) until `release_stall()` opens it — a
    /// network wait of unbounded length, for tests that prove what the
    /// client does (and does not hold) while the server is silent.
    stall_fetch_once: Mutex<Option<usize>>,
    /// Gate the stalled fetch waits on; closed until `release_stall()`.
    stall_gate: (Mutex<bool>, Condvar),
    /// When `Some((user, token))`, the server only accepts
    /// `AUTHENTICATE XOAUTH2` with exactly that user/token pair and
    /// refuses `LOGIN` — the XOAUTH2-only mode the OAuth sync tests use.
    xoauth2: Mutex<Option<(String, String)>>,
    /// When `Some(uid)`, the next `UID FETCH` that would deliver that uid
    /// silently skips it (and clears the knob — one shot). The uid stays
    /// in the mailbox, so `UID SEARCH` keeps returning it: the scripted
    /// "searched but never delivered" case (a message expunged between
    /// the two commands, which RFC 3501 lets the server answer by simply
    /// omitting it from the FETCH response).
    hidden_uid: Mutex<Option<u32>>,
}

/// Handle to the running mock server.
pub struct MockImap {
    pub addr: SocketAddr,
    mailboxes: Arc<Mutex<BTreeMap<String, MockMailbox>>>,
    log: Arc<Mutex<ServerLog>>,
    knobs: Arc<Knobs>,
}

impl MockImap {
    /// Start the server on an ephemeral loopback port.
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock imap");
        let addr = listener.local_addr().expect("mock addr");
        let mailboxes = Arc::new(Mutex::new(BTreeMap::new()));
        let log = Arc::new(Mutex::new(ServerLog::default()));
        let knobs = Arc::new(Knobs {
            fail_fetch_once: Mutex::new(None),
            drop_after_fetch_once: Mutex::new(None),
            fetch_ordinal: Mutex::new(0),
            stall_fetch_once: Mutex::new(None),
            stall_gate: (Mutex::new(false), Condvar::new()),
            xoauth2: Mutex::new(None),
            hidden_uid: Mutex::new(None),
        });
        {
            let mailboxes = Arc::clone(&mailboxes);
            let log = Arc::clone(&log);
            let knobs = Arc::clone(&knobs);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let mailboxes = Arc::clone(&mailboxes);
                    let log = Arc::clone(&log);
                    let knobs = Arc::clone(&knobs);
                    std::thread::spawn(move || {
                        if let Err(e) = serve(stream, &mailboxes, &log, &knobs) {
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
            knobs,
        }
    }

    /// Switch the server into XOAUTH2-only mode: `LOGIN` is refused and
    /// `AUTHENTICATE XOAUTH2` succeeds only for exactly this user/token
    /// pair (the SASL string must be `user={user}\x01auth=Bearer
    /// {token}\x01\x01`).
    pub fn set_xoauth2(&self, user: &str, access_token: &str) {
        *self.knobs.xoauth2.lock().unwrap() = Some((user.to_string(), access_token.to_string()));
    }

    /// Answer exactly the nth UID FETCH command seen on this server (across
    /// all connections, 1-based) with a tagged `BAD`, simulating a server
    /// that breaks mid-folder. `None` (the default) serves normally.
    pub fn fail_uid_fetch_once(&self, ordinal: Option<usize>) {
        *self.knobs.fail_fetch_once.lock().unwrap() = ordinal;
    }

    /// Close the connection immediately after the nth UID FETCH (across
    /// all connections, 1-based) completes successfully — the fetch
    /// response itself is fully delivered first. One-shot: the setting
    /// clears itself once it fires, so retried connections serve
    /// normally. `None` (the default) never drops.
    pub fn drop_after_uid_fetch_once(&self, ordinal: Option<usize>) {
        *self.knobs.drop_after_fetch_once.lock().unwrap() = ordinal;
    }

    /// Make exactly the nth UID FETCH (across all connections, 1-based)
    /// stall: the server parks before answering and stays silent until
    /// [`MockImap::release_stall`] — an arbitrarily long network wait the
    /// test controls. One-shot like the other knobs.
    pub fn stall_uid_fetch_once(&self, ordinal: usize) {
        *self.knobs.stall_fetch_once.lock().unwrap() = Some(ordinal);
    }

    /// Let a stalled fetch (see [`MockImap::stall_uid_fetch_once`])
    /// proceed.
    pub fn release_stall(&self) {
        let (gate, signal) = &self.knobs.stall_gate;
        *gate.lock().unwrap() = true;
        signal.notify_all();
    }

    /// Make exactly the next `UID FETCH` of `uid` skip delivering it —
    /// a successfully completed FETCH response that simply lacks the
    /// searched uid (one shot). Used by the missing-fetch scenarios.
    pub fn hide_uid_from_fetch_once(&self, uid: u32) {
        *self.knobs.hidden_uid.lock().unwrap() = Some(uid);
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
    knobs: &Knobs,
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
                if knobs.xoauth2.lock().unwrap().is_some() {
                    write_line(
                        &mut writer,
                        format!("{tag} NO LOGIN disabled on this server; use AUTHENTICATE XOAUTH2")
                            .as_bytes(),
                    )?;
                } else {
                    write_line(&mut writer, format!("{tag} OK LOGIN completed").as_bytes())?;
                }
            }
            "AUTHENTICATE" => {
                let mechanism = rest.split(' ').next().unwrap_or_default();
                if mechanism.eq_ignore_ascii_case("XOAUTH2") {
                    // Server-first empty challenge, then the base64 SASL
                    // initial response on the next line (the exchange the
                    // imap crate performs).
                    write_line(&mut writer, b"+ ")?;
                    let mut response = String::new();
                    reader.read_line(&mut response)?;
                    let decoded = base64::engine::general_purpose::STANDARD
                        .decode(response.trim())
                        .ok()
                        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                        .unwrap_or_else(|| format!("<undecodable: {}>", response.trim()));
                    log.lock().unwrap().authentications.push(decoded.clone());
                    let expected = knobs
                        .xoauth2
                        .lock()
                        .unwrap()
                        .clone()
                        .map(|(user, token)| vault_core::oauth::xoauth2_sasl(&user, &token));
                    match expected {
                        Some(expected) if decoded == expected => write_line(
                            &mut writer,
                            format!("{tag} OK AUTHENTICATE completed").as_bytes(),
                        )?,
                        Some(_) => write_line(
                            &mut writer,
                            format!("{tag} NO [AUTHENTICATIONFAILED] invalid credentials")
                                .as_bytes(),
                        )?,
                        None => write_line(
                            &mut writer,
                            format!("{tag} BAD AUTHENTICATE not enabled on this server").as_bytes(),
                        )?,
                    }
                } else {
                    write_line(
                        &mut writer,
                        format!("{tag} NO unsupported authentication mechanism: {mechanism}")
                            .as_bytes(),
                    )?;
                }
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
                        let ordinal = {
                            let mut n = knobs.fetch_ordinal.lock().unwrap();
                            *n += 1;
                            *n
                        };
                        let should_fail = {
                            let threshold = knobs.fail_fetch_once.lock().unwrap();
                            threshold.is_some_and(|n| ordinal == n)
                        };
                        if should_fail {
                            write_line(
                                &mut writer,
                                format!("{tag} BAD simulated mid-folder fetch failure").as_bytes(),
                            )?;
                            continue;
                        }
                        // A stalled fetch parks on the gate (a network the
                        // test froze); the client's read blocks exactly
                        // like a silent server.
                        let should_stall = {
                            let mut knob = knobs.stall_fetch_once.lock().unwrap();
                            match *knob {
                                Some(n) if n == ordinal => {
                                    *knob = None;
                                    true
                                }
                                _ => false,
                            }
                        };
                        if should_stall {
                            let (gate, signal) = &knobs.stall_gate;
                            let mut open = gate.lock().unwrap();
                            while !*open {
                                open = signal.wait(open).unwrap();
                            }
                        }
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
                                // The scripted missing-fetch: this uid is
                                // silently omitted from this response.
                                let hidden = {
                                    let mut knob = knobs.hidden_uid.lock().unwrap();
                                    match *knob {
                                        Some(hidden) if hidden == uid => {
                                            *knob = None;
                                            true
                                        }
                                        _ => false,
                                    }
                                };
                                if hidden {
                                    continue;
                                }
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
                        // One-shot connection drop after a fully delivered
                        // fetch: the next client command finds a closed
                        // socket, exactly like a dead network link.
                        let should_drop = {
                            let mut knob = knobs.drop_after_fetch_once.lock().unwrap();
                            match *knob {
                                Some(n) if n == ordinal => {
                                    *knob = None;
                                    true
                                }
                                _ => false,
                            }
                        };
                        if should_drop {
                            return Ok(());
                        }
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
