//! Interactive `exec` over a WebSocket.
//!
//! The transport half of [`crate::hypervisor::lifecycle_trait::RuntimeLifecycle::exec`]:
//! it authorises the caller, picks an instance, takes a session slot, and
//! then relays bytes until one side stops.
//!
//! **Framing.** Client→server messages are JSON control frames (`stdin`,
//! `resize`), server→client are JSON output frames (`stdout`, `stderr`,
//! `exit`, `error`). JSON both ways, rather than raw binary for the payload,
//! because the same socket has to carry resize events and an exit code; a
//! second channel for those would need its own ordering rules against the
//! byte stream.
//!
//! **Why a WebSocket and not SSE.** Logs stream one way and use SSE. Exec
//! needs stdin, which SSE cannot carry.

use axum::{
    extract::{
        Path, RawQuery, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::api::auth::{Auth, AuthSource, require_namespace};
use crate::api::exec_sessions::{AdmissionError, ExecSessionLimiter};
use crate::api::server::{Db, RuntimeMap};
use crate::hypervisor::lifecycle_trait::{
    ExecError, ExecOutput, ExecRequest, ExecSession, TerminalSize,
};
use crate::models::deployments;

/// Scope string binding a stream ticket to one deployment's exec endpoint.
/// Distinct from the logs scope: a ticket minted to read logs must never
/// open a shell, and the scope string is what keeps those apart.
pub(crate) fn exec_scope(deployment_id: &str) -> String {
    format!("deployment:exec:{}", deployment_id)
}

/// Upper bound on a single `stdin` frame, in bytes.
///
/// A terminal sends keystrokes, and a paste is still small. This exists so a
/// client cannot make the daemon buffer an arbitrary amount before the
/// container has read anything.
const MAX_STDIN_FRAME: usize = 64 * 1024;

/// Terminal dimensions we will forward. Anything outside is a client bug or
/// a probe; Docker takes `u16` and a zero dimension is meaningless.
const MAX_TERMINAL_DIMENSION: u16 = 10_000;

/// Parsed `?…` parameters.
///
/// Hand-parsed rather than derived through `Query`: axum's extractor is
/// backed by `serde_urlencoded`, which has no notion of a repeated key and
/// refuses `?command=/bin/sh&command=-c` outright. Repetition is what keeps
/// argv a list — joining on spaces would make an argument *containing* a
/// space unrepresentable, which is exactly what `sh -c "…"` is made of.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ExecQuery {
    command: Vec<String>,
    tty: bool,
    cols: Option<u16>,
    rows: Option<u16>,
    /// Which instance to enter. Defaults to the deployment's first running
    /// instance, which is what a single-replica deployment always wants.
    container: Option<String>,
}

/// Why a query string could not be used.
#[derive(Debug, PartialEq, Eq)]
enum QueryError {
    /// A numeric parameter was not a number, or a flag was not a boolean.
    Malformed(&'static str),
}

impl ExecQuery {
    /// Parse the raw query string.
    ///
    /// Unknown keys are ignored on purpose: the dashboard appends `?ticket=`
    /// for auth (a WebSocket cannot set headers), and that key is consumed by
    /// the auth middleware upstream, never here.
    fn parse(raw: &str) -> Result<Self, QueryError> {
        let mut parsed = ExecQuery {
            // A terminal is the overwhelmingly common case, and a client that
            // wants pipes has to say so.
            tty: true,
            ..Default::default()
        };

        for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
            match key.as_ref() {
                "command" => parsed.command.push(value.into_owned()),
                "tty" => {
                    parsed.tty = match value.as_ref() {
                        "true" | "1" => true,
                        "false" | "0" => false,
                        _ => return Err(QueryError::Malformed("tty must be true or false")),
                    }
                }
                "cols" => {
                    parsed.cols = Some(
                        value
                            .parse()
                            .map_err(|_| QueryError::Malformed("cols must be a number"))?,
                    )
                }
                "rows" => {
                    parsed.rows = Some(
                        value
                            .parse()
                            .map_err(|_| QueryError::Malformed("rows must be a number"))?,
                    )
                }
                "container" => parsed.container = Some(value.into_owned()),
                _ => {}
            }
        }

        Ok(parsed)
    }
}

/// Client→server frame.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientFrame {
    Stdin { data: String },
    Resize { cols: u16, rows: u16 },
}

