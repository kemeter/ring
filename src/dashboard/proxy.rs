//! Reverse-proxy for `/api/*` requests from the dashboard to the upstream
//! Ring API. Forwards method, path, query string, headers (minus hop-by-hop
//! ones), and body. When the dashboard is in local mode, the operator's
//! bearer token from `auth.json` is injected so the browser never sees it.
//!
//! Responses are streamed rather than buffered, so a followed log stream
//! (SSE) reaches the browser line by line, and WebSocket upgrades (the exec
//! terminal) are relayed frame by frame to an upstream WebSocket.

use axum::body::Body;
use axum::extract::ws::{self, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{AUTHORIZATION, HOST, UPGRADE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use reqwest::Client;
use std::time::Duration;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::UpstreamApi;

/// How long to wait for the upstream API to accept a connection. There is
/// deliberately no limit on the whole request: a followed log stream or an
/// exec session stays open as long as the user watches it. The API bounds
/// its ordinary requests itself.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct ProxyState {
    client: Client,
    upstream: UpstreamApi,
}

impl ProxyState {
    pub(crate) fn new(upstream: UpstreamApi) -> anyhow::Result<Self> {
        let client = Client::builder().connect_timeout(CONNECT_TIMEOUT).build()?;
        Ok(Self { client, upstream })
    }
}

pub(crate) async fn proxy_handler(State(state): State<ProxyState>, req: Request) -> Response {
    let result = if is_websocket_upgrade(req.headers()) {
        let (mut parts, _) = req.into_parts();
        forward_websocket(state, &mut parts).await
    } else {
        forward(state, req).await
    };
    match result {
        Ok(resp) => resp,
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("dashboard proxy error: {}", e),
        )
            .into_response(),
    }
}

/// The upstream URL for a dashboard path: `/api/deployments?x` becomes
/// `{upstream}/deployments?x`.
fn upstream_url(base: &str, parts: &Parts) -> String {
    let path = parts
        .uri
        .path()
        .strip_prefix("/api")
        .unwrap_or(parts.uri.path());
    let query = parts
        .uri
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();
    format!("{}{}{}", base, path, query)
}

/// The request headers to send upstream: hop-by-hop ones, `Host` and the
/// dashboard's session cookie dropped, and the operator's token in place of
/// whatever the browser sent, when there is one to inject.
fn upstream_headers(state: &ProxyState, parts: &Parts) -> anyhow::Result<HeaderMap> {
    let inbound = &parts.headers;
    // `/login` and `/logout` act on the credential they carry: injecting the
    // operator's token would let the page revoke it, so they go upstream
    // with only what the browser sent.
    let path = parts
        .uri
        .path()
        .strip_prefix("/api")
        .unwrap_or(parts.uri.path());
    let inject_token = !matches!(path, "/login" | "/logout");

    let mut filtered = HeaderMap::new();
    for (name, value) in inbound.iter() {
        if is_hop_by_hop(name) || name == HOST {
            continue;
        }
        // Drop any inbound Authorization when we have a token to inject,
        // to avoid double-auth scenarios.
        if name == AUTHORIZATION && state.upstream.bearer_token.is_some() {
            continue;
        }
        filtered.append(name.clone(), value.clone());
    }
    super::guard::strip_session_cookie(&mut filtered);
    if let Some(token) = state
        .upstream
        .bearer_token
        .as_ref()
        .filter(|_| inject_token)
    {
        filtered.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token))?,
        );
    }
    Ok(filtered)
}

async fn forward(state: ProxyState, req: Request) -> anyhow::Result<Response> {
    let (parts, body) = req.into_parts();
    // Request bodies are small JSON documents: buffering them is simpler
    // than streaming and lets reqwest set Content-Length.
    let body_bytes = body.collect().await?.to_bytes();

    let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())?;
    let mut builder = state
        .client
        .request(method, upstream_url(&state.upstream.url, &parts))
        .headers(reqwest_headers(upstream_headers(&state, &parts)?));

    if !body_bytes.is_empty() {
        builder = builder.body(body_bytes.to_vec());
    }

    let resp = builder.send().await?;
    let status = StatusCode::from_u16(resp.status().as_u16())?;

    let mut out = Response::builder().status(status);
    for (name, value) in resp.headers().iter() {
        // Re-cast reqwest headers into axum's HeaderMap. They share the
        // underlying type but the crate paths differ.
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_str().as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            // Skip hop-by-hop response headers too.
            if !is_hop_by_hop(&n) {
                out = out.header(n, v);
            }
        }
    }
    Ok(out.body(Body::from_stream(resp.bytes_stream()))?)
}

