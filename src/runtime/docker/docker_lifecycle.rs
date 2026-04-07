use crate::models::deployments::Deployment;
use crate::models::volume::ResolvedMount;
use crate::runtime::lifecycle_trait::RuntimeLifecycle;
use async_trait::async_trait;
use bollard::Docker;

pub struct DockerLifecycle {
    docker: Docker,
}

impl DockerLifecycle {
    pub fn new(docker: Docker) -> Self {
        Self { docker }
    }
}

#[async_trait]
impl RuntimeLifecycle for DockerLifecycle {
    async fn apply(
        &self,
        deployment: Deployment,
        resolved_mounts: Vec<ResolvedMount>,
    ) -> Deployment {
        super::lifecycle::apply(deployment, self.docker.clone(), resolved_mounts).await
    }

    async fn list_instances(&self, deployment_id: String, status: &str) -> Vec<String> {
        super::instances::list_instances(&self.docker, deployment_id, status).await
    }

    async fn remove_instance(&self, instance_id: String) -> bool {
        super::container::remove_container_by_id(&self.docker, instance_id).await
    }
}
