use aes_gcm::aead::OsRng;
use aes_gcm::aead::rand_core::RngCore;
use argon2::{self, Config as Argon2Config};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::serializer::deserialize_null_default;

/// A user's authorization role. Each role maps to a fixed set of token scopes
/// via [`Role::scopes`]; see migration 0023.
///
/// Deliberately NOT used as the `UserRow` column type: an unknown string in the
/// database must fail loudly at parse time rather than deserialize into a
/// silent default (which would either grant or strip privileges by accident).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Role {
    /// Full access. Carries the `admin` wildcard scope, never a scope list.
    Admin,
    /// Day-to-day operation of workloads: read everything, write the resources
    /// needed to run and configure deployments.
    Operator,
    /// Read-only across every resource.
    Viewer,
}

impl Role {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Operator => "operator",
            Role::Viewer => "viewer",
        }
    }

    /// Parse a role as stored in the database. Fails on anything unknown: the
    /// `CHECK` constraint from migration 0023 should make that impossible, so
    /// an unknown value means a hand-edited or corrupt row and must not be
    /// quietly resolved to a set of permissions.
    pub(crate) fn parse(raw: &str) -> Result<Role, String> {
        match raw {
            "admin" => Ok(Role::Admin),
            "operator" => Ok(Role::Operator),
            "viewer" => Ok(Role::Viewer),
            other => Err(format!("unknown user role {other:?}")),
        }
    }

    /// The scopes a session gets when this role logs in.
    ///
    /// `Admin` returns the `admin` wildcard alone — it is never mixed with
    /// ordinary scopes, so the wildcard stays the single meaning of "full
    /// access" that the auth middleware already understands.
    ///
    /// `Operator` holds `secrets:write` on purpose. Withholding it would be a
    /// fake boundary: `deployments:write` already lets a user mount any secret
    /// of the namespace into a deployment, which the scheduler decrypts. The
    /// honest framing, documented as such, is that `deployments:write` amounts
    /// to administering the namespace's workloads.
    pub(crate) fn scopes(&self) -> Vec<String> {
        let slugs: &[&str] = match self {
            Role::Admin => &["admin"],
            Role::Operator => &[
                "deployments:read",
                "deployments:write",
                "secrets:read",
                "secrets:write",
                "configs:read",
                "configs:write",
                "namespaces:read",
                "users:read",
                "webhooks:read",
                "webhooks:write",
            ],
            Role::Viewer => &[
                "deployments:read",
                "secrets:read",
                "configs:read",
                "namespaces:read",
                "users:read",
                "webhooks:read",
            ],
        };

        slugs.iter().map(|s| s.to_string()).collect()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct User {
    pub(crate) id: String,
    pub(crate) created_at: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub(crate) updated_at: Option<String>,
    pub(crate) status: String,
    /// Authorization role: `admin`, `operator` or `viewer` (migration 0023).
    /// Kept as a `String` on the row; use [`User::role`] for the parsed value.
    #[serde(default = "default_role")]
    pub(crate) role: String,
    pub(crate) username: String,
    #[serde(skip_serializing)]
    pub(crate) password: String,
    #[serde(skip_serializing)]
    pub(crate) login_at: Option<String>,
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: String,
    created_at: String,
    updated_at: Option<String>,
    status: String,
    role: String,
    username: String,
    password: String,
    login_at: Option<String>,
}

const SELECT_COLUMNS: &str =
    "id, created_at, updated_at, status, role, username, password, login_at";

/// Least privilege when a payload omits the role. `"user"` is no longer a legal
/// value — migration 0023 constrains the column to admin/operator/viewer.
fn default_role() -> String {
    Role::Viewer.as_str().to_string()
}

impl User {
    /// True when this user may act on accounts other than its own.
    pub(crate) fn is_admin(&self) -> bool {
        self.role == Role::Admin.as_str()
    }

    /// The parsed role. `Err` when the stored string is not a known role, which
    /// the caller must treat as a failure rather than falling back to a default.
    pub(crate) fn role(&self) -> Result<Role, String> {
        Role::parse(&self.role)
    }
}

impl From<UserRow> for User {
    fn from(row: UserRow) -> Self {
        User {
            id: row.id,
            created_at: row.created_at,
            updated_at: row.updated_at,
            status: row.status,
            role: row.role,
            username: row.username,
            password: row.password,
            login_at: row.login_at,
        }
    }
}

pub(crate) async fn find(pool: &SqlitePool, id: &str) -> Result<Option<User>, sqlx::Error> {
    let sql = format!("SELECT {} FROM user WHERE id = ?", SELECT_COLUMNS);
    let row = sqlx::query_as::<_, UserRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(User::from))
}

pub(crate) async fn find_by_username(
    pool: &SqlitePool,
    username: &str,
) -> Result<Option<User>, sqlx::Error> {
    let sql = format!("SELECT {} FROM user WHERE username = ?", SELECT_COLUMNS);
    let row = sqlx::query_as::<_, UserRow>(&sql)
        .bind(username)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(User::from))
}

pub(crate) async fn find_all(pool: &SqlitePool) -> Result<Vec<User>, sqlx::Error> {
    let sql = format!("SELECT {} FROM user", SELECT_COLUMNS);
    let rows = sqlx::query_as::<_, UserRow>(&sql).fetch_all(pool).await?;

    Ok(rows.into_iter().map(User::from).collect())
}

/// Record a successful login by stamping `login_at`. The session credential
/// itself is no longer stored on the user row — it lives in the `token` table
/// (minted by the login handler); this only touches the last-login timestamp.
pub(crate) async fn login(pool: &SqlitePool, user: &User) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE user SET login_at = datetime() WHERE id = ?")
        .bind(&user.id)
        .execute(pool)
        .await?;

    Ok(())
}

