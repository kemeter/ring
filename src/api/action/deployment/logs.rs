use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    Json
};
use chrono::Utc;
use serde::Deserialize;

use crate::api::server::Db;
use crate::models::deployments;
use crate::runtime::runtime::Runtime;
use crate::models::users::User;

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    #[serde(default = "default_tail")]
    tail: Option<u64>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    container: Option<String>,
}

fn default_tail() -> Option<u64> {
    Some(100)
}

fn parse_since(since: &str) -> Option<i32> {
    let re = regex::Regex::new(r"^(\d+)(s|m|h)$").unwrap();
    if let Some(caps) = re.captures(since) {
        let value: i64 = caps[1].parse().ok()?;
        let seconds = match &caps[2] {
            "s" => value,
            "m" => value * 60,
            "h" => value * 3600,
            _ => return None,
        };
        let timestamp = Utc::now().timestamp() - seconds;
        return Some(timestamp as i32);
    }

    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(since) {
        return Some(dt.timestamp() as i32);
    }

    None
}

pub(crate) async fn logs(
    Path(id): Path<String>,
    Query(params): Query<LogsQuery>,
    _user: User,
    State(connexion): State<Db>,
) -> impl IntoResponse {
    let guard = connexion.lock().await;
    let deployment_result = deployments::find(&guard, id.clone());

    match deployment_result {
        Ok(Some(deployment)) => {
            let runtime = Runtime::new(deployment);

            let tail = params.tail.map(|t| t.to_string());
            let since = params.since.as_deref().and_then(parse_since);

            let logs = runtime.get_logs(
                tail.as_deref(),
                since,
                params.container.as_deref(),
            ).await;
            Json(logs)
        }
        Ok(None) => {
            Json(Vec::<crate::runtime::runtime::Log>::new())
        }
        Err(_) => {
            Json(Vec::<crate::runtime::runtime::Log>::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_since_relative_seconds() {
        let result = parse_since("30s");
        assert!(result.is_some());
        let now = Utc::now().timestamp() as i32;
        assert!((result.unwrap() - (now - 30)).abs() <= 1);
    }

    #[test]
    fn test_parse_since_relative_minutes() {
        let result = parse_since("10m");
        assert!(result.is_some());
        let now = Utc::now().timestamp() as i32;
        assert!((result.unwrap() - (now - 600)).abs() <= 1);
    }

    #[test]
    fn test_parse_since_relative_hours() {
        let result = parse_since("2h");
        assert!(result.is_some());
        let now = Utc::now().timestamp() as i32;
        assert!((result.unwrap() - (now - 7200)).abs() <= 1);
    }

    #[test]
    fn test_parse_since_iso8601() {
        let result = parse_since("2024-01-01T00:00:00Z");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), 1704067200);
    }

    #[test]
    fn test_parse_since_invalid() {
        assert!(parse_since("invalid").is_none());
        assert!(parse_since("30x").is_none());
        assert!(parse_since("abc123").is_none());
    }
}
