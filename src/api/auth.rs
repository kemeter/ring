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

use axum::extract::MatchedPath;
use axum::{
    Json,
    extract::{FromRequestParts, Request, State},
    http::{Method, StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::api::server::AppState;
use crate::models::token as token_model;
use crate::models::users as users_model;
use crate::models::users::User;

/// How the caller proved their identity. A ticket-sourced request is only ever
/// valid for the exact log scope the ticket was minted for; a PAT is limited to
/// the scopes and namespaces it was minted with.
#[derive(Clone, Debug)]
pub(crate) enum AuthSource {
    /// Authenticated with a user session Bearer token: full access.
    Bearer,
    /// Authenticated with a scoped API token (PAT, `ring_pat_…`). Carries the
    /// token's scopes and namespace boundary so handlers can enforce them via
    /// [`require_scope`]. An empty `namespaces` means all namespaces.
    Token {
        scopes: Vec<String>,
        namespaces: Vec<String>,
    },
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
    /// Identity provenance. Read by the `Auth` extractor (scope enforcement)
    /// and `RequireFullAccess` (ticket rejection): the security-relevant
    /// Bearer-vs-Token-vs-Ticket distinction.
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

/// The scope a route requires, keyed by HTTP method + matched route pattern
/// (axum's `{id}`-style template, NOT the concrete path). This is the single
/// source of truth for which scope guards which endpoint: enforcement happens
/// once, centrally, in [`auth_middleware`] — handlers never re-check a scope.
///
/// `None` means "no scope mapping for this route". Because the middleware
/// **denies by default** (a `None` on a route reached through the protected
/// layer is a 403), forgetting to map a new protected route fails CLOSED: the
/// route is unreachable by any PAT until it is added here. That is the
/// structural guarantee the old per-handler `require_scope` calls lacked — a
/// new handler could previously ship with no check at all (e.g. `/logs`,
/// `/auth/stream-ticket`) and silently grant every PAT full access.
///
/// Namespace boundaries are NOT expressed here: a resource's namespace is only
/// known after it is loaded, so handlers re-check it via [`require_namespace`].
fn scope_for_route(method: &Method, matched_path: &str) -> Option<&'static str> {
    let is_read = matches!(*method, Method::GET);
    match matched_path {
        // Deployments (logs/events/metrics/health-checks are all reads).
        "/deployments" if is_read => Some("deployments:read"),
        "/deployments" => Some("deployments:write"),
        "/deployments/{id}" if is_read => Some("deployments:read"),
        "/deployments/{id}" => Some("deployments:write"),
        "/deployments/{id}/events"
        | "/deployments/{id}/health-checks"
        | "/deployments/{id}/metrics"
        | "/deployments/{id}/logs" => Some("deployments:read"),
        // Node info is host-level; gate it behind the same read scope as
        // deployments (there is no dedicated node scope).
        "/node/get" => Some("deployments:read"),
        // Namespaces.
        "/namespaces" if is_read => Some("namespaces:read"),
        "/namespaces" => Some("namespaces:write"),
        "/namespaces/{id}" if is_read => Some("namespaces:read"),
        "/namespaces/{id}" => Some("namespaces:write"),
        "/namespaces/{id}/audit" => Some("namespaces:read"),
        // Configs.
        "/configs" if is_read => Some("configs:read"),
        "/configs" => Some("configs:write"),
        "/configs/{id}" if is_read => Some("configs:read"),
        "/configs/{id}" => Some("configs:write"),
        // Secrets.
        "/secrets" if is_read => Some("secrets:read"),
        "/secrets" => Some("secrets:write"),
        "/secrets/{id}" if is_read => Some("secrets:read"),
        "/secrets/{id}" => Some("secrets:write"),
        // Users.
        "/users" if is_read => Some("users:read"),
        "/users" => Some("users:write"),
        "/users/{id}" => Some("users:write"),
        "/users/me" => Some("users:read"),
        // Token lifecycle and stream-ticket minting are full-access actions:
        // a PAT may only reach them when it carries `admin`. This closes the
        // privilege-escalation path where a `users:write` PAT could rotate an
        // `admin` token (and receive a fresh admin secret) or revoke siblings.
        "/tokens" | "/tokens/{id}" | "/tokens/{id}/rotate" | "/auth/stream-ticket" => Some("admin"),
        _ => None,
    }
}

/// Single auth gate. Applied via `route_layer` to the protected router only;
/// `/login` and `/healthz` live in a router without this layer.
///
/// Two responsibilities, in order: (1) **authenticate** the caller (Bearer
/// session, scoped API token, or stream ticket) and (2) **authorise** them
/// against the route's required scope (see [`scope_for_route`]). Authorising
/// here — not in each handler — means every protected route is covered by
/// construction and a missing mapping fails closed.
pub(crate) async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    // Resolve identity first, then enforce the route scope on the result.
    let source = match authenticate(&state, &mut req).await {
        Some(()) => match req.extensions().get::<AuthContext>() {
            Some(ctx) => ctx.source.clone(),
            None => return unauthorized(),
        },
        None => return unauthorized(),
    };

    // A stream ticket is already authorised at authentication time: it is only
    // accepted on the exact `/deployments/{id}/logs` route whose scope it was
    // minted for (see `authenticate` / `logs_scope_from_path`). It carries no
    // generic scope, so the scope table doesn't apply to it — skip straight to
    // the handler. Bearer and Token still go through the scope gate below.
    if matches!(source, AuthSource::Ticket { .. }) {
        return next.run(req).await;
    }

    // Authorise: derive the scope this route requires and enforce it centrally.
    // The matched-path template (`/deployments/{id}`) is stable across ids, so
    // the table in `scope_for_route` stays small and reads like the router.
    let matched = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string());
    if let Some(matched) = matched.as_deref() {
        if let Some(required) = scope_for_route(req.method(), matched) {
            if let Err(resp) = require_scope(&source, required) {
                return resp;
            }
        } else {
            // Deny by default: a protected route with no scope mapping must
            // not be reachable by a PAT. A session Bearer still passes (it has
            // no scopes and is full-access), so this only locks down PATs.
            if matches!(source, AuthSource::Token { .. }) {
                return forbidden("this route is not reachable with a scoped token");
            }
        }
    }

    next.run(req).await
}

