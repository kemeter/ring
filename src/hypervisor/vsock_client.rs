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

    // Host-to-guest goes through the device's BASE multiplexing socket, then the
    // `CONNECT <port>` handshake below selects the guest listener. The
    // `<uds_path>_<port>` form is the *guest-to-host* socket (created only when
    // the guest dials out on that port), so connecting to it for an inbound call
    // fails intermittently with ENOENT. Use the base socket and let the
    // handshake do the port routing.
    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(uds_path))
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

/// Explain a vsock connect failure for a `command` health check.
///
/// A bare io error leaves the operator with nothing to act on, and there are two
/// quite different causes:
///
/// * the guest side is at fault — `ring-agent` isn't installed, isn't running,
///   or isn't listening on [`VSOCK_PORT`] yet;
/// * the VM has no vsock device at all. It is attached at boot, and only when
///   the deployment already declares a `command` check, so one added to a
///   running deployment cannot work until that VM restarts — neither hypervisor
///   can hot-plug it.
///
/// `host_socket_present` reports whether the host-side vsock socket is on disk.
/// It is a *hint*, not proof: a crashed VMM can leave the socket behind until
/// the reconciler reaps it, and a live VM can have its socket unlinked while
/// keeping the device. So it only orders the two causes — the message names both
/// either way, rather than asserting one and sending the operator to the wrong
/// place.
pub(crate) fn connect_failure_message(
    runtime: &str,
    cid: u32,
    source: &str,
    host_socket_present: bool,
) -> String {
    let (first, second) = if host_socket_present {
        (
            format!(
                "ring-agent may not be running in the guest on AF_VSOCK port {VSOCK_PORT} \
                 (install it in the image and start it at boot — see the {runtime} runtime docs)"
            ),
            "or this VM may have been booted without a vsock device".to_string(),
        )
    } else {
        (
            "this VM appears to have been booted without a vsock device".to_string(),
            format!(
                "or ring-agent may not be running in the guest on AF_VSOCK port {VSOCK_PORT} \
                 (install it in the image and start it at boot — see the {runtime} runtime docs)"
            ),
        )
    };

    format!(
        "cannot reach ring-agent in the guest (CID {cid}): {source}. {first}, {second}. \
         The device is attached at boot only when the deployment already declares a \
         `command` check, so one added to a running deployment takes effect on the next \
         VM restart — neither {runtime} nor Ring can hot-plug it"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both causes must appear whichever way the hint points: the host socket is
    /// not proof (a crashed VMM leaves it behind, a live VM can have it
    /// unlinked), so asserting one cause would send the operator to the wrong
    /// place half the time.
    #[test]
    fn both_causes_are_always_named() {
        for present in [true, false] {
            let msg = connect_failure_message("firecracker", 42, "connection refused", present);
            assert!(msg.contains("ring-agent"), "names the agent: {msg}");
            assert!(msg.contains("vsock device"), "names the device: {msg}");
            assert!(msg.contains("CID 42"), "names the CID: {msg}");
            assert!(
                msg.contains("connection refused"),
                "keeps the source: {msg}"
            );
            // The agent-side remedy must survive whichever way the hint points:
            // it is what an operator acts on when the image really is at fault.
            assert!(msg.contains("2375"), "names the port: {msg}");
            assert!(
                msg.contains("install it in the image"),
                "keeps the install remedy: {msg}"
            );
            assert!(msg.contains("runtime docs"), "points at the docs: {msg}");
        }
    }

    /// The hint decides which cause is stated first — that ordering is the whole
    /// value the host-socket check adds.
    #[test]
    fn the_hint_orders_the_two_causes() {
        // Unwrap both positions before comparing: `Option` ordering would make
        // `None < Some(_)` pass, so a vanished phrase would look like correct
        // ordering instead of failing.
        let with_socket = connect_failure_message("firecracker", 1, "e", true);
        let agent_first = with_socket
            .find("ring-agent may not be running")
            .unwrap_or_else(|| panic!("agent cause missing: {with_socket}"));
        let device_second = with_socket
            .find("may have been booted without")
            .unwrap_or_else(|| panic!("device cause missing: {with_socket}"));
        assert!(
            agent_first < device_second,
            "socket on disk → guest side first: {with_socket}"
        );

        let without_socket = connect_failure_message("firecracker", 1, "e", false);
        let device_first = without_socket
            .find("appears to have been booted without")
            .unwrap_or_else(|| panic!("device cause missing: {without_socket}"));
        let agent_second = without_socket
            .find("or ring-agent may not be running")
            .unwrap_or_else(|| panic!("agent cause missing: {without_socket}"));
        assert!(
            device_first < agent_second,
            "no socket → missing device first: {without_socket}"
        );
    }

    /// The boot-time constraint is the actionable part: it tells the operator a
    /// restart is needed, which no amount of guest-side debugging would reveal.
    #[test]
    fn the_boot_time_constraint_is_always_explained() {
        for present in [true, false] {
            let msg = connect_failure_message("cloud-hypervisor", 7, "no such file", present);
            assert!(msg.contains("attached at boot only"), "{msg}");
            assert!(msg.contains("next VM restart"), "gives the remedy: {msg}");
        }
    }

    /// The runtime name is interpolated so the message points at the right docs.
    #[test]
    fn message_names_the_runtime() {
        for runtime in ["firecracker", "cloud-hypervisor"] {
            assert!(connect_failure_message(runtime, 1, "e", true).contains(runtime));
            assert!(connect_failure_message(runtime, 1, "e", false).contains(runtime));
        }
    }
}
