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
use tracing::{debug, warn};

/// Build the ordered list of files to read for a given live log path: oldest
/// rotated backup first (`<path>.N`), then `.N-1`, ..., `.1`, then the live
/// `<path>`. Missing files are skipped silently — rotations may be sparse if
/// the VM has not produced enough output to fill every slot yet.
fn rotated_files_in_read_order(path: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    // Discover up to a generous upper bound; in practice the configured
    // `max_console_log_backups` caps this at 3-10.
    let mut idx = 1u32;
    let mut backups: Vec<(u32, PathBuf)> = Vec::new();
    while idx < 1000 {
        let candidate = backup_path(path, idx);
        if !candidate.exists() {
            break;
        }
        backups.push((idx, candidate));
        idx += 1;
    }
    // Sort descending (oldest first when reading: highest index is oldest).
    backups.sort_by(|a, b| b.0.cmp(&a.0));
    files.extend(backups.into_iter().map(|(_, p)| p));
    files.push(path.to_path_buf());
    files
}

fn backup_path(path: &Path, idx: u32) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(format!(".{}", idx));
    PathBuf::from(s)
}

/// Read the console log for a single instance into a vector of lines.
///
/// `tail`: parsed as `"<n>"` (last N lines) or `"all"` / unset for the whole
/// file. Anything that doesn't parse falls back to the whole file.
///
/// `since`: number of seconds; drops lines whose age (estimated from the
/// file mtime relative to "now", with all lines treated as appended at the
/// same moment for now) is older. Approximate by design — the serial
/// console has no native timestamps.
///
/// Reads through any rotated backups (`<path>.1`, `.2`, ...) so a `--tail N`
/// that spans a rotation boundary still returns the requested history.
pub(crate) async fn read_lines(path: &Path, tail: Option<&str>, since: Option<i32>) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for file in rotated_files_in_read_order(path) {
        let bytes = match tokio::fs::read(&file).await {
            Ok(b) => b,
            Err(_) => continue,
        };
        // The serial console is not guaranteed to be UTF-8 (early kernel output
        // may carry stray bytes). Lossy conversion keeps us robust.
        let text = String::from_utf8_lossy(&bytes);
        lines.extend(text.lines().map(|s| s.to_string()));
    }
    if lines.is_empty() && !path.exists() {
        return Vec::new();
    }

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

/// Rotate `<path>` if it has grown past `max_bytes`.
///
/// Shifts `<path>.{N-1}` → `<path>.N`, dropping anything past `max_backups`,
/// then renames `<path>` to `<path>.1`. Cloud Hypervisor re-creates the live
/// file on its next write (the VMM holds the file by name, not by inode,
/// because `serial.mode = "File"` is a path-based config) so this is safe to
/// call while a VM is running.
///
/// No-op when:
/// - `max_bytes == 0` (rotation disabled by config)
/// - `<path>` doesn't exist (VM not yet booted, or never produced output)
/// - `<path>` is at or below the threshold
pub(crate) async fn rotate_if_needed(path: &Path, max_bytes: u64, max_backups: u32) {
    if max_bytes == 0 {
        return;
    }
    let size = match tokio::fs::metadata(path).await {
        Ok(m) => m.len(),
        Err(_) => return,
    };
    if size <= max_bytes {
        return;
    }

    // Drop the oldest backup if it would exceed the configured count.
    if max_backups == 0 {
        // Just truncate by removing the live file; CH will re-create it.
        if let Err(e) = tokio::fs::remove_file(path).await {
            warn!("console log rotate: failed to drop {:?}: {}", path, e);
        }
        return;
    }

    let oldest = backup_path(path, max_backups);
    if oldest.exists() {
        if let Err(e) = tokio::fs::remove_file(&oldest).await {
            warn!(
                "console log rotate: failed to remove oldest backup {:?}: {}",
                oldest, e
            );
        }
    }

    // Shift .N-1 → .N down to .1 → .2.
    for idx in (1..max_backups).rev() {
        let from = backup_path(path, idx);
        let to = backup_path(path, idx + 1);
        if from.exists()
            && let Err(e) = tokio::fs::rename(&from, &to).await
        {
            warn!(
                "console log rotate: failed to shift {:?} -> {:?}: {}",
                from, to, e
            );
            return;
        }
    }

    // Finally, live -> .1.
    let target = backup_path(path, 1);
    if let Err(e) = tokio::fs::rename(path, &target).await {
        warn!(
            "console log rotate: failed to rotate {:?} -> {:?}: {}",
            path, target, e
        );
        return;
    }
    debug!(
        "console log rotated: {:?} (size {} > {})",
        path, size, max_bytes
    );
}

