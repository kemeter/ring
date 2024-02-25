use clap::{ArgAction, Command};
use clap::Arg;
use log::info;
use clap::ArgMatches;
use std::fs;
use std::env;
use ureq::json;
use std::collections::HashMap;
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

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("apply")
        .name("apply")
        .about("Apply a configuration file")
        .arg(
            Arg::new("file")
                .short('f')
                .long("file")
                .value_name("FILE")
                .help("Sets a custom config file")
                // .takes_value(),
        )
        .arg(
            Arg::new("env-file")
                .required(false)
                .help("Use a .env file to set environment variables")
                .long("env-file")
                .short('e')
                // .takes_value()
        )
        .arg(
            Arg::new("dry-run")
                .required(false)
                .long("dry-run")
                .short('d')
                .action(ArgAction::SetTrue)
                .help("previews the object that would be sent to your cluster, without actually sending it.")
        )
        .arg(
            Arg::new("force")
                .long("force")
                .help("Force update configuration")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("verbose")
                .long("verbose")
                .help("Verbose output")
                .action(ArgAction::SetTrue)
        )
        .about("Apply a configuration file")
}

pub(crate) fn apply(args: &ArgMatches, mut configuration: Config) {
    debug!("Apply configuration");

    let binding = String::from("ring.yaml");
    let file = args.get_one::<String>("file").unwrap_or(&binding);
    let contents = match fs::read_to_string(file) {
        Ok(contents) => contents,
        Err(e) => {
            eprintln!("Error: Failed to read file '{}': {}", file, e);
            std::process::exit(1);
        }
    };

    let docs = serde_yaml::from_str::<Value>(&contents).unwrap();
    let deployments = docs["deployments"].as_mapping().unwrap();

    let auth_config_file = format!("{}/auth.json", get_config_dir());

    if !Path::new(&auth_config_file).exists() {
        return println!("Account not found. Login first");
    }

    let binding = String::from("");
    let env_file = args.get_one::<String>("env-file").unwrap_or(&binding);
    parse_env_file(env_file);

    let auth_config = load_auth_config(configuration.name.clone());

    for (_, deployment_data) in deployments.iter() {
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
                        let secret_value = secret_value.as_str().unwrap().to_string();
                        let value_format = env_resolver(&secret_value);
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

        if args.contains_id("verbose") {
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
        }

        if args.contains_id("dry-run") {
            let mut url = format!("{}/deployments", api_url);

            if args.contains_id("force") {
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

fn parse_env_file(env_file: &str) {
    if env_file != "" {
        let env_file_content = fs::read_to_string(env_file).unwrap();
        for variable in env_file_content.lines() {
            let variable_split: Vec<&str> = variable.splitn(2, "=").collect();

            if variable_split.len() == 2 {
                let key = variable_split[0];
                let value = variable_split[1].trim_matches('"');

                if env::var(key).is_ok() {
                    continue;
                }

                env::set_var(key, value);
            }
        }
    }
}

fn env_resolver(text: &str) -> String {
    let tag_regex: Regex = Regex::new(r"\$[a-zA-Z][0-9a-zA-Z_]*").unwrap();
    let mut content = String::from(text);

    for variable in tag_regex.find_iter(text) {
        let variable_str = variable.as_str();
        let key = variable_str[1..].to_string();

        let value = match env::var(&key) {
            Ok(val) => val,
            Err(_) => variable_str.to_string(),
        };
        content = content.replace(variable_str, &value);
    }

    content
}

#[cfg(test)]
mod tests {
    use std::{env, fs};
    use std::io::Write;
    use crate::commands::apply::{env_resolver, parse_env_file};

    #[test]
    fn test_env_resolver() {
        env::set_var("APP_VERSION", "v1");

        let result = env_resolver("registry.hub.docker.com/busybox:$APP_VERSION");
        assert_eq!(result, String::from("registry.hub.docker.com/busybox:v1"));

        env::set_var("REGISTRY", "hub.docker.com");
        let result = env_resolver("registry.$REGISTRY/busybox:$APP_VERSION");
        assert_eq!(result, String::from("registry.hub.docker.com/busybox:v1"));

        let result = env_resolver("APP$TEST");
        assert_eq!(result, String::from("APP$TEST"));
    }

    #[test]
    fn test_parse_env_file_with_different_types() {
        // Create a temporary .env file for the test
        let temp_dir = tempdir::TempDir::new("test_env_file").unwrap();
        let env_file_path = temp_dir.path().join(".env");
        let mut env_file = fs::File::create(&env_file_path).unwrap();
        env_file
            .write_all(b"DATABASE_URL=postgres://test:J4OqcB7jPTGYx@127.0.0.1/alpacode?serverVersion=14&charset=utf8\n")
            .unwrap();
        env_file.write_all(b"INT_VAR=42\n").unwrap();
        env_file.write_all(b"BOOL_VAR=true\n").unwrap();
        let env_file_content = env_file_path.to_str().unwrap();

        // Call the function to parse the .env file
        parse_env_file(env_file_content);

        // Verify that environment variables have been set correctly
        let expected_url =
            "postgres://test:J4OqcB7jPTGYx@127.0.0.1/alpacode?serverVersion=14&charset=utf8";
        assert_eq!(env::var("DATABASE_URL").unwrap(), expected_url);

        let int_var: i32 = env::var("INT_VAR").unwrap().parse().unwrap();
        assert_eq!(int_var, 42);

        let bool_var: bool = env::var("BOOL_VAR").unwrap().parse().unwrap();
        assert_eq!(bool_var, true);

        // Clean up the temporary file after the test
        temp_dir.close().unwrap();
    }
}
