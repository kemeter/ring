use bollard::{
    Docker,
    models::{HostConfig, Mount, MountTypeEnum, EndpointSettings, ContainerCreateBody, NetworkCreateRequest, NetworkConnectRequest},
    query_parameters::{
        CreateImageOptionsBuilder,
        CreateContainerOptionsBuilder,
        StartContainerOptionsBuilder,
        StopContainerOptionsBuilder,
        LogsOptionsBuilder,
        ListContainersOptionsBuilder,
        RemoveContainerOptionsBuilder,
        InspectNetworkOptionsBuilder,
        InspectContainerOptions
    },
    container::LogOutput,
    auth::DockerCredentials,
    exec::{CreateExecOptions, StartExecOptions},
};
use futures::StreamExt;
use futures::stream::Stream;
use std::collections::HashMap;
use std::pin::Pin;
use crate::models::deployments::Deployment;
use std::convert::TryInto;
use crate::api::dto::deployment::DeploymentVolume;
use std::default::Default;
use crate::models::config::Config;
use crate::runtime::error::RuntimeError;
use crate::runtime::types::InstanceStatus;

struct DockerImage {
    name: String,
    tag: String,
    auth: Option<(String, String, String)>,
}

impl From<bollard::errors::Error> for RuntimeError {
    fn from(err: bollard::errors::Error) -> Self {
        let err_msg = err.to_string();
        if err_msg.contains("404") || err_msg.contains("not found") || err_msg.contains("manifest unknown") {
            RuntimeError::ImageNotFound(err_msg)
        } else {
            RuntimeError::Other(err_msg)
        }
    }
}

pub(crate) async fn apply(mut deployment: Deployment, configs: HashMap<String, Config>) -> Deployment {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(e) => {
            error!("Failed to connect to Docker: {}", e);
            deployment.status = "Error".to_string();
            return deployment;
        }
    };

    deployment.instances = list_instances(deployment.id.to_string(), "active").await;

    // Handle the processing based on deployment type
    if deployment.kind == "job" {
        return handle_job_deployment(deployment, docker, configs).await;
    } else {
        return handle_worker_deployment(deployment, docker, configs).await;
    }
}

async fn handle_job_deployment(mut deployment: Deployment, docker: Docker, configs: HashMap<String, Config>) -> Deployment {
    if deployment.status == "deleted" {
        debug!("{} marked as deleted. Remove all instances", deployment.id);
        let instance_count = deployment.instances.len();
        for instance in deployment.instances.iter_mut() {
            remove_container(docker.clone(), instance.to_string()).await;
            info!("Docker container {} deleted", instance);
        }

        if instance_count > 0 {
            deployment.emit_event(
                "info",
                format!("Deleted {} container(s) for job marked as deleted", instance_count),
                "docker",
                Some("ContainerDeletion")
            );
        }

        return deployment;
    }

    // Check all instances for jobs (running + completed/failed)
    let all_instances = list_instances(deployment.id.to_string(), "all").await;

    if !all_instances.is_empty() {
        // Check the status of the existing container
        for instance_id in &all_instances {
            match check_container_status(docker.clone(), instance_id.clone()).await {
                InstanceStatus::Running => {
                    deployment.status = "running".to_string();
                    break;
                }
                InstanceStatus::Completed => {
                    deployment.status = "completed".to_string();
                    break;
                }
                InstanceStatus::Failed => {
                    deployment.status = "failed".to_string();
                    break;
                }
            }
        }
    } else {
        // Create the job if it does not exist yet
        if deployment.status == "creating" || deployment.status == "pending" {
            match create_container(&mut deployment, &docker, configs).await {
                Ok(_) => {
                    deployment.status = "running".to_string();
                }
                Err(RuntimeError::ImageNotFound(msg)) => {
                    error!("Image not found for job {}: {}", deployment.id, msg);
                    deployment.status = "ImagePullBackOff".to_string();
                    deployment.emit_event(
                        "error",
                        format!("Image '{}' not found", deployment.image),
                        "docker",
                        Some("ImagePullBackOff")
                    );
                }
                Err(RuntimeError::ImagePullFailed(msg)) => {
                    error!("Image pull failed for job {}: {}", deployment.id, msg);
                    deployment.status = "ImagePullBackOff".to_string();
                    deployment.emit_event(
                        "error",
                        format!("Failed to pull image '{}'", deployment.image),
                        "docker",
                        Some("ImagePullBackOff")
                    );
                }
                Err(RuntimeError::InstanceCreationFailed(msg)) => {
                    error!("Container creation failed for job {}: {}", deployment.id, msg);
                    deployment.status = "CreateContainerError".to_string();
                    deployment.emit_event(
                        "error",
                        format!("Container creation failed: {}", msg),
                        "docker",
                        Some("InstanceCreationFailed")
                    );
                }
                Err(RuntimeError::NetworkCreationFailed(msg)) => {
                    error!("Network creation failed for job {}: {}", deployment.id, msg);
                    deployment.status = "NetworkError".to_string();
                    deployment.emit_event(
                        "error",
                        format!("Failed to create network for namespace '{}'", deployment.namespace),
                        "docker",
                        Some("NetworkCreationFailed")
                    );
                }
                Err(RuntimeError::ConfigNotFound(msg)) => {
                    error!("Config not found for job {}: {}", deployment.id, msg);
                    deployment.status = "ConfigError".to_string();
                    deployment.emit_event(
                        "error",
                        format!("Config not found in namespace '{}'", deployment.namespace),
                        "docker",
                        Some("ConfigError")
                    );
                }
                Err(RuntimeError::ConfigKeyNotFound(msg)) => {
                    error!("Config key not found for job {}: {}", deployment.id, msg);
                    deployment.status = "ConfigError".to_string();
                    deployment.emit_event(
                        "error",
                        format!("Config key not found in namespace '{}'", deployment.namespace),
                        "docker",
                        Some("ConfigError")
                    );
                }
                Err(RuntimeError::FileSystemError(msg)) => {
                    error!("File system error for job {}: {}", deployment.id, msg);
                    deployment.status = "FileSystemError".to_string();
                    deployment.emit_event(
                        "error",
                        "Failed to access file system for config mount".to_string(),
                        "docker",
                        Some("FileSystemError")
                    );
                }
                Err(RuntimeError::ConnectionFailed(msg)) => {
                    error!("Connection failed for job {}: {}", deployment.id, msg);
                    deployment.status = "Error".to_string();
                    deployment.emit_event(
                        "error",
                        format!("Connection failed: {}", msg),
                        "docker",
                        Some("ConnectionFailed")
                    );
                }
                Err(RuntimeError::Other(msg)) => {
                    error!("Unknown error for job {}: {}", deployment.id, msg);
                    deployment.status = "Error".to_string();
                    deployment.emit_event(
                        "error",
                        format!("Docker error: {}", msg),
                        "docker",
                        Some("RuntimeError")
                    );
                }
            }
        }
    }

    debug!("Job runtime apply {:?}", deployment.id);
    deployment
}

