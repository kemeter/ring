use clap::App;
use clap::Arg;
use clap::SubCommand;
use log::info;
use clap::ArgMatches;
use std::fs;
use std::io::prelude::*;
use yaml_rust::YamlLoader;
use std::str;
use std::env;
use ureq::json;
use ureq::Error;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use serde::de::Unexpected::Str;
use crate::config::config::{Config, get_config_dir};
use crate::config::config::load_auth_config;
use regex::Regex;

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
            Arg::with_name("dry-run")
                .long("dry-run")
                .short("d")
                .help("previews the object that would be sent to your cluster, without actually sending it.")
        )
        .about("Apply a configuration file")
}

pub(crate) fn apply(args: &ArgMatches, mut configuration: Config) {
    info!("Apply configuration");

    let file = args.value_of("file").unwrap_or("ring.yaml");

    let contents = fs::read_to_string(file).unwrap();

    let docs = YamlLoader::load_from_str(&contents).unwrap();
    let deployments = &docs[0]["deployments"].as_hash().unwrap();

    let auth_config_file= format!("{}/auth.json", get_config_dir());

    if !Path::new(&auth_config_file).exists() {
        return println!("Account not found. Login first");
    }

    let auth_config = load_auth_config();

    for entry in deployments.iter() {
        let deployment_name = entry.0.as_str().unwrap();

        let configs = &docs[0]["deployments"][deployment_name].as_hash().unwrap();

        let mut namespace: &str = "";
        let mut runtime: &str = "";
        let mut image= String::new();
        let mut name: &str = "";
        let mut replicas = 0;
        let mut labels = String::from("{}");
        let mut secrets = String::from("{}");

        for key in configs.iter() {

            let label = key.0.as_str().unwrap();
            let value = &docs[0]["deployments"][deployment_name][label];

            if "runtime" == label && "docker" != value.as_str().unwrap() {
                println!("Runtime \"{}\" not supported", value.as_str().unwrap());
                continue;
            }

            if "namespace" == label {
                namespace = value.as_str().unwrap();
            }

            if "name" == label {
                name = value.as_str().unwrap();
            }

            if "runtime" == label {
                runtime = value.as_str().unwrap();
            }

            if "image" == label {

                let v = value.as_str().unwrap();
                image = env_resolver(v.to_string());
            }

            if "replicas" == label {
                replicas = value.as_i64().unwrap();
            }

            if "labels" == label {
                let labels_vec = value.as_vec().unwrap();

                if labels_vec.len() > 0 {
                    let mut map = HashMap::new();

                    for l in labels_vec {
                        for v in l.as_hash().unwrap().iter() {
                            map.insert(v.0.as_str().unwrap(), v.1.as_str().unwrap());
                        }
                    }

                    labels = serde_json::to_string(&map).unwrap();
                }
            }

            if "secrets" == label {
                let secrets_vec = value.as_hash().unwrap();
                let mut map = HashMap::new();

                for v in secrets_vec.iter() {
                    let mut secret_value = String::from(v.1.as_str().unwrap());
                    secret_value.remove(0);

                    let value_format = env::var(&secret_value).unwrap_or(v.1.as_str().unwrap().to_string());
                    map.insert(v.0.as_str().unwrap(), value_format);
                }

                secrets = serde_json::to_string(&map).unwrap();
            }
        }

        let api_url = configuration.get_api_url();

        info!("push configuration: {}", api_url);

        let json = json!({
            "image": image,
            "name": name,
            "runtime": runtime,
            "namespace": namespace,
            "replicas": replicas,
            "labels": labels,
            "secrets": secrets
        });

        if args.is_present("dry-run") {
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
        } else {
            let request = ureq::post(&format!("{}/deployments", api_url))
                .set("Authorization", &format!("Bearer {}", auth_config.token))
                .send_json(json);

            match request {
                Ok(_response) => {
                    println!("deployment {} created", name);
                }
                Err(error) => {
                    println!("{:?}", error)
                }
            }
        }
    }
}

fn env_resolver(text: String) -> String {
    let tag_regex: Regex = Regex::new(
            r"\$[a-zA-Z][0-9a-zA-Z_]*"
        ).unwrap();
    let list: HashSet<&str> = tag_regex.find_iter(text.as_str()).map(|mat| mat.as_str()).collect();
    let mut content = text.clone();

    for variable in list.into_iter() {
        let key = variable.replace("$", "");

        let value = match env::var(key) {
            Ok(val) => String::from(val),
            Err(e) => String::from(variable),
        };
        content = content.replace(variable, value.as_str());
    }

    return String::from(content);
}

#[cfg(test)]
mod tests {
    use std::env;
    use crate::commands::apply::env_resolver;

    #[test]
    fn test_env_resolver() {
        env::set_var("APP_VERSION", "v1");

        let result = env_resolver(String::from("registry.hub.docker.com/busybox:$APP_VERSION"));
        assert_eq!(result, String::from("registry.hub.docker.com/busybox:v1"));

        env::set_var("REGISTRY", "hub.docker.com");
        let result = env_resolver(String::from("registry.$REGISTRY/busybox:$APP_VERSION"));
        assert_eq!(result, String::from("registry.hub.docker.com/busybox:v1"));
    }
}