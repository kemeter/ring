use bollard::{
    Docker,
    models::{HostConfig, Mount, MountTypeEnum, EndpointSettings, ContainerCreateBody, NetworkCreateRequest, NetworkConnectRequest},
    query_parameters::{
        CreateImageOptionsBuilder,
        CreateContainerOptionsBuilder,
        StartContainerOptionsBuilder,
        StopContainerOptionsBuilder,
        RemoveContainerOptionsBuilder,
        InspectNetworkOptionsBuilder,
    },
    auth::DockerCredentials,
};
use futures::StreamExt;
use std::collections::HashMap;
use crate::models::deployments::{Deployment, parse_memory_string};
use crate::api::dto::deployment::DeploymentVolume;
use crate::models::config::Config;
use crate::runtime::error::RuntimeError;
use super::{DockerImage, tiny_id};

fn build_user_config(deployment_config: &Option<crate::models::deployments::DeploymentConfig>) -> Option<String> {
    let user = deployment_config.as_ref()?.user.as_ref()?;
    match (user.id, user.group) {
        (Some(uid), Some(gid)) => Some(format!("{}:{}", uid, gid)),
        (Some(uid), None) => Some(uid.to_string()),
        _ => None,
    }
}

fn get_privileged_config(deployment_config: &Option<crate::models::deployments::DeploymentConfig>) -> Option<bool> {
    deployment_config
        .as_ref()
        .and_then(|c| c.user.as_ref())
        .and_then(|u| u.privileged)
}

