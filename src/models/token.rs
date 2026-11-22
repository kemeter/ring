use chrono::{DateTime, Utc};
use rand::TryRng;
use rand::rngs::SysRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use uuid::Uuid;

/// Clear-token prefix. The full secret is `ring_pat_<random>`; this marker
/// lets the auth middleware route PATs to the token table before falling back
/// to the session-token path, and lets the CLI recognise its own output.
pub(crate) const TOKEN_PREFIX: &str = "ring_pat_";

/// What a token row is, stored in the `kind` column. A login session and a
/// PAT live in the same table but are different things: a session is minted by
/// `/login`, carries full access, and is managed only through `/logout`; a PAT
/// is created and managed by the user via `ring token`. Branching on `kind`
/// (rather than a magic `name`) keeps `name` a free-text label — a user may
/// name a PAT "session" without it colliding with the session machinery.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    /// A login session minted by `/login` (full access, ended by `/logout`).
    Session,
    /// A Personal Access Token created via `ring token create`.
    Pat,
}

impl TokenKind {
    /// On-disk value in the `kind` column. Must match the migration's
    /// `DEFAULT 'pat'` and backfill (`20220101000022_drop_user_token.sql`).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TokenKind::Session => "session",
            TokenKind::Pat => "pat",
        }
    }

    /// Parse the stored value back, defaulting to `Pat` for anything
    /// unexpected — a token we can't classify is treated as a managed PAT
    /// (visible, addressable), never silently hidden as a session.
    fn from_str(s: &str) -> Self {
        match s {
            "session" => TokenKind::Session,
            _ => TokenKind::Pat,
        }
    }
}

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
    "deployments:exec",
    "secrets:read",
    "secrets:write",
    "configs:read",
    "configs:write",
    "namespaces:read",
    "namespaces:write",
    "volumes:read",
    "volumes:write",
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
    pub(crate) kind: TokenKind,
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
    kind: String,
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

const SELECT_COLUMNS: &str = "id, user_id, name, kind, token_hash, token_prefix, scopes, namespaces, created_at, expire_at, last_used_at, revoked_at";