/// Server→client frame.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerFrame {
    Stdout { data: String },
    Stderr { data: String },
    Exit { code: Option<i64> },
    Error { message: String },
}

/// Map a runtime-level failure onto an HTTP status.
///
/// The status is what a client can act on; the daemon's own words are not
/// forwarded, since they describe an infrastructure we do not expose.
fn status_for(error: &ExecError) -> StatusCode {
    match error {
        ExecError::UnsupportedRuntime(_) => StatusCode::NOT_IMPLEMENTED,
        ExecError::InstanceUnavailable(_) => StatusCode::NOT_FOUND,
        ExecError::Failed(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub(crate) async fn exec(
    Path(id): Path<String>,
    RawQuery(raw_query): RawQuery,
    auth: Auth,
    State(pool): State<Db>,
    State(runtimes): State<RuntimeMap>,
    State(limiter): State<ExecSessionLimiter>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // Refuse before doing any work when exec is switched off: the answer does
    // not depend on the deployment, the runtime, or the command.
    if !limiter.is_enabled() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            axum::Json(json!({ "error": "exec is disabled on this server" })),
        )
            .into_response();
    }

    let params = match ExecQuery::parse(raw_query.as_deref().unwrap_or_default()) {
        Ok(params) => params,
        Err(QueryError::Malformed(message)) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({ "error": message })),
            )
                .into_response();
        }
    };

    if params.command.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({ "error": "command is required" })),
        )
            .into_response();
    }

    if let Some(response) = invalid_size(&params) {
        return response;
    }

    // Same shape as the logs route: a PAT carries a namespace boundary the
    // middleware cannot check, because the namespace is not known until the
    // deployment is loaded. A ticket is already pinned to this deployment.
    let deployment = match deployments::find(&pool, &id).await {
        Ok(Some(d)) => d,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    if matches!(auth.source, AuthSource::Token { .. })
        && let Err(resp) = require_namespace(&auth.source, &deployment.namespace)
    {
        return resp.into_response();
    }

    let Some(runtime) = runtimes.get(&deployment.runtime).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // Resolve the instance before upgrading. A client that asked for a
    // container that is not there deserves an HTTP status, not a socket that
    // opens and immediately closes with a reason it has to parse.
    let instance =
        match resolve_instance(&*runtime, &deployment.id, params.container.as_deref()).await {
            Ok(instance) => instance,
            Err(status) => return status.into_response(),
        };

    // Take the slot last: everything above can refuse the request, and a slot
    // held across those paths would be a leak on every rejection.
    let slot = match limiter.admit() {
        Ok(slot) => slot,
        Err(AdmissionError::Disabled) => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                axum::Json(json!({ "error": "exec is disabled on this server" })),
            )
                .into_response();
        }
        Err(AdmissionError::AtCapacity) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(json!({ "error": "too many concurrent exec sessions" })),
            )
                .into_response();
        }
    };

    let request = ExecRequest {
        command: params.command.clone(),
        tty: params.tty,
        size: terminal_size(&params),
    };

    let session = match runtime.exec(&instance, request).await {
        Ok(session) => session,
        Err(e) => {
            let status = status_for(&e);
            tracing::warn!(
                deployment = %deployment.id,
                instance = %instance,
                "exec refused: {e}"
            );
            return (status, axum::Json(json!({ "error": public_message(&e) }))).into_response();
        }
    };

    let max_duration = slot.max_duration();
    let idle_timeout = slot.idle_timeout();

    ws.on_upgrade(move |socket| async move {
        // The slot lives until this future ends, however it ends. Moving it
        // in is what ties capacity to the session rather than to the
        // handshake.
        let _slot = slot;
        relay(socket, session, max_duration, idle_timeout).await;
    })
    .into_response()
}

/// Reject nonsensical terminal dimensions before they reach the runtime.
fn invalid_size(params: &ExecQuery) -> Option<axum::response::Response> {
    let bad = |value: Option<u16>| matches!(value, Some(v) if v == 0 || v > MAX_TERMINAL_DIMENSION);

    if bad(params.cols) || bad(params.rows) {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({
                    "error": format!(
                        "cols and rows must be between 1 and {MAX_TERMINAL_DIMENSION}"
                    )
                })),
            )
                .into_response(),
        );
    }
    None
}

