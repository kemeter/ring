use crate::config::config::{get_config_dir, load_auth_config, Config};
use clap::{Arg, ArgAction, ArgMatches, Command};
use log::{debug, info};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::Path;
use ureq::json;

#[derive(Debug)]
enum ApplyError {
    FileRead(std::io::Error),
    YamlParse(serde_yaml::Error),
    Validation(String),
    Http(Box<ureq::Error>),
    Auth(String),
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ApplyError::FileRead(e) => write!(f, "Failed to read file: {}", e),
            ApplyError::YamlParse(e) => write!(f, "Invalid YAML: {}", e),
            ApplyError::Validation(msg) => write!(f, "Validation error: {}", msg),
            ApplyError::Http(e) => write!(f, "HTTP error: {}", e),
            ApplyError::Auth(msg) => write!(f, "Auth error: {}", msg),
        }
    }
}

impl Error for ApplyError {}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Deployment {
    #[serde(default)]
    namespace: String,

    #[serde(default = "default_runtime")]
    runtime: String,

    #[serde(default = "default_kind")]
    kind: String,

    image: String,
    name: String,

    #[serde(default)]
    replicas: u32,

    #[serde(default, deserialize_with = "deserialize_labels")]
    labels: HashMap<String, String>,

    #[serde(default)]
    secrets: HashMap<String, String>,

    #[serde(default, deserialize_with = "deserialize_volumes")]
    volumes: Vec<Volume>,

    #[serde(default)]
    config: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Volume {
    source: String,
    destination: String,
    driver: String,
    permission: String,
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    deployments: HashMap<String, Deployment>,
}

fn default_runtime() -> String { "docker".to_string() }
fn default_kind() -> String { "worker".to_string() }

fn deserialize_labels<'de, D>(deserializer: D) -> Result<HashMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde_yaml::Value;
    use serde::de::Error;

    let value = Value::deserialize(deserializer)?;
    let mut labels = HashMap::new();

    match value {
        Value::Sequence(seq) if seq.is_empty() => {
            // Empty array returns empty HashMap
        }
        Value::Sequence(seq) => {
            for item in seq {
                if let Value::Mapping(map) = item {
                    for (k, v) in map {
                        if let (Some(key), Some(value)) = (k.as_str(), v.as_str()) {
                            labels.insert(key.to_string(), value.to_string());
                        }
                    }
                }
            }
        }
        Value::Mapping(map) => {
            for (k, v) in map {
                if let (Some(key), Some(value)) = (k.as_str(), v.as_str()) {
                    labels.insert(key.to_string(), value.to_string());
                }
            }
        }
        Value::Null => {
            // Null returns empty HashMap
        }
        _ => {
            return Err(D::Error::custom("labels must be an array or object"));
        }
    }

    Ok(labels)
}

fn deserialize_volumes<'de, D>(deserializer: D) -> Result<Vec<Volume>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let volumes_raw: Vec<String> = Vec::deserialize(deserializer).unwrap_or_default();
    let mut volumes = Vec::new();

    for volume_str in volumes_raw {
        let parts: Vec<&str> = volume_str.split(':').collect();
        if parts.len() >= 2 {
            volumes.push(Volume {
                source: parts[0].to_string(),
                destination: parts[1].to_string(),
                driver: "local".to_string(),
                permission: if parts.len() >= 3 { parts[2].to_string() } else { "rw".to_string() },
            });
        }
    }

    Ok(volumes)
}

impl Deployment {
    fn validate(&self) -> Result<(), ApplyError> {
        if self.name.trim().is_empty() {
            return Err(ApplyError::Validation("Deployment name cannot be empty".to_string()));
        }

        if self.image.trim().is_empty() {
            return Err(ApplyError::Validation("Deployment image cannot be empty".to_string()));
        }

        if self.runtime != "docker" {
            return Err(ApplyError::Validation(
                format!("Runtime '{}' not supported. Only 'docker' is supported.", self.runtime)
            ));
        }

        Ok(())
    }

    fn resolve_env_vars(&mut self) {
        self.namespace = env_resolver(&self.namespace);
        self.name = env_resolver(&self.name);
        self.image = env_resolver(&self.image);

        for (_, value) in self.secrets.iter_mut() {
            *value = env_resolver(value);
        }

        for (_, value) in self.config.iter_mut() {
            *value = env_resolver(value);
        }
    }
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
        )
        .arg(
            Arg::new("env-file")
                .required(false)
                .help("Use a .env file to set environment variables")
                .long("env-file")
                .short('e')
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
}

