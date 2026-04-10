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

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Config {
    pub(crate) current: bool,
    #[serde(skip_deserializing)]
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) api: config::api::Api,
    pub(crate) user: config::user::User,
    #[serde(default)]
    pub(crate) scheduler: Scheduler,
    #[serde(default)]
    pub(crate) docker: DockerConfig,
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
            },
            user: config::user::User {
                salt: "changeme".to_string(),
            },
            scheduler: Scheduler::default(),
            docker: DockerConfig::default(),
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
                for (context_name, mut config) in contexts.contexts {
                    config.name = context_name.clone();

                    if context_name == context_current {
                        debug!("Switch to context from {}", context_name);
                        return config;
                    }

                    if context_current.is_empty() && config.current {
                        debug!("Switch to context {}", context_name);
                        return config;
                    }
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
}
