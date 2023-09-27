use shiplift::{ContainerOptions, Docker, PullOptions, NetworkCreateOptions, ContainerConnectionOptions, RegistryAuth};
use futures::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use crate::models::deployments::Deployment;
use uuid::Uuid;
use std::convert::TryInto;
use crate::api::dto::deployment::DeploymentVolume;
use std::iter::FromIterator;

struct DockerImage {
    name: String,
    tag: String,
    auth: Option<(String, String)>,
}

pub(crate) async fn apply(mut config: Deployment) -> Deployment {
    let docker = Docker::new();

    info!("docker runtime search");

    config.instances = list_instances(config.id.to_string()).await;

    if config.status == "deleted" {
        debug!("{} mark as delete. Remove all instance", config.id.to_string());
        for instance in config.instances.iter_mut() {
            remove_container(docker.clone(), instance.to_string()).await;

            info!("container {} delete", instance);
        }
    } else {
        let number_instances: usize = config.instances.len();

        if number_instances < config.replicas.try_into().unwrap() {
            info!("create container {}", config.image.clone());

            create_container(&mut config, &docker).await
        }

        if number_instances > config.replicas.try_into().unwrap() {
            let first_container_id = &config.instances[0];

            remove_container(docker.clone(), first_container_id.to_string()).await;
        }

        debug!("docker runtime apply {:?}", config);
    }

    return config;
}

async fn pull_image(docker: Docker, image_config: DockerImage) {
    let image = image_config.name.clone();
    info!("pull docker image: {}", image.clone());

    match docker.images().get(image.clone()).inspect().await {
        Ok(_) => { },
        Err(_) => {
            let mut builder = PullOptions::builder();
            builder.image(image).tag(image_config.tag.clone());

            if image_config.auth.is_some() {
                let (username, password) = image_config.auth.unwrap();
                let auth = RegistryAuth::builder()
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
                    Err(e) => eprintln!("Error: {}", e),
                }
            }
        },
    }
}

async fn create_container<'a>(deployment: &mut Deployment, docker: &Docker) {
    debug!("create container for : {}", &deployment.id);
    let image_name = deployment.image.clone();
    let split: Vec<&str> = image_name.split(':').collect();
    let image = split[0];
    let tag = split[1];

    let image = match &deployment.config {
        Some(config) => DockerImage {
            name: image.parse().unwrap(),
            tag: tag.parse().unwrap(),
            auth: Some((config.username.clone(), config.password.clone())),
        },
        None => DockerImage {
            name: image.parse().unwrap(),
            tag: tag.parse().unwrap(),
            auth: None,
        },
    };
    pull_image(docker.clone(), image).await;

    let network_name = format!("ring_{}", deployment.namespace.clone());
    create_network(docker.clone(), network_name.clone()).await;

    let mut container_options = ContainerOptions::builder(deployment.image.as_str());
    let tiny_id = tiny_id();
    let container_name = format!("{}_{}_{}", &deployment.namespace, &deployment.name, tiny_id);

    container_options.name(&container_name);
    let mut labels = HashMap::new();

    labels.insert("ring_deployment", deployment.id.as_str());

    //let labels_format = Deployment::deserialize_labels(&deployment.labels);
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
        Err(e) => eprintln!("Error: {}", e),
    }
}

async fn remove_container(docker: Docker, container_id: String) {
    match docker.containers().get(&container_id).stop(Some(Duration::from_millis(10))).await {
        Ok(_info) => {
            println!("{:?}", _info);
        },
        Err(_e) => {
            println!("{:?}", _e);
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
                Ok(info) => println!("{:?}", info),
                Err(_e) => eprintln!("Error: {}", e),
            }
        },
    }
}

pub(crate) async fn list_instances(id: String) -> Vec<std::string::String> {
    let docker = Docker::new();
    let mut instances: Vec<String> = Vec::new();

    match docker.containers().list(&Default::default()).await {
        Ok(containers) => {
            for container in containers {
                if container.state == "running" {
                    let container_id = &container.id;

                    for (label, value) in container.labels.into_iter() {
                        if "ring_deployment" == label && value == id {
                            instances.push(container_id.to_string());
                        }
                    }
                }
            }
        }
        Err(e) => eprintln!("Error: {}", e),
    }

    return instances;
}

fn tiny_id()-> String {
    let id = Uuid::new_v4().to_string();

    let (_, name) = id.rsplit_once('-').unwrap();

    return String::from(name);
}