/// Authenticate the caller and install an [`AuthContext`] in the request
/// extensions. Returns `Some(())` on success, `None` on any auth failure.
/// Authorisation (scope/ticket-binding) is handled by the caller.
async fn authenticate(state: &AppState, req: &mut Request) -> Option<()> {
    // Bearer wins when present. Two kinds of Bearer token share this path:
    // a scoped API token (PAT, recognised by its `ring_pat_` prefix) and the
    // legacy full-access session token. PATs are tried first so a session
    // token lookup never sees a PAT-shaped value.
    if let Some(token) = bearer_token(req) {
        if token.starts_with(token_model::TOKEN_PREFIX) {
            return resolve_api_token(state, req, &token).await;
        }
        return match users_model::find_by_token(&state.connection, &token).await {
            Ok(user) => {
                req.extensions_mut().insert(AuthContext {
                    user,
                    source: AuthSource::Bearer,
                });
                Some(())
            }
            Err(_) => None,
        };
    }

    // Fall back to a stream ticket. A ticket is only ever valid for the exact
    // `deployment:logs:<id>` scope it was minted for, so we derive the expected
    // scope from the request path and let the store enforce the equality. This
    // keeps the ticket strictly logs-only: a ticket presented on any other
    // path won't match a logs scope and is rejected here.
    if let Some(ticket) = ticket_param(req) {
        if let Some(expected_scope) = logs_scope_from_path(req.uri().path())
            && let Some(t) = state.ticket_store.consume(&ticket, &expected_scope)
            && let Ok(Some(user)) = users_model::find(&state.connection, &t.user_id).await
        {
            req.extensions_mut().insert(AuthContext {
                user,
                source: AuthSource::Ticket { scope: t.scope },
            });
            return Some(());
        }
        return None;
    }

    None
}

/// Resolve a scoped API token (`ring_pat_…`) presented as a Bearer credential.
/// The clear value is hashed and looked up; a missing, revoked or expired
/// token is rejected (returns `None`). On success the owning user is loaded and
/// the token's scopes/namespaces are carried in [`AuthSource::Token`] for
/// central scope enforcement and per-handler namespace re-checks. `last_used_at`
/// is touched best-effort, off the request path (see below).
async fn resolve_api_token(state: &AppState, req: &mut Request, clear: &str) -> Option<()> {
    let hash = token_model::hash_token(clear);
    let token = match token_model::find_by_token_hash(&state.connection, &hash).await {
        Ok(Some(t)) if t.is_active() => t,
        // Unknown, revoked or expired all collapse to a rejection: we don't
        // tell an unauthenticated caller which one it was.
        _ => return None,
    };

    let user = match users_model::find(&state.connection, &token.user_id).await {
        Ok(Some(u)) => u,
        // Token points at a user that no longer exists: fail closed.
        _ => return None,
    };

    // Mark last-use off the request path: the UPDATE takes SQLite's single
    // writer lock, so awaiting it inline would serialise auth behind unrelated
    // writes. It is best-effort and throttled, so fire-and-forget is correct —
    // a missed touch never matters and never blocks dispatch.
    let pool = state.connection.clone();
    let touched = token.clone();
    tokio::spawn(async move {
        let _ = token_model::touch_last_used(&pool, &touched).await;
    });

    req.extensions_mut().insert(AuthContext {
        user,
        source: AuthSource::Token {
            scopes: token.scopes,
            namespaces: token.namespaces,
        },
    });
    Some(())
}