async fn pull_image(docker: Docker, image_config: DockerImage) -> Result<(), RuntimeError> {
    let image = image_config.name.clone();
    let tag = image_config.tag.clone();
    let image_name = format!("{}:{}", image, tag);
    info!("Pull docker image: {}", image_name);

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

    let credentials = image_config.auth.map(|(server, username, password)| {
        DockerCredentials {
            username: Some(username),
            password: Some(password),
            serveraddress: Some(server),
            ..Default::default()
        }
    });

    let mut stream = docker.create_image(Some(create_image_options), None, credentials);

    let mut has_error = false;
    let mut last_error = String::new();

    while let Some(pull_result) = stream.next().await {
        match pull_result {
            Ok(_) => {}
            Err(e) => {
                let error_msg = e.to_string();
                error!("Docker image pull error: {}", error_msg);
                has_error = true;
                last_error = error_msg.clone();

                if error_msg.contains("404") || error_msg.contains("not found") || error_msg.contains("manifest unknown") {
                    return Err(RuntimeError::ImageNotFound(last_error));
                }
            }
        }
    }

    if has_error {
        return Err(RuntimeError::ImagePullFailed(last_error));
    }

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

pub(crate) async fn create_container(deployment: &mut Deployment, docker: &Docker, configs: HashMap<String, Config>) -> Result<(), RuntimeError> {
    debug!("Create container for deployment id: {}", &deployment.id);
    let (image, tag) = match deployment.image.split_once(':') {
        Some((image, tag)) => (image.to_string(), tag.to_string()),
        None => (deployment.image.clone(), "latest".to_string()),
    };

    let mut image_config = DockerImage { name: image, tag, auth: None };

    if let Some(config) = &deployment.config {
        if let (Some(server), Some(username), Some(password)) =
            (&config.server, &config.username, &config.password)
        {
            image_config.auth = Some((server.clone(), username.clone(), password.clone()));
        }
    }

    let should_pull = deployment.config
        .as_ref()
        .map(|config| config.image_pull_policy.as_str() != "Never")
        .unwrap_or(true);

    if should_pull {
        pull_image(docker.clone(), image_config).await?;
    }

    let network_name = format!("ring_{}", deployment.namespace);
    create_network(docker.clone(), network_name.clone()).await?;

    let temporary_id = tiny_id();
    let container_name = format!("{}_{}_{}", &deployment.namespace, &deployment.name, temporary_id);

    let mut labels = HashMap::new();
    labels.insert("ring_deployment".to_string(), deployment.id.clone());
    for (key, value) in deployment.labels.iter() {
        labels.insert(key.clone(), value.clone());
    }

    let envs: Vec<String> = deployment.secrets
        .iter()
        .map(|(key, value)| format!("{}={}", key, value))
        .collect();

    let volumes_collection: Vec<DeploymentVolume> = serde_json::from_str(&deployment.volumes)
        .map_err(|e| RuntimeError::InstanceCreationFailed(format!("Failed to parse volumes: {}", e)))?;

    let mut mounts: Vec<Mount> = vec![];
    for volume in volumes_collection {
        mounts.push(create_mount_from_volume(volume, configs.clone(), deployment.id.to_string())?);
    }

    let user_config = build_user_config(&deployment.config);
    let privileged_config = get_privileged_config(&deployment.config);

    let host_config = HostConfig {
        mounts: Some(mounts),
        privileged: privileged_config,
        nano_cpus: deployment.resources.as_ref()
            .and_then(|r| r.cpu_limit.map(|cpu| (cpu * 1_000_000_000.0) as i64)),
        memory: deployment.resources.as_ref()
            .and_then(|r| r.memory_limit.as_ref().and_then(|m| parse_memory_string(m).ok())),
        memory_reservation: deployment.resources.as_ref()
            .and_then(|r| r.memory_reservation.as_ref().and_then(|m| parse_memory_string(m).ok())),
        cpu_shares: deployment.resources.as_ref()
            .and_then(|r| r.cpu_shares),
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

pub(super) fn create_mount_from_volume(volume: DeploymentVolume, configs: HashMap<String, Config>, deployment_id: String) -> Result<Mount, RuntimeError> {
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

pub(crate) async fn remove_container(docker: Docker, container_id: String) {
    let stop_options = StopContainerOptionsBuilder::new().build();

    match docker.stop_container(&container_id, Some(stop_options)).await {
        Ok(_) => debug!("Container {} stopped successfully", container_id),
        Err(e) => debug!("Error stopping container {}: {:?}", container_id, e),
    }

    let remove_options = RemoveContainerOptionsBuilder::new().build();
    match docker.remove_container(&container_id, Some(remove_options)).await {
        Ok(_) => info!("Container {} removed successfully", container_id),
        Err(e) => error!("Error removing container {}: {:?}", container_id, e),
    }
}

pub(crate) async fn remove_container_by_id(docker: &Docker, container_id: String) {
    remove_container(docker.clone(), container_id).await;
}

async fn create_network(docker: Docker, network_name: String) -> Result<(), RuntimeError> {
    debug!("Start Docker create network: {}", network_name);

    let inspect_options = InspectNetworkOptionsBuilder::new().build();
    match docker.inspect_network(&network_name, Some(inspect_options)).await {
        Ok(_) => {
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
                }
                Err(e) => {
                    error!("Docker network create error: {}", e);
                    Err(RuntimeError::NetworkCreationFailed(format!("Failed to create network {}: {}", network_name, e)))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::deployments::UserConfig;

    #[test]
    fn test_build_user_config_with_uid_and_gid() {
        let config = Some(crate::models::deployments::DeploymentConfig {
            image_pull_policy: String::from("always"),
            server: None,
            username: None,
            password: None,
            user: Some(UserConfig { id: Some(1000), group: Some(1000), privileged: Some(false) }),
        });
        assert_eq!(build_user_config(&config), Some("1000:1000".to_string()));
    }

    #[test]
    fn test_build_user_config_with_uid_only() {
        let config = Some(crate::models::deployments::DeploymentConfig {
            image_pull_policy: String::from("always"),
            server: None,
            username: None,
            password: None,
            user: Some(UserConfig { id: Some(1000), group: None, privileged: Some(false) }),
        });
        assert_eq!(build_user_config(&config), Some("1000".to_string()));
    }

    #[test]
    fn test_build_user_config_none() {
        assert_eq!(build_user_config(&None), None);
    }

    #[test]
    fn test_get_privileged_config() {
        let config = Some(crate::models::deployments::DeploymentConfig {
            image_pull_policy: String::from("always"),
            server: None,
            username: None,
            password: None,
            user: Some(UserConfig { id: Some(0), group: Some(0), privileged: Some(true) }),
        });
        assert_eq!(get_privileged_config(&config), Some(true));
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
        configs.insert("test-config".to_string(), Config {
            id: "9d74dfba-f6ad-4e67-a24d-4041b9b709d4 ".to_string(),
            created_at: "2010-03-15 11:41:00".to_string(),
            updated_at: None,
            namespace: "kemeter".to_string(),
            name: "secret_de_la_mort_qui_tue".to_string(),
            data: r#"{"nginx.conf":"server { listen 80; server_name localhost; location / { root /usr/share/nginx/html; index index.html index.htm; } }"}"#.to_string(),
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
        configs.insert("test-config".to_string(), Config {
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            created_at: "2010-03-15 11:41:00".to_string(),
            updated_at: None,
            namespace: "kemeter".to_string(),
            name: "".to_string(),
            data: r#"{"existing_key": "value"}"#.to_string(),
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
        assert!(matches!(create_mount_from_volume(volume, configs, "test-deployment".to_string()), Err(RuntimeError::ConfigKeyNotFound(_))));
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
