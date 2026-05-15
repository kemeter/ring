//! Centralised authentication.
//!
//! Auth used to be an `impl FromRequestParts for User` that every protected
//! handler triggered by taking a `User` parameter. That made the public/private
//! boundary an implicit per-signature convention and duplicated a DB lookup per
//! request. It also forced the SSE logs route to re-implement auth by hand.
//!
//! Now a single [`auth_middleware`] resolves the caller once (Bearer token OR
//! scoped stream ticket), puts an [`AuthContext`] in the request extensions and
//! short-circuits with 401 otherwise. The router declares which routes the
//! layer covers (see `server::router`), so `/login` and `/healthz` are public
//! by construction, not by accident.
//!
//! Identity provenance matters: a Bearer token grants full access; a stream
//! ticket is bound to a single `deployment:logs:<id>` scope and must NEVER
//! authorise anything else (see [`AuthSource`] and `RequireFullAccess`).

use axum::{
    Json,
    extract::{FromRequestParts, Request, State},
    http::{StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::api::server::AppState;
use crate::models::users as users_model;
use crate::models::users::User;

/// How the caller proved their identity. A ticket-sourced request is only ever
/// valid for the exact log scope the ticket was minted for.
#[derive(Clone, Debug)]
pub(crate) enum AuthSource {
    /// Authenticated with a user Bearer token: full access.
    Bearer,
    /// Authenticated with a stream ticket scoped to this string
    /// (e.g. `deployment:logs:<id>`): logs-only, scope-restricted.
    // `scope` is read by `RequireFullAccess` (currently dead_code until a
    // route adopts it) and is the audit trail of *what* a ticket unlocked;
    // keep it even though no live path reads it yet.
    Ticket {
        #[allow(dead_code)]
        scope: String,
    },
}

/// Resolved identity for the current request, injected by [`auth_middleware`]
/// and read back by the [`User`] extractor and the logs handler.
#[derive(Clone, Debug)]
pub(crate) struct AuthContext {
    pub(crate) user: User,
    /// Identity provenance. Consumed by `RequireFullAccess`; not yet read by a
    /// live route, but it's the security-relevant Bearer-vs-Ticket distinction
    /// and the future audit log's source field.
    #[allow(dead_code)]
    pub(crate) source: AuthSource,
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "Invalid token" })),
    )
        .into_response()
}

fn bearer_token(req: &Request) -> Option<String> {
    let value = req.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(|s| s.to_string())
}

/// Pull `?ticket=<t>` out of the raw query string. We avoid the `Query`
/// extractor here because the middleware must not consume the request body or
/// fail other query parsing — it only cares about this one optional key.
fn ticket_param(req: &Request) -> Option<String> {
    let query = req.uri().query()?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == "ticket")
        .map(|(_, v)| v.into_owned())
}

/// Single auth gate. Applied via `route_layer` to the protected router only;
/// `/login` and `/healthz` live in a router without this layer.
pub(crate) async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    // Bearer wins when present: it's the full-access path.
    if let Some(token) = bearer_token(&req) {
        return match users_model::find_by_token(&state.connection, &token).await {
            Ok(user) => {
                req.extensions_mut().insert(AuthContext {
                    user,
                    source: AuthSource::Bearer,
                });
                next.run(req).await
            }
            Err(_) => unauthorized(),
        };
    }

    // Fall back to a stream ticket. A ticket is only ever valid for the exact
    // `deployment:logs:<id>` scope it was minted for, so we derive the expected
    // scope from the request path and let the store enforce the equality. This
    // keeps the ticket strictly logs-only: a ticket presented on any other
    // path won't match a logs scope and is rejected here.
    if let Some(ticket) = ticket_param(&req) {
        if let Some(expected_scope) = logs_scope_from_path(req.uri().path())
            && let Some(t) = state.ticket_store.consume(&ticket, &expected_scope)
            && let Ok(Some(user)) = users_model::find(&state.connection, &t.user_id).await
        {
            req.extensions_mut().insert(AuthContext {
                user,
                source: AuthSource::Ticket { scope: t.scope },
            });
            return next.run(req).await;
        }
        return unauthorized();
    }

    unauthorized()
}

/// Derive the expected ticket scope from the request path. Only the SSE logs
/// route `/deployments/{id}/logs` can be reached with a ticket; any other path
/// yields `None`, so the ticket can't authorise anything else. The scope
/// string format is owned by `deployment::logs::logs_scope` — we call it here
/// so the two never drift.
fn logs_scope_from_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/deployments/")?;
    let id = rest.strip_suffix("/logs")?;
    // Reject nested/empty segments so only the exact route shape matches.
    if id.is_empty() || id.contains('/') {
        return None;
    }
    Some(crate::api::action::deployment::logs::logs_scope(id))
}

