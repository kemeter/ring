use std::collections::HashMap;
use std::fs;
use std::env;
use serde::Deserialize;
use local_ip_address::local_ip;
use crate::config;
use toml::de::Error as TomlError;

#[derive(Deserialize, Debug, Clone)]
struct Contexts {
    contexts: HashMap<String, Config>,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Config {
    pub(crate) current: bool,
    #[serde(skip_deserializing)]
    pub(crate) name: String,
    pub(crate) ip: String,
    pub(crate) api: config::api::Api,
    pub(crate) user: config::user::User,
}

impl Config {
    pub(crate) fn get_api_url(&mut self)-> String {
        return format!("{}://{}:{}", self.api.scheme, self.ip, self.api.port);
    }
}

#[derive(Deserialize, Debug)]
pub(crate) struct AuthConfig {
    pub(crate) token: String
}

pub(crate) fn get_config_dir() -> String {
    return match env::var_os("RING_CONFIG_DIR") {
        Some(variable) => variable.into_string().unwrap(),
        None => format!("{}/.config/kemeter/ring", env::var("HOME").unwrap())
    };
}

pub(crate) fn load_config() -> Config {
    let home_dir = get_config_dir();

    let context_current = get_current_context();

    let file = format!("{}/config.toml", home_dir);

    debug!("load config file {}", file);

    if fs::metadata(file.clone()).is_ok() {
        let contents = fs::read_to_string(file).unwrap();
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

    return Config {
        current: true,
        name: "default".to_string(),
        ip: local_ip().unwrap().to_string(),
        api: config::api::Api {
            scheme: "http".to_string(),
            port: 3030
        },
        user: config::user::User {
            salt: "changeme".to_string()
        }
    }
}

fn get_current_context() -> String {
    let args: Vec<_> = std::env::args().collect();
    let mut context_current = String::new();

    for arg in args {
        let path = arg.clone().to_string();
        if path.starts_with("--c") || path.starts_with("--context") {
            context_current = arg
                .replace("--context", "")
                .replace("--c", "")
                .replace("=", "");
        }
    }
    context_current
}

pub(crate) fn load_auth_config() -> AuthConfig {
    let home_dir = get_config_dir();
    let file = format!("{}/auth.json", home_dir);
    let contents = fs::read_to_string(file).unwrap();

    let config: AuthConfig = serde_json::from_str(&contents).unwrap();

    return config;
}