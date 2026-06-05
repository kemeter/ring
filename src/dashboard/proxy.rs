//! Reverse-proxy for `/api/*` requests from the dashboard to the upstream
//! Ring API. Forwards method, path, query string, headers (minus hop-by-hop
//! ones), and body. When the dashboard is in local mode, the operator's
//! bearer token from `auth.json` is injected so the browser never sees it.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, HOST};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use reqwest::Client;
use std::time::Duration;

use super::UpstreamApi;

#[derive(Clone)]
pub(crate) struct ProxyState {
    client: Client,
    upstream: UpstreamApi,
}

impl ProxyState {
    pub(crate) fn new(upstream: UpstreamApi) -> anyhow::Result<Self> {
        let client = Client::builder()
            // Long enough for `--follow` log streams once we wire those up,
            // short enough that a hung backend doesn't pile up sockets.
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self { client, upstream })
    }
}

pub(crate) async fn proxy_handler(State(state): State<ProxyState>, req: Request) -> Response {
    match forward(state, req).await {
        Ok(resp) => resp,
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("dashboard proxy error: {}", e),
        )
            .into_response(),
    }
}

async fn forward(state: ProxyState, req: Request) -> anyhow::Result<Response> {
    let (parts, body) = req.into_parts();
    let body_bytes = body.collect().await?.to_bytes();

    // Strip the `/api` prefix the dashboard uses so the upstream API sees
    // the real path. e.g. `/api/deployments` → `/deployments`.
    let upstream_path = parts
        .uri
        .path()
        .strip_prefix("/api")
        .unwrap_or(parts.uri.path());
    let upstream_query = parts
        .uri
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();
    let upstream_url = format!("{}{}{}", state.upstream.url, upstream_path, upstream_query);

    let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())?;
    let mut builder = state.client.request(method, &upstream_url);

    // Forward headers, dropping hop-by-hop ones the proxy must rewrite.
    let mut filtered = HeaderMap::new();
    for (name, value) in parts.headers.iter() {
        if is_hop_by_hop(name) {
            continue;
        }
        // `Host` belongs to the upstream, not us.
        if name == HOST {
            continue;
        }
        // Drop any inbound Authorization when we have a token to inject,
        // to avoid double-auth scenarios.
        if name == AUTHORIZATION && state.upstream.bearer_token.is_some() {
            continue;
        }
        filtered.insert(name.clone(), value.clone());
    }
    if let Some(token) = &state.upstream.bearer_token {
        filtered.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token))?,
        );
    }
    builder = builder.headers(reqwest_headers(filtered));

    if !body_bytes.is_empty() {
        builder = builder.body(body_bytes.to_vec());
    }

    let resp = builder.send().await?;
    let status = StatusCode::from_u16(resp.status().as_u16())?;
    let resp_headers = resp.headers().clone();
    let resp_bytes = resp.bytes().await?;

    let mut out = Response::builder().status(status);
    for (name, value) in resp_headers.iter() {
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
    Ok(out.body(Body::from(resp_bytes.to_vec()))?)
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
            out.insert(n, v);
        }
    }
    out
}

// Methods imported for `into_parts` on axum::http::Request<Body>.
const _: fn() = || {
    let _ = Method::GET;
    let _ = Uri::from_static("/");
};
