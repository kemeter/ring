//! Reading task logs.
//!
//! Unlike Docker, containerd does not buffer container logs behind an API call —
//! the task's stdio is whatever sink we configured at `CreateTask` time. The
//! lifecycle points stdout/stderr at a per-instance host file
//! (`/var/log/ring/containerd/<id>.log`); here we read and tail that file.

use futures::stream::{self, Stream};
use std::pin::Pin;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Where the shim writes each instance's stdout and stderr.
pub(crate) const LOG_DIR: &str = "/var/log/ring/containerd";

fn log_file(instance_id: &str) -> String {
    format!("{}/{}.log", LOG_DIR, instance_id)
}

/// Copy-truncate every instance log past `max_bytes` once a minute, keeping
/// `max_files - 1` backups. The shim holds the file by inode, so it cannot be
/// renamed away. The logs used to grow until the disk was full.
pub(crate) fn spawn_log_rotator(max_bytes: u64, max_files: u32) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if max_bytes == 0 {
            tracing::info!("containerd log rotation disabled (max_log_bytes = 0)");
            return;
        }
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            crate::hypervisor::console_logs::copy_truncate_matching_in_dir(
                std::path::Path::new(LOG_DIR),
                ".log",
                max_bytes,
                max_files.saturating_sub(1).max(1),
            )
            .await;
        }
    })
}

/// Read the instance log file into lines, honouring an optional `tail` count.
/// `since` is accepted for trait parity but not applied — the file lines carry
/// no reliable timestamp containerd guarantees, so time-based filtering is left
/// to the log content's own timestamps (see `extract_date`).
pub(crate) async fn read_logs(
    instance_id: &str,
    tail: Option<&str>,
    _since: Option<i32>,
) -> Vec<String> {
    // The rotated backups first, oldest first, then the live file.
    let path = log_file(instance_id);
    let mut content = String::new();
    for file in
        crate::hypervisor::console_logs::rotated_files_in_read_order(std::path::Path::new(&path))
    {
        match tokio::fs::read_to_string(&file).await {
            Ok(c) => content.push_str(&c),
            Err(e) => debug!("could not read log file {}: {}", file.display(), e),
        }
    }
    let mut lines: Vec<String> = content
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.trim().is_empty())
        .collect();

    if let Some(tail) = tail
        && let Ok(n) = tail.parse::<usize>()
        && lines.len() > n
    {
        lines = lines.split_off(lines.len() - n);
    }
    lines
}

/// Stream the instance log file, following appends. Emits the existing content
/// (subject to `tail`) then polls for new lines.
pub(crate) async fn stream_logs(
    instance_id: String,
    tail: Option<&str>,
    since: Option<i32>,
) -> Pin<Box<dyn Stream<Item = String> + Send>> {
    let path = log_file(&instance_id);
    // Seed with the current tail.
    let initial = read_logs(&instance_id, tail, since).await;

    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return Box::pin(stream::iter(initial)),
    };

    // Resume following at EOF so we only emit new appends after the initial
    // snapshot. `unfold` drives the reader without pulling in an extra
    // async-stream dependency.
    use tokio::io::AsyncSeekExt;
    let mut reader = BufReader::new(file);
    let _ = reader.seek(std::io::SeekFrom::End(0)).await;

    let follow = stream::unfold(reader, |mut reader| async move {
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Ok(_) => {
                    let trimmed = line.trim_end().to_string();
                    if !trimmed.trim().is_empty() {
                        return Some((trimmed, reader));
                    }
                }
                Err(_) => return None,
            }
        }
    });

    use futures::StreamExt;
    Box::pin(stream::iter(initial).chain(follow))
}
