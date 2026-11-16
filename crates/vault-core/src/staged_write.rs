//! Staged file publication for exports (U06): never truncate a valid
//! user file before the replacement is complete.
//!
//! The streaming export commands used to open the chosen destination
//! with create/truncate before streaming began, so a serialization,
//! database, write, or flush failure mid-stream replaced a previous
//! export with an empty or partial file. [`staged_write`] inverts that:
//! the export streams into a private `.{stem}-{pid}.part` sibling of the
//! destination, and only a fully written, flushed, and fsynced file is
//! renamed over the destination (`fs::rename` replaces an existing
//! destination on every supported platform, Windows included; a
//! destination that cannot be replaced — e.g. locked on Windows —
//! surfaces as an error with the temp removed and the prior file
//! intact). Every failure path removes the temp sibling through the
//! [`TempPath`] drop guard, so an interrupted or failed export leaves
//! the prior destination byte-for-byte unchanged and nothing beside it.
//! The pattern is the one proven by [`crate::backup`]'s container
//! writer (temp sibling, fsync, rename) and [`crate::safe_migration`].

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Stream an export into a private temp sibling of `dest` and publish it
/// over `dest` only after the write, flush, and fsync all succeed.
///
/// `write` receives the buffered staged file; when it returns `Ok`, the
/// staged bytes are flushed to disk, fsynced, and renamed onto `dest`.
/// When it returns `Err` — a mid-stream serialization, database, or I/O
/// failure — the staged temp is removed and a pre-existing `dest` is
/// left byte-for-byte unchanged (a `dest` that did not exist never
/// appears).
pub fn staged_write<T, F>(dest: &Path, write: F) -> Result<T>
where
    F: FnOnce(&mut BufWriter<File>) -> Result<T>,
{
    stage_file(dest, |out| write(out).map(Some))
        .map(|value| value.expect("stage_file publishes when the writer succeeds"))
}

/// Like [`staged_write`], for writers that can decline to publish after
/// looking at the archive: `Ok(None)` from `write` aborts with the staged
/// temp removed and the destination untouched (the whole-conversation
/// export's anchor-gone path), while `Ok(Some(_))` publishes as in
/// [`staged_write`].
pub fn staged_write_opt<T, F>(dest: &Path, write: F) -> Result<Option<T>>
where
    F: FnOnce(&mut BufWriter<File>) -> Result<Option<T>>,
{
    stage_file(dest, write)
}

/// Create the staged sibling of `dest` and run the stage → publish
/// pipeline over it.
fn stage_file<T, F>(dest: &Path, write: F) -> Result<Option<T>>
where
    F: FnOnce(&mut BufWriter<File>) -> Result<Option<T>>,
{
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export".to_string());
    let tmp: PathBuf = parent.join(format!(".{stem}-{}.part", std::process::id()));
    let file = File::create(&tmp)?;
    let mut out = BufWriter::new(file);
    stage_and_publish(&tmp, dest, &mut out, write)
}