/// Open the same WebSocket upstream, then relay frames both ways until
/// either side closes. The upstream handshake happens first, so a refusal
/// (401, 404, 429…) reaches the browser as that HTTP status instead of as a
/// socket that opens and dies.
async fn forward_websocket(state: ProxyState, parts: &mut Parts) -> anyhow::Result<Response> {
    let upgrade = match WebSocketUpgrade::from_request_parts(parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(rejection) => return Ok(rejection.into_response()),
    };

    let url = upstream_url(&state.upstream.url, parts);
    let url = match url.split_once("://") {
        Some(("https", rest)) => format!("wss://{rest}"),
        Some((_, rest)) => format!("ws://{rest}"),
        None => url,
    };
    let mut request = url.into_client_request()?;
    for (name, value) in upstream_headers(&state, parts)? {
        let Some(name) = name else { continue };
        // tungstenite writes its own handshake headers.
        if name.as_str().starts_with("sec-websocket-") {
            continue;
        }
        if let (Ok(n), Ok(v)) = (
            tungstenite::http::HeaderName::from_bytes(name.as_str().as_bytes()),
            tungstenite::http::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            request.headers_mut().insert(n, v);
        }
    }

    let (upstream, handshake) = match tokio_tungstenite::connect_async(request).await {
        Ok(pair) => pair,
        Err(tungstenite::Error::Http(refusal)) => {
            let status = StatusCode::from_u16(refusal.status().as_u16())?;
            let body = refusal.body().clone().unwrap_or_default();
            let mut out = Response::builder().status(status);
            if let Some(content_type) = refusal.headers().get("content-type") {
                out = out.header("content-type", content_type.as_bytes());
            }
            return Ok(out.body(Body::from(body))?);
        }
        Err(e) => return Err(e.into()),
    };

    let mut response = upgrade
        .on_upgrade(move |browser| relay(browser, upstream))
        .into_response();
    // Pass on what the API says about the session, such as which instance
    // an exec landed in.
    for (name, value) in handshake.headers() {
        let name = name.as_str();
        if name.starts_with("x-")
            && let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_bytes(value.as_bytes()),
            )
        {
            response.headers_mut().insert(n, v);
        }
    }
    Ok(response)
}

type UpstreamSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn relay(browser: WebSocket, upstream: UpstreamSocket) {
    let (mut to_browser, mut from_browser) = browser.split();
    let (mut to_upstream, mut from_upstream) = upstream.split();

    let downstream = async {
        while let Some(Ok(message)) = from_upstream.next().await {
            let Some(message) = to_axum(message) else {
                continue;
            };
            let closing = matches!(message, ws::Message::Close(_));
            if to_browser.send(message).await.is_err() || closing {
                break;
            }
        }
        let _ = to_browser.close().await;
    };
    let upstream_bound = async {
        while let Some(Ok(message)) = from_browser.next().await {
            let message = to_tungstenite(message);
            let closing = matches!(message, tungstenite::Message::Close(_));
            if to_upstream.send(message).await.is_err() || closing {
                break;
            }
        }
        let _ = to_upstream.close().await;
    };

    // Whichever side goes away first ends the session for both.
    tokio::select! {
        _ = downstream => {}
        _ = upstream_bound => {}
    }
}

fn to_axum(message: tungstenite::Message) -> Option<ws::Message> {
    Some(match message {
        tungstenite::Message::Text(text) => ws::Message::Text(text.as_str().into()),
        tungstenite::Message::Binary(data) => ws::Message::Binary(data),
        tungstenite::Message::Ping(data) => ws::Message::Ping(data),
        tungstenite::Message::Pong(data) => ws::Message::Pong(data),
        tungstenite::Message::Close(frame) => {
            ws::Message::Close(frame.map(|frame| ws::CloseFrame {
                code: frame.code.into(),
                reason: frame.reason.as_str().into(),
            }))
        }
        // Raw frames only show up when writing, never when reading.
        tungstenite::Message::Frame(_) => return None,
    })
}

