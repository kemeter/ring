use serde::{Deserialize, Serialize};
use std::time::Duration;

fn default_threshold() -> u32 {
    3
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub(crate) enum HealthCheck {
    #[serde(rename = "tcp")]
    Tcp {
        port: u16,
        interval: String,
        timeout: String,
        #[serde(default = "default_threshold")]
        threshold: u32,
        on_failure: FailureAction,
    },
    #[serde(rename = "http")]
    Http {
        url: String,
        interval: String,
        timeout: String,
        #[serde(default = "default_threshold")]
        threshold: u32,
        on_failure: FailureAction,
    },
    #[serde(rename = "command")]
    Command {
        command: String,
        interval: String,
        timeout: String,
        #[serde(default = "default_threshold")]
        threshold: u32,
        on_failure: FailureAction,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FailureAction {
    Restart,
    Stop,
    Alert,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct HealthCheckResult {
    pub(crate) id: String,
    pub(crate) deployment_id: String,
    pub(crate) check_type: String,
    pub(crate) status: HealthCheckStatus,
    pub(crate) message: Option<String>,
    pub(crate) created_at: String,
    pub(crate) started_at: String,
    pub(crate) finished_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub(crate) enum HealthCheckStatus {
    Success,
    Failed,
    Timeout,
}

impl HealthCheck {
    pub(crate) fn parse_duration(duration_str: &str) -> Result<Duration, String> {
        if duration_str.ends_with('s') {
            let seconds = duration_str[..duration_str.len() - 1]
                .parse::<u64>()
                .map_err(|_| format!("Invalid duration format: {}", duration_str))?;
            Ok(Duration::from_secs(seconds))
        } else if duration_str.ends_with("ms") {
            let millis = duration_str[..duration_str.len() - 2]
                .parse::<u64>()
                .map_err(|_| format!("Invalid duration format: {}", duration_str))?;
            Ok(Duration::from_millis(millis))
        } else {
            Err(format!("Invalid duration format: {}", duration_str))
        }
    }


    pub(crate) fn timeout(&self) -> &str {
        match self {
            HealthCheck::Tcp { timeout, .. } => timeout,
            HealthCheck::Http { timeout, .. } => timeout,
            HealthCheck::Command { timeout, .. } => timeout,
        }
    }

    pub(crate) fn threshold(&self) -> u32 {
        match self {
            HealthCheck::Tcp { threshold, .. } => *threshold,
            HealthCheck::Http { threshold, .. } => *threshold,
            HealthCheck::Command { threshold, .. } => *threshold,
        }
    }

    pub(crate) fn on_failure(&self) -> &FailureAction {
        match self {
            HealthCheck::Tcp { on_failure, .. } => on_failure,
            HealthCheck::Http { on_failure, .. } => on_failure,
            HealthCheck::Command { on_failure, .. } => on_failure,
        }
    }

    pub(crate) fn check_type(&self) -> &str {
        match self {
            HealthCheck::Tcp { .. } => "tcp",
            HealthCheck::Http { .. } => "http",
            HealthCheck::Command { .. } => "command",
        }
    }
}

impl Default for HealthCheck {
    fn default() -> Self {
        HealthCheck::Tcp {
            port: 8080,
            interval: "30s".to_string(),
            timeout: "5s".to_string(),
            threshold: 3,
            on_failure: FailureAction::Restart,
        }
    }
}