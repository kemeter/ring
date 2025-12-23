
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
            let s = serializer.serialize_struct("DeploymentConfig", 0)?;
            s.end()
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct DeploymentOutput {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) status: String,
    pub(crate) restart_count: u32,
    pub(crate) name: String,
    pub(crate) runtime: String,
    pub(crate) kind: String,
    pub(crate) namespace: String,
    pub(crate) image: String,
    pub(crate) command: Vec<String>,
    #[serde(serialize_with = "serialize_option_deployment_config")]
    pub(crate) config: Option<DeploymentConfig>,
    pub(crate) replicas: u32,
    pub(crate) ports: Vec<String>,
    pub(crate) labels: HashMap<String, String>,
    pub(crate) instances: Vec<String>,
    pub(crate) secrets: HashMap<String, String>,
    pub(crate) volumes: Vec<DeploymentVolume>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) health_checks: Vec<crate::models::health_check::HealthCheck>
}

impl DeploymentOutput {
    pub(crate) fn from_to_model(deployment: Deployment) -> DeploymentOutput {
        let labels: HashMap<String, String> = deployment.labels;
        let secrets: HashMap<String, String> = deployment.secrets;

        let volumes: Vec<DeploymentVolume> = serde_json::from_str(&deployment.volumes)
            .unwrap_or_else(|e| {
                eprintln!("🚨 Erreur volumes : {} pour deployment {}", e, deployment.name);
                Vec::new()
            });


        return DeploymentOutput {
            id: deployment.id,
            created_at: deployment.created_at,
            updated_at: deployment.updated_at.unwrap_or("".to_string()),
            status: deployment.status,
            restart_count: deployment.restart_count,
            name: deployment.name,
            namespace: deployment.namespace,
            runtime: deployment.runtime,
            kind: deployment.kind,
            image: deployment.image,
            command: deployment.command,
            config: deployment.config,
            replicas: deployment.replicas,
            ports: [].to_vec(),
            labels: labels,
            secrets: secrets,
            volumes: volumes,
            instances: [].to_vec(),
            health_checks: deployment.health_checks
        };
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct DeploymentVolume {
    #[serde(default)]
    pub(crate) r#type: String,
    pub(crate) source: Option<String>,
    pub(crate) key: Option<String>,
    pub(crate) destination: String,
    pub(crate) driver: String,
    pub(crate) permission: String
}