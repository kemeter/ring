use aes_gcm::aead::OsRng;
use aes_gcm::aead::rand_core::RngCore;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use uuid::Uuid;

/// Clear-token prefix. The full secret is `ring_pat_<random>`; this marker
/// lets the auth middleware route PATs to the token table before falling back
/// to the session-token path, and lets the CLI recognise its own output.
pub(crate) const TOKEN_PREFIX: &str = "ring_pat_";

/// Number of leading characters of the clear token kept in `token_prefix` for
/// display in listings (e.g. "ring_pat_a1b2c3"). Enough to disambiguate, not
/// enough to be a secret.
const PREFIX_DISPLAY_LEN: usize = TOKEN_PREFIX.len() + 6;

/// All scopes a token may carry, as `verb:resource` slugs. `admin` is the
/// catch-all. This is a closed set on purpose — a token cannot hold a scope
/// the server doesn't understand, so validation rejects unknown slugs at
/// creation rather than silently granting nothing.
pub(crate) const KNOWN_SCOPES: &[&str] = &[
    "deployments:read",
    "deployments:write",
    "secrets:read",
    "secrets:write",
    "configs:read",
    "configs:write",
    "namespaces:read",
    "namespaces:write",
    "users:read",
    "users:write",
    "webhooks:read",
    "webhooks:write",
    "admin",
];

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Token {
    pub(crate) id: String,
    pub(crate) user_id: String,
    pub(crate) name: String,
    pub(crate) token_prefix: String,
    pub(crate) scopes: Vec<String>,
    /// Empty = all namespaces.
    pub(crate) namespaces: Vec<String>,
    pub(crate) created_at: String,
    pub(crate) expire_at: Option<String>,
    pub(crate) last_used_at: Option<String>,
    pub(crate) revoked_at: Option<String>,
}

#[derive(sqlx::FromRow)]
struct TokenRow {
    id: String,
    user_id: String,
    name: String,
    /// Selected by `SELECT_COLUMNS` for symmetry with the table but not mapped
    /// onto `Token`: auth looks tokens up *by* this hash in SQL, nothing reads
    /// the value back off a loaded token.
    #[allow(dead_code)]
    token_hash: String,
    token_prefix: String,
    /// JSON arrays, same storage pattern as deployment.labels/volumes.
    scopes: String,
    namespaces: String,
    created_at: String,
    expire_at: Option<String>,
    last_used_at: Option<String>,
    revoked_at: Option<String>,
}

const SELECT_COLUMNS: &str = "id, user_id, name, token_hash, token_prefix, scopes, namespaces, created_at, expire_at, last_used_at, revoked_at";

impl From<TokenRow> for Token {
    fn from(row: TokenRow) -> Self {
        // Lenient deserialization with a logged fallback, mirroring how
        // deployment labels/volumes are read: a malformed JSON column must not
        // make the whole token unreadable (which on the auth path would lock a
        // user out), it degrades to "no scopes / all namespaces" and is logged.
        let scopes = serde_json::from_str(&row.scopes).unwrap_or_else(|e| {
            warn!("Failed to deserialize scopes for token {}: {}", row.id, e);
            Vec::new()
        });
        let namespaces = serde_json::from_str(&row.namespaces).unwrap_or_else(|e| {
            warn!(
                "Failed to deserialize namespaces for token {}: {}",
                row.id, e
            );
            Vec::new()
        });

        Token {
            id: row.id,
            user_id: row.user_id,
            name: row.name,
            token_prefix: row.token_prefix,
            scopes,
            namespaces,
            created_at: row.created_at,
            expire_at: row.expire_at,
            last_used_at: row.last_used_at,
            revoked_at: row.revoked_at,
        }
    }
}

