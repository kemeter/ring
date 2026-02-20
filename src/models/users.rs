use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;
use argon2::{self, Config as Argon2Config};

use crate::serializer::deserialize_null_default;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct User {
    pub(crate) id: String,
    pub(crate) created_at: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub(crate) updated_at: Option<String>,
    pub(crate) status: String,
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
    username: String,
    password: String,
    token: Option<String>,
    login_at: Option<String>,
}

const SELECT_COLUMNS: &str = "id, created_at, updated_at, status, username, password, token, login_at";

impl From<UserRow> for User {
    fn from(row: UserRow) -> Self {
        User {
            id: row.id,
            created_at: row.created_at,
            updated_at: row.updated_at,
            status: row.status,
            username: row.username,
            password: row.password,
            token: row.token.unwrap_or_default(),
            login_at: row.login_at,
        }
    }
}

pub(crate) async fn find(pool: &SqlitePool, id: String) -> Result<Option<User>, sqlx::Error> {
    let sql = format!("SELECT {} FROM user WHERE id = ?", SELECT_COLUMNS);
    let row = sqlx::query_as::<_, UserRow>(&sql)
        .bind(&id)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(User::from))
}

pub(crate) async fn find_by_username(pool: &SqlitePool, username: &str) -> Result<Option<User>, sqlx::Error> {
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
    let rows = sqlx::query_as::<_, UserRow>(&sql)
        .fetch_all(pool)
        .await?;

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

pub(crate) async fn create(pool: &SqlitePool, username: &str, password: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO user (id, created_at, status, username, password, token) VALUES (?, datetime(), ?, ?, ?, ?)"
    )
    .bind(Uuid::new_v4().to_string())
    .bind("active")
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

/// Hash a password using Argon2id
pub(crate) fn hash_password(password: &str, salt: &str) -> Result<String, argon2::Error> {
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

    argon2::hash_encoded(
        password.as_bytes(),
        salt.as_bytes(),
        &argon2_config
    )
}
