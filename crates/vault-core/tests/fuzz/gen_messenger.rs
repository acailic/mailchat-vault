//! Structured Messenger DYI export-tree generator: whole
//! `messages/inbox/<thread>/message_*.json` trees of raw JSON text (not
//! `serde_json::Value`s), with a controlled rate of damage on top of
//! well-formed exports — a broken `"not json"` part file in every fifth
//! seed. Byte-level damage on top is the harness's `mutate` pass, as
//! for every other format.

use super::rng::Rng;

/// The message mix every thread cycles through: text, photos, text,
/// audio_files, reactions-only. The last slot has neither `content`
/// nor a media field — the source's skippable entry.
const MIX_CYCLE: usize = 5;

/// Every 7th message repeats its predecessor byte for byte: identical
/// sender and timestamp (and body), so the id hash collides and the
/// occurrence counter must still hand out distinct ids.
const DUPLICATE_EVERY: usize = 7;

/// One generated DYI tree: `(relative path, raw bytes)` pairs under an
/// implicit export root — `messages/inbox/<thread>/message_1.json`
/// per thread (1-3 threads), plus, in every fifth seed, a broken
/// `"not json"` part file inside the first thread. Thread folders are
/// `thread_{1000 + index}` — the `_<digits>` suffix the thread-key
/// logic reads — and the first pair is always a well-formed
/// `message_1.json` fit for the detection probe.
pub fn export(rng: &mut Rng) -> Vec<(String, Vec<u8>)> {
    let broken_part = rng.below(5) == 0;
    let mut files = Vec::new();
    for t in 0..rng.range(1, 3) {
        let id = 1000 + t as u64;
        let folder = format!("thread_{id}");
        let messages = rng.range(1, 12);
        files.push((
            format!("messages/inbox/{folder}/message_1.json"),
            thread_json(id, &folder, messages).into_bytes(),
        ));
    }
    if broken_part {
        // The truncated part export tools sometimes write: one `Failed`
        // item names it and the rest of the tree imports — never a
        // panic, never a dead pick.
        files.push((
            "messages/inbox/thread_1000/message_2.json".to_string(),
            b"not json".to_vec(),
        ));
    }
    files
}

