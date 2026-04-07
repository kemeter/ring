use crate::models::deployments::Deployment;
use crate::models::volume::ResolvedMount;
use async_trait::async_trait;

#[async_trait]
pub trait RuntimeLifecycle: Send + Sync {
    /// Apply the desired state for a deployment (create/remove containers to match replicas).
    async fn apply(
        &self,
        deployment: Deployment,
        resolved_mounts: Vec<ResolvedMount>,
    ) -> Deployment;

    /// List active instance IDs for a deployment.
    async fn list_instances(&self, deployment_id: String, status: &str) -> Vec<String>;

    /// Remove a single instance by ID. Returns true if successful.
    async fn remove_instance(&self, instance_id: String) -> bool;
}
