//! ring-agent — in-guest companion to the Cloud Hypervisor runtime.
//!
//! Listens on AF_VSOCK port 2375 (well-known to the host-side client) and
//! services length-prefixed JSON requests. Today the only supported request
//! is `Exec`, used to back `health_checks: [{ type: command, ... }]` on CH
//! deployments where Ring has no `docker exec` equivalent.
//!
//! Wire format:
//!   request:  [u32 BE length][JSON ExecRequest]
//!   response: [u32 BE length][JSON ExecResponse]
//!
//! One connection per request — no multiplexing, no streaming. Health checks
//! are short-lived and idempotent; complexity isn't worth it yet.

use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_vsock::{VMADDR_CID_ANY, VsockAddr, VsockListener};

const VSOCK_PORT: u32 = 2375;
const MAX_REQUEST_BYTES: u32 = 1 << 20; // 1 MiB cap so a malformed length can't OOM us.
// Per-stream cap on captured guest output. The host-side client refuses
// frames larger than 1 MiB; truncating each stream to 256 KiB keeps the
// JSON envelope (plus exit_code etc.) under that limit with margin.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Exec(ExecRequest),
}

#[derive(Deserialize)]
struct ExecRequest {
    argv: Vec<String>,
    #[serde(default)]
    env: Vec<(String, String)>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Response {
    Exec(ExecResponse),
    Error { message: String },
}

#[derive(Serialize)]
struct ExecResponse {
    exit_code: i32,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::io::Result<()> {
    let addr = VsockAddr::new(VMADDR_CID_ANY, VSOCK_PORT);
    let listener = VsockListener::bind(addr)?;
    eprintln!("ring-agent listening on vsock port {}", VSOCK_PORT);

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("accept failed: {}", e);
                continue;
            }
        };
        tokio::spawn(async move {
            if let Err(e) = handle(stream).await {
                eprintln!("connection from {:?} failed: {}", peer, e);
            }
        });
    }
}

async fn handle(mut stream: tokio_vsock::VsockStream) -> std::io::Result<()> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_REQUEST_BYTES {
        write_response(
            &mut stream,
            &Response::Error {
                message: format!("request too large: {} bytes", len),
            },
        )
        .await?;
        return Ok(());
    }

    let mut body = vec![0u8; len as usize];
    stream.read_exact(&mut body).await?;

    let response = match serde_json::from_slice::<Request>(&body) {
        Ok(Request::Exec(req)) => match run_exec(req).await {
            Ok(r) => Response::Exec(r),
            Err(e) => Response::Error { message: e },
        },
        Err(e) => Response::Error {
            message: format!("malformed request: {}", e),
        },
    };

    write_response(&mut stream, &response).await
}

async fn run_exec(req: ExecRequest) -> Result<ExecResponse, String> {
    let mut argv = req.argv.into_iter();
    let program = argv
        .next()
        .ok_or_else(|| "argv must have at least one element".to_string())?;

    let mut cmd = Command::new(&program);
    cmd.args(argv).stdout(Stdio::piped()).stderr(Stdio::piped());
    // Start with an empty environment; the caller supplies the explicit set.
    // Avoids leaking arbitrary host/system env into the probe command.
    cmd.env_clear();
    for (k, v) in req.env {
        cmd.env(k, v);
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn '{}' failed: {}", program, e))?;

    let timeout = req
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(30));

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => Ok(ExecResponse {
            exit_code: out.status.code().unwrap_or(-1),
            stdout: truncate_lossy(&out.stdout),
            stderr: truncate_lossy(&out.stderr),
            timed_out: false,
        }),
        Ok(Err(e)) => Err(format!("wait failed: {}", e)),
        Err(_) => Ok(ExecResponse {
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("timed out after {}ms", timeout.as_millis()),
            timed_out: true,
        }),
    }
}

/// Lossy-decode then truncate. We always trim to `MAX_OUTPUT_BYTES` *after*
/// turning bytes into a `String` so the cut never lands inside a multi-byte
/// UTF-8 sequence (lossy_into_owned has already replaced any invalid bytes
/// with `U+FFFD`).
fn truncate_lossy(bytes: &[u8]) -> String {
    let mut s = String::from_utf8_lossy(bytes).into_owned();
    if s.len() > MAX_OUTPUT_BYTES {
        // Find the largest char boundary <= MAX_OUTPUT_BYTES.
        let mut end = MAX_OUTPUT_BYTES;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("\n[truncated]");
    }
    s
}

async fn write_response(
    stream: &mut tokio_vsock::VsockStream,
    response: &Response,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(response)
        .unwrap_or_else(|_| br#"{"type":"error","message":"serialize failed"}"#.to_vec());
    // Hard cap. Anything bigger than u32::MAX bytes can't be framed; in
    // practice we already trim per-stream output to MAX_OUTPUT_BYTES so the
    // serialized body comfortably fits, but assert defensively.
    if body.len() > u32::MAX as usize {
        let fallback = br#"{"type":"error","message":"response exceeds frame size"}"#.to_vec();
        let len = (fallback.len() as u32).to_be_bytes();
        stream.write_all(&len).await?;
        stream.write_all(&fallback).await?;
        return stream.flush().await;
    }
    let len = (body.len() as u32).to_be_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&body).await?;
    stream.flush().await
}
