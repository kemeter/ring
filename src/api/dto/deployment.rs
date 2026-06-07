use crate::models::deployments::{
    Deployment, DeploymentConfig, DeploymentPort, EnvValue, NetworkConfig, Resource,
};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::HashMap;

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
    pub(crate) ports: Vec<DeploymentPort>,
    pub(crate) labels: HashMap<String, String>,
    /// Running instances of this deployment. Each carries its id and — when it
    /// has a reachable network — its routable guest address, so consumers
    /// (service-discovery providers, proxies) can route to a specific instance.
    /// Mirrors the Nomad/Consul "service instance = address" model. No display
    /// name: only Docker has a distinct container name; VM runtimes would just
    /// echo the id, so a name field would carry no extra information.
    pub(crate) instances: Vec<DeploymentInstance>,
    pub(crate) environment: HashMap<String, EnvValue>,
    pub(crate) volumes: Vec<DeploymentVolume>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) health_checks: Vec<crate::models::health_check::HealthCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resources: Option<Resource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) image_digest: Option<String>,
    /// Set during a rolling update: points to the deployment row this
    /// one supersedes. The previous deployment stays `running` until the
    /// new one is healthy, then the scheduler tears it down.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) network: Option<NetworkConfig>,
}

impl DeploymentOutput {
    pub(crate) fn from_to_model(deployment: Deployment) -> DeploymentOutput {
        let labels: HashMap<String, String> = deployment.labels;
        let environment: HashMap<String, EnvValue> = deployment.environment;

        let volumes: Vec<DeploymentVolume> = serde_json::from_str(&deployment.volumes)
            .unwrap_or_else(|e| {
                log::warn!(
                    "Failed to parse volumes for deployment {}: {}",
                    deployment.name,
                    e
                );
                Vec::new()
            });

        DeploymentOutput {
            id: deployment.id,
            created_at: deployment.created_at,
            updated_at: deployment.updated_at.unwrap_or("".to_string()),
            status: deployment.status.to_string(),
            restart_count: deployment.restart_count,
            name: deployment.name,
            namespace: deployment.namespace,
            runtime: deployment.runtime,
            kind: deployment.kind,
            image: deployment.image,
            command: deployment.command,
            config: deployment.config,
            replicas: deployment.replicas,
            ports: deployment.ports,
            labels,
            environment,
            volumes,
            instances: [].to_vec(),
            health_checks: deployment.health_checks,
            resources: deployment.resources,
            image_digest: deployment.image_digest,
            parent_id: deployment.parent_id,
            network: deployment.network,
        }
    }
}

/// One running instance of a deployment. `address` is the routable guest IP
/// (present for VM runtimes and Docker containers that joined a network);
/// `None` when the instance has no reachable address (e.g. no published ports).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct DeploymentInstance {
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) address: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct DeploymentVolume {
    #[serde(default)]
    pub(crate) r#type: String,
    pub(crate) source: Option<String>,
    pub(crate) key: Option<String>,
    pub(crate) destination: String,
    pub(crate) driver: String,
    pub(crate) permission: String,
}