pub(crate) async fn create(
    pool: &SqlitePool,
    username: &str,
    password: &str,
) -> Result<(), sqlx::Error> {
    // New accounts start as `viewer` (read-only), the least-privileged role.
    // Promotion is a separate, explicitly authorized act -- creating an account
    // must never be a way to mint privileges.
    sqlx::query(
        "INSERT INTO user (id, created_at, status, role, username, password) VALUES (?, datetime(), ?, ?, ?, ?)"
    )
    .bind(Uuid::new_v4().to_string())
    .bind("active")
    .bind(Role::Viewer.as_str())
    .bind(username)
    .bind(password)
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn update(pool: &SqlitePool, user: &User) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE user SET username = ?, password = ?, role = ?, updated_at = datetime() WHERE id = ?",
    )
    .bind(&user.username)
    .bind(&user.password)
    .bind(&user.role)
    .bind(&user.id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Number of accounts holding the `admin` role. Used to refuse the demotion or
/// deletion of the last admin, which would leave the instance unadministrable
/// with no way back in through the API.
pub(crate) async fn count_admins(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM user WHERE role = ?")
        .bind(Role::Admin.as_str())
        .fetch_one(pool)
        .await?;

    Ok(count)
}

pub(crate) async fn delete(pool: &SqlitePool, user: &User) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM user WHERE id = ?")
        .bind(&user.id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Hash a password using Argon2id with a unique, randomly generated salt.
///
/// The salt MUST be unique per password (OWASP Password Storage): a shared or
/// static salt lets an attacker who obtains the database attack every hash in
/// parallel and reveals which users share a password. `hash_encoded` embeds
/// this salt in the returned string, so `verify_encoded` reads it back from
/// the stored hash — no salt needs to be carried in config.
///
/// We use `OsRng` (OS entropy) rather than a thread-local PRNG: Ring forks
/// processes (the scheduler), and a userspace CSPRNG isn't guaranteed to
/// reseed across fork. This mirrors the nonce generation in `models::secret`.
pub(crate) fn hash_password(password: &str) -> Result<String, argon2::Error> {
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);

    let argon2_config = Argon2Config {
        variant: argon2::Variant::Argon2id,
        version: argon2::Version::Version13,
        mem_cost: 65536,
        time_cost: 2,
        lanes: 4,
        secret: &[],
        ad: &[],
        hash_length: 32,
    };

    argon2::hash_encoded(password.as_bytes(), &salt, &argon2_config)
}

#[cfg(test)]
mod tests {
    use super::{Role, hash_password};
    use crate::models::token::KNOWN_SCOPES;

    #[test]
    fn every_role_scope_is_a_known_scope() {
        // Scopes are validated against KNOWN_SCOPES at token creation. A role
        // granting a slug that isn't in that set would mint a session the
        // server refuses to honour, so the two lists must not drift apart.
        for role in [Role::Admin, Role::Operator, Role::Viewer] {
            for scope in role.scopes() {
                assert!(
                    KNOWN_SCOPES.contains(&scope.as_str()),
                    "role {} grants unknown scope {scope:?}",
                    role.as_str()
                );
            }
        }
    }

    #[test]
    fn admin_holds_only_the_wildcard() {
        // `admin` means full access on its own; mixing it with ordinary scopes
        // would create two competing encodings of the same privilege.
        assert_eq!(Role::Admin.scopes(), vec!["admin".to_string()]);
    }

    #[test]
    fn viewer_is_read_only_and_operator_can_write() {
        let viewer = Role::Viewer.scopes();
        assert!(
            viewer.iter().all(|s| s.ends_with(":read")),
            "viewer must not hold any write scope: {viewer:?}"
        );

        let operator = Role::Operator.scopes();
        assert!(operator.contains(&"deployments:write".to_string()));
        // Never admin: an operator must not reach token minting or user writes.
        assert!(!operator.contains(&"admin".to_string()));
        assert!(!operator.contains(&"users:write".to_string()));
        assert!(!operator.contains(&"namespaces:write".to_string()));
    }

    #[test]
    fn unknown_role_fails_loudly() {
        // Fail-closed: a corrupt/hand-edited row must not resolve to a default
        // set of permissions.
        assert!(Role::parse("root").is_err());
        assert!(Role::parse("user").is_err(), "'user' was dropped by 0023");
        assert_eq!(Role::parse("admin").unwrap(), Role::Admin);
    }

    #[test]
    fn same_password_yields_distinct_hashes() {
        // Unique per-password salt (OWASP): the same password hashed twice
        // must not collide, otherwise shared passwords are detectable in DB.
        let a = hash_password("hunter2").unwrap();
        let b = hash_password("hunter2").unwrap();
        assert_ne!(a, b, "salt is not unique per call");
    }

    #[test]
    fn fresh_hash_verifies() {
        let h = hash_password("hunter2").unwrap();
        assert!(argon2::verify_encoded(&h, b"hunter2").unwrap());
        assert!(!argon2::verify_encoded(&h, b"wrong").unwrap());
    }

    #[test]
    fn legacy_shared_salt_hash_still_verifies() {
        // Regression guard: accounts created by the OLD code path used a
        // shared static salt. argon2 embeds the salt in the encoded string,
        // so `verify_encoded` reads it back from the stored hash — switching
        // hash_password to a random salt must NOT lock out existing users.
        // This is a real "changeme"/salt="changeme" argon2id hash.
        let legacy = "$argon2id$v=19$m=65536,t=2,p=4$Y2hhbmdlbWU$HxyGA81ORfjb63QVOi3+t/eBaFPmdSbf4OZc4pBG8DM";
        assert!(
            argon2::verify_encoded(legacy, b"changeme").unwrap(),
            "an existing account's password must still verify after the fix"
        );
    }
}