async fn handle_worker_deployment(mut deployment: Deployment, docker: Docker, configs: HashMap<String, Config>) -> Deployment {
    if deployment.restart_count >= 5 && deployment.status != "deleted" {
        deployment.status = "CrashLoopBackOff".to_string();
        return deployment;
    }

    if deployment.status == "CrashLoopBackOff" {
        return deployment;
    }

    if deployment.status == "deleted" {
        debug!("{} marked as deleted. Remove all instances", deployment.id);
        let instance_count = deployment.instances.len();
        for instance in deployment.instances.iter_mut() {
            remove_container(docker.clone(), instance.to_string()).await;
            info!("Docker container {} deleted", instance);
        }

        if instance_count > 0 {
            deployment.emit_event(
                "info",
                format!("Deleted {} container(s) for worker marked as deleted", instance_count),
                "docker",
                Some("ContainerDeletion")
            );
        }
    } else {
        // Calculate difference and act accordingly
        let current_count: usize = deployment.instances.len();
        let target_count: usize = match deployment.replicas.try_into() {
            Ok(count) => count,
            Err(_) => {
                error!("Invalid replicas count for deployment {}: {}", deployment.id, deployment.replicas);
                deployment.status = "Failed".to_string();
                return deployment;
            }
        };

        debug!("Current instances: {}, Target instances: {}", current_count, target_count);

        match current_count.cmp(&target_count) {
            std::cmp::Ordering::Less => {
                debug!("Scaling up: {} -> {} (creating 1 container)", current_count, target_count);

                // Attempt to create container with error handling
                match create_container(&mut deployment, &docker, configs).await {
                    Ok(_) => {
                        // Container created successfully
                        deployment.restart_count += 1;

                        deployment.emit_event(
                            "info",
                            format!("Scaled up from {} to {} replicas", current_count, current_count + 1),
                            "docker",
                            Some("ScaleUp")
                        );

                        if deployment.status == "pending" || deployment.status == "creating" {
                            deployment.status = "running".to_string();
                        }
                    }
                    Err(RuntimeError::ImageNotFound(msg)) => {
                        error!("Image not found for deployment {}: {}", deployment.id, msg);
                        deployment.status = "ImagePullBackOff".to_string();
                        deployment.restart_count += 1;
                        deployment.emit_event(
                            "error",
                            format!("Image '{}' not found", deployment.image),
                            "docker",
                            Some("ImagePullBackOff")
                        );
                    }
                    Err(RuntimeError::ImagePullFailed(msg)) => {
                        error!("Image pull failed for deployment {}: {}", deployment.id, msg);
                        deployment.status = "ImagePullBackOff".to_string();
                        deployment.restart_count += 1;
                        deployment.emit_event(
                            "error",
                            format!("Failed to pull image '{}'", deployment.image),
                            "docker",
                            Some("ImagePullBackOff")
                        );
                    }
                    Err(RuntimeError::InstanceCreationFailed(msg)) => {
                        error!("Docker container creation failed for deployment {}: {}", deployment.id, msg);
                        deployment.status = "CreateContainerError".to_string();
                        deployment.restart_count += 1;
                        deployment.emit_event(
                            "error",
                            format!("Container creation failed: {}", msg),
                            "docker",
                            Some("InstanceCreationFailed")
                        );
                    }
                    Err(RuntimeError::NetworkCreationFailed(msg)) => {
                        error!("Network creation failed for deployment {}: {}", deployment.id, msg);
                        deployment.status = "NetworkError".to_string();
                        deployment.restart_count += 1;
                        deployment.emit_event(
                            "error",
                            format!("Failed to create network for namespace '{}'", deployment.namespace),
                            "docker",
                            Some("NetworkCreationFailed")
                        );
                    }
                    Err(RuntimeError::ConfigNotFound(msg)) => {
                        error!("Config not found for deployment {}: {}", deployment.id, msg);
                        deployment.status = "ConfigError".to_string();
                        deployment.restart_count += 1;
                        deployment.emit_event(
                            "error",
                            format!("Config not found in namespace '{}'", deployment.namespace),
                            "docker",
                            Some("ConfigError")
                        );
                    }
                    Err(RuntimeError::ConfigKeyNotFound(msg)) => {
                        error!("Config key not found for deployment {}: {}", deployment.id, msg);
                        deployment.status = "ConfigError".to_string();
                        deployment.restart_count += 1;
                        deployment.emit_event(
                            "error",
                            format!("Config key not found in namespace '{}'", deployment.namespace),
                            "docker",
                            Some("ConfigError")
                        );
                    }
                    Err(RuntimeError::FileSystemError(msg)) => {
                        error!("File system error for deployment {}: {}", deployment.id, msg);
                        deployment.status = "FileSystemError".to_string();
                        deployment.restart_count += 1;
                        deployment.emit_event(
                            "error",
                            "Failed to access file system for config mount".to_string(),
                            "docker",
                            Some("FileSystemError")
                        );
                    }
                    Err(RuntimeError::ConnectionFailed(msg)) => {
                        error!("Connection failed for deployment {}: {}", deployment.id, msg);
                        deployment.status = "Error".to_string();
                        deployment.restart_count += 1;
                        deployment.emit_event(
                            "error",
                            format!("Connection failed: {}", msg),
                            "docker",
                            Some("ConnectionFailed")
                        );
                    }
                    Err(RuntimeError::Other(msg)) => {
                        error!("Unknown error for deployment {}: {}", deployment.id, msg);
                        deployment.status = "Error".to_string();
                        deployment.restart_count += 1;
                        deployment.emit_event(
                            "error",
                            format!("Docker error: {}", msg),
                            "docker",
                            Some("RuntimeError")
                        );
                    }
                }
            }
            std::cmp::Ordering::Greater => {
                if target_count == 0 {
                    info!("Scaling deployment {} down to 0: removing container ({} remaining)", 
                          deployment.name, current_count - 1);
                } else {
                    debug!("Scaling down: {} -> {} (removing 1 container)", current_count, target_count);
                }
                
                if let Some(container_id) = deployment.instances.first().cloned() {
                    remove_container(docker.clone(), container_id.clone()).await;
                    // Synchronize local state with deletion
                    deployment.instances.remove(0);
                    info!("Container {} removed from deployment {}", container_id, deployment.id);

                    deployment.emit_event(
                        "info",
                        format!("Scaled down from {} to {} replicas (removed container {})", current_count, current_count - 1, container_id),
                        "docker",
                        Some("ScaleDown")
                    );
                }
            }
            std::cmp::Ordering::Equal => {
                debug!("Replicas count matches target: {} instances", current_count);
            }
        }
    }

    debug!("Worker runtime apply {:?}", deployment.id);
    deployment
}

