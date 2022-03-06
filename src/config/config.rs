use std::fs::File;
use std::io::Read;
use std::fs;
use std::env;
use serde::Deserialize;
use local_ip_address::local_ip;
use crate::config;

#[derive(Deserialize, Debug)]
pub(crate) struct Config {
    pub(crate) ip: String,
    pub(crate) api: config::api::Api,
}

pub(crate) fn get_config_dir() -> String {
    let config_dir = env::var("HOME").unwrap();

    return format!("{}/.config/kemeter/ring", config_dir);
}

pub(crate) fn load_config() -> Config {
    let home_dir = get_config_dir();

    let file = format!("{}/config.toml", home_dir);

    if fs::metadata(file.clone()).is_ok() {
        let mut config = File::open(file).expect("Unable to open file");
        let mut contents = String::new();

        config.read_to_string(&mut contents).expect("Unable to read file");

        let config: Config = toml::from_str(&contents).unwrap();

        return config;
    }

    let my_local_ip = local_ip().unwrap();

    return Config {
        ip: my_local_ip.to_string(),
        api: config::api::Api {
            scheme: "http".to_string(),
            port: 3030
        }
    }
}
