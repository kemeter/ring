use aes_gcm::aead::OsRng;
use aes_gcm::aead::rand_core::RngCore;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::net::IpAddr;
use uuid::Uuid;

/// Validate a subscriber URL, returning `Err(reason)` if it is unsafe to call.
///
/// The worker POSTs to this URL server-side, so an unrestricted URL is an SSRF
/// primitive. We require http(s) and reject the targets that grant a real
/// privilege escalation: loopback (`127.0.0.1`/`::1`/`localhost`, where the
/// host's own unauthenticated admin services listen) and link-local
/// (`169.254.0.0/16`, the cloud metadata range that hands out IAM credentials).
///
/// Private RFC-1918 / ULA addresses are deliberately **allowed**: on an
/// orchestrator the legitimate subscriber is usually an internal cluster
/// service on a private IP, and a `webhooks:write` holder can already deploy
/// containers onto that network — so reaching a private IP via webhook grants
/// nothing new, while blocking it would break the normal use case.
///
/// This is a syntactic guard, not a full SSRF defense: a hostname that resolves
/// to a blocked IP via DNS is not caught here (that needs resolution at delivery
/// time). It is paired with `redirect(Policy::none())` on the delivery client so
/// a subscriber can't 3xx-bounce the request to a blocked target.
pub(crate) fn url_safety_violation(url: &str) -> Option<String> {
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return Some("must be a valid http or https URL".to_string()),
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return Some("must be an http or https URL".to_string());
    }
    let host = match parsed.host_str() {
        Some(h) => h,
        None => return Some("URL must have a host".to_string()),
    };
    // Reject the obvious loopback hostname; IP literals are checked below.
    if host.eq_ignore_ascii_case("localhost") {
        return Some("URL must not target localhost".to_string());
    }
    // `host_str` keeps the brackets around an IPv6 literal (`[::1]`); strip them
    // so the literal parses as an `IpAddr`.
    let host_ip = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = host_ip.parse::<IpAddr>()
        && is_blocked_ip(&ip)
    {
        return Some(format!("URL must not target an internal address ({})", ip));
    }
    None
}

/// True for the addresses a subscriber URL must never point at: loopback (host
/// admin services), link-local (`169.254.0.0/16` cloud metadata) and the
/// unspecified address. Private RFC-1918 / ULA ranges are intentionally NOT
/// blocked — see `url_safety_violation`.
fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_link_local() || v4.is_unspecified(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // Link-local (fe80::/10). ULA (fc00::/7) is the IPv6 analogue of
                // RFC-1918 and, like it, is allowed.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

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

    #[test]
    fn safe_urls_pass() {
        assert!(url_safety_violation("https://hooks.example.com/ring").is_none());
        assert!(url_safety_violation("http://example.com:8080/path").is_none());
        // A public IP literal is fine.
        assert!(url_safety_violation("https://93.184.216.34/hook").is_none());
        // Private RFC-1918 / ULA are allowed: legitimate internal subscribers.
        assert!(url_safety_violation("http://10.0.0.5/hook").is_none());
        assert!(url_safety_violation("http://192.168.1.10/hook").is_none());
        assert!(url_safety_violation("http://172.17.0.1/hook").is_none());
        assert!(url_safety_violation("http://[fc00::1]/hook").is_none());
    }

    #[test]
    fn non_http_schemes_are_rejected() {
        assert!(url_safety_violation("ftp://example.com").is_some());
        assert!(url_safety_violation("file:///etc/passwd").is_some());
        assert!(url_safety_violation("not a url").is_some());
    }

    #[test]
    fn escalation_targets_are_rejected() {
        // Loopback (host admin services), link-local/metadata, unspecified —
        // the targets that grant a real privilege escalation.
        assert!(url_safety_violation("http://localhost/hook").is_some());
        assert!(url_safety_violation("http://127.0.0.1/hook").is_some());
        assert!(url_safety_violation("http://169.254.169.254/latest/meta-data").is_some());
        assert!(url_safety_violation("http://0.0.0.0/hook").is_some());
        assert!(url_safety_violation("http://[::1]/hook").is_some());
        assert!(url_safety_violation("http://[fe80::1]/hook").is_some());
    }
}
