use crate::models::security_level::SecurityLevel;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Serialize, Deserialize, Debug, Clone, sqlx::FromRow)]
pub(crate) struct Namespace {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: Option<String>,
    pub(crate) name: String,
    /// Its own security level, `None` when it follows the server's default.
    pub(crate) security_level: Option<SecurityLevel>,
}

pub(crate) async fn find(pool: &SqlitePool, id: &str) -> Result<Option<Namespace>, sqlx::Error> {
    sqlx::query_as::<_, Namespace>(
        "SELECT id, created_at, updated_at, name, security_level FROM namespace WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub(crate) async fn find_by_name(
    pool: &SqlitePool,
    name: &str,
) -> Result<Option<Namespace>, sqlx::Error> {
    sqlx::query_as::<_, Namespace>(
        "SELECT id, created_at, updated_at, name, security_level FROM namespace WHERE name = ?",
    )
    .bind(name)
    .fetch_optional(pool)
    .await
}

pub(crate) async fn find_all(pool: &SqlitePool) -> Result<Vec<Namespace>, sqlx::Error> {
    sqlx::query_as::<_, Namespace>(
        "SELECT id, created_at, updated_at, name, security_level FROM namespace ORDER BY name",
    )
    .fetch_all(pool)
    .await
}

pub(crate) async fn create(pool: &SqlitePool, namespace: Namespace) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO namespace (id, created_at, updated_at, name, security_level) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&namespace.id)
    .bind(&namespace.created_at)
    .bind(&namespace.updated_at)
    .bind(&namespace.name)
    .bind(namespace.security_level)
        .execute(pool)
        .await?;

    Ok(())
}

/// The security level deployments of namespace `name` are held to: its own,
/// else `default`. A namespace that does not exist yet gets `default` too,
/// which is the level it is created with.
pub(crate) async fn security_level(
    pool: &SqlitePool,
    name: &str,
    default: SecurityLevel,
) -> Result<SecurityLevel, sqlx::Error> {
    let own: Option<Option<SecurityLevel>> =
        sqlx::query_scalar("SELECT security_level FROM namespace WHERE name = ?")
            .bind(name)
            .fetch_optional(pool)
            .await?;
    Ok(own.flatten().unwrap_or(default))
}

/// Set the security level of namespace `name`, `None` to follow the server's
/// default again.
pub(crate) async fn set_security_level(
    pool: &SqlitePool,
    name: &str,
    level: Option<SecurityLevel>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE namespace SET security_level = ?, updated_at = ? WHERE name = ?")
        .bind(level)
        .bind(chrono::Utc::now().to_string())
        .bind(name)
        .execute(pool)
        .await?;
    Ok(())
}