impl Token {
    /// True once the token has been revoked.
    pub(crate) fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// True when `expire_at` is set and already in the past.
    pub(crate) fn is_expired(&self) -> bool {
        match &self.expire_at {
            None => false,
            Some(ts) => match DateTime::parse_from_rfc3339(ts) {
                Ok(exp) => exp <= Utc::now(),
                // An unparyable expiry is treated as expired: fail closed, a
                // token we can't reason about must not grant access.
                Err(_) => true,
            },
        }
    }

    /// Whether this token is currently usable for auth.
    pub(crate) fn is_active(&self) -> bool {
        !self.is_revoked() && !self.is_expired()
    }
}

/// SHA-256 of the clear token, hex-encoded. Tokens are high-entropy secrets
/// (not human passwords), so a fast hash with a constant-time DB lookup is the
/// right tool — Argon2 (used for `user.password`) would add latency to every
/// authenticated request for no security gain against brute force.
pub(crate) fn hash_token(clear: &str) -> String {
    let digest = Sha256::digest(clear.as_bytes());
    hex::encode(digest)
}

/// Generate a fresh clear token and its derived (prefix, hash). The clear
/// value is returned to the caller exactly once — it is never stored.
///
/// `OsRng` (OS entropy) rather than a thread-local PRNG: Ring forks processes
/// (the scheduler) and a userspace CSPRNG isn't guaranteed to reseed across
/// fork. This mirrors `models::users::hash_password` and `models::secret`.
fn generate_token() -> (String, String, String) {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let clear = format!("{}{}", TOKEN_PREFIX, hex::encode(bytes));
    let prefix = clear.chars().take(PREFIX_DISPLAY_LEN).collect::<String>();
    let hash = hash_token(&clear);
    (clear, prefix, hash)
}

/// Create a token for `user_id`. Returns the clear value (`ring_pat_...`)
/// alongside the persisted row — the clear value is shown to the user once and
/// then unrecoverable.
pub(crate) async fn create(
    pool: &SqlitePool,
    user_id: &str,
    name: &str,
    scopes: &[String],
    namespaces: &[String],
    expire_at: Option<&str>,
) -> Result<(String, Token), sqlx::Error> {
    let (clear, prefix, hash) = generate_token();
    let id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();
    let scopes_json = serde_json::to_string(scopes).unwrap_or_else(|_| "[]".to_string());
    let namespaces_json = serde_json::to_string(namespaces).unwrap_or_else(|_| "[]".to_string());

    sqlx::query(
        "INSERT INTO token (id, user_id, name, token_hash, token_prefix, scopes, namespaces, created_at, expire_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(name)
    .bind(&hash)
    .bind(&prefix)
    .bind(&scopes_json)
    .bind(&namespaces_json)
    .bind(&created_at)
    .bind(expire_at)
    .execute(pool)
    .await?;

    let token = Token {
        id,
        user_id: user_id.to_string(),
        name: name.to_string(),
        token_prefix: prefix,
        scopes: scopes.to_vec(),
        namespaces: namespaces.to_vec(),
        created_at,
        expire_at: expire_at.map(|s| s.to_string()),
        last_used_at: None,
        revoked_at: None,
    };

    Ok((clear, token))
}

/// Resolve a clear token to its row by hash. Returns any matching row,
/// including revoked/expired ones — the caller decides via `is_active()` so it
/// can distinguish 401-unknown from 401-revoked if needed.
pub(crate) async fn find_by_token_hash(
    pool: &SqlitePool,
    hash: &str,
) -> Result<Option<Token>, sqlx::Error> {
    let sql = format!("SELECT {} FROM token WHERE token_hash = ?", SELECT_COLUMNS);
    let row = sqlx::query_as::<_, TokenRow>(&sql)
        .bind(hash)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(Token::from))
}

pub(crate) async fn find(pool: &SqlitePool, id: &str) -> Result<Option<Token>, sqlx::Error> {
    let sql = format!("SELECT {} FROM token WHERE id = ?", SELECT_COLUMNS);
    let row = sqlx::query_as::<_, TokenRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(Token::from))
}

