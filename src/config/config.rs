use std::collections::HashMap;
use std::fs;
use std::env;
use serde::{Deserialize, Serialize};
use local_ip_address::local_ip;
use crate::config;
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
pub(crate) struct Config {
    pub(crate) current: bool,
    #[serde(skip_deserializing)]
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) api: config::api::Api,
    pub(crate) user: config::user::User,
    #[serde(default)]
    pub(crate) scheduler: Scheduler,
}

impl Config {
    pub(crate) fn get_api_url(&mut self)-> String {
        return format!("{}://{}:{}", self.api.scheme, self.host, self.api.port);
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            current: true,
            name: "default".to_string(),
            host: local_ip().unwrap_or_else(|_| {
                warn!("Failed to get local IP, using localhost");
                "127.0.0.1".parse().unwrap()
            }).to_string(),
            api: config::api::Api {
                scheme: "http".to_string(),
                port: 3030
            },
            user: config::user::User {
                salt: "changeme".to_string()
            },
            scheduler: Scheduler::default()
        }
    }
}


#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct AuthConfig {
    pub(crate) token: String
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct AuthToken {
    token: String,
}

pub(crate) fn get_config_dir() -> String {
    return match env::var_os("RING_CONFIG_DIR") {
        Some(variable) => variable.into_string().unwrap_or_else(|_| {
            error!("RING_CONFIG_DIR contains invalid Unicode");
            format!("{}/.config/kemeter/ring", env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
        }),
        None => format!("{}/.config/kemeter/ring", env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
    };
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
                        return config
                    }

                    if context_current.is_empty() && config.current {
                        debug!("Switch to context {}", context_name);
                        return config
                    }
                }
            }
            Err(err) => {
                error!("Error while deserializing the TOML file : {}", err);
            }
        }
    }

    debug!("Switch to default configuration");

    return Config::default();
}

pub(crate) fn load_auth_config(context_name: String) -> AuthConfig {
    let home_dir = get_config_dir();
    let file = format!("{}/auth.json", home_dir);
    let auth_file_content = match fs::read_to_string(file) {
        Ok(content) => content,
        Err(e) => {
            error!("Failed to read auth file: {}", e);
            return AuthConfig {
                token: String::new()
            };
        }
    };

    let context_auth: HashMap<String, AuthToken> = match serde_json::from_str(&auth_file_content) {
        Ok(auth) => auth,
        Err(e) => {
            error!("Failed to parse auth file: {}", e);
            return AuthConfig {
                token: String::new()
            };
        }
    };

    match context_auth.get(&context_name) {
        Some(auth_token) => AuthConfig {
            token: auth_token.token.clone()
        },
        None => {
            eprintln!("Error: Context '{}' does not exist in a configuration file", context_name);
            std::process::exit(1);
        }
    }
}