async fn check_container_status(docker: Docker, container_id: String) -> InstanceStatus {
    let inspect_options = InspectContainerOptions {
        size: true,
        ..Default::default()
    };
    match docker.inspect_container(&container_id, Some(inspect_options)).await {
        Ok(info) => {
            if let Some(state) = info.state {
                if state.running == Some(true) {
                    InstanceStatus::Running
                } else if state.exit_code == Some(0) {
                    InstanceStatus::Completed
                } else {
                    InstanceStatus::Failed
                }
            } else {
                InstanceStatus::Failed
            }
        }
        Err(e) => {
            debug!("Failed to inspect container {}: {}", container_id, e);
            InstanceStatus::Failed
        }
    }
}

async fn pull_image(docker: Docker, image_config: DockerImage) -> Result<(), RuntimeError> {
    let image = image_config.name.clone();
    let tag = image_config.tag.clone();
    let image_name = format!("{}:{}", image, tag);
    info!("Pull docker image: {}", image_name);

    // Check if image already exists locally
    match docker.inspect_image(&image_name).await {
        Ok(_) => {
            debug!("Docker image {} already exists locally", image_name);
            return Ok(());
        }
        Err(_) => {
            debug!("Docker image {} not found locally, pulling...", image_name);
        }
    }

    let create_image_options = CreateImageOptionsBuilder::new()
        .from_image(&image)
        .tag(&tag)
        .build();

    let credentials = if let Some((server, username, password)) = image_config.auth {
        Some(DockerCredentials {
            username: Some(username),
            password: Some(password),
            serveraddress: Some(server),
            ..Default::default()
        })
    } else {
        None
    };

    let mut stream = docker.create_image(
        Some(create_image_options),
        None,
        credentials,
    );

    let mut has_error = false;
    let mut last_error = String::new();

    while let Some(pull_result) = stream.next().await {
        match pull_result {
            Ok(_output) => {
                // Log success if needed
            }
            Err(e) => {
                let error_msg = e.to_string();
                error!("Docker image pull error: {}", error_msg);
                has_error = true;
                last_error = error_msg.clone();

                // If 404 or "not found" error, stop immediately
                if error_msg.contains("404") || error_msg.contains("not found") || error_msg.contains("manifest unknown") {
                    return Err(RuntimeError::ImageNotFound(last_error));
                }
            }
        }
    }

    if has_error {
        return Err(RuntimeError::ImagePullFailed(last_error));
    }

    // Check one last time that the image is available after pull
    match docker.inspect_image(&image_name).await {
        Ok(_) => {
            info!("Docker successfully pulled image {}", image_name);
            Ok(())
        }
        Err(e) => {
            error!("Docker image {} still not available after pull: {}", image_name, e);
            Err(RuntimeError::ImageNotFound(format!("Image {} not available after pull", image_name)))
        }
    }
}

