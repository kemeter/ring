use shiplift::{ContainerOptions, Docker, PullOptions, NetworkCreateOptions, ContainerConnectionOptions, RegistryAuth, LogsOptions};
use futures::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use crate::models::deployments::Deployment;
use std::convert::TryInto;
use crate::api::dto::deployment::DeploymentVolume;
use std::iter::FromIterator;
use shiplift::tty::TtyChunk;

struct DockerImage {
    name: String,
    tag: String,
    auth: Option<(String, String, String)>,
}

#[derive(Debug)]
pub enum DockerError {
    ImageNotFound(String),
    ImagePullFailed(String),
    ContainerCreationFailed(String),
    Other(String),
}

impl From<shiplift::Error> for DockerError {
    fn from(err: shiplift::Error) -> Self {
        let err_msg = err.to_string();
        if err_msg.contains("404") || err_msg.contains("not found") || err_msg.contains("manifest unknown") {
            DockerError::ImageNotFound(err_msg)
        } else {
            DockerError::Other(err_msg)
        }
    }
}

pub(crate) async fn apply(mut deployment: Deployment) -> Deployment {
    let docker = Docker::new();

    if deployment.restart_count >= 5 && deployment.status != "deleted" {
        deployment.status = "CrashLoopBackOff".to_string();
        return deployment;
    }

    deployment.instances = list_instances(deployment.id.to_string(), "running").await;

    if deployment.status == "CrashLoopBackOff" {
        return deployment;
    }

    if deployment.status == "deleted" {
        debug!("{} mark as delete. Remove all instance", deployment.id.to_string());
        for instance in deployment.instances.iter_mut() {
            remove_container(docker.clone(), instance.to_string()).await;
            info!("docker container {} delete", instance);
        }
    } else {
        // Calculate difference and act accordingly
        let current_count: usize = deployment.instances.len();
        let target_count: usize = deployment.replicas.try_into().unwrap();

        debug!("Current instances: {}, Target instances: {}", current_count, target_count);

        match current_count.cmp(&target_count) {
            std::cmp::Ordering::Less => {
                debug!("Scaling up: {} -> {} (creating 1 container)", current_count, target_count);

                // Attempt to create container with error handling
                match create_container(&mut deployment, &docker).await {
                    Ok(_) => {
                        // Container created successfully
                        if deployment.status == "pending" || deployment.status == "creating" {
                            deployment.status = "running".to_string();
                        }
                    }
                    Err(DockerError::ImageNotFound(msg)) => {
                        error!("Image not found for deployment {}: {}", deployment.id, msg);
                        deployment.status = "ImagePullBackOff".to_string();
                        deployment.restart_count += 1;
                    }
                    Err(DockerError::ImagePullFailed(msg)) => {
                        error!("Image pull failed for deployment {}: {}", deployment.id, msg);
                        deployment.status = "ImagePullBackOff".to_string();
                        deployment.restart_count += 1;
                    }
                    Err(DockerError::ContainerCreationFailed(msg)) => {
                        error!("docker dontainer creation failed for deployment {}: {}", deployment.id, msg);
                        deployment.status = "CreateContainerError".to_string();
                        deployment.restart_count += 1;
                    }
                    Err(DockerError::Other(msg)) => {
                        error!("Unknown error for deployment {}: {}", deployment.id, msg);
                        deployment.status = "Error".to_string();
                        deployment.restart_count += 1;
                    }
                }
            }
            std::cmp::Ordering::Greater => {
                debug!("Scaling down: {} -> {} (removing 1 container)", current_count, target_count);
                if let Some(container_id) = deployment.instances.first().cloned() {
                    remove_container(docker.clone(), container_id.clone()).await;
                    // Synchronize local state with deletion
                    deployment.instances.remove(0);
                    info!("Container {} removed from deployment {}", container_id, deployment.id);
                }
            }
            std::cmp::Ordering::Equal => {
                debug!("Replicas count matches target: {} instances", current_count);
            }
        }

        debug!("docker runtime apply {:?}", deployment.id.to_string());
    }

    return deployment;
}

async fn pull_image(docker: Docker, image_config: DockerImage) -> Result<(), DockerError> {
    let image = image_config.name.clone();
    let tag = image_config.tag.clone();
    info!("pull docker image: {}:{}", image.clone(), tag);

    // Check if image already exists locally
    match docker.images().get(image.clone()).inspect().await {
        Ok(_) => {
            debug!("Docker image {}:{} already exists locally", image, tag);
            return Ok(());
        }
        Err(_) => {
            debug!("Docker image {}:{} not found locally, pulling...", image, tag);
        }
    }

    let mut builder = PullOptions::builder();
    builder.image(image.clone()).tag(image_config.tag.clone());

    if image_config.auth.is_some() {
        let (server, username, password) = image_config.auth.unwrap();
        let auth = RegistryAuth::builder()
            .server_address(server)
            .username(username)
            .password(password)
            .build();

        builder.auth(auth);
    }

    let mut stream = docker
        .images()
        .pull(&builder.build());

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
                    return Err(DockerError::ImageNotFound(last_error));
                }
            }
        }
    }

    if has_error {
        return Err(DockerError::ImagePullFailed(last_error));
    }

    // Check one last time that the image is available after pull
    match docker.images().get(image.clone()).inspect().await {
        Ok(_) => {
            info!("Docker successfully pulled image {}:{}", image, tag);
            Ok(())
        }
        Err(e) => {
            error!("Docker image {}:{} still not available after pull: {}", image, tag, e);
            Err(DockerError::ImageNotFound(format!("Image {}:{} not available after pull", image, tag)))
        }
    }
}