/// The stage → publish pipeline: run `write`, then (on `Ok(Some)`)
/// flush, fsync, and rename the staged file onto `dest`.
///
/// Generic over the writer so the failure-injection tests below can
/// drive real write and flush failures through this exact code path
/// without filling a real disk.
fn stage_and_publish<W, T, F>(tmp: &Path, dest: &Path, out: &mut W, write: F) -> Result<Option<T>>
where
    W: Write,
    F: FnOnce(&mut W) -> Result<Option<T>>,
{
    let _guard = TempPath(tmp);
    match write(out)? {
        Some(value) => {
            out.flush()?;
            fsync_file(tmp)?;
            fs::rename(tmp, dest)?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

/// fsync a finished staged file by path (the writer's buffered bytes
/// were already pushed into it by the flush; the same idiom as
/// [`crate::backup`]'s container publish).
fn fsync_file(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(Error::Io)
}

/// A temporary path removed on drop (best effort — a hard crash's
/// leftovers are dot-prefixed `.part` files, never mistaken for real
/// outputs).
struct TempPath<'a>(&'a Path);

impl Drop for TempPath<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use tempfile::TempDir;

    /// A writer that succeeds for its first `0` counted writes and fails
    /// every later one — the mid-stream disk/pipe failure injection.
    struct WriteFailsAfter(usize);

    impl Write for WriteFailsAfter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.0 == 0 {
                return Err(io::Error::other("injected mid-stream write failure"));
            }
            self.0 -= 1;
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A writer whose writes succeed and whose flush fails — the
    /// flush-time failure injection.
    struct FlushFails;

    impl Write for FlushFails {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("injected flush failure"))
        }
    }

    /// Every `.part` artifact left beside the destinations of `dir`.
    fn part_artifacts(dir: &TempDir) -> Vec<String> {
        fs::read_dir(dir.path())
            .expect("read tempdir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".part"))
            .collect()
    }

    #[test]
    fn success_replaces_existing_destination_with_identical_bytes() {
        let dir = TempDir::new().expect("tempdir");
        let dest = dir.path().join("results.csv");
        fs::write(&dest, "the previous export").expect("seed prior destination");
        let chunks = ["header row\n", "row one\n", "row two\n"];

        let counted = staged_write(&dest, |out| {
            let mut n = 0u64;
            for chunk in chunks {
                out.write_all(chunk.as_bytes()).map_err(Error::from)?;
                n += 1;
            }
            Ok(n)
        })
        .expect("staged export succeeds");

        assert_eq!(counted, chunks.len() as u64);
        // Byte-for-byte what a direct write of the same export produces.
        let direct = dir.path().join("direct.csv");
        fs::write(&direct, chunks.concat()).expect("direct write");
        assert_eq!(
            fs::read(&dest).expect("read destination"),
            fs::read(&direct).expect("read direct"),
            "published export must equal a direct write"
        );
        assert!(part_artifacts(&dir).is_empty(), "no .part may remain");
    }

    #[test]
    fn success_to_a_new_path_creates_the_destination() {
        let dir = TempDir::new().expect("tempdir");
        let dest = dir.path().join("first-export.json");
        staged_write(&dest, |out| out.write_all(b"[]\n").map_err(Error::from))
            .expect("staged export succeeds");
        assert_eq!(fs::read(&dest).expect("read destination"), b"[]\n");
        assert!(part_artifacts(&dir).is_empty());
    }

    #[test]
    fn mid_stream_failure_keeps_existing_destination_byte_for_byte() {
        let dir = TempDir::new().expect("tempdir");
        let dest = dir.path().join("search.csv");
        let prior = b"previous complete export".as_slice();
        fs::write(&dest, prior).expect("seed prior destination");

        let err = staged_write(&dest, |out| -> Result<()> {
            out.write_all(b"partial rows").map_err(Error::from)?;
            Err(Error::InvalidExport("injected mid-stream failure".into()))
        })
        .expect_err("failure must surface");

        assert!(
            err.to_string().contains("injected mid-stream failure"),
            "error must carry the cause: {err}"
        );
        assert_eq!(fs::read(&dest).expect("read destination"), prior);
        assert!(part_artifacts(&dir).is_empty(), "temp must be cleaned up");
    }

    #[test]
    fn failure_to_a_new_path_leaves_no_destination() {
        let dir = TempDir::new().expect("tempdir");
        let dest = dir.path().join("never-created.csv");
        staged_write(&dest, |out| -> Result<()> {
            out.write_all(b"partial").map_err(Error::from)?;
            Err(Error::InvalidExport("injected mid-stream failure".into()))
        })
        .expect_err("failure must surface");
        assert!(!dest.exists(), "no misleading final export file");
        assert!(part_artifacts(&dir).is_empty());
    }

    #[test]
    fn writer_write_failure_after_some_rows_keeps_destination() {
        // The write-level (not closure-level) failure injection: the
        // writer errors after the first buffered chunk, mid-export.
        let dir = TempDir::new().expect("tempdir");
        let dest = dir.path().join("chat.csv");
        let prior = b"the previous chat export".as_slice();
        fs::write(&dest, prior).expect("seed prior destination");
        let tmp = dir.path().join(".chat-fails.part");
        fs::write(&tmp, b"").expect("stage temp file");

        let mut writer = WriteFailsAfter(1);
        let err = stage_and_publish(&tmp, &dest, &mut writer, |w| {
            w.write_all(b"first chunk").map_err(Error::from)?;
            w.write_all(b"second chunk").map_err(Error::from)?;
            Ok(Some(()))
        })
        .expect_err("write failure must surface");

        assert!(
            err.to_string()
                .contains("injected mid-stream write failure"),
            "error must carry the cause: {err}"
        );
        assert_eq!(fs::read(&dest).expect("read destination"), prior);
        assert!(!tmp.exists(), "temp must be cleaned up");
        assert!(part_artifacts(&dir).is_empty());
    }

    #[test]
    fn flush_failure_keeps_destination_and_cleans_temp() {
        let dir = TempDir::new().expect("tempdir");
        let dest = dir.path().join("flush.csv");
        let prior = b"the previous export".as_slice();
        fs::write(&dest, prior).expect("seed prior destination");
        let tmp = dir.path().join(".flush-fails.part");
        fs::write(&tmp, b"").expect("stage temp file");

        let mut writer = FlushFails;
        let err = stage_and_publish(&tmp, &dest, &mut writer, |w| {
            w.write_all(b"complete export bytes").map_err(Error::from)?;
            Ok(Some(()))
        })
        .expect_err("flush failure must surface");

        assert!(
            err.to_string().contains("injected flush failure"),
            "error must carry the cause: {err}"
        );
        assert_eq!(fs::read(&dest).expect("read destination"), prior);
        assert!(!tmp.exists(), "temp must be cleaned up");
        assert!(part_artifacts(&dir).is_empty());
    }

    #[test]
    fn rename_failure_is_reported_and_preserves_the_prior_destination() {
        // The final-replacement failure injection: the destination is a
        // directory, so the publish rename cannot replace it.
        let dir = TempDir::new().expect("tempdir");
        let dest = dir.path().join("occupied");
        fs::create_dir(&dest).expect("destination directory");

        let err = staged_write(&dest, |out| {
            out.write_all(b"complete export").map_err(Error::from)
        })
        .expect_err("rename over a directory must fail");

        assert!(
            matches!(err, Error::Io(_)),
            "rename failure surfaces as an io error: {err}"
        );
        assert!(dest.is_dir(), "the prior destination must be untouched");
        assert!(part_artifacts(&dir).is_empty(), "temp must be cleaned up");
    }

    #[test]
    fn declining_to_publish_keeps_destination_and_cleans_temp() {
        let dir = TempDir::new().expect("tempdir");
        let dest = dir.path().join("conversation.csv");
        let prior = b"the previous export".as_slice();
        fs::write(&dest, prior).expect("seed prior destination");

        let outcome = staged_write_opt(&dest, |out| -> Result<Option<()>> {
            out.write_all(b"").map_err(Error::from)?;
            Ok(None)
        })
        .expect("declining is not an error");

        assert_eq!(outcome, None);
        assert_eq!(fs::read(&dest).expect("read destination"), prior);
        assert!(part_artifacts(&dir).is_empty());
    }
}
