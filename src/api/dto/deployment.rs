
use serde::{Serialize, Deserialize, Serializer};
use std::collections::HashMap;
use serde::ser::SerializeStruct;
use crate::models::deployments::{Deployment, DeploymentConfig};

fn serialize_option_deployment_config<S>(
    opt: &Option<DeploymentConfig>,
    serializer: S,
) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
{
    match opt {
        Some(config) => config.serialize(serializer),
        None => {
            let mut s = serializer.serialize_struct("DeploymentConfig", 0)?;
            s.end()
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct DeploymentDTO {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) status: String,
    pub(crate) name: String,
    pub(crate) runtime: String,
    pub(crate) kind: String,
    pub(crate) namespace: String,
    pub(crate) image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) config: Option<DeploymentConfig>,
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
    pub(crate) kind: String,
    pub(crate) namespace: String,
    pub(crate) image: String,
    #[serde(serialize_with = "serialize_option_deployment_config")]
    pub(crate) config: Option<DeploymentConfig>,
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
    let labels: HashMap<String, String> = Deployment::deserialize_labels(&deployment.labelsjson);
    let secrets: HashMap<String, String> = Deployment::deserialize_labels(&deployment.secretsjson);
    let volumes: Vec<DeploymentVolume> = serde_json::from_str(&deployment.volumes).unwrap();

    return DeploymentOutput {
        id: deployment.id,
        created_at: deployment.created_at,
        status: deployment.status,
        name: deployment.name,
        namespace: deployment.namespace,
        runtime: deployment.runtime,
        kind: deployment.kind,
        image: deployment.image,
        config: deployment.config,
        replicas: deployment.replicas,
        ports: [].to_vec(),
        labels: labels,
        secrets: secrets,
        volumes: volumes,
        instances: [].to_vec()
    };
}
