//! Read and stream the per-VM serial console log file.
//!
//! Cloud Hypervisor's `serial.mode = "File"` makes the VMM append every byte
//! the guest writes to `/dev/console` (cloud-init banner, kernel messages,
//! systemd journal when redirected, app stdout when configured) to a single
//! file. This module turns that file into the line-oriented stream the
//! `RuntimeLifecycle` trait expects:
//!
//! - [`read_lines`] — synchronous one-shot read with `tail` (last N lines)
//!   and `since` (drop lines older than N seconds, best-effort: the console
//!   has no native timestamps so we fall back to file mtime windowing).
//! - [`stream_lines`] — async streaming reader that follows the file as CH
//!   appends to it, equivalent to `tail -f`.
//!
//! Lines are returned raw — callers (`get_logs` / `stream_logs` in
//! `lifecycle.rs`) wrap them in a `Log` with `classify_log` / `extract_date`.

use futures::stream::{self, Stream, StreamExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader, SeekFrom};

/// Read the console log for a single instance into a vector of lines.
///
/// `tail`: parsed as `"<n>"` (last N lines) or `"all"` / unset for the whole
/// file. Anything that doesn't parse falls back to the whole file.
///
/// `since`: number of seconds; drops lines whose age (estimated from the
/// file mtime relative to "now", with all lines treated as appended at the
/// same moment for now) is older. Approximate by design — the serial
/// console has no native timestamps.
pub(crate) async fn read_lines(path: &Path, tail: Option<&str>, since: Option<i32>) -> Vec<String> {
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };

    // The serial console is not guaranteed to be UTF-8 (early kernel output
    // may carry stray bytes). Lossy conversion keeps us robust.
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();

    if let Some(n_str) = tail {
        if n_str != "all"
            && let Ok(n) = n_str.parse::<usize>()
            && lines.len() > n
        {
            lines = lines.split_off(lines.len() - n);
        }
    }

    if let Some(secs) = since
        && secs > 0
    {
        // Best-effort: if the whole file's mtime is older than `secs` ago,
        // drop everything. We do not try to date individual lines because
        // the console output has no built-in timestamps.
        if let Ok(meta) = tokio::fs::metadata(path).await
            && let Ok(mtime) = meta.modified()
            && let Ok(elapsed) = mtime.elapsed()
            && elapsed.as_secs() as i32 > secs
        {
            return Vec::new();
        }
    }

    lines
}

