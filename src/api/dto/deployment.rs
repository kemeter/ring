use chrono::{NaiveDateTime};
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use crate::models::deployments::Deployment;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct DeploymentDTO {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) status: String,
    pub(crate) name: String,
    pub(crate) runtime: String,
    pub(crate) namespace: String,
    pub(crate) image: String,
    pub(crate) replicas: u32,
    pub(crate) ports: Vec<String>,
    pub(crate) labels: HashMap<String, String>,
    pub(crate) secrets: HashMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) volumes: Vec<DeploymentVolume>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) instances: Vec<String>
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct DeploymentOutput {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) status: String,
    pub(crate) name: String,
    pub(crate) runtime: String,
    pub(crate) namespace: String,
    pub(crate) image: String,
    pub(crate) replicas: u32,
    pub(crate) ports: Vec<String>,
    pub(crate) labels: HashMap<String, String>,
    pub(crate) instances: Vec<String>,
    pub(crate) secrets: HashMap<String, String>,
    pub(crate) volumes: Vec<DeploymentVolume>
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct DeploymentVolume {
    pub(crate) source: String,
    pub(crate) destination: String,
    pub(crate) driver: String,
    pub(crate) permission: String
}

pub(crate) fn hydrate_deployment_output(deployment: Deployment) -> DeploymentOutput {
    let labels: HashMap<String, String> = Deployment::deserialize_labels(&deployment.labels);
    let secrets: HashMap<String, String> = Deployment::deserialize_labels(&deployment.secrets);
    let volumes: Vec<DeploymentVolume> = serde_json::from_str(&deployment.volumes).unwrap();

    return DeploymentOutput {
        id: deployment.id,
        created_at: deployment.created_at,
        status: deployment.status,
        name: deployment.name,
        namespace: deployment.namespace,
        runtime: deployment.runtime,
        image: deployment.image,
        replicas: deployment.replicas,
        ports: [].to_vec(),
        labels: labels,
        secrets: secrets,
        volumes: volumes,
        instances: [].to_vec()
    };
}
