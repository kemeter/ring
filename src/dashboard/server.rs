//! Embed the SvelteKit static build and serve it over axum. The same code
//! powers both the embedded mode (inside `ring server start`) and the
//! local mode (`ring dashboard`); the only difference is which upstream
//! API the `/api/*` proxy points at.

use axum::Router;
use axum::body::Body;
use axum::extract::{FromRef, Path, State};
use axum::http::{StatusCode, Uri, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use rust_embed::RustEmbed;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use tracing::info;

use super::Mode;
use super::guard::{LocalGuard, guard};
use super::proxy::{ProxyState, proxy_handler};

/// `dashboard/build/` is produced by `bun run build` in the dashboard/
/// directory. We embed every file at compile time so a single Rust binary
/// ships with the UI. CI must run `bun run build` before `cargo build`.
#[derive(RustEmbed)]
#[folder = "dashboard/build/"]
struct Assets;

#[derive(Clone)]
struct DashboardState {
    proxy: ProxyState,
    /// Told to the SPA through a `<meta>` tag, so it knows synchronously,
    /// before any page mounts, whether it has to sign in.
    mode_name: &'static str,
}

impl FromRef<DashboardState> for ProxyState {
    fn from_ref(state: &DashboardState) -> Self {
        state.proxy.clone()
    }
}

pub(crate) async fn serve(mode: Mode, listen_address: &str) -> anyhow::Result<()> {
    let addr = SocketAddr::from_str(listen_address).map_err(|e| {
        anyhow::anyhow!(
            "Invalid dashboard listen address '{}': {}",
            listen_address,
            e
        )
    })?;
    let app = router(&mode, addr)?;

    info!("Dashboard listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

pub(crate) fn router(mode: &Mode, listen: SocketAddr) -> anyhow::Result<Router> {
    let state = DashboardState {
        proxy: ProxyState::new(mode.upstream())?,
        mode_name: match mode {
            Mode::Embedded { .. } => "embedded",
            Mode::Local { .. } => "local",
        },
    };

    let app: Router = Router::new()
        .route("/", get(index))
        .route("/api/{*path}", any(proxy_handler))
        .route("/{*path}", get(asset))
        .with_state(state);

    Ok(match mode {
        Mode::Local { session_secret, .. } => {
            let local_guard = Arc::new(LocalGuard::new(listen, session_secret.clone()));
            app.layer(middleware::from_fn_with_state(local_guard, guard))
        }
        Mode::Embedded { .. } => app,
    })
}

async fn index(State(state): State<DashboardState>) -> Response {
    serve_file("index.html", state.mode_name)
}

/// Match the conventions adapter-static produces: `foo` → `foo`,
/// `foo.html`, or `foo/index.html`. Anything we can't resolve falls back
/// to `index.html` so the SPA's client-side router can handle it.
async fn asset(
    State(state): State<DashboardState>,
    Path(path): Path<String>,
    uri: Uri,
) -> Response {
    if let Some(response) = try_serve(&path, state.mode_name) {
        return response;
    }
    let trimmed = uri.path().trim_start_matches('/');
    if !trimmed.is_empty()
        && let Some(response) = try_serve(trimmed, state.mode_name)
    {
        return response;
    }
    serve_file("index.html", state.mode_name)
}

fn try_serve(path: &str, mode_name: &str) -> Option<Response> {
    if Assets::get(path).is_some() {
        return Some(serve_file(path, mode_name));
    }
    let with_html = format!("{path}.html");
    if Assets::get(&with_html).is_some() {
        return Some(serve_file(&with_html, mode_name));
    }
    let index = format!("{}/index.html", path.trim_end_matches('/'));
    if Assets::get(&index).is_some() {
        return Some(serve_file(&index, mode_name));
    }
    None
}

fn serve_file(path: &str, mode_name: &str) -> Response {
    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let mut data = content.data.into_owned();
            if mime.subtype() == mime_guess::mime::HTML {
                data = with_mode_meta(&data, mode_name);
            }
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .body(Body::from(data))
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

/// Add `<meta name="ring-dashboard-mode">` to an HTML page, read by
/// `dashboard/src/lib/auth.ts`.
fn with_mode_meta(html: &[u8], mode_name: &str) -> Vec<u8> {
    let page = String::from_utf8_lossy(html);
    let meta = format!(r#"<meta name="ring-dashboard-mode" content="{mode_name}"></head>"#);
    page.replacen("</head>", &meta, 1).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::UpstreamApi;
    use crate::dashboard::guard::SESSION_COOKIE;
    use axum::http::HeaderMap;
    use axum::routing::any;
    use axum_test::TestServer;

    const SECRET: &str = "launch-secret";
    const HOST: &str = "127.0.0.1:3031";

    /// An upstream API that answers with the credentials it was sent.
    async fn echo_upstream() -> String {
        async fn echo(headers: HeaderMap) -> String {
            let get = |name: &str| {
                headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-")
                    .to_string()
            };
            format!("auth={} cookie={}", get("authorization"), get("cookie"))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(any(echo)))
                .await
                .unwrap();
        });
        format!("http://{addr}")
    }

    async fn local_dashboard() -> TestServer {
        let mode = Mode::Local {
            upstream: UpstreamApi {
                url: echo_upstream().await,
                bearer_token: Some("operator-token".into()),
            },
            session_secret: SECRET.into(),
        };
        TestServer::new(router(&mode, HOST.parse().unwrap()).unwrap())
    }

    fn session() -> String {
        format!("{SESSION_COOKIE}={SECRET}")
    }

    #[tokio::test]
    async fn proxies_with_the_operator_token_when_the_session_is_valid() {
        let server = local_dashboard().await;
        let response = server
            .get("/api/deployments")
            .add_header("host", HOST)
            .add_header("cookie", format!("other=1; {}", session()))
            .await;
        response.assert_status_ok();
        // The token is injected and the session secret stays on this side.
        response.assert_text("auth=Bearer operator-token cookie=other=1");
    }

    #[tokio::test]
    async fn refuses_a_request_without_the_session_secret() {
        let server = local_dashboard().await;
        for path in ["/api/deployments", "/"] {
            server
                .get(path)
                .add_header("host", HOST)
                .await
                .assert_status(StatusCode::FORBIDDEN);
        }
        server
            .get("/api/deployments")
            .add_header("host", HOST)
            .add_header("cookie", format!("{SESSION_COOKIE}=guess"))
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn refuses_a_foreign_host_even_with_the_session() {
        // What a DNS-rebinding page makes the browser send.
        let server = local_dashboard().await;
        server
            .get("/api/deployments")
            .add_header("host", "evil.example:3031")
            .add_header("cookie", session())
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn refuses_a_foreign_origin_even_with_the_session() {
        let server = local_dashboard().await;
        server
            .delete("/api/deployments/abc")
            .add_header("host", HOST)
            .add_header("origin", "https://evil.example")
            .add_header("cookie", session())
            .await
            .assert_status(StatusCode::FORBIDDEN);
        server
            .get("/api/deployments")
            .add_header("host", HOST)
            .add_header("origin", "http://localhost:3031")
            .add_header("cookie", session())
            .await
            .assert_status_ok();
    }

    #[tokio::test]
    async fn swaps_the_secret_in_the_url_for_a_strict_cookie() {
        let server = local_dashboard().await;
        let response = server
            .get("/deployments")
            .add_query_param("session", SECRET)
            .add_header("host", "localhost:3031")
            .await;
        response.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(response.header("location"), "/deployments");
        let cookie = response.header("set-cookie");
        let cookie = cookie.to_str().unwrap();
        assert!(cookie.starts_with(&session()), "{cookie}");
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("SameSite=Strict"), "{cookie}");

        server
            .get("/")
            .add_query_param("session", "guess")
            .add_header("host", HOST)
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn does_not_inject_the_operator_token_on_login_or_logout() {
        let server = local_dashboard().await;
        for path in ["/api/login", "/api/logout"] {
            server
                .post(path)
                .add_header("host", HOST)
                .add_header("cookie", session())
                .await
                .assert_text("auth=- cookie=-");
        }
    }

    #[tokio::test]
    async fn tells_the_spa_which_mode_it_runs_in() {
        let server = local_dashboard().await;
        let page = server
            .get("/")
            .add_header("host", HOST)
            .add_header("cookie", session())
            .await;
        page.assert_status_ok();
        assert!(
            page.text()
                .contains(r#"<meta name="ring-dashboard-mode" content="local">"#)
        );
    }

    #[tokio::test]
    async fn embedded_mode_needs_no_session() {
        let mode = Mode::Embedded { api_port: 1 };
        let server = TestServer::new(router(&mode, HOST.parse().unwrap()).unwrap());
        let page = server.get("/").await;
        page.assert_status_ok();
        assert!(page.text().contains(r#"content="embedded""#));
    }
}
