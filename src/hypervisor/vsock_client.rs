//! Host-side client that talks to `ring-agent` running inside a guest VM.
//!
//! Wire format mirrors `crates/ring-agent/src/main.rs`:
//!   - request:  [u32 BE length][JSON `Request`]
//!   - response: [u32 BE length][JSON `Response`]
//!
//! One TCP-style connection per request. The agent does not multiplex.
//!
//! Two transports reach the same agent protocol:
//!   - Cloud Hypervisor: kernel AF_VSOCK on the host (`exec`).
//!   - Firecracker: vsock multiplexed over a host Unix socket (`exec_uds`),
//!     which first performs Firecracker's `CONNECT <port>` handshake.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
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

    let stream = tokio::time::timeout(CONNECT_TIMEOUT, VsockStream::connect(addr))
        .await
        .map_err(|_| VsockError::Connect {
            cid,
            source: std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"),
        })?
        .map_err(|e| VsockError::Connect { cid, source: e })?;

    exchange(stream, argv, env, timeout).await
}

/// Firecracker variant: connect to the host-side multiplexing Unix socket
/// (`<uds_path>` is the device's `uds_path`; the agent port is appended as
/// `<uds_path>_<port>`), perform the `CONNECT <port>` handshake, then speak the
/// same agent protocol as [`exec`].
///
/// `cid` is carried only so connect failures report a stable identifier
/// consistent with the Cloud Hypervisor path; Firecracker addresses the agent
/// through the socket, not the CID.
pub(crate) async fn exec_uds(
    cid: u32,
    uds_path: &str,
    argv: &[String],
    env: &[(String, String)],
    timeout: Duration,
) -> Result<ExecResponse, VsockError> {
    debug_assert!(
        timeout < READ_TIMEOUT,
        "vsock exec timeout {:?} must be < READ_TIMEOUT {:?}",
        timeout,
        READ_TIMEOUT
    );

    let port_path = format!("{}_{}", uds_path, VSOCK_PORT);
    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(&port_path))
        .await
        .map_err(|_| VsockError::Connect {
            cid,
            source: std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"),
        })?
        .map_err(|e| VsockError::Connect { cid, source: e })?;

    // Firecracker host-initiated handshake: write `CONNECT <port>\n`, then the
    // device replies `OK <host_port>\n` before relaying bytes to the guest
    // listener. Treat a missing/!OK line as a connect failure.
    let handshake = format!("CONNECT {}\n", VSOCK_PORT);
    tokio::time::timeout(WRITE_TIMEOUT, stream.write_all(handshake.as_bytes()))
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "vsock connect write timed out",
            )
        })??;
    read_ok_line(&mut stream, cid).await?;

    exchange(stream, argv, env, timeout).await
}

/// Read Firecracker's `OK <port>\n` acknowledgement line, byte by byte (the
/// payload that follows must not be over-read into a buffer). Anything other
/// than a line starting with `OK` is a failed guest-side connect.
async fn read_ok_line<S: AsyncRead + Unpin>(stream: &mut S, cid: u32) -> Result<(), VsockError> {
    let mut line = Vec::with_capacity(16);
    let mut byte = [0u8; 1];
    loop {
        tokio::time::timeout(CONNECT_TIMEOUT, stream.read_exact(&mut byte))
            .await
            .map_err(|_| VsockError::Connect {
                cid,
                source: std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "handshake read timed out",
                ),
            })?
            .map_err(|e| VsockError::Connect { cid, source: e })?;
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() > 64 {
            break;
        }
    }
    if line.starts_with(b"OK") {
        Ok(())
    } else {
        Err(VsockError::Connect {
            cid,
            source: std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!(
                    "Firecracker vsock handshake rejected: {}",
                    String::from_utf8_lossy(&line)
                ),
            ),
        })
    }
}

/// Send one framed `Exec` request and read the framed response over an already
/// connected stream. Transport-agnostic: shared by the kernel-AF_VSOCK (CH) and
/// Unix-socket (Firecracker) paths.
async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    argv: &[String],
    env: &[(String, String)],
    timeout: Duration,
) -> Result<ExecResponse, VsockError> {
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
