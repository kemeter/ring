use crate::models::deployments::{
    Deployment, DeploymentConfig, DeploymentPort, EnvValue, NetworkConfig, Resource,
};
use crate::models::volume::{OnChange, ReloadSignal};
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
    /// Why the deployment is backing off or failed; null in any other status.
    #[serde(default)]
    pub(crate) failure: Option<FailureOutput>,
    pub(crate) restart_count: u32,
    pub(crate) name: String,
    pub(crate) runtime: String,
    pub(crate) kind: String,
    pub(crate) namespace: String,
    pub(crate) image: String,
    pub(crate) command: Vec<String>,
    #[serde(serialize_with = "serialize_option_deployment_config")]
    pub(crate) config: Option<DeploymentConfig>,
    /// What the manifest declared. On an autoscaled deployment this is the
    /// starting count, NOT the number of instances currently targeted — read
    /// `desired_replicas` for that.
    pub(crate) replicas: u32,
    /// Autoscaling policy, absent when the deployment holds a fixed count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) autoscale: Option<crate::models::deployments::Autoscale>,
    /// The count Ring is actually reconciling towards, i.e. what the runtimes
    /// use. Absent when the deployment is not autoscaled, in which case
    /// `replicas` is the target.
    ///
    /// This is the *effective* value, not the raw stored decision: a policy
    /// edited to a narrower range applies immediately, so reporting the stored
    /// number would show a target the runtimes are not aiming at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) desired_replicas: Option<u32>,
    /// The manifest's `restart` block, absent when it has none.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) restart: Option<crate::models::deployments::RestartSpec>,
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
        // Computed before the struct is taken apart below: `target_replicas()`
        // needs the whole deployment, and reports the effective count rather
        // than the raw stored decision.
        let effective_target = deployment
            .autoscale
            .as_ref()
            .map(|_| deployment.target_replicas());

        let labels: HashMap<String, String> = deployment.labels;
        let environment: HashMap<String, EnvValue> = deployment.environment;

        let volumes: Vec<DeploymentVolume> = serde_json::from_str(&deployment.volumes)
            .unwrap_or_else(|e| {
                warn!(
                    "Failed to parse volumes for deployment {}: {}",
                    deployment.name, e
                );
                Vec::new()
            });

        DeploymentOutput {
            id: deployment.id,
            created_at: deployment.created_at,
            updated_at: deployment.updated_at.unwrap_or("".to_string()),
            status: deployment.status.to_string(),
            failure: deployment.reason.map(|reason| FailureOutput {
                reason: reason.to_string(),
                message: deployment.message.unwrap_or_default(),
            }),
            restart_count: deployment.restart_count,
            name: deployment.name,
            namespace: deployment.namespace,
            runtime: deployment.runtime,
            kind: deployment.kind,
            image: deployment.image,
            command: deployment.command,
            config: deployment.config,
            replicas: deployment.replicas,
            desired_replicas: effective_target,
            autoscale: deployment.autoscale,
            restart: deployment.restart,
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

/// Why a deployment is backing off or failed: a stable `reason` to branch on
/// and a human-readable `message`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct FailureOutput {
    pub(crate) reason: String,
    pub(crate) message: String,
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
    #[serde(default, skip_serializing_if = "OnChange::is_none")]
    pub(crate) on_change: OnChange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reload_signal: Option<ReloadSignal>,
}

#[cfg(test)]
mod tests {
    use super::DeploymentOutput;
    use serde_json::Value;

    /// A `GET /deployments` response recorded from a v0.12.0 server.
    const V0_12_0_RESPONSE: &str = include_str!("../../fixtures/deployments_v0_12_0.json");

    #[test]
    fn the_cli_reads_the_response_of_a_v0_12_0_server() {
        let deployments: Vec<DeploymentOutput> = serde_json::from_str(V0_12_0_RESPONSE).unwrap();

        assert_eq!(deployments.len(), 2);
        assert!(deployments.iter().all(|d| d.failure.is_none()));
    }

    #[test]
    fn a_v0_12_0_cli_still_finds_every_field_it_knows() {
        let recorded: Vec<Value> = serde_json::from_str(V0_12_0_RESPONSE).unwrap();
        let parsed: Vec<DeploymentOutput> = serde_json::from_str(V0_12_0_RESPONSE).unwrap();
        let served = serde_json::to_value(&parsed).unwrap();

        for (recorded, served) in recorded.iter().zip(served.as_array().unwrap()) {
            for field in recorded.as_object().unwrap().keys() {
                assert!(served.get(field).is_some(), "`{field}` is no longer served");
            }
        }
    }
}
