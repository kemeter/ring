use crate::cli::problem_json::render_response_error;
use crate::config::auth::load_auth_config;
use crate::config::config::{Config, get_config_dir};
use crate::exit_code;
use crate::models::deployments::EnvValue;
use clap::{Arg, ArgAction, ArgMatches, Command};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, to_string_pretty};
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
    // The failure has already been printed to stderr (typically by
    // `render_response_error`). Carry only the status so `apply()` can map
    // it to the right exit code without re-printing.
    Reported(u16),
    Auth(String),
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ApplyError::FileRead(e) => write!(f, "Failed to read file: {}", e),
            ApplyError::YamlParse(e) => write!(f, "Invalid YAML: {}", e),
            ApplyError::Validation(msg) => write!(f, "Validation error: {}", msg),
            ApplyError::Http(e) => write!(f, "HTTP error: {}", e),
            ApplyError::Reported(status) => write!(f, "request failed with status {}", status),
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
    environment: HashMap<String, EnvValue>,

    #[serde(default)]
    volumes: Vec<Volume>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    config: Option<DeploymentConfig>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    command: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    resources: Option<Resources>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    health_checks: Vec<HealthCheck>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ports: Vec<Port>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    network: Option<NetworkConfig>,
}

/// Runtime config block of a deployment. Mirrors the API's `DeploymentConfig`
/// so a manifest can carry structured fields (notably `user`) instead of a flat
/// string map — the previous `HashMap<String, String>` could not express the
/// `user` struct the API expects.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
struct DeploymentConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_pull_policy: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    server: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    username: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<UserConfig>,

    /// Resolve registry credentials from the host's Docker config instead of
    /// inlining `server`/`username`/`password`. Mutually exclusive with them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    use_host_auth: bool,

    /// Name of a Secret holding registry credentials (`dockerconfigjson`).
    /// Mutually exclusive with inline credentials and `use_host_auth`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_pull_secret: Option<String>,
}

/// Numeric uid/gid the container runs as (forwarded to Docker's `User`).
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
struct UserConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    privileged: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct NetworkConfig {
    #[serde(default = "default_network_mode")]
    mode: String,
}