impl From<TokenRow> for Token {
    fn from(row: TokenRow) -> Self {
        // A malformed JSON column must not make the whole row unreadable, but
        // it must fail closed, like an unparseable expiry: unreadable scopes
        // grant none. Unreadable namespaces cannot fall back to the empty
        // list, which means "every namespace" and would widen a scoped token;
        // the token gets no scopes instead.
        let mut scopes = serde_json::from_str(&row.scopes).unwrap_or_else(|e| {
            warn!("Failed to deserialize scopes for token {}: {}", row.id, e);
            Vec::new()
        });
        let namespaces = serde_json::from_str(&row.namespaces).unwrap_or_else(|e| {
            warn!(
                "Failed to deserialize namespaces for token {}: {}; it is granted nothing",
                row.id, e
            );
            scopes = Vec::new();
            Vec::new()
        });

        Token {
            id: row.id,
            user_id: row.user_id,
            name: row.name,
            kind: TokenKind::from_str(&row.kind),
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

    /// True when this row is a login session (not a user-managed PAT). Sessions
    /// are hidden from `ring token list` and not addressable by id — they are
    /// managed only through `/login` and `/logout`.
    pub(crate) fn is_session(&self) -> bool {
        self.kind == TokenKind::Session
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
/// `SysRng` (OS entropy) rather than a thread-local PRNG: Ring forks processes
/// (the scheduler) and a userspace CSPRNG isn't guaranteed to reseed across
/// fork. This mirrors `models::users::hash_password` and `models::secret`.
fn generate_token() -> (String, String, String) {
    let mut bytes = [0u8; 32];
    SysRng
        .try_fill_bytes(&mut bytes)
        .expect("OS random generator failed");
    let clear = format!("{}{}", TOKEN_PREFIX, hex::encode(bytes));
    let prefix = clear.chars().take(PREFIX_DISPLAY_LEN).collect::<String>();
    let hash = hash_token(&clear);
    (clear, prefix, hash)
}

/// Create a token for `user_id`. Returns the clear value (`ring_pat_...`)
/// alongside the persisted row — the clear value is shown to the user once and
/// then unrecoverable.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn create<'e, E>(
    executor: E,
    user_id: &str,
    name: &str,
    kind: TokenKind,
    scopes: &[String],
    namespaces: &[String],
    expire_at: Option<&str>,
) -> Result<(String, Token), sqlx::Error>
where
    E: sqlx::SqliteExecutor<'e>,
{
    let (clear, prefix, hash) = generate_token();
    let id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();
    let scopes_json = serde_json::to_string(scopes).unwrap_or_else(|_| "[]".to_string());
    let namespaces_json = serde_json::to_string(namespaces).unwrap_or_else(|_| "[]".to_string());

    sqlx::query(
        "INSERT INTO token (id, user_id, name, kind, token_hash, token_prefix, scopes, namespaces, created_at, expire_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(name)
    .bind(kind.as_str())
    .bind(&hash)
    .bind(&prefix)
    .bind(&scopes_json)
    .bind(&namespaces_json)
    .bind(&created_at)
    .bind(expire_at)
    .execute(executor)
    .await?;

    let token = Token {
        id,
        user_id: user_id.to_string(),
        name: name.to_string(),
        kind,
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
    let row = sqlx::query_as::<_, TokenRow>(sqlx::AssertSqlSafe(sql))
        .bind(hash)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(Token::from))
}

pub(crate) async fn find(pool: &SqlitePool, id: &str) -> Result<Option<Token>, sqlx::Error> {
    let sql = format!("SELECT {} FROM token WHERE id = ?", SELECT_COLUMNS);
    let row = sqlx::query_as::<_, TokenRow>(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(Token::from))
}

pub(crate) async fn find_all_for_user(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<Token>, sqlx::Error> {
    // Exclude login sessions: they live in this table but are not PATs the user
    // created or manages, so they must not appear in `ring token list`.
    let sql = format!(
        "SELECT {} FROM token WHERE user_id = ? AND kind = ? ORDER BY created_at DESC",
        SELECT_COLUMNS
    );
    let rows = sqlx::query_as::<_, TokenRow>(sqlx::AssertSqlSafe(sql))
        .bind(user_id)
        .bind(TokenKind::Pat.as_str())
        .fetch_all(pool)
        .await?;

    Ok(rows.into_iter().map(Token::from).collect())
}

/// Soft-delete: mark the token revoked. Idempotent — re-revoking is a no-op.
pub(crate) async fn revoke<'e, E>(executor: E, id: &str) -> Result<(), sqlx::Error>
where
    E: sqlx::SqliteExecutor<'e>,
{
    let now = Utc::now().to_rfc3339();
    sqlx::query("UPDATE token SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
        .bind(&now)
        .bind(id)
        .execute(executor)
        .await?;

    Ok(())
}

/// Replace the scopes of a live token. Its value, and so every caller holding
/// it, stays as it is: what it may do changes, not who holds it.
pub(crate) async fn update_scopes(
    pool: &SqlitePool,
    id: &str,
    scopes: &[String],
) -> Result<(), sqlx::Error> {
    let scopes_json = serde_json::to_string(scopes).unwrap_or_else(|_| "[]".to_string());
    sqlx::query("UPDATE token SET scopes = ? WHERE id = ? AND revoked_at IS NULL")
        .bind(&scopes_json)
        .bind(id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Rotate: revoke the existing token and mint a new one carrying the same
/// name/scopes/namespaces/expiry. Returns the new clear value.
///
/// Both writes commit together: a rotation that fails, or whose request is
/// cancelled half-way, leaves the old token working instead of revoked with
/// no replacement.
pub(crate) async fn rotate(
    pool: &SqlitePool,
    existing: &Token,
) -> Result<(String, Token), sqlx::Error> {
    let mut tx = pool.begin().await?;
    revoke(&mut *tx, &existing.id).await?;
    let rotated = create(
        &mut *tx,
        &existing.user_id,
        &existing.name,
        existing.kind,
        &existing.scopes,
        &existing.namespaces,
        existing.expire_at.as_deref(),
    )
    .await?;
    tx.commit().await?;
    Ok(rotated)
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

    /// Tokens are looked up by this hash: a change would invalidate every
    /// stored token. The value comes from Node's SHA-256.
    #[test]
    fn hash_token_matches_a_stored_hash() {
        assert_eq!(
            hash_token("ring_pat_stored"),
            "28d0f400a6304986b4073c43a96455611c3935aee2a42dfef1824dc58fb61b52"
        );
    }

    #[test]
    fn generate_yields_prefixed_token_and_stable_hash() {
        let (clear, prefix, hash) = generate_token();
        assert!(clear.starts_with(TOKEN_PREFIX));
        assert!(prefix.starts_with(TOKEN_PREFIX));
        // "ring_pat_" and 6 characters: enough to tell tokens apart, too few
        // to give a token away.
        assert_eq!(prefix.len(), 15);
        assert_eq!(prefix, clear[..15]);
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
            kind: TokenKind::Pat,
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

    /// A rotation whose new token cannot be stored leaves the old one working.
    #[tokio::test]
    async fn a_failed_rotation_keeps_the_old_token() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let (_, token) = create(
            &pool,
            "1c5a5fe9-84e0-4a18-821e-8058232c2c23",
            "ci",
            TokenKind::Pat,
            &["admin".to_string()],
            &[],
            None,
        )
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER refuse_insert BEFORE INSERT ON token \
             BEGIN SELECT RAISE(ABORT, 'injected'); END",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert!(rotate(&pool, &token).await.is_err());

        let old = find(&pool, &token.id).await.unwrap().unwrap();
        assert!(
            !old.is_revoked(),
            "the old token was revoked with no replacement"
        );
    }

    async fn migrated_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    /// A token scoped to tenant-a whose `namespaces` column no longer parses
    /// must not become a token for every namespace: it is granted nothing.
    #[tokio::test]
    async fn unreadable_namespaces_fail_closed() {
        let pool = migrated_pool().await;
        let (_, token) = create(
            &pool,
            "1c5a5fe9-84e0-4a18-821e-8058232c2c23",
            "ci",
            TokenKind::Pat,
            &["deployments:read".to_string()],
            &["tenant-a".to_string()],
            None,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE token SET namespaces = 'not-json' WHERE id = ?")
            .bind(&token.id)
            .execute(&pool)
            .await
            .unwrap();

        let loaded = find(&pool, &token.id).await.unwrap().unwrap();

        assert!(loaded.scopes.is_empty(), "it must be granted no scope");
    }
}