fn build_user_config(deployment_config: &Option<crate::models::deployments::DeploymentConfig>) -> Option<String> {
    if let Some(config) = deployment_config {
        if let Some(user_config) = &config.user {
            match (user_config.id, user_config.group) {
                (Some(uid), Some(gid)) => Some(format!("{}:{}", uid, gid)),
                (Some(uid), None) => Some(uid.to_string()),
                _ => None,
            }
        } else {
            None
        }
    } else {
        None
    }
}

fn get_privileged_config(deployment_config: &Option<crate::models::deployments::DeploymentConfig>) -> Option<bool> {
    deployment_config
        .as_ref()
        .and_then(|c| c.user.as_ref())
        .and_then(|u| u.privileged)
}

async fn create_container<'a>(deployment: &mut Deployment, docker: &Docker, configs: HashMap<String, Config>) -> Result<(), RuntimeError> {
    debug!("Create container for deployment id: {}", &deployment.id);
    let (image, tag) = match deployment.image.split_once(':') {
        Some((image, tag)) => (image.to_string(), tag.to_string()),
        None => (deployment.image.clone(), "latest".to_string()),
    };

    let mut image_config = DockerImage {
        name: image,
        tag: tag,
        auth: None,
    };

    let image_config = match &deployment.config {
        Some(config) => {
            match (&config.server, &config.username, &config.password) {
                (Some(server), Some(username), Some(password)) => {
                    image_config.auth = Some((server.clone(), username.clone(), password.clone()));
                }
                _ => {}
            }

            image_config
        }
        None => {
            image_config
        }
    };

    let should_pull = deployment.config
        .as_ref()
        .map(|config| config.image_pull_policy.as_str() != "Never")
        .unwrap_or(true);

    if should_pull {
        pull_image(docker.clone(), image_config).await?;
    }

    let network_name = format!("ring_{}", deployment.namespace.clone());
    create_network(docker.clone(), network_name.clone()).await?;

    let temporary_id = tiny_id();
    let container_name = format!("{}_{}_{}", &deployment.namespace, &deployment.name, temporary_id);

    let mut labels = HashMap::new();
    labels.insert("ring_deployment".to_string(), deployment.id.clone());

    let labels_format = &deployment.labels;
    for (key, value) in labels_format.iter() {
        labels.insert(key.clone(), value.clone());
    }

    let secrets_format = &deployment.secrets;
    let mut envs: Vec<String> = vec![];
    for (key, value) in secrets_format {
        envs.push(format!("{}={}", key, value))
    }

    let volumes_collection: Vec<DeploymentVolume> = serde_json::from_str(&deployment.volumes)
        .map_err(|e| RuntimeError::InstanceCreationFailed(format!("Failed to parse volumes: {}", e)))?;
    let mut mounts: Vec<Mount> = vec![];

    for volume in volumes_collection {
        let mount = create_mount_from_volume(volume, configs.clone(), deployment.id.to_string())?;
        mounts.push(mount);
    }


    let user_config = build_user_config(&deployment.config);
    let privileged_config = get_privileged_config(&deployment.config);

    let host_config = HostConfig {
        mounts: Some(mounts),
        privileged: privileged_config,
        ..Default::default()
    };

    let config = ContainerCreateBody {
        image: Some(deployment.image.clone()),
        cmd: Some(deployment.command.clone()),
        env: Some(envs),
        labels: Some(labels),
        host_config: Some(host_config),
        user: user_config,
        ..Default::default()
    };

    let options = CreateContainerOptionsBuilder::new()
        .name(&container_name)
        .build();

    match docker.create_container(Some(options), config).await {
        Ok(container) => {
            debug!("Docker create container {:?}", container.id);
            deployment.instances.push(container.id.to_string());

            // Connect to network
            let endpoint_config = EndpointSettings {
                aliases: Some(vec![deployment.name.clone(), container_name.clone()]),
                ..Default::default()
            };

            let connect_request = NetworkConnectRequest {
                container: Some(container.id.clone()),
                endpoint_config: Some(endpoint_config),
            };

            docker
                .connect_network(&network_name, connect_request)
                .await
                .map_err(|e| RuntimeError::InstanceCreationFailed(format!("Docker failed to connect to network: {}", e)))?;

            // Start container
            let start_options = StartContainerOptionsBuilder::new().build();
            docker
                .start_container(&container.id, Some(start_options))
                .await
                .map_err(|e| RuntimeError::InstanceCreationFailed(format!("Docker failed to start container: {}", e)))?;

            info!("Docker container {} created and started successfully", container_name);
            Ok(())
        }
        Err(e) => {
            error!("Docker failed to create container: {}", e);
            Err(RuntimeError::from(e))
        }
    }
}

