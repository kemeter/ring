//! Access control for the local dashboard (`ring dashboard`).
//!
//! The local proxy injects the operator's bearer token into every `/api/*`
//! request, so anything that can reach its port acts as the operator. A
//! loopback bind is not enough on its own: any web page open in the browser
//! can send requests to `127.0.0.1`, and a DNS-rebinding page can even read
//! the answers. Three checks close that:
//!
//! - the `Host` header must name the address the dashboard listens on, which
//!   defeats DNS rebinding (the browser sends the attacker's host name);
//! - an `Origin` header, when present, must be that same address, which
//!   refuses cross-site requests the browser makes on a page's behalf;
//! - every request must carry the secret generated for this launch. The URL
//!   `ring dashboard` opens holds it once as `?session=…`; the dashboard
//!   swaps it for a `SameSite=Strict` cookie and redirects to the bare path,
//!   so the browser never sends it to another site.
//!
//! The embedded dashboard (`ring server start`) injects no credential, so it
//! has nothing to protect this way and runs without the guard.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{COOKIE, HOST, LOCATION, ORIGIN, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rand::RngExt;
use rand::distr::Alphanumeric;
use std::net::SocketAddr;
use std::sync::Arc;

/// Query parameter carrying the secret in the URL `ring dashboard` opens.
pub(crate) const SESSION_PARAM: &str = "session";
/// Cookie the secret is moved to after the first request.
pub(crate) const SESSION_COOKIE: &str = "ring_dashboard_session";

const SECRET_LENGTH: usize = 43;

#[derive(Debug)]
pub(crate) struct LocalGuard {
    secret: String,
    allowed_hosts: Vec<String>,
}

impl LocalGuard {
    pub(crate) fn new(listen: SocketAddr, secret: String) -> Self {
        Self {
            secret,
            allowed_hosts: allowed_hosts(listen),
        }
    }

    pub(crate) fn secret(&self) -> &str {
        &self.secret
    }

    fn host_allowed(&self, host: &str) -> bool {
        self.allowed_hosts
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
    }

    fn origin_allowed(&self, origin: &str) -> bool {
        origin
            .strip_prefix("http://")
            .is_some_and(|host| self.host_allowed(host))
    }

    fn matches_secret(&self, candidate: &str) -> bool {
        constant_time_eq(candidate.as_bytes(), self.secret.as_bytes())
    }
}

/// Compare without leaking, through timing, how long a matching prefix is.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A fresh random secret for one `ring dashboard` launch.
pub(crate) fn generate_secret() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(SECRET_LENGTH)
        .map(char::from)
        .collect()
}

/// The `Host` values a browser sends for this listen address. A loopback or
/// wildcard bind is reached as `localhost` too, so both spellings pass.
fn allowed_hosts(listen: SocketAddr) -> Vec<String> {
    let port = listen.port();
    let mut hosts = vec![listen.to_string()];
    if listen.ip().is_loopback() || listen.ip().is_unspecified() {
        for name in ["localhost", "127.0.0.1", "[::1]"] {
            let host = format!("{name}:{port}");
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }
    }
    hosts
}

pub(crate) async fn guard(
    State(guard): State<Arc<LocalGuard>>,
    req: Request,
    next: Next,
) -> Response {
    let host = req
        .headers()
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| req.uri().authority().map(|a| a.to_string()));
    if !host.as_deref().is_some_and(|h| guard.host_allowed(h)) {
        return forbidden("This dashboard only answers on the address it listens on.");
    }

    if let Some(origin) = req.headers().get(ORIGIN) {
        let allowed = origin.to_str().is_ok_and(|o| guard.origin_allowed(o));
        if !allowed {
            return forbidden("Cross-origin requests are refused.");
        }
    }

    if let Some(candidate) = query_param(req.uri().query(), SESSION_PARAM) {
        if !guard.matches_secret(&candidate) {
            return forbidden(OPEN_FROM_CLI);
        }
        return Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header(LOCATION, req.uri().path())
            .header(
                SET_COOKIE,
                format!(
                    "{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict",
                    guard.secret()
                ),
            )
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    let has_session = session_cookie(req.headers()).is_some_and(|c| guard.matches_secret(&c));
    if !has_session {
        return forbidden(OPEN_FROM_CLI);
    }

    next.run(req).await
}

const OPEN_FROM_CLI: &str =
    "Open the dashboard with the link `ring dashboard` printed; it holds this session's secret.";

fn forbidden(message: &str) -> Response {
    (StatusCode::FORBIDDEN, message.to_string()).into_response()
}

fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (key == name).then(|| value.to_string())
    })
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == SESSION_COOKIE).then(|| value.to_string())
        })
}

/// Remove the dashboard's own cookie from a request about to be forwarded,
/// so the session secret never reaches the upstream API.
pub(crate) fn strip_session_cookie(headers: &mut HeaderMap) {
    let kept: Vec<String> = headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .filter(|pair| {
            !pair.is_empty()
                && pair
                    .split_once('=')
                    .is_none_or(|(key, _)| key != SESSION_COOKIE)
        })
        .map(str::to_owned)
        .collect();
    headers.remove(COOKIE);
    if !kept.is_empty()
        && let Ok(value) = kept.join("; ").parse()
    {
        headers.insert(COOKIE, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_bind_accepts_localhost_spellings() {
        let hosts = allowed_hosts("127.0.0.1:3031".parse().unwrap());
        assert_eq!(
            hosts,
            vec!["127.0.0.1:3031", "localhost:3031", "[::1]:3031"]
        );
    }

    #[test]
    fn routable_bind_accepts_only_its_own_address() {
        let hosts = allowed_hosts("192.168.1.10:3031".parse().unwrap());
        assert_eq!(hosts, vec!["192.168.1.10:3031"]);
    }

    #[test]
    fn origin_must_be_plain_http_on_an_allowed_host() {
        let guard = LocalGuard::new("127.0.0.1:3031".parse().unwrap(), "s".into());
        assert!(guard.origin_allowed("http://localhost:3031"));
        assert!(!guard.origin_allowed("http://localhost:3032"));
        assert!(!guard.origin_allowed("https://evil.example"));
        assert!(!guard.origin_allowed("null"));
    }

    #[test]
    fn strip_session_cookie_keeps_the_others() {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            format!("a=1; {SESSION_COOKIE}=secret; b=2")
                .parse()
                .unwrap(),
        );
        strip_session_cookie(&mut headers);
        assert_eq!(headers.get(COOKIE).unwrap(), "a=1; b=2");

        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, format!("{SESSION_COOKIE}=secret").parse().unwrap());
        strip_session_cookie(&mut headers);
        assert!(headers.get(COOKIE).is_none());
    }

    #[test]
    fn generated_secrets_differ() {
        assert_ne!(generate_secret(), generate_secret());
        assert_eq!(generate_secret().len(), SECRET_LENGTH);
    }
}
