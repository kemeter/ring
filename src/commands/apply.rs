use clap::App;
use clap::Arg;
use clap::SubCommand;
use log::info;
use clap::ArgMatches;
use std::fs;
use std::io::prelude::*;
use std::env;
use ureq::json;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use crate::config::config::{Config, get_config_dir};
use crate::config::config::load_auth_config;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_yaml::{Value, Mapping};

#[derive(Debug, Deserialize, Serialize)]
struct Deployment {
    namespace: String,
    runtime: String,
    kind: String,
    image: String,
    name: String,
    replicas: u32,
    labels: HashMap<String, String>,
    secrets: HashMap<String, String>,
    volumes: Vec<Value>,
    config: HashMap<String, String>,
}

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("apply")
        .name("apply")
        .arg(
            Arg::with_name("file")
                .short("f")
                .long("file")
                .value_name("FILE")
                .help("Sets a custom config file")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("env-file")
                .required(false)
                .help("Use a .env file to set environment variables")
                .long("env-file")
                .short("e")
                .takes_value(true)
        )
        .arg(
            Arg::with_name("dry-run")
                .long("dry-run")
                .short("d")
                .help("previews the object that would be sent to your cluster, without actually sending it.")
        )
        .arg(
            Arg::with_name("force")
                .long("force")
                .help("Force update configuration")
        )
        .about("Apply a configuration file")
}

pub(crate) fn apply(args: &ArgMatches, mut configuration: Config) {
    info!("Apply configuration");

    let file = args.value_of("file").unwrap_or("ring.yaml");

    let contents = fs::read_to_string(file).unwrap();

    let docs = serde_yaml::from_str::<Value>(&contents).unwrap();
    let deployments = docs["deployments"].as_mapping().unwrap();

    let auth_config_file = format!("{}/auth.json", get_config_dir());

    if !Path::new(&auth_config_file).exists() {
        return println!("Account not found. Login first");
    }

    let env_file = args.value_of("env-file").unwrap_or("");
    if env_file != "" {
        let env_file_content = fs::read_to_string(env_file).unwrap();
        for variable in env_file_content.lines() {
            let variable_split: Vec<&str> = variable.split("=").collect();
            env::set_var(variable_split[0], variable_split[1]);
        };
    }

    let auth_config = load_auth_config(configuration.name.clone());

    for (deployment_name, deployment_data) in deployments.iter() {
        let deployment_data = deployment_data.as_mapping().unwrap();

        let mut deployment = Deployment {
            namespace: String::new(),
            runtime: String::new(),
            kind: String::from("worker"),
            image: String::new(),
            name: String::new(),
            replicas: 0,
            labels: Default::default(),
            secrets: Default::default(),
            volumes: vec![],
            config: Default::default(),
        };

        for (label, value) in deployment_data.iter() {
            let label = label.as_str().unwrap();

            match label {
                "runtime" if "docker" != value.as_str().unwrap() => {
                    println!("Runtime \"{}\" not supported", value.as_str().unwrap());
                    continue;
                }
                "namespace" => deployment.namespace = env_resolver(value.as_str().unwrap()),
                "name" => deployment.name = env_resolver(value.as_str().unwrap()),
                "runtime" => deployment.runtime = env_resolver(value.as_str().unwrap()),
                "image" => deployment.image = env_resolver(value.as_str().unwrap()),
                "replicas" => deployment.replicas = value.as_i64().unwrap() as u32,
                "kind" => deployment.kind = env_resolver(value.as_str().unwrap()),
                "volumes" => {
                    for volume in value.as_sequence().unwrap() {
                        let volume_string: Vec<&str> = volume.as_str().unwrap().split(":").collect();

                        let permission = if volume_string.len() == 3 { volume_string[2] } else { "rw" };

                        let mut volume_obj = Mapping::new();
                        volume_obj.insert(Value::String("source".to_string()), Value::String(volume_string[0].to_string()));
                        volume_obj.insert(Value::String("destination".to_string()), Value::String(volume_string[1].to_string()));
                        volume_obj.insert(Value::String("driver".to_string()), Value::String("local".to_string()));
                        volume_obj.insert(Value::String("permission".to_string()), Value::String(permission.to_string()));

                        deployment.volumes.push(Value::Mapping(volume_obj));
                    }
                }
                "labels" => {
                    let labels_seq = value.as_sequence().unwrap();
                    if !labels_seq.is_empty() {
                        for l in labels_seq {
                            for (k, v) in l.as_mapping().unwrap().iter() {
                                deployment.labels.insert(k.as_str().unwrap().to_string(), v.as_str().unwrap().to_string());
                            }
                        }
                    }
                }
                "secrets" => {
                    let secrets_map = value.as_mapping().unwrap();
                    for (secret_key, secret_value) in secrets_map.iter() {
                        let secret_key = secret_key.as_str().unwrap().to_string();
                        let mut secret_value = secret_value.as_str().unwrap().to_string();
                        secret_value.remove(0);
                        let value_format = env::var(&secret_value).unwrap_or_else(|_| secret_value.clone());
                        deployment.secrets.insert(secret_key, value_format);
                    }
                }
                "config" => {
                    let config_map = value.as_mapping().unwrap();
                    for (config_key, config_value) in config_map.iter() {
                        deployment.config.insert(config_key.as_str().unwrap().to_string(), config_value.as_str().unwrap().to_string());
                    }
                }
                _ => {}
            }
        }

        let api_url = configuration.get_api_url();

        info!("push configuration: {}", api_url);

        let json = json!(deployment);

        if args.is_present("dry-run") {
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
        } else {
            let mut url = format!("{}/deployments", api_url);

            if args.is_present("force") {
                url.push_str("?force=true");
            }

            let request = ureq::post(&url)
                .set("Authorization", &format!("Bearer {}", auth_config.token))
                .send_json(json);

            match request {
                Ok(_response) => {
                    println!("deployment {} created", deployment.name);
                }
                Err(error) => {
                    println!("{:?}", error)
                }
            }
        }
    }
}

fn env_resolver(text: &str) -> String {
    let tag_regex: Regex = Regex::new(r"\$[a-zA-Z][0-9a-zA-Z_]*").unwrap();
    let list: HashSet<&str> = tag_regex.find_iter(text).map(|mat| mat.as_str()).collect();
    let mut content = String::from(text);

    for variable in list.into_iter() {
        let key = variable.replace("$", "");

        let value = match env::var(key) {
            Ok(val) => String::from(val),
            Err(_) => String::from(variable),
        };
        content = content.replace(variable, &value);
    }

    content
}