/// One thread file: participants, a title, a `thread_path` with the
/// numeric suffix, and `messages` entries per the mix cycle —
/// timestamps strictly increasing, except the every-7th byte-identical
/// repeat of its neighbor.
fn thread_json(id: u64, folder: &str, messages: usize) -> String {
    let base_ms = 1_762_981_200_000 + id * 86_400_000;
    let mut entries: Vec<String> = Vec::with_capacity(messages);
    for i in 0..messages {
        let entry = if i % DUPLICATE_EVERY == DUPLICATE_EVERY - 1 && !entries.is_empty() {
            entries[i - 1].clone()
        } else {
            let sender = if i % 2 == 0 { "Ana" } else { "Marko" };
            let ts = base_ms + (i as u64) * 60_000;
            match i % MIX_CYCLE {
                0 | 2 => format!(
                    "{{\"sender_name\":\"{sender}\",\"timestamp_ms\":{ts},\
                     \"content\":\"zdravo iz threada {id} broj {i}\"}}"
                ),
                1 => format!(
                    "{{\"sender_name\":\"{sender}\",\"timestamp_ms\":{ts},\
                     \"photos\":[{{\"uri\":\"messages/inbox/{folder}/photos/slika_{i}.jpg\",\
                     \"creation_timestamp\":{}}}]}}",
                    ts / 1000
                ),
                3 => format!(
                    "{{\"sender_name\":\"{sender}\",\"timestamp_ms\":{ts},\
                     \"audio_files\":[{{\"uri\":\"messages/inbox/{folder}/audio/audioclip_{i}.mp4\",\
                     \"creation_timestamp\":{}}}]}}",
                    ts / 1000
                ),
                _ => format!(
                    "{{\"sender_name\":\"{sender}\",\"timestamp_ms\":{ts},\
                     \"reactions\":[{{\"reaction\":\"❤\",\"actor\":\"Ana\"}}]}}"
                ),
            }
        };
        entries.push(entry);
    }
    format!(
        "{{\"participants\":[{{\"name\":\"Ana\"}},{{\"name\":\"Marko\"}}],\
         \"title\":\"thread {id}\",\"thread_path\":\"inbox/{folder}\",\
         \"is_still_participant\":true,\"messages\":[{}]}}",
        entries.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vault_core::ingest::ingest;
    use vault_core::messenger::{MessengerSource, MESSENGER_SOURCE};
    use vault_core::search::build_match_query;
    use vault_core::storage::Storage;

    #[test]
    fn generates_deterministic_variety() {
        let a = export(&mut Rng::new(1));
        let b = export(&mut Rng::new(1));
        assert_eq!(a, b);
        assert_ne!(a, export(&mut Rng::new(2)));
    }

    #[test]
    fn bounded_size() {
        for seed in 0..500u64 {
            let total: usize = export(&mut Rng::new(seed))
                .iter()
                .map(|(_, bytes)| bytes.len())
                .sum();
            assert!(total < 5_000_000, "seed {seed}: {total} bytes");
        }
    }

    #[test]
    fn broken_part_appears_in_roughly_one_seed_in_five() {
        let mut with = 0;
        for seed in 0..50u64 {
            let files = export(&mut Rng::new(seed));
            let broken: Vec<&String> = files
                .iter()
                .filter(|(_, b)| b == b"not json")
                .map(|(p, _)| p)
                .collect();
            if broken.is_empty() {
                continue;
            }
            with += 1;
            // Exactly one broken part, a message_*.json inside the
            // first thread's folder.
            assert_eq!(broken.len(), 1, "seed {seed}");
            assert!(
                broken[0].starts_with("messages/inbox/thread_1000/message_"),
                "seed {seed}: {}",
                broken[0]
            );
        }
        assert!(
            with > 0 && with < 50,
            "roughly one in five seeds: {with}/50"
        );
    }

    #[test]
    fn thread_rules_hold_across_seeds() {
        // Folder names carry the `_<digits>` suffix the thread-key logic
        // needs, media uris are root-relative and name their own thread,
        // and timestamps never decrease (with an equal adjacent pair —
        // the occurrence-counter case — whenever a thread reaches seven
        // messages).
        for seed in 0..100u64 {
            for (path, bytes) in export(&mut Rng::new(seed)) {
                let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                    continue; // the broken part
                };
                assert!(path.starts_with("messages/inbox/thread_"), "{path}");
                let thread = path
                    .rsplit('/')
                    .nth(1)
                    .unwrap_or_else(|| panic!("{path}: no thread folder"));
                assert_eq!(v["thread_path"], format!("inbox/{thread}"), "{path}");
                let suffix = thread.rsplit_once('_').unwrap().1;
                assert!(suffix.bytes().all(|b| b.is_ascii_digit()), "{thread}");
                let messages = v["messages"].as_array().unwrap();
                let mut stamps: Vec<i64> = Vec::new();
                let mut equal_pair = 0;
                for (i, entry) in messages.iter().enumerate() {
                    let ts = entry["timestamp_ms"].as_i64().unwrap();
                    if let Some(&prev) = stamps.last() {
                        assert!(ts >= prev, "{path} entry {i}: timestamps decreased");
                        if ts == prev {
                            equal_pair += 1;
                        }
                    }
                    stamps.push(ts);
                    for field in ["photos", "audio_files"] {
                        for item in entry[field].as_array().into_iter().flatten() {
                            let uri = item["uri"].as_str().unwrap();
                            assert!(
                                uri.starts_with(&format!("messages/inbox/{thread}/")),
                                "{uri}"
                            );
                        }
                    }
                }
                if messages.len() >= 7 {
                    assert!(equal_pair > 0, "{path}: no equal-timestamp pair");
                }
            }
        }
    }

    #[test]
    fn ingest_is_idempotent_counts_and_searchable_across_200_seeds() {
        for seed in 0..200u64 {
            let files = export(&mut Rng::new(seed));
            let dir = tempfile::tempdir().unwrap();
            for (path, bytes) in &files {
                let full = dir.path().join(path);
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(&full, bytes).unwrap();
            }
            let source = MessengerSource::new(dir.path());
            let storage = Storage::open_in_memory().unwrap();
            let broken = files
                .iter()
                .any(|(_, b)| serde_json::from_slice::<serde_json::Value>(b).is_err());
            // Skip-and-record: a broken part is one counted failure
            // naming the file; the healthy threads and parts import.
            let expected = expected_inserted(&files);
            let first = ingest(&storage, &source).unwrap();
            assert_eq!(first.source, MESSENGER_SOURCE, "seed {seed}");
            assert_eq!(first.inserted, expected, "seed {seed}");
            assert_eq!(first.failed, u64::from(broken), "seed {seed}");
            let again = ingest(&storage, &source).unwrap();
            assert_eq!(again.inserted, 0, "seed {seed}");
            assert_eq!(again.skipped, expected, "seed {seed}");
            assert_eq!(storage.stats().unwrap().total, expected, "seed {seed}");
            assert!(
                !storage
                    .search_filtered(&build_match_query("zdravo"), None, None, None, false, 10)
                    .unwrap()
                    .is_empty(),
                "seed {seed}: known body string not searchable"
            );
        }
    }

    /// Independent oracle for the mix rules: an entry imports when it
    /// has a body or a non-empty media array (the reactions-only mix
    /// slot has neither). Every-7th byte-identical repeats count too —
    /// distinct generated ids, both stored. Unparseable files (the
    /// broken part) contribute nothing and count as the run's one
    /// failure instead.
    fn expected_inserted(files: &[(String, Vec<u8>)]) -> u64 {
        files
            .iter()
            .filter_map(|(_, bytes)| {
                let v = serde_json::from_slice::<serde_json::Value>(bytes).ok()?;
                v["messages"].as_array().map(|entries| {
                    entries
                        .iter()
                        .filter(|e| {
                            let body = e["content"].as_str().is_some_and(|s| !s.is_empty());
                            let media = [
                                "photos",
                                "videos",
                                "audio_files",
                                "files",
                                "gifs",
                                "sticker",
                            ]
                            .iter()
                            .any(|f| e[*f].as_array().is_some_and(|a| !a.is_empty()));
                            body || media
                        })
                        .count() as u64
                })
            })
            .sum()
    }
}