fn create_mount_from_volume(volume: DeploymentVolume, configs: HashMap<String, Config>, deployment_id: String) -> Result<Mount, RuntimeError> {

    let mount = if volume.r#type.as_str() == "bind" {

        let volume_source = volume.source.ok_or_else(||
            RuntimeError::InstanceCreationFailed("Bind volume requires a source".to_string()))?;
        let type_mount = if volume_source.starts_with('/') { Some(MountTypeEnum::BIND) } else { Some(MountTypeEnum::VOLUME) };

        Mount {
            target: Some(volume.destination),
            source: Some(volume_source),
            typ: type_mount,
            read_only: Some(volume.permission == "ro"),
            ..Default::default()
        }
    } else if volume.r#type.as_str() == "volume" {

        let volume_name = volume.source.ok_or_else(||
            RuntimeError::InstanceCreationFailed("Named volume requires a source".to_string()))?;

        Mount {
            target: Some(volume.destination),
            source: Some(volume_name),
            typ: Some(MountTypeEnum::VOLUME),
            read_only: Some(volume.permission == "ro"),
            ..Default::default()
        }
    } else {
        let config_name = volume.source.as_ref().ok_or_else(||
            RuntimeError::InstanceCreationFailed("Config volume requires a source".to_string()))?;

        let config = configs.get(config_name)
            .ok_or_else(|| RuntimeError::ConfigNotFound(format!("Config '{}' not found", config_name)))?;

        let config_data: HashMap<String, String> = serde_json::from_str(&config.data)?;

        let key = volume.key.as_ref()
            .ok_or_else(|| RuntimeError::ConfigKeyNotFound("Missing 'key' field for config volume".to_string()))?;

        let content = config_data.get(key)
            .ok_or_else(|| RuntimeError::ConfigKeyNotFound(format!("Key '{}' not found in config '{}'", key, config_name)))?;

        let temp_dir = format!("/tmp/ring_configs/{}", deployment_id);
        std::fs::create_dir_all(&temp_dir)?;

        let temporary_id = tiny_id();

        let temp_file = format!("{}/{}", temp_dir, temporary_id);
        std::fs::write(&temp_file, content)?;

        debug!("Created temporary config file: {} -> {}", temp_file, volume.destination);

        Mount {
            target: Some(volume.destination),
            source: Some(temp_file),
            typ: Some(MountTypeEnum::BIND),
            read_only: Some(volume.permission == "ro"),
            ..Default::default()
        }
    };
    Ok(mount)
}

pub(crate) async fn remove_container_by_id(container_id: String) {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(e) => {
            error!("Failed to connect to Docker: {}", e);
            return;
        }
    };
    
    remove_container(docker, container_id).await;
}

pub(crate) async fn execute_health_check_for_instance(container_id: String, health_check: crate::models::health_check::HealthCheck) -> (crate::models::health_check::HealthCheckStatus, Option<String>) {
    use crate::models::health_check::{HealthCheck, HealthCheckStatus};
    
    let container_ip = match health_check {
        HealthCheck::Command { .. } => None,
        _ => get_container_ip(&container_id).await
    };
    
    match health_check {
        HealthCheck::Tcp { port, .. } => {
            match container_ip {
                Some(ip) => execute_tcp_check_for_container(&ip, port).await,
                None => (HealthCheckStatus::Failed, Some(format!("Could not get IP for container {}", container_id)))
            }
        },
        HealthCheck::Http { url, .. } => {
            match container_ip {
                Some(ip) => execute_http_check_for_container(&ip, &url).await,
                None => (HealthCheckStatus::Failed, Some(format!("Could not get IP for container {}", container_id)))
            }
        },
        HealthCheck::Command { command, .. } => {
            execute_command_check_for_container(&container_id, &command).await
        }
    }
}

async fn get_container_ip(container_id: &str) -> Option<String> {
    let docker = Docker::connect_with_local_defaults().ok()?;
    
    let inspect_result = docker.inspect_container(container_id, None::<InspectContainerOptions>).await.ok()?;
    
    if let Some(networks) = inspect_result.network_settings?.networks {
        if let Some(bridge) = networks.get("bridge") {
            if let Some(ip) = &bridge.ip_address {
                if !ip.is_empty() {
                    return Some(ip.clone());
                }
            }
        }
        
        for (_, network) in networks {
            if let Some(ip) = network.ip_address {
                if !ip.is_empty() {
                    return Some(ip);
                }
            }
        }
    }
    
    None
}

async fn execute_tcp_check_for_container(container_ip: &str, port: u16) -> (crate::models::health_check::HealthCheckStatus, Option<String>) {
    use crate::models::health_check::HealthCheckStatus;
    use tokio::net::TcpStream;
    use std::time::Duration;

    let addr = format!("{}:{}", container_ip, port);

    match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&addr)).await {
        Ok(Ok(_stream)) => {
            (HealthCheckStatus::Success, Some(format!("TCP connection to {} successful", addr)))
        },
        Ok(Err(e)) => {
            (HealthCheckStatus::Failed, Some(format!("TCP connection failed: {}", e)))
        },
        Err(_) => {
            (HealthCheckStatus::Failed, Some(format!("TCP connection timed out for {}", addr)))
        }
    }
}

