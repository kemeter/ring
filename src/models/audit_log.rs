//! Namespace-scoped audit log of write actions.
//!
//! A deliberately light record of "who did what": one row per successful
//! create/update/delete on a deployment, secret, config or namespace. Unlike
//! `deployment_event` (per-deployment runtime telemetry that is wiped when a
//! deployment is cleaned up), this survives target deletion so a post-mortem
//! stays possible. Retention is namespace-bound: the trail goes away only
//! when the namespace itself is deleted.
//!
//! NOTE: storage layer lands first; handler call sites, the HTTP endpoint and
//! the CLI come in follow-up commits. The crate-local `dead_code` allow below
//! is transitional and removed once those consumers exist.
#![allow(dead_code)]

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub(crate) struct AuditEntry {
    pub(crate) id: String,
    pub(crate) timestamp: String,
    pub(crate) user_id: Option<String>,
    pub(crate) action: String,
    pub(crate) target_type: String,
    pub(crate) target_name: String,
    pub(crate) namespace: Option<String>,
}

impl AuditEntry {
    pub(crate) fn new(
        user_id: Option<String>,
        action: &str,
        target_type: &str,
        target_name: &str,
        namespace: Option<&str>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            user_id,
            action: action.to_string(),
            target_type: target_type.to_string(),
            target_name: target_name.to_string(),
            namespace: namespace.map(|n| n.to_string()),
        }
    }
}

/// Record a write action. Call this at the success point of a handler, never
/// before the action is committed — the log must reflect what actually
/// happened, not what was attempted.
pub(crate) async fn record(
    pool: &SqlitePool,
    user_id: Option<&str>,
    action: &str,
    target_type: &str,
    target_name: &str,
    namespace: Option<&str>,
) -> Result<(), sqlx::Error> {
    let entry = AuditEntry::new(
        user_id.map(|s| s.to_string()),
        action,
        target_type,
        target_name,
        namespace,
    );

    sqlx::query(
        "INSERT INTO audit_log (id, timestamp, user_id, action, target_type, target_name, namespace)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&entry.id)
    .bind(&entry.timestamp)
    .bind(&entry.user_id)
    .bind(&entry.action)
    .bind(&entry.target_type)
    .bind(&entry.target_name)
    .bind(&entry.namespace)
    .execute(pool)
    .await?;

    Ok(())
}

/// All audit entries for a namespace, most recent first.
pub(crate) async fn find_by_namespace(
    pool: &SqlitePool,
    namespace: &str,
    limit: Option<u32>,
) -> Result<Vec<AuditEntry>, sqlx::Error> {
    let safe_limit = limit.unwrap_or(100).min(1000) as i64;

    sqlx::query_as::<_, AuditEntry>(
        "SELECT id, timestamp, user_id, action, target_type, target_name, namespace
         FROM audit_log WHERE namespace = ? ORDER BY timestamp DESC LIMIT ?",
    )
    .bind(namespace)
    .bind(safe_limit)
    .fetch_all(pool)
    .await
}

/// Drop a namespace's whole audit trail. Called when the namespace itself is
/// deleted — retention is namespace-bound by design.
pub(crate) async fn delete_by_namespace(
    pool: &SqlitePool,
    namespace: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM audit_log WHERE namespace = ?")
        .bind(namespace)
        .execute(pool)
        .await?;

    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn record_then_find_by_namespace() {
        let pool = test_pool().await;
        record(
            &pool,
            Some("u1"),
            "create",
            "deployment",
            "api",
            Some("ns1"),
        )
        .await
        .unwrap();
        record(&pool, Some("u1"), "delete", "secret", "db-url", Some("ns1"))
            .await
            .unwrap();
        record(&pool, Some("u2"), "create", "config", "nginx", Some("ns2"))
            .await
            .unwrap();

        let ns1 = find_by_namespace(&pool, "ns1", None).await.unwrap();
        assert_eq!(ns1.len(), 2);
        // Most recent first.
        assert_eq!(ns1[0].action, "delete");
        assert_eq!(ns1[0].target_name, "db-url");
        assert_eq!(ns1[1].action, "create");

        let ns2 = find_by_namespace(&pool, "ns2", None).await.unwrap();
        assert_eq!(ns2.len(), 1);
        assert_eq!(ns2[0].user_id.as_deref(), Some("u2"));
    }

    #[tokio::test]
    async fn entry_survives_unrelated_deletes_but_namespace_delete_clears_it() {
        let pool = test_pool().await;
        record(
            &pool,
            Some("u1"),
            "create",
            "deployment",
            "api",
            Some("ns1"),
        )
        .await
        .unwrap();
        record(
            &pool,
            Some("u1"),
            "create",
            "deployment",
            "web",
            Some("ns2"),
        )
        .await
        .unwrap();

        // Deleting ns1's trail must not touch ns2.
        let removed = delete_by_namespace(&pool, "ns1").await.unwrap();
        assert_eq!(removed, 1);
        assert!(
            find_by_namespace(&pool, "ns1", None)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            find_by_namespace(&pool, "ns2", None).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn limit_is_capped() {
        let pool = test_pool().await;
        for i in 0..5 {
            record(
                &pool,
                Some("u1"),
                "create",
                "deployment",
                &format!("d{i}"),
                Some("ns"),
            )
            .await
            .unwrap();
        }
        let rows = find_by_namespace(&pool, "ns", Some(2)).await.unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn null_user_is_accepted() {
        let pool = test_pool().await;
        record(&pool, None, "delete", "namespace", "ns1", None)
            .await
            .unwrap();
        // namespace-level action: stored with NULL namespace, not queryable
        // by namespace (expected — it has no parent context).
        let rows = find_by_namespace(&pool, "ns1", None).await.unwrap();
        assert!(rows.is_empty());
    }
}