/// `User` is now a thin read of the [`AuthContext`] the middleware installed.
/// No header parsing, no DB hit. Fails CLOSED (500) if the context is missing,
/// which only happens if a `User`-taking handler is mounted on a route the
/// auth layer doesn't cover — a wiring bug we want to surface loudly, never a
/// silent anonymous `User`.
impl<S> FromRequestParts<S> for User
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthContext>()
            .map(|ctx| ctx.user.clone())
            .ok_or_else(|| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "auth context missing: route not behind auth middleware" })),
                )
                    .into_response()
            })
    }
}

/// Extractor for handlers that must reject ticket-scoped identities. Use this
/// (instead of `User`) on any route a stream ticket must not reach. The logs
/// handler does NOT use it: it accepts tickets but checks the scope itself.
#[allow(dead_code)]
pub(crate) struct RequireFullAccess(pub(crate) User);

impl<S> FromRequestParts<S> for RequireFullAccess
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        match parts.extensions.get::<AuthContext>() {
            Some(ctx) => match ctx.source {
                AuthSource::Bearer => Ok(RequireFullAccess(ctx.user.clone())),
                AuthSource::Ticket { .. } => Err(unauthorized()),
            },
            None => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "auth context missing: route not behind auth middleware" })),
            )
                .into_response()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::logs_scope_from_path;
    use crate::api::server::tests::{login, new_test_app};
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    #[test]
    fn scope_only_derived_for_exact_logs_route() {
        assert_eq!(
            logs_scope_from_path("/deployments/abc/logs").as_deref(),
            Some("deployment:logs:abc")
        );
        // Any other path must yield None so a ticket authorises nothing else.
        assert_eq!(logs_scope_from_path("/deployments"), None);
        assert_eq!(logs_scope_from_path("/deployments/abc"), None);
        assert_eq!(logs_scope_from_path("/deployments/abc/events"), None);
        assert_eq!(logs_scope_from_path("/deployments//logs"), None);
        assert_eq!(logs_scope_from_path("/deployments/a/b/logs"), None);
        assert_eq!(logs_scope_from_path("/secrets/abc"), None);
        assert_eq!(logs_scope_from_path("/"), None);
    }

    #[tokio::test]
    async fn public_routes_need_no_auth() {
        let server = TestServer::new(new_test_app().await).unwrap();
        // /healthz and /login live in the router without the auth layer.
        assert_eq!(server.get("/healthz").await.status_code(), StatusCode::OK);
        let login = server
            .post("/login")
            .json(&json!({ "username": "admin", "password": "changeme" }))
            .await;
        assert_eq!(login.status_code(), StatusCode::OK);
    }

    #[tokio::test]
    async fn protected_route_without_token_is_401() {
        let server = TestServer::new(new_test_app().await).unwrap();
        assert_eq!(
            server.get("/deployments").await.status_code(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn unknown_route_is_404_not_401() {
        // The route_layer pitfall: auth must NOT turn a missing route into a
        // 401. No token, nonexistent path → 404.
        let server = TestServer::new(new_test_app().await).unwrap();
        assert_eq!(
            server.get("/does-not-exist").await.status_code(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn valid_bearer_reaches_protected_route() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let res = server
            .get("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(res.status_code(), StatusCode::OK);
    }

    #[tokio::test]
    async fn ticket_authorizes_its_scoped_logs_route() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let mint = server
            .post("/auth/stream-ticket")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "scope": "deployment:logs:dep1" }))
            .await;
        let ticket = mint.json::<serde_json::Value>()["ticket"]
            .as_str()
            .unwrap()
            .to_string();

        // Same deployment id as the ticket scope → middleware lets it through
        // (handler then returns empty logs for an unknown deployment).
        let res = server
            .get("/deployments/dep1/logs")
            .add_query_param("ticket", &ticket)
            .await;
        assert_eq!(res.status_code(), StatusCode::OK);
    }

    #[tokio::test]
    async fn ticket_rejected_on_different_deployment_scope() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let mint = server
            .post("/auth/stream-ticket")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "scope": "deployment:logs:dep1" }))
            .await;
        let ticket = mint.json::<serde_json::Value>()["ticket"]
            .as_str()
            .unwrap()
            .to_string();

        // Ticket minted for dep1 must NOT open dep2's logs.
        let res = server
            .get("/deployments/dep2/logs")
            .add_query_param("ticket", &ticket)
            .await;
        assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn ticket_does_not_authorize_non_logs_route() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let mint = server
            .post("/auth/stream-ticket")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "scope": "deployment:logs:dep1" }))
            .await;
        let ticket = mint.json::<serde_json::Value>()["ticket"]
            .as_str()
            .unwrap()
            .to_string();

        // A ticket is logs-only: presenting it on any other protected route
        // must be rejected (no logs scope derivable from this path).
        let res = server
            .get("/deployments")
            .add_query_param("ticket", &ticket)
            .await;
        assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
    }
}