fn terminal_size(params: &ExecQuery) -> Option<TerminalSize> {
    match (params.cols, params.rows) {
        (Some(cols), Some(rows)) => Some(TerminalSize { cols, rows }),
        // A half-specified size is worse than none: the missing dimension
        // would have to be invented, and a wrong guess is what the initial
        // resize exists to avoid.
        _ => None,
    }
}

/// What the client is told when the runtime refuses.
///
/// `UnsupportedRuntime` is safe to pass through — it describes Ring's own
/// capabilities. The other two can carry daemon prose (image names, paths,
/// socket errors), so they collapse to a fixed sentence; the detail goes to
/// the log where an operator can see it.
fn public_message(error: &ExecError) -> String {
    match error {
        ExecError::UnsupportedRuntime(m) => m.clone(),
        ExecError::InstanceUnavailable(_) => "instance is not available for exec".to_string(),
        ExecError::Failed(_) => "could not start exec session".to_string(),
    }
}

/// Pick the instance to enter.
///
/// `list_instances_with_names` only ever returns containers Ring manages, so
/// an explicitly requested container is matched against that list rather than
/// passed through: it is the difference between naming one of your own
/// instances and naming anything on the host.
async fn resolve_instance(
    runtime: &dyn crate::hypervisor::lifecycle_trait::RuntimeLifecycle,
    deployment_id: &str,
    requested: Option<&str>,
) -> Result<String, StatusCode> {
    let instances = runtime
        .list_instances_with_names(deployment_id.to_string(), "running")
        .await;

    match requested {
        Some(name) => instances
            .into_iter()
            .find(|(id, instance_name)| id == name || instance_name == name)
            .map(|(id, _)| id)
            .ok_or(StatusCode::NOT_FOUND),
        None => instances
            .into_iter()
            .next()
            .map(|(id, _)| id)
            .ok_or(StatusCode::NOT_FOUND),
    }
}