fn to_tungstenite(message: ws::Message) -> tungstenite::Message {
    match message {
        ws::Message::Text(text) => tungstenite::Message::Text(text.as_str().into()),
        ws::Message::Binary(data) => tungstenite::Message::Binary(data),
        ws::Message::Ping(data) => tungstenite::Message::Ping(data),
        ws::Message::Pong(data) => tungstenite::Message::Pong(data),
        ws::Message::Close(frame) => {
            tungstenite::Message::Close(frame.map(|frame| tungstenite::protocol::CloseFrame {
                code: frame.code.into(),
                reason: frame.reason.as_str().into(),
            }))
        }
    }
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

/// Per RFC 7230 §6.1, these headers apply only to the immediate connection
/// and must not be forwarded by a proxy.
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn reqwest_headers(headers: HeaderMap) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::new();
    for (name, value) in headers.iter() {
        if let (Ok(n), Ok(v)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            out.append(n, v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::{any, get};
    use std::convert::Infallible;

    async fn spawn(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("127.0.0.1:{}", addr.port())
    }

    /// A proxy in front of `upstream`, signing requests with `operator-token`.
    async fn spawn_proxy(upstream: &str) -> String {
        let state = ProxyState::new(UpstreamApi {
            url: format!("http://{upstream}"),
            bearer_token: Some("operator-token".into()),
        })
        .unwrap();
        spawn(
            Router::new()
                .route("/api/{*path}", any(proxy_handler))
                .with_state(state),
        )
        .await
    }

    #[tokio::test]
    async fn streams_a_response_that_never_ends() {
        // Like a followed log stream: one event, then nothing, forever.
        let upstream = spawn(Router::new().route(
            "/logs",
            get(|| async {
                let first = futures::stream::once(async {
                    Ok::<_, Infallible>(axum::body::Bytes::from("data: first\n\n"))
                });
                Body::from_stream(first.chain(futures::stream::pending()))
            }),
        ))
        .await;
        let proxy = spawn_proxy(&upstream).await;

        let mut response = reqwest::get(format!("http://{proxy}/api/logs"))
            .await
            .unwrap();
        let chunk = tokio::time::timeout(Duration::from_secs(5), response.chunk())
            .await
            .expect("the first event must arrive before the stream ends")
            .unwrap()
            .unwrap();
        assert_eq!(&chunk[..], b"data: first\n\n");
    }

    #[tokio::test]
    async fn relays_a_websocket_with_the_operator_token() {
        // Echoes every frame, prefixed with the Authorization it was opened with.
        let upstream = spawn(Router::new().route(
            "/exec",
            get(|upgrade: WebSocketUpgrade, headers: HeaderMap| async move {
                let auth = headers
                    .get(AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-")
                    .to_string();
                let mut response = upgrade
                    .on_upgrade(move |mut socket| async move {
                        while let Some(Ok(ws::Message::Text(text))) = socket.recv().await {
                            let reply = format!("{auth} {}", text.as_str());
                            if socket.send(ws::Message::Text(reply.into())).await.is_err() {
                                break;
                            }
                        }
                    })
                    .into_response();
                response
                    .headers_mut()
                    .insert("x-ring-exec-instance", HeaderValue::from_static("web-1"));
                response
            }),
        ))
        .await;
        let proxy = spawn_proxy(&upstream).await;

        let (mut socket, handshake) =
            tokio_tungstenite::connect_async(format!("ws://{proxy}/api/exec"))
                .await
                .unwrap();
        assert_eq!(handshake.headers()["x-ring-exec-instance"], "web-1");

        socket
            .send(tungstenite::Message::Text("ls".into()))
            .await
            .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(reply.to_text().unwrap(), "Bearer operator-token ls");
    }

    #[tokio::test]
    async fn passes_on_an_upstream_websocket_refusal() {
        let upstream = spawn(Router::new().route(
            "/exec",
            get(|| async { (StatusCode::NOT_FOUND, "no such deployment") }),
        ))
        .await;
        let proxy = spawn_proxy(&upstream).await;

        match tokio_tungstenite::connect_async(format!("ws://{proxy}/api/exec")).await {
            Err(tungstenite::Error::Http(response)) => {
                assert_eq!(response.status().as_u16(), 404);
                assert_eq!(response.body().as_deref(), Some(&b"no such deployment"[..]));
            }
            other => panic!("expected a 404 refusal, got {other:?}"),
        }
    }
}
