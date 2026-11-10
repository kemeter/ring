//! Drain the stderr of a long-lived helper process (socat, virtiofsd).
//!
//! A piped stderr that nobody reads fills the pipe buffer, after which the
//! helper blocks on its next write and stops serving. Each line is forwarded
//! to the log instead, at debug level.

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tracing::debug;

/// Read `child`'s piped stderr until it closes, logging each line under
/// `label`. Does nothing when stderr was not piped.
pub(crate) fn drain_stderr(child: &mut Child, label: String) {
    let Some(stderr) = child.stderr.take() else {
        return;
    };
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            debug!(target: "ring::hypervisor::child_output", "{}: {}", label, line);
        }
    });
}