async fn create_container<'a>(deployment: &mut Deployment, docker: &Docker) -> Result<(), DockerError> {
    debug!("create container for deployment id : {}", &deployment.id);
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

    // Always try to pull image if policy requires it 
    if let Some(config) = &deployment.config {
        if config.image_pull_policy == "Always" || config.image_pull_policy == "IfNotPresent" {
            pull_image(docker.clone(), image_config).await?;
        }
    } else {
        // Try to pull image by default
        pull_image(docker.clone(), image_config).await?;
    }

    let network_name = format!("ring_{}", deployment.namespace.clone());
    create_network(docker.clone(), network_name.clone()).await;

    let mut container_options = ContainerOptions::builder(deployment.image.as_str());
    let tiny_id = tiny_id();
    let container_name = format!("{}_{}_{}", &deployment.namespace, &deployment.name, tiny_id);

    container_options.name(&container_name);
    let mut labels = HashMap::new();

    labels.insert("ring_deployment", deployment.id.as_str());

    let labels_format = &deployment.labels;

    for (key, value) in labels_format.iter() {
        labels.insert(key, value);
    }

    let secrets_format = &deployment.secrets;

    let mut envs: Vec<String> = vec![];
    for (key, value) in secrets_format {
        envs.push(format!("{}={}", key, value))
    }

    container_options.labels(&labels);
    container_options.env(envs);

    let volumes_collection: Vec<DeploymentVolume> = serde_json::from_str(&deployment.volumes).unwrap();

    let mut volumes: Vec<String> = vec![];
    for volume in volumes_collection {
        let format: String = format!("{}:{}:{}", volume.source, volume.destination, volume.permission);
        volumes.push(format);
    }

    let v = Vec::from_iter(volumes.iter().map(String::as_str));
    container_options.volumes(v);

    match docker
        .containers()
        .create(&container_options.build())
        .await
    {
        Ok(container) => {
            debug!("Docker create container {:?}", container.id);
            deployment.instances.push(container.id.to_string());

            let networks = docker.networks();
            let mut builder = ContainerConnectionOptions::builder(&container.id);
            builder.aliases(vec![&deployment.name, &container_name]);

            networks
                .get(&network_name)
                .connect(&builder.build())
                .await
                .map_err(|e| DockerError::ContainerCreationFailed(format!("Docker failed to connect to network: {}", e)))?;

            docker.containers()
                .get(container.id)
                .start()
                .await
                .map_err(|e| DockerError::ContainerCreationFailed(format!("Docker failed to start container: {}", e)))?;

            info!("Docker container {} created and started successfully", container_name);
            Ok(())
        }
        Err(e) => {
            error!("Docker Failed to create container: {}", e);
            Err(DockerError::from(e))
        }
    }
}

async fn remove_container(docker: Docker, container_id: String) {
    match docker.containers().get(&container_id).stop(Some(Duration::from_millis(10))).await {
        Ok(_info) => {
            debug!("{:?}", _info);
        }
        Err(_e) => {
            debug!("{:?}", _e);
        }
    };

    info!("remove container: {}", &container_id);
}

async fn create_network(docker: Docker, network_name: String) {
    debug!("start Docker create network: {}", network_name);

    match docker.networks().get(&network_name).inspect().await {
        Ok(_network_info) => {
            debug!("Docker network {:?} already exist", network_name);
        }
        Err(e) => {
            info!("Docker create network: {}", network_name);

            match docker
                .networks()
                .create(
                    &NetworkCreateOptions::builder(network_name.as_ref())
                        .driver("bridge")
                        .build(),
                )
                .await
            {
                Ok(info) => debug!("{:?}", info),
                Err(_e) => debug!("Docker network create error: {}", e),
            }
        }
    }
}

pub(crate) async fn list_instances(id: String, status: &str) -> Vec<String> {
    let docker = Docker::new();
    let mut instances: Vec<String> = Vec::new();

    match docker.containers().list(&Default::default()).await {
        Ok(containers) => {
            for container in containers {
                if status == "all" || container.state == status {
                    let container_id = &container.id;

                    for (label, value) in container.labels.into_iter() {
                        if "ring_deployment" == label && value == id {
                            instances.push(container_id.to_string());
                        }
                    }
                }
            }
        }
        Err(e) => debug!("docker list instances error: {}", e),
    }

    return instances;
}

pub(crate) async fn logs(container_id: String) -> Vec<String> {
    let docker = Docker::new();

    let mut logs_stream = docker
        .containers()
        .get(&container_id)
        .logs(&LogsOptions::builder().stdout(true).stderr(true).build());

    let mut logs = vec![];

    while let Some(log_result) = logs_stream.next().await {
        match log_result {
            Ok(chunk) => {
                logs.push(print_chunk(chunk).replace("\n", ""))
            }
            Err(e) => debug!("Docker get logs errors: {}", e),
        }
    }

    return logs;
}

fn print_chunk(chunk: TtyChunk) -> String {
    match chunk {
        TtyChunk::StdOut(bytes) => format!("{}", std::str::from_utf8(&bytes).unwrap()),
        TtyChunk::StdErr(bytes) => format!("{}", std::str::from_utf8(&bytes).unwrap()),
        TtyChunk::StdIn(_) => unreachable!(),
    }
}

fn tiny_id() -> String {
    use rand::Rng;

    let mut rng = rand::rng();
    format!("{:08x}", rng.random::<u32>())
}