async fn execute_http_check_for_container(container_ip: &str, url: &str) -> (crate::models::health_check::HealthCheckStatus, Option<String>) {
    use crate::models::health_check::HealthCheckStatus;

    let target_url = url.replace("localhost", container_ip);

    match reqwest::get(&target_url).await {
        Ok(response) => {
            let code = response.status().as_u16();
            if (200..300).contains(&code) {
                (HealthCheckStatus::Success, Some(format!("HTTP check successful ({}) for {}", code, target_url)))
            } else {
                (HealthCheckStatus::Failed, Some(format!("HTTP check failed with status {} for {}", code, target_url)))
            }
        },
        Err(e) => {
            (HealthCheckStatus::Failed, Some(format!("HTTP request failed for {}: {}", target_url, e)))
        }
    }
}

async fn execute_command_check_for_container(container_id: &str, command: &str) -> (crate::models::health_check::HealthCheckStatus, Option<String>) {
    use crate::models::health_check::HealthCheckStatus;
    
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(e) => {
            return (HealthCheckStatus::Failed, Some(format!("Failed to connect to Docker: {}", e)));
        }
    };
    
    // Parse command using shell-words to properly handle quotes and escaping
    let cmd_parts = match shell_words::split(command) {
        Ok(parts) if parts.is_empty() => {
            return (HealthCheckStatus::Failed, Some("Empty command".to_string()));
        }
        Ok(parts) => parts,
        Err(e) => {
            return (HealthCheckStatus::Failed, Some(format!("Invalid command syntax: {}", e)));
        }
    };
    
    // Create exec instance
    let exec_options = CreateExecOptions {
        cmd: Some(cmd_parts),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        ..Default::default()
    };
    
    let exec_result = match docker.create_exec(container_id, exec_options).await {
        Ok(result) => result,
        Err(e) => {
            return (HealthCheckStatus::Failed, Some(format!("Failed to create exec: {}", e)));
        }
    };
    
    // Start execution
    let start_exec_options = StartExecOptions {
        detach: false,
        ..Default::default()
    };
    
    match docker.start_exec(&exec_result.id, Some(start_exec_options)).await {
        Ok(_) => {
            (HealthCheckStatus::Success, Some("Command executed successfully".to_string()))
        },
        Err(e) => {
            (HealthCheckStatus::Failed, Some(format!("Failed to execute command: {}", e)))
        }
    }
}

async fn remove_container(docker: Docker, container_id: String) {
    let stop_options = StopContainerOptionsBuilder::new()
        .build();

    match docker.stop_container(&container_id, Some(stop_options)).await {
        Ok(_) => {
            debug!("Container {} stopped successfully", container_id);
        }
        Err(e) => {
            debug!("Error stopping container {}: {:?}", container_id, e);
        }
    }

    let remove_options = RemoveContainerOptionsBuilder::new().build();
    match docker.remove_container(&container_id, Some(remove_options)).await {
        Ok(_) => {
            info!("Container {} removed successfully", container_id);
        }
        Err(e) => {
            error!("Error removing container {}: {:?}", container_id, e);
        }
    }
}

async fn create_network(docker: Docker, network_name: String) -> Result<(), RuntimeError> {
    debug!("Start Docker create network: {}", network_name);

    let inspect_options = InspectNetworkOptionsBuilder::new().build();
    match docker.inspect_network(&network_name, Some(inspect_options)).await {
        Ok(_network_info) => {
            debug!("Docker network {} already exists", network_name);
            Ok(())
        }
        Err(_) => {
            info!("Docker create network: {}", network_name);

            let create_request = NetworkCreateRequest {
                name: network_name.clone(),
                ..Default::default()
            };

            match docker.create_network(create_request).await {
                Ok(info) => {
                    debug!("Network created: {:?}", info);
                    Ok(())
                },
                Err(e) => {
                    error!("Docker network create error: {}", e);
                    Err(RuntimeError::NetworkCreationFailed(format!("Failed to create network {}: {}", network_name, e)))
                }
            }
        }
    }
}

pub(crate) async fn list_instances(id: String, status: &str) -> Vec<String> {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(e) => {
            error!("Failed to connect to Docker: {}", e);
            return Vec::new();
        }
    };

    let mut instances: Vec<String> = Vec::new();

    let options = if status == "all" {
        ListContainersOptionsBuilder::new()
            .all(true)
            .build()
    } else if status == "active" {
        // "active" = running + created + restarting (prevents race conditions)
        let filters = HashMap::from([
            ("status".to_string(), vec![
                "running".to_string(),
                "created".to_string(),
                "restarting".to_string(),
            ])
        ]);
        ListContainersOptionsBuilder::new()
            .all(true)
            .filters(&filters)
            .build()
    } else {
        let filters = HashMap::from([("status".to_string(), vec![status.to_string()])]);
        ListContainersOptionsBuilder::new()
            .all(false)
            .filters(&filters)
            .build()
    };

    match docker.list_containers(Some(options)).await {
        Ok(containers) => {
            for container in containers {
                if let Some(labels) = container.labels {
                    if let Some(deployment_id) = labels.get("ring_deployment") {
                        if deployment_id == &id {
                            if let Some(container_id) = container.id {
                                instances.push(container_id);
                            }
                        }
                    }
                }
            }
        }
        Err(e) => debug!("Docker list instances error: {}", e),
    }

    return instances;
}