/// Walk `socket_dir` once and rotate every `*.console.log` that has grown
/// past the threshold. Called on a 60-second cadence by the rotator task.
pub(crate) async fn rotate_all_in_dir(dir: &Path, max_bytes: u64, max_backups: u32) {
    if max_bytes == 0 {
        return;
    }
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.ends_with(".console.log") {
            continue;
        }
        rotate_if_needed(&path, max_bytes, max_backups).await;
    }
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

    #[tokio::test]
    async fn rotate_no_op_when_under_threshold() {
        let path = scratch_file("rot-under");
        tokio::fs::write(&path, b"small").await.unwrap();
        rotate_if_needed(&path, 1024, 3).await;
        assert!(path.exists());
        assert!(!backup_path(&path, 1).exists());
        tokio::fs::remove_file(&path).await.ok();
    }

    #[tokio::test]
    async fn rotate_disabled_when_max_bytes_zero() {
        let path = scratch_file("rot-disabled");
        tokio::fs::write(&path, b"large-enough-payload")
            .await
            .unwrap();
        rotate_if_needed(&path, 0, 3).await;
        assert!(path.exists());
        assert!(!backup_path(&path, 1).exists());
        tokio::fs::remove_file(&path).await.ok();
    }

    #[tokio::test]
    async fn rotate_shifts_and_caps_backups() {
        let path = scratch_file("rot-shift");
        // Seed an existing .1 and .2 to exercise the shift path.
        tokio::fs::write(&path, b"current-payload-over-threshold")
            .await
            .unwrap();
        tokio::fs::write(backup_path(&path, 1), b"prev-1")
            .await
            .unwrap();
        tokio::fs::write(backup_path(&path, 2), b"prev-2")
            .await
            .unwrap();
        // max_bytes=8 < 30 bytes in `path`, max_backups=2 so .2 must drop.
        rotate_if_needed(&path, 8, 2).await;
        // Live file is gone (CH would re-create it on next write).
        assert!(!path.exists());
        // Previous .1 → .2, previous .2 dropped.
        assert_eq!(
            tokio::fs::read(backup_path(&path, 2)).await.unwrap(),
            b"prev-1"
        );
        // Old live content → .1.
        assert_eq!(
            tokio::fs::read(backup_path(&path, 1)).await.unwrap(),
            b"current-payload-over-threshold"
        );
        // .3 must not exist (we capped at 2).
        assert!(!backup_path(&path, 3).exists());

        tokio::fs::remove_file(backup_path(&path, 1)).await.ok();
        tokio::fs::remove_file(backup_path(&path, 2)).await.ok();
    }

    #[tokio::test]
    async fn rotate_zero_backups_drops_live() {
        let path = scratch_file("rot-zero-backups");
        tokio::fs::write(&path, b"payload-over-threshold")
            .await
            .unwrap();
        rotate_if_needed(&path, 4, 0).await;
        assert!(!path.exists());
        assert!(!backup_path(&path, 1).exists());
    }

    #[tokio::test]
    async fn read_lines_reads_backup_when_live_missing_or_empty() {
        // Post-rotation: the live file may not yet exist (CH hasn't written
        // anything new since the rename). `--tail all` must still return the
        // history that landed in `.1`.
        let path = scratch_file("post-rotate-missing");
        tokio::fs::write(backup_path(&path, 1), b"a\nb\nc\nd\ne\n")
            .await
            .unwrap();
        // Don't create `path` itself — simulate the gap before CH writes.
        let lines = read_lines(&path, Some("all"), None).await;
        assert_eq!(lines, vec!["a", "b", "c", "d", "e"]);
        tokio::fs::remove_file(backup_path(&path, 1)).await.ok();

        // Also when the live file exists but is empty (zero bytes).
        let path2 = scratch_file("post-rotate-empty");
        tokio::fs::write(backup_path(&path2, 1), b"a\nb\nc\nd\ne\n")
            .await
            .unwrap();
        tokio::fs::write(&path2, b"").await.unwrap();
        let lines = read_lines(&path2, Some("all"), None).await;
        assert_eq!(lines, vec!["a", "b", "c", "d", "e"]);
        tokio::fs::remove_file(&path2).await.ok();
        tokio::fs::remove_file(backup_path(&path2, 1)).await.ok();
    }

    #[tokio::test]
    async fn read_lines_reads_across_backups_in_chronological_order() {
        let path = scratch_file("read-across");
        // Oldest content lives in .2, newer in .1, newest in the live file.
        tokio::fs::write(backup_path(&path, 2), b"old-a\nold-b\n")
            .await
            .unwrap();
        tokio::fs::write(backup_path(&path, 1), b"mid-a\nmid-b\n")
            .await
            .unwrap();
        tokio::fs::write(&path, b"new-a\nnew-b\n").await.unwrap();

        let lines = read_lines(&path, None, None).await;
        assert_eq!(
            lines,
            vec!["old-a", "old-b", "mid-a", "mid-b", "new-a", "new-b"]
        );

        // tail=3 must pick the last three across the boundary.
        let tail = read_lines(&path, Some("3"), None).await;
        assert_eq!(tail, vec!["mid-b", "new-a", "new-b"]);

        tokio::fs::remove_file(&path).await.ok();
        tokio::fs::remove_file(backup_path(&path, 1)).await.ok();
        tokio::fs::remove_file(backup_path(&path, 2)).await.ok();
    }

    #[tokio::test]
    async fn rotate_all_in_dir_targets_only_console_logs() {
        let dir = std::env::temp_dir().join(format!(
            "ring-rot-dir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let log = dir.join("vm1.console.log");
        let other = dir.join("vm1.raw");
        tokio::fs::write(&log, b"big-enough-payload").await.unwrap();
        tokio::fs::write(&other, b"big-enough-payload")
            .await
            .unwrap();

        rotate_all_in_dir(&dir, 4, 1).await;

        assert!(!log.exists(), "console log should have been rotated");
        assert!(backup_path(&log, 1).exists());
        assert!(other.exists(), "non-console files must be left alone");

        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