/// The level of every namespace that has one of its own, by name.
pub(crate) async fn security_levels(
    pool: &SqlitePool,
) -> Result<std::collections::HashMap<String, SecurityLevel>, sqlx::Error> {
    let rows: Vec<(String, SecurityLevel)> = sqlx::query_as(
        "SELECT name, security_level FROM namespace WHERE security_level IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

pub(crate) async fn delete_by_name(pool: &SqlitePool, name: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM namespace WHERE name = ?")
        .bind(name)
        .execute(pool)
        .await?;

    Ok(result.rows_affected())
}

/// Number of resources still living in a namespace. A namespace is only
/// deletable when this is zero — we never cascade-delete deployments,
/// secrets or configs out from under the operator. A "deleted"-status
/// deployment is a tombstone, not a live resource, so it does not count.
pub(crate) async fn count_resources(pool: &SqlitePool, name: &str) -> Result<i64, sqlx::Error> {
    let deployments: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM deployment WHERE namespace = ? AND status <> 'deleted'",
    )
    .bind(name)
    .fetch_one(pool)
    .await?;

    let secrets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM secret WHERE namespace = ?")
        .bind(name)
        .fetch_one(pool)
        .await?;

    let configs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM config WHERE namespace = ?")
        .bind(name)
        .fetch_one(pool)
        .await?;

    Ok(deployments + secrets + configs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    const SECURITY_LEVEL_MIGRATION: i64 = 20220101000030;

    /// A database migrated up to, but not including, `version`.
    async fn pool_before(version: i64) -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for migration in sqlx::migrate!("./migrations")
            .iter()
            .filter(|m| m.version < version)
        {
            sqlx::raw_sql(migration.sql.clone())
                .execute(&pool)
                .await
                .unwrap();
        }
        pool
    }

    async fn insert_deployment(
        pool: &SqlitePool,
        namespace: &str,
        status: &str,
        config: &str,
        volumes: &str,
        network_mode: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO deployment (id, created_at, status, namespace, runtime, kind, name, \
             config, volumes, network_mode) \
             VALUES (?, '2024-01-01', ?, ?, 'docker', 'worker', ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(status)
        .bind(namespace)
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(config)
        .bind(volumes)
        .bind(network_mode)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn the_upgrade_keeps_namespaces_reaching_the_host_running() {
        let pool = pool_before(SECURITY_LEVEL_MIGRATION).await;
        for name in ["plain", "privileged", "bind", "deleted-only"] {
            sqlx::query("INSERT INTO namespace (id, created_at, name) VALUES (?, '2024-01-01', ?)")
                .bind(uuid::Uuid::new_v4().to_string())
                .bind(name)
                .execute(&pool)
                .await
                .unwrap();
        }
        let bind = r#"[{"type":"bind","source":"/srv","destination":"/srv","driver":"local","permission":"ro"}]"#;
        let named = r#"[{"type":"volume","source":"data","destination":"/data","driver":"local","permission":"rw"}]"#;
        insert_deployment(&pool, "plain", "running", "{}", named, Some("bridge")).await;
        insert_deployment(
            &pool,
            "privileged",
            "running",
            r#"{"user":{"privileged":true}}"#,
            "[]",
            None,
        )
        .await;
        insert_deployment(&pool, "bind", "backing_off", "{}", bind, None).await;
        insert_deployment(&pool, "deleted-only", "deleted", "{}", bind, None).await;
        // Made before namespaces were stored: no row of its own.
        insert_deployment(&pool, "legacy", "running", "{}", "[]", Some("host")).await;
        // A column that is not JSON must not fail the migration.
        insert_deployment(&pool, "plain", "running", "not json", "not json", None).await;

        sqlx::raw_sql(
            sqlx::migrate!("./migrations")
                .iter()
                .find(|m| m.version == SECURITY_LEVEL_MIGRATION)
                .unwrap()
                .sql
                .clone(),
        )
        .execute(&pool)
        .await
        .unwrap();

        let levels = security_levels(&pool).await.unwrap();
        let mut pinned: Vec<&str> = levels.keys().map(String::as_str).collect();
        pinned.sort();
        assert_eq!(pinned, ["bind", "legacy", "privileged"]);
        assert!(levels.values().all(|l| *l == SecurityLevel::Privileged));
        assert!(find_by_name(&pool, "legacy").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_namespace_without_a_level_follows_the_default() {
        let pool = pool_before(i64::MAX).await;
        create(
            &pool,
            Namespace {
                id: uuid::Uuid::new_v4().to_string(),
                created_at: "2024-01-01".to_string(),
                updated_at: None,
                name: "pinned".to_string(),
                security_level: Some(SecurityLevel::Restricted),
            },
        )
        .await
        .unwrap();

        let level = |name: &'static str, default| {
            let pool = pool.clone();
            async move { security_level(&pool, name, default).await.unwrap() }
        };
        assert_eq!(
            level("pinned", SecurityLevel::Privileged).await,
            SecurityLevel::Restricted
        );
        assert_eq!(
            level("unknown", SecurityLevel::Privileged).await,
            SecurityLevel::Privileged
        );
    }
}