pub(crate) async fn list_instances_with_names(id: String, status: &str) -> Vec<(String, String)> {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(e) => {
            error!("Failed to connect to Docker: {}", e);
            return Vec::new();
        }
    };

    let mut instances: Vec<(String, String)> = Vec::new();

    let options = if status == "all" {
        ListContainersOptionsBuilder::new()
            .all(true)
            .build()
    } else if status == "active" {
        let filters = HashMap::from([
            ("status".to_string(), vec![
                "running".to_string(),
                "created".to_string(),
                "restarting".to_string(),
            ])
        ]);
        ListContainersOptionsBuilder::new()
            .all(true)
            .filters(&filters)
            .build()
    } else {
        let filters = HashMap::from([("status".to_string(), vec![status.to_string()])]);
        ListContainersOptionsBuilder::new()
            .all(false)
            .filters(&filters)
            .build()
    };

    match docker.list_containers(Some(options)).await {
        Ok(containers) => {
            for container in containers {
                if let Some(labels) = &container.labels {
                    if let Some(deployment_id) = labels.get("ring_deployment") {
                        if deployment_id == &id {
                            if let Some(container_id) = &container.id {
                                let name = container.names
                                    .as_ref()
                                    .and_then(|names| names.first())
                                    .map(|n| n.trim_start_matches('/').to_string())
                                    .unwrap_or_else(|| container_id[..12].to_string());
                                instances.push((container_id.clone(), name));
                            }
                        }
                    }
                }
            }
        }
        Err(e) => debug!("Docker list instances error: {}", e),
    }

    instances
}

pub(crate) async fn logs(container_id: String, tail: Option<&str>, since: Option<i32>) -> Vec<String> {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(e) => {
            error!("Failed to connect to Docker: {}", e);
            return Vec::new();
        }
    };

    // Check if container exists first
    match docker.inspect_container(&container_id, None::<InspectContainerOptions>).await {
        Ok(_) => {
            // Container exists, proceed with logs
        }
        Err(e) => {
            debug!("Container {} not found or not accessible: {}", container_id, e);
            return Vec::new();
        }
    }

    let mut builder = LogsOptionsBuilder::new()
        .stdout(true)
        .stderr(true);

    if let Some(tail_value) = tail {
        builder = builder.tail(tail_value);
    }

    if let Some(since_value) = since {
        builder = builder.since(since_value);
    }

    let options = builder.build();

    let mut logs_stream = docker.logs(&container_id, Some(options));
    let mut logs = vec![];

    while let Some(log_result) = logs_stream.next().await {
        match log_result {
            Ok(chunk) => {
                let log_line = format_log_output(chunk).replace("\n", "");
                if !log_line.trim().is_empty() {
                    logs.push(log_line);
                }
            }
            Err(e) => {
                debug!("Docker get logs errors for container {}: {}", container_id, e);
                break; // Stop on error instead of continuing
            }
        }
    }

    return logs;
}

pub(crate) async fn logs_stream(
    container_id: String,
    tail: Option<&str>,
    since: Option<i32>,
) -> Pin<Box<dyn Stream<Item = String> + Send>> {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(e) => {
            error!("Failed to connect to Docker: {}", e);
            return Box::pin(futures::stream::empty());
        }
    };

    match docker.inspect_container(&container_id, None::<InspectContainerOptions>).await {
        Ok(_) => {}
        Err(e) => {
            debug!("Container {} not found or not accessible: {}", container_id, e);
            return Box::pin(futures::stream::empty());
        }
    }

    let mut builder = LogsOptionsBuilder::new()
        .stdout(true)
        .stderr(true)
        .follow(true);

    if let Some(tail_value) = tail {
        builder = builder.tail(tail_value);
    }

    if let Some(since_value) = since {
        builder = builder.since(since_value);
    }

    let options = builder.build();

    let stream = docker.logs(&container_id, Some(options))
        .filter_map(|result| async {
            match result {
                Ok(chunk) => {
                    let log_line = format_log_output(chunk).replace("\n", "");
                    if !log_line.trim().is_empty() {
                        Some(log_line)
                    } else {
                        None
                    }
                }
                Err(e) => {
                    debug!("Docker stream logs error: {}", e);
                    None
                }
            }
        });

    Box::pin(stream)
}

fn format_log_output(output: LogOutput) -> String {
    match output {
        LogOutput::StdOut { message } => {
            String::from_utf8_lossy(&message).to_string()
        }
        LogOutput::StdErr { message } => {
            String::from_utf8_lossy(&message).to_string()
        }
        LogOutput::StdIn { message } => {
            String::from_utf8_lossy(&message).to_string()
        }
        LogOutput::Console { message } => {
            String::from_utf8_lossy(&message).to_string()
        }
    }
}