/// Pump bytes between the socket and the session until either side stops.
///
/// The two deadlines are enforced here rather than by a tower layer because a
/// layer wraps the request→response head and would either kill the upgrade or
/// never see the session at all.
async fn relay(
    socket: WebSocket,
    session: ExecSession,
    max_duration: Duration,
    idle_timeout: Duration,
) {
    let ExecSession {
        mut output,
        mut input,
        resize,
        wait,
    } = session;

    let (mut sender, mut receiver) = socket.split();
    let deadline = tokio::time::Instant::now() + max_duration;

    // Set when the exec's own output ends, so the loop can stop asking a
    // finished stream for more while still draining what the client sends.
    let mut output_done = false;
    let mut stream_error: Option<String> = None;
    // Set once the client stops sending. The session continues: only its
    // input half is over.
    let mut client_gone = false;

    loop {
        let idle = tokio::time::sleep(idle_timeout);
        tokio::pin!(idle);

        tokio::select! {
            // Bias towards output so a chatty command cannot be starved by a
            // client that is also sending, and so the exit path is reached
            // promptly once the process ends.
            biased;

            _ = tokio::time::sleep_until(deadline) => {
                let _ = send(&mut sender, ServerFrame::Error {
                    message: "session exceeded its maximum duration".to_string(),
                }).await;
                break;
            }

            chunk = output.next(), if !output_done => {
                match chunk {
                    Some(Ok(ExecOutput::Stdout(bytes))) => {
                        if send(&mut sender, ServerFrame::Stdout {
                            data: encode(&bytes),
                        }).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(ExecOutput::Stderr(bytes))) => {
                        if send(&mut sender, ServerFrame::Stderr {
                            data: encode(&bytes),
                        }).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        // The stream failed rather than ended. Say so, and
                        // stop: an exit code read after this would describe a
                        // process whose output we no longer trust.
                        stream_error = Some(public_message(&e));
                        output_done = true;
                    }
                    None => output_done = true,
                }
            }

            incoming = receiver.next(), if !client_gone => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientFrame>(&text) {
                            Ok(ClientFrame::Stdin { data }) => {
                                let Some(bytes) = decode(&data) else {
                                    let _ = send(&mut sender, ServerFrame::Error {
                                        message: "stdin payload is not valid base64".to_string(),
                                    }).await;
                                    continue;
                                };
                                if bytes.len() > MAX_STDIN_FRAME {
                                    let _ = send(&mut sender, ServerFrame::Error {
                                        message: format!(
                                            "stdin frame exceeds {MAX_STDIN_FRAME} bytes"
                                        ),
                                    }).await;
                                    continue;
                                }
                                if input.write_all(&bytes).await.is_err() {
                                    break;
                                }
                                let _ = input.flush().await;
                            }
                            Ok(ClientFrame::Resize { cols, rows }) => {
                                if cols == 0
                                    || rows == 0
                                    || cols > MAX_TERMINAL_DIMENSION
                                    || rows > MAX_TERMINAL_DIMENSION
                                {
                                    continue;
                                }
                                resize(TerminalSize { cols, rows }).await;
                            }
                            Err(_) => {
                                let _ = send(&mut sender, ServerFrame::Error {
                                    message: "unrecognised frame".to_string(),
                                }).await;
                            }
                        }
                    }
                    // The client is done *sending*. That is not the end of
                    // the session: a non-interactive caller closes as soon as
                    // its own stdin runs out (`ring exec … -- echo hi` inside
                    // a `$(…)` closes immediately), and the command's output
                    // is still on its way. Breaking here raced that output
                    // away and returned an empty result.
                    //
                    // So stop reading input, close the process's stdin so a
                    // command waiting on EOF can finish, and keep draining
                    // output until it ends on its own.
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                        let _ = input.shutdown().await;
                        client_gone = true;
                    }
                    Some(Ok(_)) => {}
                }
            }

            _ = &mut idle => {
                let _ = send(&mut sender, ServerFrame::Error {
                    message: "session idle timeout".to_string(),
                }).await;
                break;
            }
        }

        // Output is finished: report why, then stop. Waiting for the client
        // to close would hold a slot open for a session that is already over.
        if output_done {
            match stream_error.take() {
                Some(message) => {
                    let _ = send(&mut sender, ServerFrame::Error { message }).await;
                }
                None => {
                    let code = wait.await;
                    let _ = send(&mut sender, ServerFrame::Exit { code }).await;
                }
            }
            break;
        }
    }

    let _ = sender.close().await;
}

async fn send(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    frame: ServerFrame,
) -> Result<(), ()> {
    let payload = serde_json::to_string(&frame).map_err(|_| ())?;
    sender
        .send(Message::Text(payload.into()))
        .await
        .map_err(|_| ())
}