fn load_config_file(file_path: &str) -> Result<ConfigFile, ApplyError> {
    let contents = fs::read_to_string(file_path)
        .map_err(ApplyError::FileRead)?;

    let config: ConfigFile = serde_yaml::from_str(&contents)
        .map_err(ApplyError::YamlParse)?;

    Ok(config)
}

fn check_auth(config_dir: &str) -> Result<(), ApplyError> {
    let auth_config_file = format!("{}/auth.json", config_dir);

    if !Path::new(&auth_config_file).exists() {
        return Err(ApplyError::Auth("Account not found. Login first".to_string()));
    }

    Ok(())
}

fn preview_deployment(deployment: &Deployment, api_url: &str, force: bool, verbose: bool) {
    println!("DRY RUN - Deployment '{}'", deployment.name);
    println!("Would POST to: {}/deployments", api_url);

    if force {
        println!("Force mode enabled");
    }

    if verbose {
        let json = json!(deployment);
        println!("Configuration:");
        println!("{}", serde_json::to_string_pretty(&json).unwrap_or_else(|_| "Invalid JSON".to_string()));
    }

    println!("---");
}

fn deploy_to_server(
    deployment: &Deployment,
    api_url: &str,
    auth_token: &str,
    force: bool
) -> Result<(), ApplyError> {
    let mut url = format!("{}/deployments", api_url);

    if force {
        url.push_str("?force=true");
    }

    let json = json!(deployment);

    let response = ureq::post(&url)
        .set("Authorization", &format!("Bearer {}", auth_token))
        .send_json(json)
        .map_err(|e| ApplyError::Http(Box::new(e)))?;

    info!("Deployment '{}' created successfully (status: {})", deployment.name, response.status());
    println!("Deployment '{}' created", deployment.name);

    Ok(())
}

pub(crate) fn apply(args: &ArgMatches, configuration: Config) {
    if let Err(e) = apply_internal(args, configuration) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn apply_internal(args: &ArgMatches, mut configuration: Config) -> Result<(), ApplyError> {
    debug!("Apply configuration");

    let binding = "ring.yaml".to_string();
    let file = args.get_one::<String>("file").unwrap_or(&binding);
    let config_file = load_config_file(file)?;

    check_auth(&get_config_dir())?;

    let binding = String::new();
    let env_file = args.get_one::<String>("env-file").unwrap_or(&binding);
    parse_env_file(env_file);

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name);

    let is_dry_run = args.get_flag("dry-run");
    let is_verbose = args.get_flag("verbose");
    let is_force = args.get_flag("force");

    let mut success_count = 0;
    let mut error_count = 0;

    for (deployment_name, mut deployment) in config_file.deployments {
        println!("Processing deployment '{}'", deployment_name);

        if let Err(e) = deployment.validate() {
            eprintln!("Warning: Skipping '{}': {}", deployment_name, e);
            error_count += 1;
            continue;
        }

        deployment.resolve_env_vars();

        if is_verbose {
            let json = json!(deployment);
            println!("Configuration:");
            println!("{}", serde_json::to_string_pretty(&json).unwrap_or_else(|_| "Invalid JSON".to_string()));
        }

        if is_dry_run {
            preview_deployment(&deployment, &api_url, is_force, is_verbose);
            success_count += 1;
        } else {
            match deploy_to_server(&deployment, &api_url, &auth_config.token, is_force) {
                Ok(()) => success_count += 1,
                Err(e) => {
                    eprintln!("Failed to deploy '{}': {}", deployment_name, e);
                    error_count += 1;
                }
            }
        }
    }

    println!("\nSummary:");
    println!("  Successful: {}", success_count);
    if error_count > 0 {
        println!("  Failed: {}", error_count);
    }

    if is_dry_run {
        println!("\nDRY RUN COMPLETE - No actual changes were made");
        println!("To deploy for real, remove the --dry-run flag");
    }

    Ok(())
}

fn parse_env_file(env_file: &str) {
    if env_file.is_empty() {
        return;
    }

    let env_file_content = match fs::read_to_string(env_file) {
        Ok(content) => content,
        Err(e) => {
            eprintln!("Warning: Failed to read env file '{}': {}", env_file, e);
            return;
        }
    };

    for (line_num, line) in env_file_content.lines().enumerate() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.splitn(2, '=').collect();

        if parts.len() == 2 {
            let key = parts[0].trim();
            let value = parts[1].trim_matches('"').trim_matches('\'');

            if env::var(key).is_err() {
                env::set_var(key, value);
            }
        } else {
            eprintln!("Warning: Invalid env line {} in '{}': {}", line_num + 1, env_file, line);
        }
    }
}