fn default_network_mode() -> String {
    "bridge".to_string()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Port {
    published: u16,
    target: u16,
    /// Host interface to bind the published port on. Defaults to `0.0.0.0`
    /// (all interfaces) when omitted. `skip_serializing_if` keeps the payload
    /// to the API identical to the pre-`host_ip` shape when unset, so the
    /// server's `#[serde(default)]` fills in the default unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host_ip: Option<String>,
    /// Transport protocol, `tcp` (default) or `udp`. Omitted from the payload
    /// when unset so the wire shape is unchanged for TCP-only manifests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    protocol: Option<String>,
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
        #[serde(default)]
        readiness: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_healthy_time: Option<String>,
    },
    Http {
        url: String,
        interval: String,
        timeout: String,
        #[serde(default = "default_hc_threshold")]
        threshold: u32,
        on_failure: FailureAction,
        #[serde(default)]
        readiness: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_healthy_time: Option<String>,
    },
    Command {
        command: String,
        interval: String,
        timeout: String,
        #[serde(default = "default_hc_threshold")]
        threshold: u32,
        on_failure: FailureAction,
        #[serde(default)]
        readiness: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_healthy_time: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String>,
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

#[derive(Debug, Deserialize, Serialize, Clone)]
struct ConfigDefinition {
    namespace: String,
    name: String,
    // The JSON payload sent to the server: a `{"<filename>": "<contents>"}`
    // object. May be written inline, built from `files`, or both — see
    // `resolve_config_data`. Always `Some` once resolution has run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<String>,
    // Map of `filename -> path` (relative to the manifest) whose contents are
    // read at load time and merged into `data`. Never sent to the server.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    files: HashMap<String, String>,
    // Whether `$VAR` interpolation runs on this config's payload. When unset,
    // inline `data` is interpolated (backwards compatible) but `files` contents
    // stay verbatim — so an nginx/Prometheus file full of `$host`/`$labels`
    // isn't mangled. `true` forces interpolation on files too; `false` keeps the
    // whole payload (inline + files) verbatim. Never sent to the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    interpolate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    labels: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    namespaces: HashMap<String, NamespaceDefinition>,
    #[serde(default)]
    configs: HashMap<String, ConfigDefinition>,
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

        if self.runtime != "docker"
            && self.runtime != "podman"
            && self.runtime != "containerd"
            && self.runtime != "cloud-hypervisor"
            && self.runtime != "firecracker"
        {
            return Err(ApplyError::Validation(format!(
                "Runtime '{}' not supported. Supported runtimes: docker, podman, containerd, cloud-hypervisor, firecracker.",
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
            if let EnvValue::Plain(s) = value {
                *s = env_resolver(s, env_vars);
            }
        }

        if let Some(config) = self.config.as_mut() {
            for s in [
                &mut config.image_pull_policy,
                &mut config.server,
                &mut config.username,
                &mut config.password,
            ]
            .into_iter()
            .flatten()
            {
                *s = env_resolver(s, env_vars);
            }
        }

        for arg in self.command.iter_mut() {
            *arg = env_resolver(arg, env_vars);
        }

        // The API rejects a `config` or `secret` volume whose permission is
        // not `ro`. The CLI default is `rw`, so force `ro` here — a manifest
        // carrying one of these volume types must apply without the user
        // spelling out the permission the server is going to require anyway.
        for volume in self.volumes.iter_mut() {
            if volume.volume_type == "config" || volume.volume_type == "secret" {
                volume.permission = "ro".to_string();
            }
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

fn load_config_file(
    file_path: &str,
    env_vars: &HashMap<String, String>,
) -> Result<ConfigFile, ApplyError> {
    let contents = fs::read_to_string(file_path).map_err(ApplyError::FileRead)?;

    let mut config: ConfigFile = serde_yaml::from_str(&contents).map_err(ApplyError::YamlParse)?;

    // `files` references are relative to the manifest's directory, so resolve
    // them while we still know where the manifest lives.
    let manifest_dir = Path::new(file_path).parent().unwrap_or(Path::new("."));
    for (key, def) in config.configs.iter_mut() {
        resolve_config_data(key, def, manifest_dir, env_vars)?;
    }

    Ok(config)
}

/// Turn a config's `data` (inline JSON) and `files` (filename -> path) into a
/// single JSON object string stored back into `data`. Rules:
///   - `data` alone: kept as-is (backwards compatible).
///   - `files` alone: builds `{"<name>": "<contents>", ...}`.
///   - both: merged, but a filename present in *both* is a hard error, and a
///     non-object `data` cannot be merged into.
///   - neither: hard error — a config must carry some payload.
///
/// `$VAR` interpolation is applied here, per-value, according to `interpolate`:
///   - unset: inline `data` values are interpolated, `files` contents stay
///     verbatim (so an nginx/Prometheus file full of `$host`/`$labels` survives).
///   - `true`: everything is interpolated, including `files` contents.
///   - `false`: nothing is interpolated — the whole payload is verbatim.
fn resolve_config_data(
    key: &str,
    def: &mut ConfigDefinition,
    manifest_dir: &Path,
    env_vars: &HashMap<String, String>,
) -> Result<(), ApplyError> {
    if def.data.is_none() && def.files.is_empty() {
        return Err(ApplyError::Validation(format!(
            "config '{}': either 'data' or 'files' must be set",
            key
        )));
    }

    let interpolate = def.interpolate.unwrap_or(false);
    let interpolate_inline = def.interpolate.unwrap_or(true);

    // Fast path: inline data only, no files to merge — keep it untouched so
    // arbitrary (even non-JSON-object) inline payloads keep working. Inline
    // data is interpolated unless `interpolate: false` opts out.
    if def.files.is_empty() {
        if let Some(raw) = &def.data
            && interpolate_inline
        {
            def.data = Some(env_resolver(raw, env_vars));
        }
        return Ok(());
    }

    // Start from the inline `data` if present, else an empty object. Inline
    // values follow `interpolate_inline`; file-backed values follow
    // `interpolate` (verbatim by default).
    let mut merged: serde_json::Map<String, serde_json::Value> = match &def.data {
        Some(raw) => {
            let mut obj: serde_json::Map<String, serde_json::Value> = serde_json::from_str(raw)
                .map_err(|_| {
                    ApplyError::Validation(format!(
                        "config '{}': 'data' must be a JSON object to merge with 'files'",
                        key
                    ))
                })?;
            if interpolate_inline {
                for value in obj.values_mut() {
                    if let serde_json::Value::String(s) = value {
                        *value = serde_json::Value::String(env_resolver(s, env_vars));
                    }
                }
            }
            obj
        }
        None => serde_json::Map::new(),
    };

    for (name, rel_path) in &def.files {
        if merged.contains_key(name) {
            return Err(ApplyError::Validation(format!(
                "config '{}': key '{}' is defined in both 'data' and 'files'",
                key, name
            )));
        }
        let full_path = manifest_dir.join(rel_path);
        let raw = fs::read_to_string(&full_path).map_err(|e| {
            ApplyError::Validation(format!(
                "config '{}': failed to read file '{}' for key '{}': {}",
                key,
                full_path.display(),
                name,
                e
            ))
        })?;
        // File contents are verbatim by default; `interpolate: true` opts in.
        let contents = if interpolate {
            env_resolver(&raw, env_vars)
        } else {
            raw
        };
        merged.insert(name.clone(), serde_json::Value::String(contents));
    }

    def.data = Some(serde_json::Value::Object(merged).to_string());
    def.files.clear();
    Ok(())
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
        let context = format!("Failed to create namespace '{}'", namespace.name);
        let code = render_response_error(&context, response).await;
        Err(ApplyError::Reported(code))
    }
}

async fn create_config_on_server(
    config: &ConfigDefinition,
    api_url: &str,
    auth_token: &str,
    client: &reqwest::Client,
) -> Result<(), ApplyError> {
    let url = format!("{}/configs", api_url);

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", auth_token))
        .json(&json!(config))
        .send()
        .await
        .map_err(ApplyError::Http)?;

    let status = response.status();

    if status.is_success() {
        info!(
            "Config '{}' created successfully in namespace '{}'",
            config.name, config.namespace
        );
        println!(
            "Config '{}' created in namespace '{}'",
            config.name, config.namespace
        );
        Ok(())
    } else if status == reqwest::StatusCode::CONFLICT {
        // The config already exists. `apply` is declarative, so update it in
        // place instead of skipping — otherwise edits to a config (Grafana
        // dashboards, datasources, prometheus.yml, ...) never reach the server.
        update_config_on_server(config, api_url, auth_token, client).await
    } else {
        let context = format!("Failed to create config '{}'", config.name);
        let code = render_response_error(&context, response).await;
        Err(ApplyError::Reported(code))
    }
}

/// Look up an existing config by namespace + name and PUT the new content onto
/// it, so a re-applied config map is updated rather than skipped.
async fn update_config_on_server(
    config: &ConfigDefinition,
    api_url: &str,
    auth_token: &str,
    client: &reqwest::Client,
) -> Result<(), ApplyError> {
    let list_url = format!(
        "{}/configs?namespace={}&name={}",
        api_url, config.namespace, config.name
    );

    let list_response = client
        .get(&list_url)
        .header("Authorization", format!("Bearer {}", auth_token))
        .send()
        .await
        .map_err(ApplyError::Http)?;

    if !list_response.status().is_success() {
        let context = format!("Failed to look up existing config '{}'", config.name);
        let code = render_response_error(&context, list_response).await;
        return Err(ApplyError::Reported(code));
    }

    let configs: Vec<serde_json::Value> = list_response.json().await.map_err(ApplyError::Http)?;

    let Some(id) = first_config_id(&configs) else {
        return Err(ApplyError::Validation(format!(
            "Config '{}' reported as existing but could not be found for update",
            config.name
        )));
    };

    let update_url = format!("{}/configs/{}", api_url, id);
    let response = client
        .put(&update_url)
        .header("Authorization", format!("Bearer {}", auth_token))
        .json(&json!(config))
        .send()
        .await
        .map_err(ApplyError::Http)?;

    if response.status().is_success() {
        info!(
            "Config '{}' updated in namespace '{}'",
            config.name, config.namespace
        );
        println!(
            "Config '{}' updated in namespace '{}'",
            config.name, config.namespace
        );
        Ok(())
    } else {
        let context = format!("Failed to update config '{}'", config.name);
        let code = render_response_error(&context, response).await;
        Err(ApplyError::Reported(code))
    }
}

/// Extract the `id` of the first config from a `GET /configs` list response.
/// Returns `None` when the list is empty or the first entry has no string id.
fn first_config_id(configs: &[serde_json::Value]) -> Option<&str> {
    configs
        .first()
        .and_then(|config| config.get("id"))
        .and_then(serde_json::Value::as_str)
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
        let context = format!("Unable to apply deployment '{}'", deployment.name);
        let code = render_response_error(&context, response).await;
        Err(ApplyError::Reported(code))
    }
}

pub(crate) async fn apply(args: &ArgMatches, configuration: Config, client: &reqwest::Client) {
    if let Err(e) = apply_internal(args, configuration, client).await {
        // `Reported` means render_response_error has already written a
        // structured error to stderr — re-printing would just duplicate the
        // problem+json output.
        if !matches!(e, ApplyError::Reported(_)) {
            eprintln!("Error: {}", e);
        }
        match e {
            ApplyError::Http(err) => exit_code::from_reqwest_error(&err).exit(),
            ApplyError::Reported(status) => exit_code::from_http_status(status).exit(),
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

    // Env vars are resolved before loading the manifest: `load_config_file`
    // needs them to interpolate `$VAR` per-value while it knows which values
    // come from inline `data` versus file-backed `files`.
    let env_binding = String::new();
    let env_file = args.get_one::<String>("env-file").unwrap_or(&env_binding);
    let env_vars = parse_env_file(env_file);

    let binding = "ring.yaml".to_string();
    let file = args.get_one::<String>("file").unwrap_or(&binding);
    let config_file = load_config_file(file, &env_vars)?;

    check_auth(&get_config_dir())?;

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
            if !matches!(e, ApplyError::Reported(_)) {
                eprintln!("Failed to create namespace '{}': {}", key, e);
            }
            if first_error.is_none() {
                first_error = Some(e);
            }
        }
    }

    // Create configs after namespaces, before deployments: a deployment with a
    // `type: config` volume can only resolve once the config exists.
    for (key, mut config) in config_file.configs {
        config.namespace = env_resolver(&config.namespace, &env_vars);
        config.name = env_resolver(&config.name, &env_vars);
        // `data` is already resolved and interpolated by `resolve_config_data`
        // at load time (which is where the inline-vs-files interpolation policy
        // lives), so the payload is sent as-is here.

        if is_dry_run {
            println!(
                "DRY RUN - Would create config '{}' in namespace '{}'",
                config.name, config.namespace
            );
        } else if let Err(e) =
            create_config_on_server(&config, &api_url, &auth_config.token, client).await
        {
            if !matches!(e, ApplyError::Reported(_)) {
                eprintln!("Failed to create config '{}': {}", key, e);
            }
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
                to_string_pretty(&json).unwrap_or_else(|_| "Invalid JSON".to_string())
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
                    // `Reported` means render_response_error already wrote a
                    // structured RFC 7807 dump to stderr — duplicating the
                    // legacy one-liner here just adds noise.
                    if !matches!(e, ApplyError::Reported(_)) {
                        eprintln!("Failed to deploy '{}': {}", deployment_name, e);
                    }
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
            config: None,
            command: Vec::new(),
            resources: None,
            health_checks: Vec::new(),
            ports: Vec::new(),
            network: None,
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
            config: None,
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "serve --port $PORT".to_string(),
            ],
            resources: None,
            health_checks: Vec::new(),
            ports: Vec::new(),
            network: None,
        };

        let mut env_vars = HashMap::new();
        env_vars.insert("PORT".to_string(), "8080".to_string());
        deployment.resolve_env_vars(&env_vars);

        assert_eq!(deployment.command[2], "serve --port 8080");
    }

    #[test]
    fn test_config_volume_permission_forced_to_ro() {
        let yaml_content = r#"
deployments:
  nginx:
    name: nginx
    image: nginx:1.19.5
    volumes:
      - type: config
        source: test-config2
        key: "test.conf"
        destination: /var/config/test.conf
        permission: rw
      - type: bind
        source: /tmp/data
        destination: /data
        permission: rw
"#;
        let config: ConfigFile = serde_yaml::from_str(yaml_content).unwrap();
        let mut nginx = config.deployments["nginx"].clone();
        nginx.resolve_env_vars(&HashMap::new());

        // The config volume is forced to `ro` (the API rejects anything else);
        // the bind volume keeps its declared `rw`.
        assert_eq!(nginx.volumes[0].volume_type, "config");
        assert_eq!(nginx.volumes[0].permission, "ro");
        assert_eq!(nginx.volumes[1].volume_type, "bind");
        assert_eq!(nginx.volumes[1].permission, "rw");
    }

    #[test]
    fn test_config_file_with_configs() {
        let yaml_content = r#"
configs:
  entrypoints:
    namespace: ring
    name: "test-config2"
    data: '{"test.conf":"server {}"}'

deployments:
  nginx:
    name: nginx
    namespace: ring
    image: nginx:1.19.5
    volumes:
      - type: config
        source: test-config2
        key: "test.conf"
        destination: /var/config/test.conf
"#;

        let config: ConfigFile = serde_yaml::from_str(yaml_content).unwrap();

        assert_eq!(config.configs.len(), 1);
        let entry = &config.configs["entrypoints"];
        assert_eq!(entry.namespace, "ring");
        assert_eq!(entry.name, "test-config2");
        assert_eq!(entry.data.as_deref(), Some(r#"{"test.conf":"server {}"}"#));
        assert_eq!(entry.labels, None);
        assert_eq!(config.deployments.len(), 1);
    }

    fn config_def(data: Option<&str>, files: &[(&str, &str)]) -> ConfigDefinition {
        config_def_interp(data, files, None)
    }

    fn config_def_interp(
        data: Option<&str>,
        files: &[(&str, &str)],
        interpolate: Option<bool>,
    ) -> ConfigDefinition {
        ConfigDefinition {
            namespace: "ns".to_string(),
            name: "n".to_string(),
            data: data.map(|s| s.to_string()),
            files: files
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            interpolate,
            labels: None,
        }
    }

    fn no_env() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn test_resolve_config_data_files_only() {
        let dir = std::env::temp_dir().join("ring_cfg_files_only");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("config.yaml"), "providers:\n  docker: true\n").unwrap();

        let mut def = config_def(None, &[("config.yaml", "config.yaml")]);
        resolve_config_data("c", &mut def, &dir, &no_env()).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(def.data.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["config.yaml"], "providers:\n  docker: true\n");
        assert!(def.files.is_empty());
    }

    #[test]
    fn test_resolve_config_data_merge() {
        let dir = std::env::temp_dir().join("ring_cfg_merge");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("big.yaml"), "k: v\n").unwrap();

        let mut def = config_def(Some(r#"{"inline.txt":"hi"}"#), &[("big.yaml", "big.yaml")]);
        resolve_config_data("c", &mut def, &dir, &no_env()).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(def.data.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["inline.txt"], "hi");
        assert_eq!(parsed["big.yaml"], "k: v\n");
    }

    #[test]
    fn test_resolve_config_data_key_collision() {
        let dir = std::env::temp_dir().join("ring_cfg_collision");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("dup.yaml"), "x: 1\n").unwrap();

        let mut def = config_def(
            Some(r#"{"dup.yaml":"already here"}"#),
            &[("dup.yaml", "dup.yaml")],
        );
        let err = resolve_config_data("c", &mut def, &dir, &no_env()).unwrap_err();
        assert!(matches!(err, ApplyError::Validation(m) if m.contains("both 'data' and 'files'")));
    }

    #[test]
    fn test_resolve_config_data_non_object_data_with_files() {
        let dir = std::env::temp_dir().join("ring_cfg_nonobj");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("f.yaml"), "x: 1\n").unwrap();

        let mut def = config_def(Some("just plain text"), &[("f.yaml", "f.yaml")]);
        let err = resolve_config_data("c", &mut def, &dir, &no_env()).unwrap_err();
        assert!(matches!(err, ApplyError::Validation(m) if m.contains("must be a JSON object")));
    }

    #[test]
    fn test_resolve_config_data_missing_file() {
        let dir = std::env::temp_dir().join("ring_cfg_missing");
        let _ = fs::create_dir_all(&dir);

        let mut def = config_def(None, &[("config.yaml", "./does-not-exist.yaml")]);
        let err = resolve_config_data("c", &mut def, &dir, &no_env()).unwrap_err();
        assert!(matches!(
            err,
            ApplyError::Validation(m)
                if m.contains("failed to read file") && m.contains("does-not-exist.yaml")
        ));
    }

    #[test]
    fn test_resolve_config_data_neither_set() {
        let mut def = config_def(None, &[]);
        let err = resolve_config_data("c", &mut def, Path::new("."), &no_env()).unwrap_err();
        assert!(matches!(err, ApplyError::Validation(m) if m.contains("either 'data' or 'files'")));
    }

    #[test]
    fn test_resolve_config_data_inline_only_untouched() {
        let mut def = config_def(Some("arbitrary non-json payload"), &[]);
        resolve_config_data("c", &mut def, Path::new("."), &no_env()).unwrap();
        assert_eq!(def.data.as_deref(), Some("arbitrary non-json payload"));
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn test_resolve_config_data_files_verbatim_by_default() {
        // A file full of `$VAR` (nginx/Prometheus style) stays verbatim: file
        // contents are not interpolated unless the config opts in.
        let dir = std::env::temp_dir().join("ring_cfg_files_verbatim");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("site.conf"), "server_name $RING_TEST_HOST;\n").unwrap();

        let mut def = config_def(None, &[("site.conf", "site.conf")]);
        resolve_config_data(
            "c",
            &mut def,
            &dir,
            &env(&[("RING_TEST_HOST", "example.com")]),
        )
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(def.data.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["site.conf"], "server_name $RING_TEST_HOST;\n");
    }

    #[test]
    fn test_resolve_config_data_files_interpolated_when_opted_in() {
        // `interpolate: true` runs `$VAR` substitution on file contents too.
        let dir = std::env::temp_dir().join("ring_cfg_files_interp");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("site.conf"), "server_name $RING_TEST_HOST2;\n").unwrap();

        let mut def = config_def_interp(None, &[("site.conf", "site.conf")], Some(true));
        resolve_config_data(
            "c",
            &mut def,
            &dir,
            &env(&[("RING_TEST_HOST2", "example.com")]),
        )
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(def.data.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["site.conf"], "server_name example.com;\n");
    }

    #[test]
    fn test_resolve_config_data_inline_interpolated_by_default() {
        // Inline `data` is interpolated even when files keep their contents
        // verbatim — the frontier between manifest template and file payload.
        let dir = std::env::temp_dir().join("ring_cfg_frontier");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("f.conf"), "raw $RING_TEST_KEEP\n").unwrap();

        let mut def = config_def(
            Some(r#"{"inline":"hi $RING_TEST_SUB"}"#),
            &[("f.conf", "f.conf")],
        );
        resolve_config_data(
            "c",
            &mut def,
            &dir,
            &env(&[("RING_TEST_SUB", "there"), ("RING_TEST_KEEP", "NOPE")]),
        )
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(def.data.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["inline"], "hi there"); // inline interpolated
        assert_eq!(parsed["f.conf"], "raw $RING_TEST_KEEP\n"); // file verbatim
    }

    #[test]
    fn test_resolve_config_data_interpolate_false_keeps_inline_verbatim() {
        // `interpolate: false` opts the whole payload out, inline included.
        let mut def =
            config_def_interp(Some(r#"{"inline":"hi $RING_TEST_OFF"}"#), &[], Some(false));
        resolve_config_data(
            "c",
            &mut def,
            Path::new("."),
            &env(&[("RING_TEST_OFF", "x")]),
        )
        .unwrap();
        assert_eq!(
            def.data.as_deref(),
            Some(r#"{"inline":"hi $RING_TEST_OFF"}"#)
        );
    }

    // End-to-end through `load_config_file`: a real manifest on disk with a
    // `files:` reference resolved relative to the manifest's own directory.
    #[test]
    fn test_load_config_file_resolves_files_relative_to_manifest() {
        let dir = std::env::temp_dir().join("ring_cfg_loadfile");
        let _ = fs::create_dir_all(dir.join("sozune"));
        fs::write(
            dir.join("sozune/config.yaml"),
            "providers:\n  docker:\n    enabled: true\n",
        )
        .unwrap();
        let manifest = dir.join("manifest.yaml");
        fs::write(
            &manifest,
            r#"
configs:
  sozune-config:
    namespace: proxy
    name: "sozune-config"
    files:
      config.yaml: ./sozune/config.yaml
deployments:
  app:
    name: app
    image: myapp:latest
"#,
        )
        .unwrap();

        let config = load_config_file(manifest.to_str().unwrap(), &no_env()).unwrap();
        let entry = &config.configs["sozune-config"];
        let parsed: serde_json::Value =
            serde_json::from_str(entry.data.as_deref().unwrap()).unwrap();
        assert_eq!(
            parsed["config.yaml"],
            "providers:\n  docker:\n    enabled: true\n"
        );
        assert!(entry.files.is_empty());
    }

    #[test]
    fn test_config_file_without_configs() {
        let yaml_content = r#"
deployments:
  api:
    name: api
    image: myapp:latest
    runtime: docker
"#;

        let config: ConfigFile = serde_yaml::from_str(yaml_content).unwrap();
        assert_eq!(config.configs.len(), 0);
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

    #[test]
    fn first_config_id_returns_id_of_first_entry() {
        let configs = vec![
            serde_json::json!({"id": "abc-123", "name": "app-config"}),
            serde_json::json!({"id": "def-456", "name": "other"}),
        ];
        assert_eq!(first_config_id(&configs), Some("abc-123"));
    }

    #[test]
    fn first_config_id_none_when_list_is_empty() {
        assert_eq!(first_config_id(&[]), None);
    }

    #[test]
    fn first_config_id_none_when_id_is_missing_or_not_a_string() {
        let no_id = vec![serde_json::json!({"name": "app-config"})];
        assert_eq!(first_config_id(&no_id), None);

        let non_string_id = vec![serde_json::json!({"id": 42})];
        assert_eq!(first_config_id(&non_string_id), None);
    }
}
