use std::fs::File;
use std::io::Read;
use std::fs;
use std::env;
use serde::Deserialize;
use local_ip_address::local_ip;
use crate::config;

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Config {
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
    return match env::var_os("RING_CONFIG_FILE") {
        Some(variable) => variable.into_string().unwrap(),
        None => format!("{}/.config/kemeter/ring", env::var("HOME").unwrap())
    };
}

pub(crate) fn load_config() -> Config {
    let home_dir = get_config_dir();
    let file = format!("{}/config.toml", home_dir);

    debug!("load config file {}", file);

    if fs::metadata(file.clone()).is_ok() {

        let contents = fs::read_to_string(file).unwrap();
        let config: Config = toml::from_str(&contents).unwrap();

        return config;
    }

    debug!("Switch to default configuration");

    return Config {
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

pub(crate) fn load_auth_config() -> AuthConfig {
    let home_dir = get_config_dir();
    let file = format!("{}/auth.json", home_dir);
    let contents = fs::read_to_string(file).unwrap();

    let config: AuthConfig = serde_json::from_str(&contents).unwrap();

    return config;
}