fn env_resolver(text: &str) -> String {
    use once_cell::sync::Lazy;

    static ENV_REGEX: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"\$[a-zA-Z][0-9a-zA-Z_]*").unwrap()
    });

    let mut content = String::from(text);

    for variable in ENV_REGEX.find_iter(text) {
        let variable_str = variable.as_str();
        let key = &variable_str[1..];

        if let Ok(value) = env::var(key) {
            content = content.replace(variable_str, &value);
        }
    }

    content
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_resolver() {
        env::set_var("APP_VERSION", "v1.0.0");
        env::set_var("REGISTRY", "hub.docker.com");

        assert_eq!(
            env_resolver("registry.$REGISTRY/app:$APP_VERSION"),
            "registry.hub.docker.com/app:v1.0.0"
        );

        assert_eq!(
            env_resolver("test:$UNDEFINED_VAR"),
            "test:$UNDEFINED_VAR"
        );
    }

    #[test]
    fn test_deployment_validation() {
        let mut deployment = Deployment {
            namespace: "test".to_string(),
            runtime: "docker".to_string(),
            kind: "worker".to_string(),
            image: "nginx:latest".to_string(),
            name: "test-app".to_string(),
            replicas: 1,
            labels: HashMap::new(),
            secrets: HashMap::new(),
            volumes: Vec::new(),
            config: HashMap::new(),
        };

        assert!(deployment.validate().is_ok());

        deployment.runtime = "invalid".to_string();
        assert!(deployment.validate().is_err());

        deployment.runtime = "docker".to_string();
        deployment.name = "".to_string();
        assert!(deployment.validate().is_err());
    }

    #[test]
    fn test_config_file_parsing() {
        let yaml_content = r#"
deployments:
  php:
    name: test-php
    image: php:7.3-fpm
    runtime: docker
    replicas: 3
    namespace: ring
    labels: []
    config:
      image_pull_policy: "IfNotPresent"
    secrets:
      DATABASE_URL: postgres://test
  nginx:
    name: test-nginx
    image: nginx:1.19.5
    runtime: docker
    replicas: 1
    volumes:
      - "/tmp/ring:/project/ring:ro"
      - "/another/path:/another/container/path"
    labels:
      - sozune.host: "nginx.localhost"
"#;

        let config: ConfigFile = serde_yaml::from_str(yaml_content).unwrap();

        assert_eq!(config.deployments.len(), 2);

        let php = &config.deployments["php"];
        assert_eq!(php.name, "test-php");
        assert_eq!(php.replicas, 3);
        assert_eq!(php.labels.len(), 0);

        let nginx = &config.deployments["nginx"];
        assert_eq!(nginx.name, "test-nginx");
        assert_eq!(nginx.volumes.len(), 2);
        assert_eq!(nginx.labels.len(), 1);
        assert_eq!(nginx.labels.get("sozune.host"), Some(&"nginx.localhost".to_string()));
    }

    #[test]
    fn test_labels_deserializer() {
        let yaml1 = r#"
deployments:
  test1:
    name: test
    image: nginx
    labels: []
"#;
        let config1: ConfigFile = serde_yaml::from_str(yaml1).unwrap();
        assert_eq!(config1.deployments["test1"].labels.len(), 0);

        let yaml2 = r#"
deployments:
  test2:
    name: test
    image: nginx
    labels:
      - app: "my-app"
      - version: "1.0"
"#;
        let config2: ConfigFile = serde_yaml::from_str(yaml2).unwrap();
        let labels2 = &config2.deployments["test2"].labels;
        assert_eq!(labels2.len(), 2);
        assert_eq!(labels2.get("app"), Some(&"my-app".to_string()));
        assert_eq!(labels2.get("version"), Some(&"1.0".to_string()));

        let yaml3 = r#"
deployments:
  test3:
    name: test
    image: nginx
    labels:
      app: "my-app"
      version: "1.0"
"#;
        let config3: ConfigFile = serde_yaml::from_str(yaml3).unwrap();
        let labels3 = &config3.deployments["test3"].labels;
        assert_eq!(labels3.len(), 2);
        assert_eq!(labels3.get("app"), Some(&"my-app".to_string()));
    }
}