/// Stream new lines as Cloud Hypervisor appends them to the console log.
///
/// The implementation polls the file on a short interval rather than using
/// inotify because the test surface is simpler and the volume of console
/// output is low (kernel + cloud-init + occasional app stderr — far below
/// what would warrant edge-triggered I/O).
///
/// On startup, we honor `tail` by seeking to the last N lines before
/// streaming new ones. `since` is applied identically to [`read_lines`]
/// (best-effort: if the file's mtime is older than the cutoff, we start
/// from end-of-file).
pub(crate) async fn stream_lines(
    path: PathBuf,
    tail: Option<String>,
    since: Option<i32>,
) -> Pin<Box<dyn Stream<Item = String> + Send>> {
    // Replay tail synchronously, then poll for growth.
    let initial = read_lines(&path, tail.as_deref(), since).await;
    let initial_stream = stream::iter(initial);

    // State carried by the `unfold` follower:
    // - `path`: the file we're tailing
    // - `last_size`: how many bytes we've already streamed
    // - `carry`: leftover bytes from a partial line at EOF that should be
    //   prepended to the next read once a newline arrives
    // - `pending`: lines we've parsed but not yet yielded (so each `next`
    //   call returns one line)
    struct FollowState {
        path: PathBuf,
        last_size: u64,
        carry: String,
        pending: std::collections::VecDeque<String>,
    }

    let init_size = tokio::fs::metadata(&path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let state = FollowState {
        path,
        last_size: init_size,
        carry: String::new(),
        pending: std::collections::VecDeque::new(),
    };

    let follow = stream::unfold(state, |mut s| async move {
        loop {
            // Drain anything we've already buffered.
            if let Some(line) = s.pending.pop_front() {
                return Some((line, s));
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            let size = match tokio::fs::metadata(&s.path).await {
                Ok(m) => m.len(),
                Err(_) => continue,
            };

            // Truncation (e.g. operator deleted the file mid-stream): reset.
            if size < s.last_size {
                s.last_size = 0;
                s.carry.clear();
            }
            if size == s.last_size {
                continue;
            }

            let mut file = match tokio::fs::File::open(&s.path).await {
                Ok(f) => f,
                Err(_) => continue,
            };
            if file.seek(SeekFrom::Start(s.last_size)).await.is_err() {
                continue;
            }

            let mut reader = BufReader::new(file);
            let mut buf = String::new();
            loop {
                buf.clear();
                let n = match reader.read_line(&mut buf).await {
                    Ok(n) => n,
                    Err(_) => break,
                };
                if n == 0 {
                    break;
                }
                if buf.ends_with('\n') {
                    let mut full = std::mem::take(&mut s.carry);
                    full.push_str(buf.trim_end_matches('\n'));
                    s.pending.push_back(full);
                } else {
                    // Partial line at EOF — wait for more bytes next tick.
                    s.carry.push_str(&buf);
                }
            }
            s.last_size = size;
        }
    });

    Box::pin(initial_stream.chain(follow))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn scratch_file(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "ring-console-{}-{}-{}.log",
            label,
            std::process::id(),
            nanos
        ))
    }

    #[tokio::test]
    async fn read_lines_returns_all_when_no_tail() {
        let path = scratch_file("all");
        tokio::fs::write(&path, b"line-1\nline-2\nline-3\n")
            .await
            .unwrap();
        let lines = read_lines(&path, None, None).await;
        assert_eq!(lines, vec!["line-1", "line-2", "line-3"]);
        tokio::fs::remove_file(&path).await.ok();
    }

    #[tokio::test]
    async fn read_lines_respects_tail_n() {
        let path = scratch_file("tail-n");
        tokio::fs::write(&path, b"a\nb\nc\nd\ne\n").await.unwrap();
        let lines = read_lines(&path, Some("2"), None).await;
        assert_eq!(lines, vec!["d", "e"]);
        tokio::fs::remove_file(&path).await.ok();
    }

    #[tokio::test]
    async fn read_lines_tail_all_returns_everything() {
        let path = scratch_file("tail-all");
        tokio::fs::write(&path, b"a\nb\nc\n").await.unwrap();
        let lines = read_lines(&path, Some("all"), None).await;
        assert_eq!(lines, vec!["a", "b", "c"]);
        tokio::fs::remove_file(&path).await.ok();
    }

    #[tokio::test]
    async fn read_lines_handles_missing_file() {
        let path = scratch_file("missing");
        let lines = read_lines(&path, None, None).await;
        assert!(lines.is_empty());
    }

    #[tokio::test]
    async fn read_lines_lossy_decodes_non_utf8() {
        let path = scratch_file("non-utf8");
        // 0xff is invalid UTF-8 — must not panic, must produce a lossy line.
        tokio::fs::write(&path, b"hello\n\xffworld\n")
            .await
            .unwrap();
        let lines = read_lines(&path, None, None).await;
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "hello");
        assert!(lines[1].contains("world"));
        tokio::fs::remove_file(&path).await.ok();
    }

    #[tokio::test]
    async fn stream_lines_replays_tail_then_follows_appends() {
        let path = scratch_file("follow");
        tokio::fs::write(&path, b"hello\n").await.unwrap();

        let mut s = stream_lines(path.clone(), None, None).await;
        // First: initial replay.
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), s.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first, "hello");

        // Now append two more lines and confirm the follower picks them up.
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        use tokio::io::AsyncWriteExt;
        f.write_all(b"world\nbye\n").await.unwrap();
        drop(f);

        let second = tokio::time::timeout(std::time::Duration::from_secs(3), s.next())
            .await
            .expect("stream should yield within 3s after append")
            .unwrap();
        assert_eq!(second, "world");

        let third = tokio::time::timeout(std::time::Duration::from_secs(3), s.next())
            .await
            .expect("stream should yield third line")
            .unwrap();
        assert_eq!(third, "bye");

        drop(s);
        tokio::fs::remove_file(&path).await.ok();
    }
}
