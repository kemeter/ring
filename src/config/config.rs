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
pub(crate) struct Config {
    pub(crate) current: bool,
    #[serde(skip_deserializing)]
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) api: config::api::Api,
    pub(crate) user: config::user::User,
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
            host: local_ip().unwrap().to_string(),
            api: config::api::Api {
                scheme: "http".to_string(),
                port: 3030
            },
            user: config::user::User {
                salt: "changeme".to_string()
            }
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
        Some(variable) => variable.into_string().unwrap(),
        None => format!("{}/.config/kemeter/ring", env::var("HOME").unwrap())
    };
}

pub(crate) fn load_config(context_current: &str) -> Config {
    let home_dir = get_config_dir();

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

    return Config::default();
}

pub(crate) fn load_auth_config(context_name: String) -> AuthConfig {
    let home_dir = get_config_dir();
    let file = format!("{}/auth.json", home_dir);
    let auth_file_content = fs::read_to_string(file).unwrap();

    let context_auth: HashMap<String, AuthToken> = serde_json::from_str(&auth_file_content).unwrap();

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