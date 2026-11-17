//! `GET /deployments/{id}/port-forward?port=N`: a WebSocket carrying a raw TCP
//! stream to a port of one of the deployment's running instances.
//!
//! Instances live on their namespace's network, which nothing outside the
//! namespace can reach. Ring runs on the host and already reaches every
//! instance there, to probe it, so it can relay a connection on behalf of a
//! client that is allowed in: binary frames in are bytes written to the
//! instance, bytes read from the instance are binary frames out.
//!
//! Gated like exec, by `deployments:exec` and the token's namespace boundary:
//! reaching a port inside a container is no more than a shell in it allows.

use axum::{
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::api::action::deployment::exec::resolve_instance;
use crate::api::auth::{Auth, AuthSource, require_namespace};
use crate::api::server::{Db, RuntimeMap};
use crate::models::audit_log;
use crate::models::deployments;

const DIAL_TIMEOUT: Duration = Duration::from_secs(5);

/// Bytes read from the instance per frame.
const READ_BUFFER: usize = 64 * 1024;

/// Largest frame accepted from the client.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
pub(crate) struct PortForwardQuery {
    port: u16,
}

pub(crate) async fn port_forward(
    Path(id): Path<String>,
    query: Result<Query<PortForwardQuery>, axum::extract::rejection::QueryRejection>,
    auth: Auth,
    State(pool): State<Db>,
    State(runtimes): State<RuntimeMap>,
    ws: WebSocketUpgrade,
) -> Response {
    let Ok(Query(query)) = query else {
        return bad_request("port is required, between 1 and 65535");
    };
    if query.port == 0 {
        return bad_request("port is required, between 1 and 65535");
    }

    let deployment = match deployments::find(&pool, &id).await {
        Ok(Some(d)) => d,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // The namespace is only known once the deployment is loaded, so the
    // boundary of a namespace-scoped token is checked here, as for exec.
    if matches!(auth.source, AuthSource::Token { .. })
        && let Err(resp) = require_namespace(&auth.source, &deployment.namespace)
    {
        return resp.into_response();
    }

    let Some(runtime) = runtimes.get(&deployment.runtime).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let instance = match resolve_instance(&*runtime, &deployment.id, None).await {
        Ok(instance) => instance,
        Err(status) => return status.into_response(),
    };

    let Some(address) = runtime.instance_address(&instance.id).await else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            axum::Json(json!({ "error": "this runtime does not expose instance addresses" })),
        )
            .into_response();
    };

    // Dialled before the upgrade: a port nothing listens on is an HTTP status
    // the client can act on, not a socket that opens and closes at once.
    let upstream = match tokio::time::timeout(
        DIAL_TIMEOUT,
        TcpStream::connect((address, query.port)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => {
            tracing::warn!(deployment = %deployment.id, port = query.port, "port-forward dial failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                axum::Json(json!({ "error": "the instance refused the connection on that port" })),
            )
                .into_response();
        }
        Err(_) => {
            return (
                StatusCode::GATEWAY_TIMEOUT,
                axum::Json(json!({ "error": "the instance did not answer on that port" })),
            )
                .into_response();
        }
    };

    // Recorded once the connection is open, like exec: the trail says what
    // happened. The bytes relayed are never recorded.
    let _ = audit_log::record(
        &pool,
        Some(&auth.user.id),
        "port-forward",
        "deployment",
        &deployment.name,
        Some(&deployment.namespace),
    )
    .await;

    ws.max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| relay(socket, upstream))
        .into_response()
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(json!({ "error": message })),
    )
        .into_response()
}

