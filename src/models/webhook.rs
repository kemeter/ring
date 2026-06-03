use aes_gcm::aead::OsRng;
use aes_gcm::aead::rand_core::RngCore;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

/// A webhook subscriber: an HTTP endpoint that receives matching events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Webhook {
    pub(crate) id: String,
    pub(crate) url: String,
    /// HMAC secret. Never serialised back to clients (shown once at creation).
    #[serde(skip_serializing)]
    pub(crate) secret: Option<String>,
    /// Subscribed event kinds; empty = all kinds.
    pub(crate) events: Vec<String>,
    pub(crate) created_at: String,
    pub(crate) revoked_at: Option<String>,
}

#[derive(sqlx::FromRow)]
struct WebhookRow {
    id: String,
    url: String,
    secret: Option<String>,
    events: String,
    created_at: String,
    revoked_at: Option<String>,
}

const SELECT_COLUMNS: &str = "id, url, secret, events, created_at, revoked_at";

impl From<WebhookRow> for Webhook {
    fn from(row: WebhookRow) -> Self {
        // Lenient JSON read with a logged fallback, like deployment labels: a
        // malformed `events` column degrades to "subscribe to all" rather than
        // making the whole row unreadable on the delivery hot path.
        let events = serde_json::from_str(&row.events).unwrap_or_else(|e| {
            log::warn!("Failed to deserialize events for webhook {}: {}", row.id, e);
            Vec::new()
        });
        Webhook {
            id: row.id,
            url: row.url,
            secret: row.secret,
            events,
            created_at: row.created_at,
            revoked_at: row.revoked_at,
        }
    }
}

impl Webhook {
    /// True when this webhook should receive `kind`: an empty filter means all
    /// kinds, otherwise the kind must be listed.
    pub(crate) fn subscribes_to(&self, kind: &str) -> bool {
        self.events.is_empty() || self.events.iter().any(|e| e == kind)
    }
}

/// Generate a fresh HMAC secret (`whsec_<hex>`). Uses `OsRng` for fork safety,
/// consistent with token/password generation elsewhere.
pub(crate) fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("whsec_{}", hex::encode(bytes))
}

pub(crate) async fn create(
    pool: &SqlitePool,
    url: &str,
    secret: Option<&str>,
    events: &[String],
) -> Result<Webhook, sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();
    let events_json = serde_json::to_string(events).unwrap_or_else(|_| "[]".to_string());

    sqlx::query("INSERT INTO webhook (id, url, secret, events, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(url)
        .bind(secret)
        .bind(&events_json)
        .bind(&created_at)
        .execute(pool)
        .await?;

    Ok(Webhook {
        id,
        url: url.to_string(),
        secret: secret.map(|s| s.to_string()),
        events: events.to_vec(),
        created_at,
        revoked_at: None,
    })
}

pub(crate) async fn find(pool: &SqlitePool, id: &str) -> Result<Option<Webhook>, sqlx::Error> {
    let sql = format!("SELECT {} FROM webhook WHERE id = ?", SELECT_COLUMNS);
    let row = sqlx::query_as::<_, WebhookRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(Webhook::from))
}

pub(crate) async fn find_all(pool: &SqlitePool) -> Result<Vec<Webhook>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM webhook ORDER BY created_at DESC",
        SELECT_COLUMNS
    );
    let rows = sqlx::query_as::<_, WebhookRow>(&sql)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(Webhook::from).collect())
}

/// Active (non-revoked) webhooks subscribed to `kind`. The worker's lookup.
pub(crate) async fn subscribers_for(
    pool: &SqlitePool,
    kind: &str,
) -> Result<Vec<Webhook>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM webhook WHERE revoked_at IS NULL",
        SELECT_COLUMNS
    );
    let rows = sqlx::query_as::<_, WebhookRow>(&sql)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(Webhook::from)
        .filter(|w| w.subscribes_to(kind))
        .collect())
}

pub(crate) async fn revoke(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    sqlx::query("UPDATE webhook SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
        .bind(&now)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wh(events: &[&str]) -> Webhook {
        Webhook {
            id: "w".into(),
            url: "http://x".into(),
            secret: None,
            events: events.iter().map(|s| s.to_string()).collect(),
            created_at: "2026-01-01T00:00:00Z".into(),
            revoked_at: None,
        }
    }

    #[test]
    fn empty_filter_subscribes_to_all() {
        let w = wh(&[]);
        assert!(w.subscribes_to("deployment.status_changed"));
        assert!(w.subscribes_to("anything.else"));
    }

    #[test]
    fn explicit_filter_matches_only_listed() {
        let w = wh(&["deployment.status_changed"]);
        assert!(w.subscribes_to("deployment.status_changed"));
        assert!(!w.subscribes_to("other.kind"));
    }

    #[test]
    fn generated_secret_is_prefixed_and_unique() {
        let a = generate_secret();
        let b = generate_secret();
        assert!(a.starts_with("whsec_"));
        assert_ne!(a, b);
    }
}
