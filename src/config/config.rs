use crate::config::api::Api;
use crate::config::server::ServerConfig;
use local_ip_address::local_ip;
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::fs;
use toml::de::Error as TomlError;

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Contexts {
    pub(crate) contexts: HashMap<String, Config>,
    /// The single, top-level `[server]` table. It is shared by the whole file
    /// (a host runs one daemon, whatever client contexts point at it), so it is
    /// parsed here and attached by `pick_context` to whichever context it
    /// returns, rather than living inside each `[contexts.<name>]`.
    #[serde(default)]
    pub(crate) server: ServerConfig,
}

/// Per-context CLIENT configuration: how a CLI reaches one server. The daemon's
/// own settings (runtimes, scheduler, dashboard) live in [`ServerConfig`] under
/// the top-level `[server]` table, not here. `server` is populated by
/// `pick_context` from the shared `[server]` table.
#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Config {
    pub(crate) current: bool,
    #[serde(skip_deserializing)]
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) api: Api,
    #[serde(skip_deserializing)]
    pub(crate) server: ServerConfig,
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
            api: Api {
                scheme: "http".to_string(),
                port: 3030,
                cors_origins: Vec::new(),
            },
            server: ServerConfig::default(),
        }
    }
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
    // The `[server]` table is shared across all contexts in the file; attach it
    // to whichever context we return so `configuration.server.*` is populated
    // regardless of which client context was picked.
    let server = contexts.server;
    let mut current_fallback: Option<Config> = None;
    for (context_name, mut config) in contexts.contexts {
        config.name = context_name.clone();
        config.server = server.clone();

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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_contexts(toml_str: &str) -> Contexts {
        toml::from_str(toml_str).expect("test TOML must parse")
    }

    const SAMPLE: &str = r#"
[server.runtime.cloud_hypervisor]
seccomp = "false"

[contexts.dev]
host = "0.0.0.0"
current = true
api.scheme = "http"
api.port = 3030

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
        // dropping the shared [server] config).
        let contexts = make_contexts(SAMPLE);
        let picked = pick_context(contexts, "default").expect("should fall back to current = true");
        assert_eq!(picked.name, "dev");
        assert_eq!(
            picked.server.runtime.cloud_hypervisor.seccomp.as_deref(),
            Some("false"),
            "shared [server] config must survive the fallback"
        );
    }

    #[test]
    fn server_table_is_shared_across_contexts() {
        // The top-level [server] table attaches to whichever context wins.
        let contexts = make_contexts(SAMPLE);
        let picked = pick_context(contexts, "prod").expect("prod should match");
        assert_eq!(
            picked.server.runtime.cloud_hypervisor.seccomp.as_deref(),
            Some("false"),
            "the shared [server] table applies to every context"
        );
    }

    #[test]
    fn runtimes_disabled_by_default() {
        // Opt-in: a config that doesn't mention runtimes leaves them all off.
        let contexts = make_contexts(SAMPLE);
        let picked = pick_context(contexts, "dev").expect("dev should match");
        assert!(!picked.server.runtime.docker.enabled);
        assert!(!picked.server.runtime.cloud_hypervisor.enabled);
    }

    #[test]
    fn runtimes_enabled_when_explicitly_set() {
        let toml_str = r#"
[server.runtime.docker]
enabled = true

[server.runtime.cloud_hypervisor]
enabled = true

[contexts.dev]
host = "0.0.0.0"
current = true
api.scheme = "http"
api.port = 3030
"#;
        let contexts = make_contexts(toml_str);
        let picked = pick_context(contexts, "dev").expect("dev should match");
        assert!(picked.server.runtime.docker.enabled);
        assert!(picked.server.runtime.cloud_hypervisor.enabled);
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
