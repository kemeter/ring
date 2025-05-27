use shiplift::{ContainerOptions, Docker, PullOptions, NetworkCreateOptions, ContainerConnectionOptions, RegistryAuth, LogsOptions};
use futures::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use crate::models::deployments::Deployment;
use uuid::Uuid;
use std::convert::TryInto;
use crate::api::dto::deployment::DeploymentVolume;
use std::iter::FromIterator;
use shiplift::tty::TtyChunk;

struct DockerImage {
    name: String,
    tag: String,
    auth: Option<(String, String, String)>,
}

pub(crate) async fn apply(mut config: Deployment) -> Deployment {
    let docker = Docker::new();

    if config.restart_count >= 5 && config.status != "deleted" {
        config.status = "CrashLoopBackOff".to_string();
        return config;
    }

    config.instances = list_instances(config.id.to_string(), "running").await;

    if config.status == "CrashLoopBackOff" {
        return config;
    }

    if config.status == "deleted" {
        debug!("{} mark as delete. Remove all instance", config.id.to_string());
        for instance in config.instances.iter_mut() {
            remove_container(docker.clone(), instance.to_string()).await;

            info!("container {} delete", instance);
        }
    } else {
        let number_instances: usize = config.instances.len();
        let replicas_expected: usize = config.replicas.try_into().unwrap();

        if number_instances < replicas_expected {
            debug!("Starting creating container process {}", config.image.clone());

            create_container(&mut config, &docker).await
        }

        if number_instances > replicas_expected {
            let first_container_id = &config.instances[0];
            info!("remove container {}", first_container_id.clone());

            remove_container(docker.clone(), first_container_id.to_string()).await;
        }

        debug!("docker runtime apply {:?}", config.id.to_string());
    }

    return config;
}

async fn pull_image(docker: Docker, image_config: DockerImage) {
    let image = image_config.name.clone();
    let tag = image_config.tag.clone();
    info!("pull docker image: {}:{}", image.clone(), tag);

    match docker.images().get(image.clone()).inspect().await {
        Ok(_) => { },
        Err(_) => {
            let mut builder = PullOptions::builder();
            builder.image(image).tag(image_config.tag.clone());

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

            while let Some(pull_result) = stream.next().await {
                match pull_result {
                    Ok(_output) => { },
                    Err(e) => error!("Docker image pull error : {}", e),
                }
            }
        },
    }
}

async fn create_container<'a>(deployment: &mut Deployment, docker: &Docker) {
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
        Some(config) =>  {
            match (&config.server, &config.username, &config.password) {
                (Some(server), Some(username), Some(password)) => {
                    image_config.auth = Some((server.clone(), username.clone(), password.clone()));
                },
                _ => {}
            }

            image_config
        },
        None =>  {
            image_config
        },
    };

    if let Some(config) = &deployment.config {
        if config.image_pull_policy == "Always" || config.image_pull_policy == "IfNotPresent" {
            pull_image(docker.clone(), image_config).await;
        }
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
            debug!("create container {:?}", container.id);
            deployment.instances.push(container.id.to_string());

            let networks = docker.networks();
            let mut builder = ContainerConnectionOptions::builder(&container.id);
            builder.aliases(vec![&deployment.name, &container_name]);

            networks
                .get(&network_name)
                .connect(&builder.build())
                .await.expect("Cannot create network");

            let _ = docker.containers().get(container.id).start().await;
        },
        Err(e) => {
            if deployment.status == "pending" || deployment.status == "creating" {
                deployment.restart_count += 1;
            }
            eprintln!("Error: {}", e)
        },
    }
}

async fn remove_container(docker: Docker, container_id: String) {
    match docker.containers().get(&container_id).stop(Some(Duration::from_millis(10))).await {
        Ok(_info) => {
            debug!("{:?}", _info);
        },
        Err(_e) => {
            debug!("{:?}", _e);
        },
    };

    info!("remove container: {}", &container_id);
}

async fn create_network(docker: Docker, network_name: String) {

    debug!("create network: {}", network_name);

    match docker.networks().get(&network_name).inspect().await {
        Ok(_network_info) => {
            debug!("network {:?} already exist", network_name);
        },
        Err(e) => {
            info!("create network: {}", network_name);

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
                Err(_e) => debug!("Error: {}", e),
            }
        },
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
        Err(e) => debug!("Error: {}", e),
    }

    return instances;
}

pub(crate) async fn logs(deployment: String) -> Vec<String> {
    let docker = Docker::new();

    let mut logs_stream = docker
        .containers()
        .get(&deployment)
        .logs(&LogsOptions::builder().stdout(true).stderr(true).build());

    let mut logs = vec![];

    while let Some(log_result) = logs_stream.next().await {
        match log_result {
            Ok(chunk) => {
                logs.push(print_chunk(chunk).replace("\n", ""))
            },
            Err(e) => debug!("Error: {}", e),
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

fn tiny_id()-> String {
    let id = Uuid::new_v4().to_string();

    let (_, name) = id.rsplit_once('-').unwrap();

    return String::from(name);
}