fn tiny_id() -> String {
    use rand::Rng;

    let mut rng = rand::rng();
    format!("{:08x}", rng.random::<u32>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use crate::models::config::Config;
    use crate::api::dto::deployment::DeploymentVolume;
    use crate::models::deployments::UserConfig;

    #[test]
    fn test_build_user_config_with_uid_and_gid() {
        let config = Some(crate::models::deployments::DeploymentConfig {
            image_pull_policy: String::from("always"),
            server: None,
            username: None,
            password: None,
            user: Some(UserConfig {
                id: Some(1000),
                group: Some(1000),
                privileged: Some(false),
            }),
        });

        let result = build_user_config(&config);
        assert_eq!(result, Some("1000:1000".to_string()));
    }

    #[test]
    fn test_build_user_config_with_uid_only() {
        let config = Some(crate::models::deployments::DeploymentConfig {
            image_pull_policy: String::from("always"),
            server: None,
            username: None,
            password: None,
            user: Some(UserConfig {
                id: Some(1000),
                group: None,
                privileged: Some(false),
            }),
        });

        let result = build_user_config(&config);
        assert_eq!(result, Some("1000".to_string()));
    }

    #[test]
    fn test_build_user_config_none() {
        let config = None;
        let result = build_user_config(&config);
        assert_eq!(result, None);
    }

    #[test]
    fn test_get_privileged_config() {
        let config = Some(crate::models::deployments::DeploymentConfig {
            image_pull_policy: String::from("always"),
            server: None,
            username: None,
            password: None,
            user: Some(UserConfig {
                id: Some(0),
                group: Some(0),
                privileged: Some(true),
            }),
        });

        let result = get_privileged_config(&config);
        assert_eq!(result, Some(true));
    }

    #[test]
    fn test_bind_volume_creation() {
        let volume = DeploymentVolume {
            r#type: "bind".to_string(),
            source: Some("/host/path".to_string()),
            destination: "/container/path".to_string(),
            driver: "local".to_string(),
            permission: "rw".to_string(),
            key: None,
        };

        let mount = create_mount_from_volume(volume, HashMap::new(), "test-deployment".to_string()).unwrap();

        assert_eq!(mount.target, Some("/container/path".to_string()));
        assert_eq!(mount.source, Some("/host/path".to_string()));
        assert_eq!(mount.typ, Some(MountTypeEnum::BIND));
        assert_eq!(mount.read_only, Some(false));
    }

    #[test]
    fn test_config_volume_creation() {
        let mut configs = HashMap::new();
        let config_data = r#"{"nginx.conf":"server { listen 80; server_name localhost; location / { root /usr/share/nginx/html; index index.html index.htm; } }"}"#;
        configs.insert("test-config".to_string(), Config {
            id: "9d74dfba-f6ad-4e67-a24d-4041b9b709d4 ".to_string(),
            created_at: "2010-03-15 11:41:00".to_string(),
            updated_at: None,
            namespace: "kemeter".to_string(),
            name: "secret_de_la_mort_qui_tue".to_string(),
            data: config_data.to_string(),
            labels: "[]".to_string(),
        });

        let volume = DeploymentVolume {
            r#type: "config".to_string(),
            source: Some("test-config".to_string()),
            destination: "/app/nginx.conf".to_string(),
            driver: "local".to_string(),
            permission: "ro".to_string(),
            key: Some("nginx.conf".to_string()),
        };

        let mount = create_mount_from_volume(volume, configs, "test-deployment".to_string()).unwrap();

        assert_eq!(mount.target, Some("/app/nginx.conf".to_string()));
        assert!(mount.source.unwrap().contains("/tmp/ring_configs/test-deployment"));
        assert_eq!(mount.read_only, Some(true));
    }

    #[test]
    fn test_config_volume_with_missing_key_should_fail() {
        let mut configs = HashMap::new();
        let config_data = r#"{"existing_key": "value"}"#;
        configs.insert("test-config".to_string(), Config {
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            created_at: "2010-03-15 11:41:00".to_string(),
            updated_at: None,
            namespace: "kemeter".to_string(),
            name: "".to_string(),
            data: config_data.to_string(),
            labels: "".to_string(),
        });

        let volume = DeploymentVolume {
            r#type: "config".to_string(),
            source: Some("test-config".to_string()),
            key: Some("missing_key".to_string()),
            destination: "/tmp/toto".to_string(),
            driver: "local".to_string(),
            permission: "ro".to_string(),
        };

        let result = create_mount_from_volume(volume, configs, "test-deployment".to_string());

        assert!(matches!(result, Err(RuntimeError::ConfigKeyNotFound(_))));
    }

    #[test]
    fn test_docker_volume_creation() {
        let volume = DeploymentVolume {
            r#type: "volume".to_string(),
            source: Some("my-docker-volume".to_string()),
            destination: "/app/data".to_string(),
            driver: "local".to_string(),
            permission: "rw".to_string(),
            key: None,
        };

        let mount = create_mount_from_volume(volume, HashMap::new(), "test-deployment".to_string()).unwrap();

        assert_eq!(mount.target, Some("/app/data".to_string()));
        assert_eq!(mount.source, Some("my-docker-volume".to_string()));
        assert_eq!(mount.typ, Some(MountTypeEnum::VOLUME));
        assert_eq!(mount.read_only, Some(false));
    }
}