/// Shuttle bytes both ways until either side ends. A close from the client
/// shuts the instance's side down; the instance closing its side closes the
/// socket.
async fn relay(socket: WebSocket, upstream: TcpStream) {
    let (mut tcp_read, mut tcp_write) = upstream.into_split();
    let (mut ws_write, mut ws_read) = socket.split();

    let to_instance = async {
        while let Some(Ok(message)) = ws_read.next().await {
            match message {
                Message::Binary(bytes) => {
                    if tcp_write.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                // Text frames carry nothing for a byte stream; pings are
                // answered by axum itself.
                _ => {}
            }
        }
        let _ = tcp_write.shutdown().await;
    };

    let to_client = async {
        let mut buffer = vec![0u8; READ_BUFFER];
        loop {
            match tcp_read.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if ws_write
                        .send(Message::Binary(buffer[..n].to_vec().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
        let _ = ws_write.send(Message::Close(None)).await;
    };

    tokio::select! {
        () = to_instance => {},
        () = to_client => {},
    }
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app, new_test_app_with_runtimes};
    use crate::hypervisor::lifecycle_trait::RuntimeLifecycle;
    use crate::hypervisor::mock::MockRuntime;
    use axum::http::StatusCode;
    use axum_test::{TestServer, WsMessage};
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// An app whose docker runtime has one running instance on the loopback, so
    /// a forwarded port reaches a listener of the test's own.
    async fn server_with_instance() -> (TestServer, String, String) {
        let runtime = MockRuntime::healthy().with_running_instance(
            "container-1",
            "cache-1",
            IpAddr::V4(Ipv4Addr::LOCALHOST),
        );
        let mut runtimes: HashMap<String, Arc<dyn RuntimeLifecycle>> = HashMap::new();
        runtimes.insert("docker".to_string(), Arc::new(runtime));
        let (_pool, app) = new_test_app_with_runtimes(Arc::new(runtimes)).await;
        let session = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::builder().http_transport().build(app);

        let created: Value = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", session))
            .json(&json!({
                "runtime": "docker",
                "name": "cache",
                "namespace": "tenant",
                "image": "valkey/valkey:8.1.4-alpine",
            }))
            .await
            .json();

        let id = created["id"].as_str().unwrap().to_string();
        (server, session, id)
    }

    /// A listener that echoes back what it reads, on a free loopback port.
    async fn echo_listener() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buffer = [0u8; 1024];
                    while let Ok(n) = stream.read(&mut buffer).await {
                        if n == 0 || stream.write_all(&buffer[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn bytes_reach_the_instance_port_and_come_back() {
        let (server, session, id) = server_with_instance().await;
        let port = echo_listener().await;

        let mut socket = server
            .get_websocket(&format!("/deployments/{id}/port-forward?port={port}"))
            .add_header("Authorization", format!("Bearer {}", session))
            .await
            .into_websocket()
            .await;

        socket
            .send_message(WsMessage::Binary(b"PING\r\n".to_vec().into()))
            .await;
        let echoed = socket.receive_message().await;

        assert_eq!(echoed, WsMessage::Binary(b"PING\r\n".to_vec().into()));
    }

    #[tokio::test]
    async fn a_port_nothing_listens_on_is_a_bad_gateway() {
        let (server, session, id) = server_with_instance().await;
        // Bound then dropped: nothing listens there any more.
        let port = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();

        let response = server
            .get_websocket(&format!("/deployments/{id}/port-forward?port={port}"))
            .add_header("Authorization", format!("Bearer {}", session))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn a_port_is_required() {
        let (server, session, id) = server_with_instance().await;

        for query in ["", "?port=0", "?port=abc", "?port=70000"] {
            let response = server
                .get_websocket(&format!("/deployments/{id}/port-forward{query}"))
                .add_header("Authorization", format!("Bearer {}", session))
                .await;

            assert_eq!(response.status_code(), StatusCode::BAD_REQUEST, "{query}");
        }
    }

    #[tokio::test]
    async fn an_unknown_deployment_is_not_found() {
        let app = new_test_app().await;
        let session = login(app.clone(), "admin", "changeme").await;

        let response = TestServer::builder()
            .http_transport()
            .build(app)
            .get_websocket(
                "/deployments/00000000-0000-0000-0000-000000000000/port-forward?port=6379",
            )
            .add_header("Authorization", format!("Bearer {}", session))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    /// The namespace boundary of a token holds here as for exec: an `exec`
    /// token of another namespace reaches no port of this one.
    #[tokio::test]
    async fn a_token_of_another_namespace_cannot_forward() {
        let (server, session, id) = server_with_instance().await;
        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", session))
            .json(&json!({ "name": "other" }))
            .await;
        let pat: Value = server
            .post("/tokens")
            .add_header("Authorization", format!("Bearer {}", session))
            .json(&json!({ "name": "other-exec", "scopes": ["deployments:exec"], "namespaces": ["other"] }))
            .await
            .json();

        let response = server
            .get_websocket(&format!("/deployments/{id}/port-forward?port=6379"))
            .add_header(
                "Authorization",
                format!("Bearer {}", pat["token"].as_str().unwrap()),
            )
            .await;

        assert_eq!(response.status_code(), StatusCode::FORBIDDEN);
    }

    /// Without `deployments:exec`, even a token of the right namespace is
    /// refused: reading deployments does not reach into them.
    #[tokio::test]
    async fn a_token_without_the_exec_scope_cannot_forward() {
        let (server, session, id) = server_with_instance().await;
        let pat: Value = server
            .post("/tokens")
            .add_header("Authorization", format!("Bearer {}", session))
            .json(&json!({ "name": "reader", "scopes": ["deployments:read"], "namespaces": [] }))
            .await
            .json();

        let response = server
            .get_websocket(&format!("/deployments/{id}/port-forward?port=6379"))
            .add_header(
                "Authorization",
                format!("Bearer {}", pat["token"].as_str().unwrap()),
            )
            .await;

        assert_eq!(response.status_code(), StatusCode::FORBIDDEN);
    }
}