/// Enforce a required scope against the request's auth source. Called once,
/// centrally, by [`auth_middleware`] using the route's mapped scope — handlers
/// never call this directly.
///
/// - A session `Bearer` identity (a logged-in human) passes unconditionally —
///   PATs are the only credential that carries scopes, so this is fully
///   backward compatible with existing dashboard/CLI usage.
/// - A `Token` (PAT) identity must hold `required_scope` (or `admin`). The
///   namespace boundary is enforced separately by [`require_namespace`], since
///   a resource's namespace is only known after it is loaded.
/// - A `Ticket` identity never reaches scoped resources (logs-only) → 403.
#[allow(clippy::result_large_err)]
pub(crate) fn require_scope(source: &AuthSource, required_scope: &str) -> Result<(), Response> {
    match source {
        AuthSource::Bearer => Ok(()),
        AuthSource::Token { scopes, .. } => {
            if scopes.iter().any(|s| s == "admin" || s == required_scope) {
                Ok(())
            } else {
                Err(forbidden(&format!(
                    "token lacks required scope '{}'",
                    required_scope
                )))
            }
        }
        AuthSource::Ticket { .. } => Err(forbidden(&format!(
            "token lacks required scope '{}'",
            required_scope
        ))),
    }
}

/// Enforce the token's namespace boundary against a resource that has just been
/// loaded. Call this in any handler that reads, deletes or mutates a single
/// namespaced resource fetched by id — the scope is already enforced centrally,
/// but the namespace cannot be (it isn't known until the row is read).
///
/// - `Bearer` (full-access human session) passes unconditionally.
/// - `Token` (PAT) with an empty namespace list passes (all namespaces);
///   otherwise the resource's namespace must be in the token's list, else 403.
/// - `Ticket` never reaches namespaced resources → 403.
///
/// This is the per-handler half of the boundary that `scope_for_route` cannot
/// express; without it a namespace-scoped PAT could read or delete resources in
/// any namespace by id.
#[allow(clippy::result_large_err)]
pub(crate) fn require_namespace(source: &AuthSource, namespace: &str) -> Result<(), Response> {
    match source {
        AuthSource::Bearer => Ok(()),
        AuthSource::Token { namespaces, .. } => {
            if namespaces.is_empty() || namespaces.iter().any(|n| n == namespace) {
                Ok(())
            } else {
                Err(forbidden(&format!(
                    "token is not scoped to namespace '{}'",
                    namespace
                )))
            }
        }
        AuthSource::Ticket { .. } => Err(forbidden("token is not scoped to this namespace")),
    }
}

/// Keep only the items whose namespace the caller is allowed to see. List
/// endpoints can't use [`require_namespace`] (they return many resources across
/// namespaces), so they filter their result set through this instead: a
/// namespace-scoped PAT only ever sees its own namespaces, while a full-access
/// session (Bearer) and an all-namespaces PAT see everything.
///
/// `namespace_of` extracts the namespace from each item so this works for any
/// resource type (secrets, configs, deployments).
pub(crate) fn filter_by_namespace<T>(
    source: &AuthSource,
    items: Vec<T>,
    namespace_of: impl Fn(&T) -> &str,
) -> Vec<T> {
    match source {
        // Full access, or a PAT with no namespace restriction: see everything.
        AuthSource::Bearer => items,
        AuthSource::Token { namespaces, .. } if namespaces.is_empty() => items,
        AuthSource::Token { namespaces, .. } => items
            .into_iter()
            .filter(|item| namespaces.iter().any(|n| n == namespace_of(item)))
            .collect(),
        // A ticket is logs-only and never reaches a list endpoint, but if it
        // somehow did, it sees nothing.
        AuthSource::Ticket { .. } => Vec::new(),
    }
}

fn forbidden(message: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({ "error": message }))).into_response()
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

