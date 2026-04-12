use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{KeepAlive, Sse},
    },
};
use chrono::Utc;
use regex::Regex;
use serde::Deserialize;
use std::sync::LazyLock;

use crate::api::server::{Db, RuntimeMap};
use crate::models::deployments;
use crate::models::users::User;
use crate::runtime::lifecycle_trait::Log;

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    #[serde(default = "default_tail")]
    tail: Option<u64>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    container: Option<String>,
    #[serde(default)]
    follow: bool,
}

fn default_tail() -> Option<u64> {
    Some(100)
}

static SINCE_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+)(s|m|h)$").unwrap());

fn parse_since(since: &str) -> Option<i32> {
    let re = &*SINCE_REGEX;
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
    State(pool): State<Db>,
    State(runtimes): State<RuntimeMap>,
) -> impl IntoResponse {
    match deployments::find(&pool, &id).await {
        Ok(Some(deployment)) => {
            let runtime = match runtimes.get(&deployment.runtime) {
                Some(rt) => rt,
                None => return StatusCode::NOT_FOUND.into_response(),
            };

            let tail = params.tail.map(|t| t.to_string());
            let since = params.since.as_deref().and_then(parse_since);

            if params.follow {
                let stream = runtime
                    .stream_logs(&deployment.id, tail.as_deref(), since, params.container.as_deref())
                    .await;

                Sse::new(stream)
                    .keep_alive(KeepAlive::default())
                    .into_response()
            } else {
                let logs = runtime
                    .get_logs(&deployment.id, tail.as_deref(), since, params.container.as_deref())
                    .await;
                Json(logs).into_response()
            }
        }
        Ok(None) => Json(Vec::<Log>::new()).into_response(),
        Err(_) => Json(Vec::<Log>::new()).into_response(),
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
    fn test_parse_since_invalid() {
        assert!(parse_since("invalid").is_none());
        assert!(parse_since("5d").is_none());
        assert!(parse_since("").is_none());
    }

    #[test]
    fn test_parse_since_rfc3339() {
        let result = parse_since("2024-01-15T10:30:00Z");
        assert!(result.is_some());
        let expected = chrono::DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z")
            .unwrap()
            .timestamp() as i32;
        assert_eq!(result.unwrap(), expected);
    }
}
