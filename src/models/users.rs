use aes_gcm::aead::OsRng;
use aes_gcm::aead::rand_core::RngCore;
use argon2::{self, Config as Argon2Config};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::serializer::deserialize_null_default;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct User {
    pub(crate) id: String,
    pub(crate) created_at: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub(crate) updated_at: Option<String>,
    pub(crate) status: String,
    /// Coarse authorization role: "user" (default, self-scoped) or "admin"
    /// (may act on other accounts). Not RBAC — see migration 0016.
    #[serde(default = "default_role")]
    pub(crate) role: String,
    pub(crate) username: String,
    #[serde(skip_serializing)]
    pub(crate) password: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) token: String,
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
    token: Option<String>,
    login_at: Option<String>,
}

const SELECT_COLUMNS: &str =
    "id, created_at, updated_at, status, role, username, password, token, login_at";

fn default_role() -> String {
    "user".to_string()
}

impl User {
    /// True when this user may act on accounts other than its own.
    pub(crate) fn is_admin(&self) -> bool {
        self.role == "admin"
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
            token: row.token.unwrap_or_default(),
            login_at: row.login_at,
        }
    }
}

pub(crate) async fn find(pool: &SqlitePool, id: &str) -> Result<Option<User>, sqlx::Error> {
    let sql = format!("SELECT {} FROM user WHERE id = ?", SELECT_COLUMNS);
    let row = sqlx::query_as::<_, UserRow>(&sql)
        .bind(&id)
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

pub(crate) async fn find_by_token(pool: &SqlitePool, token: &str) -> Result<User, sqlx::Error> {
    let sql = format!("SELECT {} FROM user WHERE token = ?", SELECT_COLUMNS);
    let row = sqlx::query_as::<_, UserRow>(&sql)
        .bind(token)
        .fetch_one(pool)
        .await?;

    Ok(User::from(row))
}

pub(crate) async fn find_all(pool: &SqlitePool) -> Result<Vec<User>, sqlx::Error> {
    let sql = format!("SELECT {} FROM user", SELECT_COLUMNS);
    let rows = sqlx::query_as::<_, UserRow>(&sql).fetch_all(pool).await?;

    Ok(rows.into_iter().map(User::from).collect())
}

pub(crate) async fn login(pool: &SqlitePool, user: User) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE user SET token = ?, login_at = datetime() WHERE id = ?")
        .bind(&user.token)
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
    // role is hard-coded to 'user': there is no API path to create an admin
    // (that would be a privilege-escalation vector). Admin is set out of band.
    sqlx::query(
        "INSERT INTO user (id, created_at, status, role, username, password, token) VALUES (?, datetime(), ?, ?, ?, ?, ?)"
    )
    .bind(Uuid::new_v4().to_string())
    .bind("active")
    .bind("user")
    .bind(username)
    .bind(password)
    .bind(Uuid::new_v4().to_string())
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn update(pool: &SqlitePool, user: &User) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE user SET username = ?, password = ?, updated_at = datetime() WHERE id = ?")
        .bind(&user.username)
        .bind(&user.password)
        .bind(&user.id)
        .execute(pool)
        .await?;

    Ok(())
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
    use super::hash_password;

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