pub(crate) async fn find_all_for_user(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<Token>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM token WHERE user_id = ? ORDER BY created_at DESC",
        SELECT_COLUMNS
    );
    let rows = sqlx::query_as::<_, TokenRow>(&sql)
        .bind(user_id)
        .fetch_all(pool)
        .await?;

    Ok(rows.into_iter().map(Token::from).collect())
}

/// Soft-delete: mark the token revoked. Idempotent — re-revoking is a no-op.
pub(crate) async fn revoke(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    sqlx::query("UPDATE token SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
        .bind(&now)
        .bind(id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Rotate: revoke the existing token and mint a new one carrying the same
/// name/scopes/namespaces/expiry. Returns the new clear value.
pub(crate) async fn rotate(
    pool: &SqlitePool,
    existing: &Token,
) -> Result<(String, Token), sqlx::Error> {
    revoke(pool, &existing.id).await?;
    create(
        pool,
        &existing.user_id,
        &existing.name,
        &existing.scopes,
        &existing.namespaces,
        existing.expire_at.as_deref(),
    )
    .await
}

/// Best-effort last-use marker, throttled to one write per minute so the auth
/// hot path doesn't issue a write on every single request. Failures are
/// swallowed by the caller — a missed `last_used_at` must never fail auth.
pub(crate) async fn touch_last_used(pool: &SqlitePool, token: &Token) -> Result<(), sqlx::Error> {
    if let Some(prev) = &token.last_used_at
        && let Ok(prev_ts) = DateTime::parse_from_rfc3339(prev)
        && (Utc::now() - prev_ts.with_timezone(&Utc)).num_seconds() < 60
    {
        return Ok(());
    }

    let now = Utc::now().to_rfc3339();
    sqlx::query("UPDATE token SET last_used_at = ? WHERE id = ?")
        .bind(&now)
        .bind(&token.id)
        .execute(pool)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_yields_prefixed_token_and_stable_hash() {
        let (clear, prefix, hash) = generate_token();
        assert!(clear.starts_with(TOKEN_PREFIX));
        assert!(prefix.starts_with(TOKEN_PREFIX));
        assert_eq!(prefix.len(), PREFIX_DISPLAY_LEN);
        // The hash is reproducible from the clear value, the prefix is not the
        // hash, and two generations differ.
        assert_eq!(hash, hash_token(&clear));
        let (clear2, _, hash2) = generate_token();
        assert_ne!(clear, clear2, "tokens must be unique");
        assert_ne!(hash, hash2);
    }

    fn token_with(scopes: &[&str], namespaces: &[&str]) -> Token {
        Token {
            id: "t".into(),
            user_id: "u".into(),
            name: "n".into(),
            token_prefix: "ring_pat_x".into(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            namespaces: namespaces.iter().map(|s| s.to_string()).collect(),
            created_at: "2026-01-01T00:00:00Z".into(),
            expire_at: None,
            last_used_at: None,
            revoked_at: None,
        }
    }

    // Scope/namespace authorisation lives in `api::auth::require_scope` (the
    // single enforcement point) and is tested there. Here we only cover the
    // model's own lifecycle (active / expired / revoked).

    #[test]
    fn expired_token_is_not_active() {
        let mut t = token_with(&["admin"], &[]);
        t.expire_at = Some("2000-01-01T00:00:00Z".into());
        assert!(t.is_expired());
        assert!(!t.is_active());
    }

    #[test]
    fn unparseable_expiry_fails_closed() {
        let mut t = token_with(&["admin"], &[]);
        t.expire_at = Some("not-a-date".into());
        assert!(
            t.is_expired(),
            "an unparseable expiry must be treated as expired"
        );
    }

    #[test]
    fn revoked_token_is_not_active() {
        let mut t = token_with(&["admin"], &[]);
        t.revoked_at = Some("2026-01-02T00:00:00Z".into());
        assert!(t.is_revoked());
        assert!(!t.is_active());
    }
}
