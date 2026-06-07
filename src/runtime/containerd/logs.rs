//! Reading task logs.
//!
//! Unlike Docker, containerd does not buffer container logs behind an API call —
//! the task's stdio is whatever sink we configured at `CreateTask` time. The
//! lifecycle points stdout/stderr at a per-instance host file
//! (`/var/log/ring/containerd/<id>.log`); here we read and tail that file.

use futures::stream::{self, Stream};
use std::pin::Pin;
use tokio::io::{AsyncBufReadExt, BufReader};

fn log_file(instance_id: &str) -> String {
    format!("/var/log/ring/containerd/{}.log", instance_id)
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
    let path = log_file(instance_id);
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => {
            debug!("could not read log file {}: {}", path, e);
            return Vec::new();
        }
    };
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
