use crate::config::config::{Config, get_config_dir, load_auth_config};
use crate::exit_code;
use clap::{Arg, ArgAction, ArgMatches, Command};
use log::{debug, info};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::Path;

#[derive(Debug)]
enum ApplyError {
    FileRead(std::io::Error),
    YamlParse(serde_yaml::Error),
    Validation(String),
    Http(reqwest::Error),
    HttpStatus(u16, String),
    Auth(String),
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ApplyError::FileRead(e) => write!(f, "Failed to read file: {}", e),
            ApplyError::YamlParse(e) => write!(f, "Invalid YAML: {}", e),
            ApplyError::Validation(msg) => write!(f, "Validation error: {}", msg),
            ApplyError::Http(e) => write!(f, "HTTP error: {}", e),
            ApplyError::HttpStatus(status, msg) => {
                write!(f, "HTTP {} error: {}", status, msg)
            }
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

    #[serde(default, deserialize_with = "crate::utils::labels::deserialize_labels")]
    labels: HashMap<String, String>,

    #[serde(default)]
    environment: HashMap<String, String>,

    #[serde(default)]
    volumes: Vec<Volume>,

    #[serde(default)]
    config: HashMap<String, String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    command: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    resources: Option<Resources>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    health_checks: Vec<HealthCheck>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Resources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limits: Option<ResourceSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requests: Option<ResourceSpec>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct ResourceSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cpu: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    memory: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
enum HealthCheck {
    Tcp {
        port: u16,
        interval: String,
        timeout: String,
        #[serde(default = "default_hc_threshold")]
        threshold: u32,
        on_failure: FailureAction,
    },
    Http {
        url: String,
        interval: String,
        timeout: String,
        #[serde(default = "default_hc_threshold")]
        threshold: u32,
        on_failure: FailureAction,
    },
    Command {
        command: String,
        interval: String,
        timeout: String,
        #[serde(default = "default_hc_threshold")]
        threshold: u32,
        on_failure: FailureAction,
    },
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "lowercase")]
enum FailureAction {
    Restart,
    Stop,
    Alert,
}

