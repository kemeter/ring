pub(crate) mod create;
pub(crate) mod get;
pub(crate) mod list;
pub(crate) mod revoke;
pub(crate) mod rotate;
pub(crate) mod validation;

pub(crate) use create::create;
pub(crate) use get::get;
pub(crate) use list::list;
pub(crate) use revoke::revoke;
pub(crate) use rotate::rotate;

use crate::api::server::Db;
use crate::api::validation::problem_response;
use crate::models::token::{self, Token};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

/// Load a token by id, enforcing the rules shared by get/revoke/rotate: a user
/// may only act on their own tokens, and login sessions are off-limits to the
/// PAT-management API (they are managed only via `/login` and `/logout`). A
/// non-owner — or a session row — gets the same 404 as a missing id, so the
/// endpoint never confirms another user's token nor a session exists. The `Err`
/// is a ready-to-return response (404 / 500) so callers can `?` it.
///
/// Centralising this means the IDOR guard (`token.user_id == user_id`) and the
/// session boundary live in exactly one place — relaxing or fixing either can't
/// be done in one handler and forgotten in the others.
#[allow(clippy::result_large_err)]
pub(crate) async fn find_owned(pool: &Db, id: &str, user_id: &str) -> Result<Token, Response> {
    match token::find(pool, id).await {
        Ok(Some(t)) if t.user_id == user_id && !t.is_session() => Ok(t),
        Ok(_) => Err(problem_response(
            StatusCode::NOT_FOUND,
            "Not Found",
            "token not found",
        )),
        Err(e) => {
            error!("Failed to look up token: {}", e);
            Err(problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to look up token",
            ))
        }
    }
}

/// Secret-free projection of a token, safe to return from list/get/revoke/
/// rotate. The clear value lives only in [`TokenCreated`]. `Deserialize` is
/// derived so the CLI (`ring token list`) reuses this exact wire shape instead
/// of maintaining a parallel struct that could silently drift.
#[derive(Serialize, Deserialize)]
pub(crate) struct TokenView {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) token_prefix: String,
    pub(crate) scopes: Vec<String>,
    pub(crate) namespaces: Vec<String>,
    pub(crate) created_at: String,
    pub(crate) expire_at: Option<String>,
    pub(crate) last_used_at: Option<String>,
    pub(crate) revoked_at: Option<String>,
}

impl From<Token> for TokenView {
    fn from(t: Token) -> Self {
        TokenView {
            id: t.id,
            name: t.name,
            token_prefix: t.token_prefix,
            scopes: t.scopes,
            namespaces: t.namespaces,
            created_at: t.created_at,
            expire_at: t.expire_at,
            last_used_at: t.last_used_at,
            revoked_at: t.revoked_at,
        }
    }
}

/// Response returned ONLY by create and rotate — the one and only time the
/// clear `ring_pat_…` value is shown. Shared by both endpoints so their wire
/// shape (documented as identical) can never drift apart. Every other endpoint
/// returns [`TokenView`] without the secret.
#[derive(Serialize)]
pub(crate) struct TokenCreated {
    pub(crate) id: String,
    pub(crate) name: String,
    /// The clear `ring_pat_…` value. Not stored server-side; copy it now.
    pub(crate) token: String,
    pub(crate) token_prefix: String,
    pub(crate) scopes: Vec<String>,
    pub(crate) namespaces: Vec<String>,
    pub(crate) created_at: String,
    pub(crate) expire_at: Option<String>,
    pub(crate) message: String,
}

impl TokenCreated {
    /// Build the once-shown response from a freshly minted token, its clear
    /// value, and an endpoint-specific message.
    pub(crate) fn new(token: Token, clear: String, message: &str) -> Self {
        TokenCreated {
            id: token.id,
            name: token.name,
            token: clear,
            token_prefix: token.token_prefix,
            scopes: token.scopes,
            namespaces: token.namespaces,
            created_at: token.created_at,
            expire_at: token.expire_at,
            message: message.to_string(),
        }
    }
}