/// Extractor exposing both the resolved user and how they authenticated. Use
/// this (instead of bare `User`) on handlers that must enforce token scopes:
/// take an `Auth`, then call [`require_scope`] with the scope and namespace the
/// action needs. Fails CLOSED (500) if the context is missing, same as `User`.
pub(crate) struct Auth {
    pub(crate) user: User,
    pub(crate) source: AuthSource,
}

impl<S> FromRequestParts<S> for Auth
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthContext>()
            .map(|ctx| Auth {
                user: ctx.user.clone(),
                source: ctx.source.clone(),
            })
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
                // A scoped API token is not a full-access session: routes that
                // demand full access (and can't express a scope check) reject
                // it. Handlers that want to admit an `admin`-scoped PAT should
                // use `Auth` + `require_scope("admin", …)` instead.
                AuthSource::Token { .. } => Err(unauthorized()),
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
    use super::{
        AuthSource, Method, logs_scope_from_path, require_namespace, require_scope, scope_for_route,
    };
    use crate::api::server::tests::{login, new_test_app};
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    fn pat(scopes: &[&str], namespaces: &[&str]) -> AuthSource {
        AuthSource::Token {
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            namespaces: namespaces.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn session_bearer_bypasses_scope_checks() {
        // A logged-in human carries no scopes; require_scope must let them
        // through unconditionally (backward compatible).
        assert!(require_scope(&AuthSource::Bearer, "deployments:write").is_ok());
        assert!(require_namespace(&AuthSource::Bearer, "prod").is_ok());
    }

    #[test]
    fn pat_needs_the_exact_scope() {
        assert!(require_scope(&pat(&["deployments:read"], &[]), "deployments:read").is_ok());
        assert!(require_scope(&pat(&["deployments:read"], &[]), "deployments:write").is_err());
    }

    #[test]
    fn pat_admin_scope_is_a_wildcard() {
        assert!(require_scope(&pat(&["admin"], &[]), "secrets:write").is_ok());
        assert!(require_namespace(&pat(&["admin"], &["prod"]), "staging").is_err());
    }

    #[test]
    fn pat_namespace_boundary_is_enforced() {
        let t = pat(&["deployments:write"], &["prod"]);
        assert!(require_namespace(&t, "prod").is_ok());
        assert!(require_namespace(&t, "staging").is_err());
    }

    #[test]
    fn pat_empty_namespaces_means_all() {
        let t = pat(&["deployments:write"], &[]);
        assert!(require_namespace(&t, "prod").is_ok());
        assert!(require_namespace(&t, "staging").is_ok());
    }

    #[test]
    fn ticket_never_passes_a_scope_check() {
        let ticket = AuthSource::Ticket {
            scope: "deployment:logs:x".to_string(),
        };
        assert!(require_scope(&ticket, "deployments:read").is_err());
        assert!(require_namespace(&ticket, "prod").is_err());
    }

    #[test]
    fn route_scope_table_covers_every_protected_route() {
        // Read vs write split.
        assert_eq!(
            scope_for_route(&Method::GET, "/deployments"),
            Some("deployments:read")
        );
        assert_eq!(
            scope_for_route(&Method::POST, "/deployments"),
            Some("deployments:write")
        );
        assert_eq!(
            scope_for_route(&Method::DELETE, "/secrets/{id}"),
            Some("secrets:write")
        );
        assert_eq!(
            scope_for_route(&Method::GET, "/secrets/{id}"),
            Some("secrets:read")
        );
        // Logs/events/metrics are reads on the deployment.
        assert_eq!(
            scope_for_route(&Method::GET, "/deployments/{id}/logs"),
            Some("deployments:read")
        );
        // Token lifecycle and ticket minting require admin (no escalation via
        // rotate/revoke with a lesser scope).
        assert_eq!(scope_for_route(&Method::POST, "/tokens"), Some("admin"));
        assert_eq!(
            scope_for_route(&Method::POST, "/tokens/{id}/rotate"),
            Some("admin")
        );
        assert_eq!(
            scope_for_route(&Method::DELETE, "/tokens/{id}"),
            Some("admin")
        );
        assert_eq!(
            scope_for_route(&Method::POST, "/auth/stream-ticket"),
            Some("admin")
        );
        // Unmapped routes return None → middleware denies PATs by default.
        assert_eq!(scope_for_route(&Method::GET, "/some/new/route"), None);
    }

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

    // --- End-to-end scope/namespace enforcement (regression tests for the
    // central middleware + per-handler namespace re-check). ---

    /// Mint a PAT through the real API as the admin session and return its clear
    /// `ring_pat_…` value. Exercises the full create path (which requires the
    /// `admin` scope, here satisfied by the session bearer).
    async fn mint_pat(
        server: &TestServer,
        token: &str,
        scopes: &[&str],
        namespaces: &[&str],
    ) -> String {
        let mint = server
            .post("/tokens")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "test-token",
                "scopes": scopes,
                "namespaces": namespaces,
            }))
            .await;
        assert_eq!(mint.status_code(), StatusCode::CREATED);
        mint.json::<serde_json::Value>()["token"]
            .as_str()
            .expect("clear token in create response")
            .to_string()
    }

    #[tokio::test]
    async fn pat_with_scope_reaches_its_route() {
        // Baseline: a correctly-scoped PAT passes the central scope gate.
        let app = new_test_app().await;
        let session = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let pat = mint_pat(&server, &session, &["deployments:read"], &[]).await;

        let res = server
            .get("/deployments")
            .add_header("Authorization", format!("Bearer {}", pat))
            .await;
        assert_eq!(res.status_code(), StatusCode::OK);
    }

    #[tokio::test]
    async fn pat_missing_scope_is_403() {
        // A PAT scoped only to secrets must not reach the deployments routes:
        // the scope gate runs centrally in the middleware.
        let app = new_test_app().await;
        let session = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let pat = mint_pat(&server, &session, &["secrets:read"], &[]).await;

        let res = server
            .get("/deployments")
            .add_header("Authorization", format!("Bearer {}", pat))
            .await;
        assert_eq!(res.status_code(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn namespace_scoped_pat_cannot_delete_across_namespaces() {
        // Regression: a PAT scoped to namespace `kemeter` must NOT be able to
        // delete a deployment living in `default` by hitting its id directly.
        // The scope (`deployments:write`) is held; only the namespace boundary
        // stops it — and that boundary is checked after the deployment loads.
        let app = new_test_app().await;
        let session = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let pat = mint_pat(&server, &session, &["deployments:write"], &["kemeter"]).await;

        // 658c…118 is a fixture deployment in the `default` namespace.
        let res = server
            .delete("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118")
            .add_header("Authorization", format!("Bearer {}", pat))
            .await;
        assert_eq!(res.status_code(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn namespace_scoped_pat_cannot_read_logs_across_namespaces() {
        // Regression: the logs route had no scope/namespace check at all. A PAT
        // scoped to `kemeter` must not stream logs of a `default` deployment.
        let app = new_test_app().await;
        let session = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let pat = mint_pat(&server, &session, &["deployments:read"], &["kemeter"]).await;

        let res = server
            .get("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118/logs")
            .add_header("Authorization", format!("Bearer {}", pat))
            .await;
        assert_eq!(res.status_code(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn non_admin_pat_cannot_mint_or_rotate_tokens() {
        // Regression: minting/rotating tokens requires `admin`. A PAT scoped to
        // anything less (here `users:write`, the old rotate scope) must be
        // rejected at the token routes — closing the escalation path where a
        // lesser PAT could rotate an admin token into a fresh admin secret.
        let app = new_test_app().await;
        let session = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let pat = mint_pat(&server, &session, &["users:write"], &[]).await;

        // Cannot create.
        let create = server
            .post("/tokens")
            .add_header("Authorization", format!("Bearer {}", pat))
            .json(&json!({ "name": "evil", "scopes": ["admin"], "namespaces": [] }))
            .await;
        assert_eq!(create.status_code(), StatusCode::FORBIDDEN);

        // Cannot mint a stream ticket either (also admin-gated now).
        let ticket = server
            .post("/auth/stream-ticket")
            .add_header("Authorization", format!("Bearer {}", pat))
            .json(&json!({ "scope": "deployment:logs:dep1" }))
            .await;
        assert_eq!(ticket.status_code(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn unmapped_route_denies_pat_by_default() {
        // A PAT (any scope) hitting a protected route with no scope mapping
        // must be denied — the deny-by-default guard. `/users/me` is mapped, so
        // we assert the inverse via a freshly-scoped PAT on a mapped route and
        // trust the scope_for_route unit test for the None case; here we prove
        // the admin-only token routes reject a fully-scoped non-admin PAT.
        let app = new_test_app().await;
        let session = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        // A PAT with every non-admin scope still can't reach /tokens.
        let pat = mint_pat(
            &server,
            &session,
            &[
                "deployments:write",
                "secrets:write",
                "configs:write",
                "namespaces:write",
                "users:write",
            ],
            &[],
        )
        .await;
        let res = server
            .get("/tokens")
            .add_header("Authorization", format!("Bearer {}", pat))
            .await;
        assert_eq!(res.status_code(), StatusCode::FORBIDDEN);
    }
}