fn default_hc_threshold() -> u32 {
    3
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Volume {
    #[serde(rename = "type")]
    volume_type: String,
    source: String,
    destination: String,
    #[serde(default = "default_driver")]
    driver: String,
    #[serde(default = "default_permission")]
    permission: String,
}

fn default_driver() -> String {
    "local".to_string()
}
fn default_permission() -> String {
    "rw".to_string()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct NamespaceDefinition {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    namespaces: HashMap<String, NamespaceDefinition>,
    deployments: HashMap<String, Deployment>,
}

fn default_runtime() -> String {
    "docker".to_string()
}
fn default_kind() -> String {
    "worker".to_string()
}

impl Deployment {
    fn validate(&self) -> Result<(), ApplyError> {
        if self.name.trim().is_empty() {
            return Err(ApplyError::Validation(
                "Deployment name cannot be empty".to_string(),
            ));
        }

        if self.image.trim().is_empty() {
            return Err(ApplyError::Validation(
                "Deployment image cannot be empty".to_string(),
            ));
        }

        if self.runtime != "docker" {
            return Err(ApplyError::Validation(format!(
                "Runtime '{}' not supported. Only 'docker' is supported.",
                self.runtime
            )));
        }

        Ok(())
    }

    fn resolve_env_vars(&mut self, env_vars: &HashMap<String, String>) {
        self.namespace = env_resolver(&self.namespace, env_vars);
        self.name = env_resolver(&self.name, env_vars);
        self.image = env_resolver(&self.image, env_vars);

        for (_, value) in self.environment.iter_mut() {
            *value = env_resolver(value, env_vars);
        }

        for (_, value) in self.config.iter_mut() {
            *value = env_resolver(value, env_vars);
        }

        for arg in self.command.iter_mut() {
            *arg = env_resolver(arg, env_vars);
        }
    }
}

pub(crate) fn command_config() -> Command {
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
    let contents = fs::read_to_string(file_path).map_err(ApplyError::FileRead)?;

    let config: ConfigFile = serde_yaml::from_str(&contents).map_err(ApplyError::YamlParse)?;

    Ok(config)
}

fn check_auth(config_dir: &str) -> Result<(), ApplyError> {
    if env::var("RING_TOKEN").ok().is_some_and(|t| !t.is_empty()) {
        return Ok(());
    }

    let auth_config_file = format!("{}/auth.json", config_dir);

    if !Path::new(&auth_config_file).exists() {
        return Err(ApplyError::Auth(
            "Account not found. Login first or set RING_TOKEN".to_string(),
        ));
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
        println!(
            "{}",
            serde_json::to_string_pretty(&json).unwrap_or_else(|_| "Invalid JSON".to_string())
        );
    }

    println!("---");
}

async fn create_namespace_on_server(
    namespace: &NamespaceDefinition,
    api_url: &str,
    auth_token: &str,
    client: &reqwest::Client,
) -> Result<(), ApplyError> {
    let url = format!("{}/namespaces", api_url);

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", auth_token))
        .json(&json!({ "name": namespace.name }))
        .send()
        .await
        .map_err(ApplyError::Http)?;

    let status = response.status();

    if status.is_success() {
        info!("Namespace '{}' created successfully", namespace.name);
        println!("Namespace '{}' created", namespace.name);
        Ok(())
    } else if status == reqwest::StatusCode::CONFLICT {
        info!("Namespace '{}' already exists, skipping", namespace.name);
        println!("Namespace '{}' already exists, skipping", namespace.name);
        Ok(())
    } else {
        let error_body = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        Err(ApplyError::HttpStatus(
            status.as_u16(),
            format!(
                "Failed to create namespace '{}': {} {}",
                namespace.name, status, error_body
            ),
        ))
    }
}

async fn deploy_to_server(
    deployment: &Deployment,
    api_url: &str,
    auth_token: &str,
    force: bool,
    client: &reqwest::Client,
) -> Result<(), ApplyError> {
    let mut url = format!("{}/deployments", api_url);

    if force {
        url.push_str("?force=true");
    }

    let json = json!(deployment);

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", auth_token))
        .json(&json)
        .send()
        .await
        .map_err(ApplyError::Http)?;

    let status = response.status();

    if status.is_success() {
        info!(
            "Deployment '{}' created successfully (status: {})",
            deployment.name, status
        );
        println!("Deployment '{}' created", deployment.name);
        Ok(())
    } else {
        let error_body = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        Err(ApplyError::HttpStatus(
            status.as_u16(),
            format!("API returned status {}: {}", status, error_body),
        ))
    }
}

pub(crate) async fn apply(args: &ArgMatches, configuration: Config, client: &reqwest::Client) {
    if let Err(e) = apply_internal(args, configuration, client).await {
        eprintln!("Error: {}", e);
        match e {
            ApplyError::Http(err) => exit_code::from_reqwest_error(&err).exit(),
            ApplyError::HttpStatus(status, _) => exit_code::from_http_status(status).exit(),
            ApplyError::Auth(_) => exit_code::ExitCode::Auth.exit(),
            ApplyError::Validation(_) => exit_code::ExitCode::General.exit(),
            _ => exit_code::ExitCode::General.exit(),
        }
    }
}

async fn apply_internal(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) -> Result<(), ApplyError> {
    debug!("Apply configuration");

    let binding = "ring.yaml".to_string();
    let file = args.get_one::<String>("file").unwrap_or(&binding);
    let config_file = load_config_file(file)?;

    check_auth(&get_config_dir())?;

    let binding = String::new();
    let env_file = args.get_one::<String>("env-file").unwrap_or(&binding);
    let env_vars = parse_env_file(env_file);

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name);

    let is_dry_run = args.get_flag("dry-run");
    let is_verbose = args.get_flag("verbose");
    let is_force = args.get_flag("force");

    let mut first_error: Option<ApplyError> = None;

    // Create namespaces first
    for (key, namespace) in &config_file.namespaces {
        if is_dry_run {
            println!("DRY RUN - Would create namespace '{}'", namespace.name);
        } else if let Err(e) =
            create_namespace_on_server(namespace, &api_url, &auth_config.token, client).await
        {
            eprintln!("Failed to create namespace '{}': {}", key, e);
            if first_error.is_none() {
                first_error = Some(e);
            }
        }
    }

    let mut success_count = 0;
    let mut error_count = 0;

    for (deployment_name, mut deployment) in config_file.deployments {
        println!("Processing deployment '{}'", deployment_name);

        if let Err(e) = deployment.validate() {
            eprintln!("Warning: Skipping '{}': {}", deployment_name, e);
            error_count += 1;
            if first_error.is_none() {
                first_error = Some(e);
            }
            continue;
        }

        deployment.resolve_env_vars(&env_vars);

        if is_verbose {
            let json = json!(deployment);
            println!("Configuration:");
            println!(
                "{}",
                serde_json::to_string_pretty(&json).unwrap_or_else(|_| "Invalid JSON".to_string())
            );
        }

        if is_dry_run {
            preview_deployment(&deployment, &api_url, is_force, is_verbose);
            success_count += 1;
        } else {
            match deploy_to_server(&deployment, &api_url, &auth_config.token, is_force, client)
                .await
            {
                Ok(()) => success_count += 1,
                Err(e) => {
                    eprintln!("Failed to deploy '{}': {}", deployment_name, e);
                    error_count += 1;
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
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

    if let Some(e) = first_error {
        return Err(e);
    }

    Ok(())
}

fn parse_env_file(env_file: &str) -> HashMap<String, String> {
    let mut env_vars = HashMap::new();

    if env_file.is_empty() {
        return env_vars;
    }

    let env_file_content = match fs::read_to_string(env_file) {
        Ok(content) => content,
        Err(e) => {
            eprintln!("Warning: Failed to read env file '{}': {}", env_file, e);
            return env_vars;
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
            env_vars.insert(key.to_string(), value.to_string());
        } else {
            eprintln!(
                "Warning: Invalid env line {} in '{}': {}",
                line_num + 1,
                env_file,
                line
            );
        }
    }

    env_vars
}

fn env_resolver(text: &str, env_vars: &HashMap<String, String>) -> String {
    use once_cell::sync::Lazy;

    static ENV_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"\$[a-zA-Z][0-9a-zA-Z_]*").unwrap());

    let mut content = String::from(text);

    for variable in ENV_REGEX.find_iter(text) {
        let variable_str = variable.as_str();
        let key = &variable_str[1..];

        // Priority: system env vars first, then file env vars
        let value = env::var(key).ok().or_else(|| env_vars.get(key).cloned());
        if let Some(value) = value {
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
        let mut env_vars = HashMap::new();
        env_vars.insert("APP_VERSION".to_string(), "v1.0.0".to_string());
        env_vars.insert("REGISTRY".to_string(), "hub.docker.com".to_string());

        assert_eq!(
            env_resolver("registry.$REGISTRY/app:$APP_VERSION", &env_vars),
            "registry.hub.docker.com/app:v1.0.0"
        );

        assert_eq!(
            env_resolver("test:$UNDEFINED_VAR", &env_vars),
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
            environment: HashMap::new(),
            volumes: Vec::new(),
            config: HashMap::new(),
            command: Vec::new(),
            resources: None,
            health_checks: Vec::new(),
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
    environment:
      DATABASE_URL: postgres://test
  nginx:
    name: test-nginx
    image: nginx:1.19.5
    runtime: docker
    replicas: 1
    volumes:
      - type: bind
        source: /tmp/ring
        destination: /project/ring
        driver: local
        permission: ro
      - type: bind
        source: /another/path
        destination: /another/container/path
        driver: local
        permission: rw
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
        assert_eq!(
            nginx.labels.get("sozune.host"),
            Some(&"nginx.localhost".to_string())
        );
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
        assert_eq!(labels3.get("version"), Some(&"1.0".to_string()));
    }

    #[test]
    fn test_config_file_with_namespaces() {
        let yaml_content = r#"
namespaces:
  production:
    name: production
  staging:
    name: staging

deployments:
  api:
    name: api
    namespace: production
    image: myapp:latest
    runtime: docker
    replicas: 3
"#;

        let config: ConfigFile = serde_yaml::from_str(yaml_content).unwrap();

        assert_eq!(config.namespaces.len(), 2);
        assert_eq!(config.namespaces["production"].name, "production");
        assert_eq!(config.namespaces["staging"].name, "staging");
        assert_eq!(config.deployments.len(), 1);
    }

    #[test]
    fn test_config_file_with_command_resources_health_checks() {
        let yaml_content = r#"
deployments:
  api:
    name: api
    image: myapp:latest
    runtime: docker
    command:
      - "/bin/sh"
      - "-c"
      - "exec myapp --port $PORT"
    resources:
      limits:
        cpu: "500m"
        memory: "512Mi"
      requests:
        cpu: "100m"
        memory: "128Mi"
    health_checks:
      - type: http
        url: http://localhost:8080/health
        interval: 30s
        timeout: 5s
        on_failure: restart
      - type: tcp
        port: 5432
        interval: 10s
        timeout: 2s
        threshold: 5
        on_failure: alert
"#;

        let config: ConfigFile = serde_yaml::from_str(yaml_content).unwrap();
        let api = &config.deployments["api"];

        assert_eq!(api.command.len(), 3);
        assert_eq!(api.command[2], "exec myapp --port $PORT");

        let resources = api.resources.as_ref().expect("resources should parse");
        let limits = resources.limits.as_ref().expect("limits should parse");
        assert_eq!(limits.cpu.as_deref(), Some("500m"));
        assert_eq!(limits.memory.as_deref(), Some("512Mi"));
        let requests = resources.requests.as_ref().expect("requests should parse");
        assert_eq!(requests.cpu.as_deref(), Some("100m"));

        assert_eq!(api.health_checks.len(), 2);
        match &api.health_checks[0] {
            HealthCheck::Http { url, .. } => {
                assert_eq!(url, "http://localhost:8080/health");
            }
            _ => panic!("expected http health check"),
        }
        match &api.health_checks[1] {
            HealthCheck::Tcp {
                port, threshold, ..
            } => {
                assert_eq!(*port, 5432);
                assert_eq!(*threshold, 5);
            }
            _ => panic!("expected tcp health check"),
        }
    }

    #[test]
    fn test_command_env_resolution() {
        let mut deployment = Deployment {
            namespace: "test".to_string(),
            runtime: "docker".to_string(),
            kind: "worker".to_string(),
            image: "nginx:latest".to_string(),
            name: "test".to_string(),
            replicas: 1,
            labels: HashMap::new(),
            environment: HashMap::new(),
            volumes: Vec::new(),
            config: HashMap::new(),
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "serve --port $PORT".to_string(),
            ],
            resources: None,
            health_checks: Vec::new(),
        };

        let mut env_vars = HashMap::new();
        env_vars.insert("PORT".to_string(), "8080".to_string());
        deployment.resolve_env_vars(&env_vars);

        assert_eq!(deployment.command[2], "serve --port 8080");
    }

    #[test]
    fn test_config_file_without_namespaces() {
        let yaml_content = r#"
deployments:
  api:
    name: api
    image: myapp:latest
    runtime: docker
"#;

        let config: ConfigFile = serde_yaml::from_str(yaml_content).unwrap();
        assert_eq!(config.namespaces.len(), 0);
        assert_eq!(config.deployments.len(), 1);
    }
}
