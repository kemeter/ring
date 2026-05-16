use crate::config;
use local_ip_address::local_ip;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use toml::de::Error as TomlError;

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Contexts {
    pub(crate) contexts: HashMap<String, Config>,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Scheduler {
    #[serde(default = "default_scheduler_interval")]
    pub(crate) interval: u64,
}

fn default_scheduler_interval() -> u64 {
    10
}

impl Default for Scheduler {
    fn default() -> Self {
        Scheduler { interval: 10 }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct DockerConfig {
    /// Docker host URL. Examples:
    /// - "unix:///var/run/docker.sock" (default)
    /// - "tcp://192.168.1.100:2375"
    /// - "tcp://192.168.1.100:2376" (with TLS)
    #[serde(default = "default_docker_host")]
    pub(crate) host: String,
}

fn default_docker_host() -> String {
    "unix:///var/run/docker.sock".to_string()
}

impl Default for DockerConfig {
    fn default() -> Self {
        DockerConfig {
            host: default_docker_host(),
        }
    }
}

/// User-facing configuration for the Cloud Hypervisor runtime. Parsed from the
/// `[contexts.<name>.runtime.cloud_hypervisor]` section of `config.toml`.
///
/// All fields are optional; when unset, `CloudHypervisorRuntimeConfig::default`
/// falls back to `$RING_CONFIG_DIR/cloud-hypervisor/...` for backward
/// compatibility.
#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct CloudHypervisorConfig {
    pub(crate) binary_path: Option<String>,
    pub(crate) firmware_path: Option<String>,
    pub(crate) socket_dir: Option<String>,
    /// Forwarded to `cloud-hypervisor --seccomp <value>`. Accepts `true`
    /// (default), `false` or `log`. Set to `false` on hosts where the kernel
    /// uses syscalls not whitelisted by CH (otherwise VMs die with SIGSYS).
    pub(crate) seccomp: Option<String>,
    /// Maximum size (bytes) for a per-VM console log before rotation kicks
    /// in. Defaults to 10 MiB. Set to 0 to disable rotation entirely.
    pub(crate) max_console_log_bytes: Option<u64>,
    /// How many rotated console log backups to keep alongside the live file
    /// (`.console.log.1`, `.console.log.2`, ...). Defaults to 3.
    pub(crate) max_console_log_backups: Option<u32>,
}

/// User-facing configuration for the embedded web dashboard. Off by default
/// to keep the server surface minimal until an operator opts in.
#[derive(Deserialize, Debug, Clone)]
pub(crate) struct DashboardConfig {
    /// When true, `ring server start` spawns the dashboard on
    /// `listen_address`. When false (the default), the dashboard is not
    /// served by this Ring instance — operators can still run
    /// `ring dashboard` locally against any API.
    #[serde(default)]
    pub(crate) enabled: bool,
    /// `host:port` for the dashboard to bind to. Distinct from the API
    /// port to keep concerns separated.
    #[serde(default = "default_dashboard_listen_address")]
    pub(crate) listen_address: String,
}

fn default_dashboard_listen_address() -> String {
    "127.0.0.1:3031".to_string()
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_address: default_dashboard_listen_address(),
        }
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct RuntimesConfig {
    #[serde(default)]
    pub(crate) cloud_hypervisor: CloudHypervisorConfig,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Config {
    pub(crate) current: bool,
    #[serde(skip_deserializing)]
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) api: config::api::Api,
    #[serde(default)]
    pub(crate) scheduler: Scheduler,
    #[serde(default)]
    pub(crate) docker: DockerConfig,
    #[serde(default)]
    pub(crate) runtime: RuntimesConfig,
    #[serde(default)]
    pub(crate) dashboard: DashboardConfig,
}

impl Config {
    pub(crate) fn get_api_url(&mut self) -> String {
        format!("{}://{}:{}", self.api.scheme, self.host, self.api.port)
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            current: true,
            name: "default".to_string(),
            host: local_ip()
                .unwrap_or_else(|_| {
                    warn!("Failed to get local IP, using localhost");
                    "127.0.0.1".parse().unwrap()
                })
                .to_string(),
            api: config::api::Api {
                scheme: "http".to_string(),
                port: 3030,
                cors_origins: Vec::new(),
            },
            scheduler: Scheduler::default(),
            docker: DockerConfig::default(),
            runtime: RuntimesConfig::default(),
            dashboard: DashboardConfig::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct AuthConfig {
    pub(crate) token: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct AuthToken {
    token: String,
}

pub(crate) fn get_config_dir() -> String {
    match env::var_os("RING_CONFIG_DIR") {
        Some(variable) => variable.into_string().unwrap_or_else(|_| {
            error!("RING_CONFIG_DIR contains invalid Unicode");
            format!(
                "{}/.config/kemeter/ring",
                env::var("HOME").unwrap_or_else(|_| "/tmp".to_string())
            )
        }),
        None => format!(
            "{}/.config/kemeter/ring",
            env::var("HOME").unwrap_or_else(|_| "/tmp".to_string())
        ),
    }
}

pub(crate) fn load_config(context_current: &str) -> Config {
    let home_dir = get_config_dir();

    let file = format!("{}/config.toml", home_dir);

    debug!("load config file {}", file);

    if fs::metadata(file.clone()).is_ok() {
        let contents = match fs::read_to_string(file) {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to read config file: {}", e);
                return Config::default();
            }
        };
        let contexts: Result<Contexts, TomlError> = toml::from_str(&contents);

        match contexts {
            Ok(contexts) => {
                if let Some(config) = pick_context(contexts, context_current) {
                    return config;
                }
            }
            Err(err) => {
                error!("Error while deserializing the TOML file : {}", err);
            }
        }
    }

    debug!("Switch to default configuration");

    Config::default()
}

/// Pick a context out of the parsed `Contexts`. Tries an exact name match
/// first, then falls back to whichever context has `current = true`.
///
/// Splitting this out of `load_config` keeps the matching logic testable
/// without touching the filesystem, and it dodges the historical trap
/// where the caller passed a placeholder name like "default" — without an
/// explicit match the loader used to silently fall through to
/// `Config::default()`, dropping any user-set runtime config.
fn pick_context(contexts: Contexts, context_current: &str) -> Option<Config> {
    let mut current_fallback: Option<Config> = None;
    for (context_name, mut config) in contexts.contexts {
        config.name = context_name.clone();

        if context_name == context_current {
            debug!("Switch to context from {}", context_name);
            return Some(config);
        }

        if config.current && current_fallback.is_none() {
            current_fallback = Some(config);
        }
    }

    if let Some(config) = &current_fallback {
        debug!(
            "No context matched name '{}', falling back to current = {}",
            context_current, config.name
        );
    }

    current_fallback
}

fn auth_token_from_env<F>(get_var: F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    get_var("RING_TOKEN").filter(|t| !t.is_empty())
}

pub(crate) fn load_auth_config(context_name: String) -> AuthConfig {
    // RING_TOKEN takes precedence over auth.json. Useful for CI and
    // stateless environments where running `ring login` is impractical.
    if let Some(token) = auth_token_from_env(|k| env::var(k).ok()) {
        return AuthConfig { token };
    }

    let home_dir = get_config_dir();
    let file = format!("{}/auth.json", home_dir);
    let auth_file_content = match fs::read_to_string(file) {
        Ok(content) => content,
        Err(e) => {
            error!("Failed to read auth file: {}", e);
            return AuthConfig {
                token: String::new(),
            };
        }
    };

    let context_auth: HashMap<String, AuthToken> = match serde_json::from_str(&auth_file_content) {
        Ok(auth) => auth,
        Err(e) => {
            error!("Failed to parse auth file: {}", e);
            return AuthConfig {
                token: String::new(),
            };
        }
    };

    match context_auth.get(&context_name) {
        Some(auth_token) => AuthConfig {
            token: auth_token.token.clone(),
        },
        None => {
            eprintln!(
                "Error: Context '{}' does not exist in a configuration file",
                context_name
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_token_from_env_returns_some_when_set() {
        let token = auth_token_from_env(|k| {
            if k == "RING_TOKEN" {
                Some("abc123".to_string())
            } else {
                None
            }
        });
        assert_eq!(token.as_deref(), Some("abc123"));
    }

    #[test]
    fn ring_token_from_env_is_none_when_unset() {
        let token = auth_token_from_env(|_| None);
        assert!(token.is_none());
    }

    #[test]
    fn ring_token_from_env_is_none_when_empty() {
        let token = auth_token_from_env(|k| {
            if k == "RING_TOKEN" {
                Some(String::new())
            } else {
                None
            }
        });
        assert!(token.is_none());
    }

    fn make_contexts(toml_str: &str) -> Contexts {
        toml::from_str(toml_str).expect("test TOML must parse")
    }

    const SAMPLE: &str = r#"
[contexts.dev]
host = "0.0.0.0"
current = true
api.scheme = "http"
api.port = 3030

[contexts.dev.runtime.cloud_hypervisor]
seccomp = "false"

[contexts.prod]
host = "10.0.0.1"
current = false
api.scheme = "http"
api.port = 3030
"#;

    #[test]
    fn pick_context_matches_by_name() {
        let contexts = make_contexts(SAMPLE);
        let picked = pick_context(contexts, "prod").expect("prod should match");
        assert_eq!(picked.name, "prod");
        assert_eq!(picked.host, "10.0.0.1");
    }

    #[test]
    fn pick_context_falls_back_to_current_when_name_does_not_match() {
        // This is the regression: the caller passes "default" but no context
        // is literally named "default". Before the fix, this returned None
        // (and load_config fell through to Config::default(), silently
        // dropping runtime.cloud_hypervisor.seccomp).
        let contexts = make_contexts(SAMPLE);
        let picked = pick_context(contexts, "default").expect("should fall back to current = true");
        assert_eq!(picked.name, "dev");
        assert_eq!(
            picked.runtime.cloud_hypervisor.seccomp.as_deref(),
            Some("false"),
            "user-set runtime config must survive the fallback"
        );
    }

    #[test]
    fn pick_context_falls_back_to_current_when_name_is_empty() {
        let contexts = make_contexts(SAMPLE);
        let picked = pick_context(contexts, "").expect("empty name should fall back");
        assert_eq!(picked.name, "dev");
    }

    #[test]
    fn pick_context_returns_none_when_nothing_matches_and_no_current() {
        let toml_str = r#"
[contexts.a]
host = "1.1.1.1"
current = false
api.scheme = "http"
api.port = 3030
"#;
        let contexts = make_contexts(toml_str);
        assert!(pick_context(contexts, "missing").is_none());
    }

    #[test]
    fn pick_context_prefers_exact_name_over_current() {
        let contexts = make_contexts(SAMPLE);
        // dev has current=true, prod is the named target. Named must win.
        let picked = pick_context(contexts, "prod").expect("prod should match");
        assert_eq!(picked.name, "prod");
    }
}
