//! Embed the SvelteKit static build and serve it over axum. The same code
//! powers both the embedded mode (inside `ring server start`) and the
//! local mode (`ring dashboard`); the only difference is which upstream
//! API the `/api/*` proxy points at.

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use rust_embed::RustEmbed;
use std::net::SocketAddr;
use std::str::FromStr;
use tracing::info;

use super::Mode;
use super::proxy::{ProxyState, proxy_handler};

/// `dashboard/build/` is produced by `bun run build` in the dashboard/
/// directory. We embed every file at compile time so a single Rust binary
/// ships with the UI. CI must run `bun run build` before `cargo build`.
#[derive(RustEmbed)]
#[folder = "dashboard/build/"]
struct Assets;

pub(crate) async fn serve(mode: Mode, listen_address: &str) -> anyhow::Result<()> {
    let proxy_state = ProxyState::new(mode.upstream())?;

    // Single router so `with_state` covers every route. Static routes
    // ignore the state via `State<ProxyState>` extractor on the proxy
    // handler only; asset handlers don't touch it.
    let app: Router = Router::new()
        .route("/", get(index))
        .route("/api/{*path}", any(proxy_handler))
        .route("/{*path}", get(asset))
        .with_state(proxy_state);

    let addr = SocketAddr::from_str(listen_address).map_err(|e| {
        anyhow::anyhow!(
            "Invalid dashboard listen address '{}': {}",
            listen_address,
            e
        )
    })?;

    info!("Dashboard listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn index(State(_): State<ProxyState>) -> Response {
    serve_file("index.html")
}

/// Match the conventions adapter-static produces: `foo` → `foo`,
/// `foo.html`, or `foo/index.html`. Anything we can't resolve falls back
/// to `index.html` so the SPA's client-side router can handle it.
async fn asset(State(_): State<ProxyState>, Path(path): Path<String>, uri: Uri) -> Response {
    if let Some(response) = try_serve(&path) {
        return response;
    }
    let trimmed = uri.path().trim_start_matches('/');
    if !trimmed.is_empty()
        && let Some(response) = try_serve(trimmed)
    {
        return response;
    }
    serve_file("index.html")
}

fn try_serve(path: &str) -> Option<Response> {
    if Assets::get(path).is_some() {
        return Some(serve_file(path));
    }
    let with_html = format!("{path}.html");
    if Assets::get(&with_html).is_some() {
        return Some(serve_file(&with_html));
    }
    let index = format!("{}/index.html", path.trim_end_matches('/'));
    if Assets::get(&index).is_some() {
        return Some(serve_file(&index));
    }
    None
}

fn serve_file(path: &str) -> Response {
    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .body(Body::from(content.data.into_owned()))
                .unwrap_or_else(|_| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "failed to build response",
                    )
                        .into_response()
                })
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
