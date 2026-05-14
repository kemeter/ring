//! Web dashboard for Ring.
//!
//! Two modes share the same SvelteKit build (`dashboard/build/`):
//!
//! - [`Mode::Embedded`] — when `ring server start` is configured with
//!   `[dashboard] enabled = true`, the server itself serves the dashboard
//!   on a dedicated port. The dashboard's `/api/*` requests are forwarded
//!   to the same Ring instance over loopback, since the API and dashboard
//!   live in the same process.
//! - [`Mode::Local`] — `ring dashboard` boots a tiny axum server on the
//!   user's laptop. It serves the same embedded assets and reverse-proxies
//!   `/api/*` to a remote Ring API, injecting the user's bearer token from
//!   `auth.json`. Lets one operator monitor any cluster without exposing
//!   the dashboard from the server.
//!
//! The bundled assets are baked at compile time by `rust-embed`, so a
//! single binary ships with the UI regardless of mode.

pub(crate) mod proxy;
pub(crate) mod server;

#[derive(Clone, Debug)]
pub(crate) struct UpstreamApi {
    /// Base URL of the Ring API, e.g. `http://prod-ring.internal:3030`.
    pub url: String,
    /// Bearer token to inject on every proxied request. None when the
    /// dashboard is embedded in the same server (no auth needed
    /// internally — the browser already authenticated against the API).
    pub bearer_token: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum Mode {
    /// Run alongside the API in `ring server start`. Proxies to localhost.
    Embedded { api_port: u16 },
    /// Run on the operator's machine via `ring dashboard`. Proxies to a
    /// remote API with an injected token.
    Local { upstream: UpstreamApi },
}

impl Mode {
    pub(crate) fn upstream(&self) -> UpstreamApi {
        match self {
            Mode::Embedded { api_port } => UpstreamApi {
                url: format!("http://127.0.0.1:{}", api_port),
                bearer_token: None,
            },
            Mode::Local { upstream } => upstream.clone(),
        }
    }
}