/// Terminal bytes are not text: a UTF-8 sequence can be split across reads,
/// and control bytes are not characters at all. Base64 keeps the payload
/// intact through a JSON frame.
fn encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::server::tests::{login, new_test_app};
    use axum::Router;
    use axum_test::TestServer;

    #[test]
    fn scope_is_bound_to_one_deployment() {
        assert_eq!(exec_scope("abc"), "deployment:exec:abc");
        assert_ne!(exec_scope("abc"), exec_scope("def"));
    }

    #[test]
    fn runtime_errors_map_onto_actionable_statuses() {
        assert_eq!(
            status_for(&ExecError::UnsupportedRuntime("x".into())),
            StatusCode::NOT_IMPLEMENTED
        );
        assert_eq!(
            status_for(&ExecError::InstanceUnavailable("x".into())),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status_for(&ExecError::Failed("x".into())),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn daemon_prose_is_not_forwarded_to_clients() {
        // The failure text can carry image names, socket paths and daemon
        // internals. Only the "unsupported runtime" case describes Ring
        // itself and is safe to pass through verbatim.
        let leaky = ExecError::Failed("dial unix /var/run/docker.sock: denied".into());
        assert!(!public_message(&leaky).contains("docker.sock"));

        let missing = ExecError::InstanceUnavailable("container 9f3a is not running".into());
        assert!(!public_message(&missing).contains("9f3a"));

        let unsupported = ExecError::UnsupportedRuntime("no exec support".into());
        assert_eq!(public_message(&unsupported), "no exec support");
    }

    #[test]
    fn repeated_command_keys_build_argv() {
        // Regression: `Query<Vec<String>>` (serde_urlencoded) rejects this
        // outright with "invalid type: string, expected a sequence", which
        // made every exec request a 400 before it reached the handler.
        let q = ExecQuery::parse("command=/bin/sh&command=-c&command=echo%20hi").unwrap();
        assert_eq!(q.command, vec!["/bin/sh", "-c", "echo hi"]);
    }

    #[test]
    fn tty_defaults_on_and_can_be_turned_off() {
        assert!(ExecQuery::parse("command=/bin/sh").unwrap().tty);
        assert!(!ExecQuery::parse("command=/bin/sh&tty=false").unwrap().tty);
        assert!(!ExecQuery::parse("command=/bin/sh&tty=0").unwrap().tty);
        assert_eq!(
            ExecQuery::parse("command=/bin/sh&tty=maybe"),
            Err(QueryError::Malformed("tty must be true or false"))
        );
    }

    #[test]
    fn unknown_query_keys_are_ignored() {
        // `?ticket=` is how a browser authenticates a WebSocket; it is
        // consumed by the auth middleware and must not trip parsing here.
        let q = ExecQuery::parse("command=/bin/sh&ticket=tk_stream_abc").unwrap();
        assert_eq!(q.command, vec!["/bin/sh"]);
    }

    #[test]
    fn non_numeric_dimensions_are_refused() {
        assert_eq!(
            ExecQuery::parse("command=/bin/sh&cols=wide"),
            Err(QueryError::Malformed("cols must be a number"))
        );
    }

    #[test]
    fn half_specified_terminal_size_is_ignored() {
        let params = |cols, rows| ExecQuery {
            command: vec!["/bin/sh".into()],
            tty: true,
            cols,
            rows,
            container: None,
        };

        // Inventing the missing dimension is exactly the wrong-size redraw
        // the initial resize exists to avoid.
        assert!(terminal_size(&params(Some(80), None)).is_none());
        assert!(terminal_size(&params(None, Some(24))).is_none());
        assert_eq!(
            terminal_size(&params(Some(80), Some(24))),
            Some(TerminalSize { cols: 80, rows: 24 })
        );
    }

    /// Requests here go through `get_websocket`, which sets the upgrade
    /// headers, on a server built with `http_transport()`.
    ///
    /// Both halves are required. A plain `get` is refused by axum's
    /// `WebSocketUpgrade` extractor with a protocol-level 400 before the
    /// handler runs, and the default in-process transport cannot upgrade a
    /// connection at all, which surfaces as 426. Either way the response says
    /// nothing about the checks these tests exist to cover.
    fn ws_server(app: Router) -> TestServer {
        TestServer::builder()
            .http_transport()
            .build(app)
            .expect("test server")
    }
    #[tokio::test]
    async fn rejects_without_credentials() {
        let res = ws_server(new_test_app().await)
            .get_websocket("/deployments/abc/exec?command=/bin/sh")
            .await;

        assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_a_request_without_a_command() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;

        let res = ws_server(app)
            .get_websocket("/deployments/abc/exec")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rejects_nonsensical_terminal_dimensions() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = ws_server(app);

        for query in [
            "command=/bin/sh&cols=0&rows=24",
            "command=/bin/sh&cols=80&rows=0",
            "command=/bin/sh&cols=99999&rows=24",
        ] {
            let res = server
                .get_websocket(&format!("/deployments/abc/exec?{query}"))
                .add_header("Authorization", format!("Bearer {}", token))
                .await;

            assert_eq!(
                res.status_code(),
                StatusCode::BAD_REQUEST,
                "expected {query} to be refused"
            );
        }
    }

    #[tokio::test]
    async fn unknown_deployment_is_not_found() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;

        let res = ws_server(app)
            .get_websocket("/deployments/does-not-exist/exec?command=/bin/sh")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn refuses_when_exec_is_disabled_in_config() {
        // The default posture: an operator has to switch exec on. A disabled
        // daemon must say so before touching the deployment or the runtime.
        use crate::api::exec_sessions::ExecSessionLimiter;
        use crate::config::server::ExecConfig;

        let limiter = ExecSessionLimiter::new(&ExecConfig::default());
        assert!(!limiter.is_enabled());
    }
}
