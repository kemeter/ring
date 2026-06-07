//! Host-side client that talks to `ring-agent` running inside a CH guest VM.
//!
//! Wire format mirrors `crates/ring-agent/src/main.rs`:
//!   - request:  [u32 BE length][JSON `Request`]
//!   - response: [u32 BE length][JSON `Response`]
//!
//! One TCP-style connection per request. The agent does not multiplex.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_vsock::{VsockAddr, VsockStream};

const VSOCK_PORT: u32 = 2375;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_RESPONSE_BYTES: u32 = 1 << 20;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request<'a> {
    Exec {
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout_ms: Option<u64>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Response {
    Exec(ExecResponse),
    Error { message: String },
}

#[derive(Deserialize, Debug)]
pub(crate) struct ExecResponse {
    pub exit_code: i32,
    #[allow(dead_code)]
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum VsockError {
    #[error("connect to CID {cid} failed: {source}")]
    Connect {
        cid: u32,
        #[source]
        source: std::io::Error,
    },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("agent reported error: {0}")]
    Agent(String),
    #[error("agent response too large: {0} bytes")]
    ResponseTooLarge(u32),
    #[error("malformed agent response: {0}")]
    Malformed(#[from] serde_json::Error),
}

/// Run `argv` inside the guest VM identified by `cid`. Blocks the calling task
/// until the command exits or the agent's own timeout fires (whichever first).
pub(crate) async fn exec(
    cid: u32,
    argv: &[String],
    env: &[(String, String)],
    timeout: Duration,
) -> Result<ExecResponse, VsockError> {
    // Invariant: the agent-side exec budget must be smaller than our read
    // timeout, otherwise the host disconnects before the agent can answer
    // and the exec process is orphaned inside the VM. Caller bug if violated.
    debug_assert!(
        timeout < READ_TIMEOUT,
        "vsock exec timeout {:?} must be < READ_TIMEOUT {:?}",
        timeout,
        READ_TIMEOUT
    );

    let addr = VsockAddr::new(cid, VSOCK_PORT);

    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, VsockStream::connect(addr))
        .await
        .map_err(|_| VsockError::Connect {
            cid,
            source: std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"),
        })?
        .map_err(|e| VsockError::Connect { cid, source: e })?;

    let request = Request::Exec {
        argv,
        env,
        timeout_ms: Some(timeout.as_millis() as u64),
    };
    let body = serde_json::to_vec(&request)?;
    let len = (body.len() as u32).to_be_bytes();
    // Without a write timeout, a full vsock send buffer (e.g. agent stuck in
    // a slow command) would block this task indefinitely and stall the
    // scheduler loop that owns the probe.
    tokio::time::timeout(WRITE_TIMEOUT, stream.write_all(&len))
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "vsock write timed out")
        })??;
    tokio::time::timeout(WRITE_TIMEOUT, stream.write_all(&body))
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "vsock write timed out")
        })??;
    tokio::time::timeout(WRITE_TIMEOUT, stream.flush())
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "vsock flush timed out")
        })??;

    let mut len_buf = [0u8; 4];
    tokio::time::timeout(READ_TIMEOUT, stream.read_exact(&mut len_buf))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "agent read timed out"))??;
    let resp_len = u32::from_be_bytes(len_buf);
    if resp_len > MAX_RESPONSE_BYTES {
        return Err(VsockError::ResponseTooLarge(resp_len));
    }
    let mut resp_buf = vec![0u8; resp_len as usize];
    tokio::time::timeout(READ_TIMEOUT, stream.read_exact(&mut resp_buf))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "agent body timed out"))??;

    match serde_json::from_slice::<Response>(&resp_buf)? {
        Response::Exec(r) => Ok(r),
        Response::Error { message } => Err(VsockError::Agent(message)),
    }
}
