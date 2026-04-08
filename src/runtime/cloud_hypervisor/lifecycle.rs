use super::client::{
    CloudHypervisorClient, ConsoleConfig, CpuConfig, DiskConfig, MemoryConfig, NetConfig,
    PayloadConfig, VmConfig,
};
use crate::models::deployments::{Deployment, DeploymentStatus};
use crate::models::volume::ResolvedMount;
use crate::runtime::lifecycle_trait::RuntimeLifecycle;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

/// Configuration for the Cloud Hypervisor runtime.
pub(crate) struct CloudHypervisorConfig {
    /// Path to the cloud-hypervisor binary.
    pub binary_path: String,
    /// Path to the Linux kernel (vmlinux).
    pub kernel_path: String,
    /// Directory for VM API sockets.
    pub socket_dir: String,
    /// Directory for VM rootfs images.
    pub rootfs_dir: String,
}

impl Default for CloudHypervisorConfig {
    fn default() -> Self {
        Self {
            binary_path: "cloud-hypervisor".to_string(),
            kernel_path: "/opt/ring/vmlinux".to_string(),
            socket_dir: "/var/run/ring/ch".to_string(),
            rootfs_dir: "/var/lib/ring/rootfs".to_string(),
        }
    }
}

pub struct CloudHypervisorLifecycle {
    config: CloudHypervisorConfig,
    /// Track running VMs: deployment_id -> list of (instance_id, socket_path)
    instances: tokio::sync::RwLock<HashMap<String, Vec<String>>>,
}

impl CloudHypervisorLifecycle {
    pub fn new(config: CloudHypervisorConfig) -> Self {
        Self {
            config,
            instances: tokio::sync::RwLock::new(HashMap::new()),
        }
    }

    fn socket_path(&self, instance_id: &str) -> PathBuf {
        PathBuf::from(&self.config.socket_dir).join(format!("{}.sock", instance_id))
    }

    fn rootfs_path(&self, image: &str) -> PathBuf {
        let safe_name = image.replace(['/', ':'], "_");
        PathBuf::from(&self.config.rootfs_dir).join(format!("{}.img", safe_name))
    }

    /// Start the cloud-hypervisor process for a VM instance.
    async fn start_vm_process(
        &self,
        instance_id: &str,
        deployment: &Deployment,
        _resolved_mounts: &[ResolvedMount],
    ) -> Result<(), String> {
        let socket = self.socket_path(instance_id);

        // Ensure directories exist
        tokio::fs::create_dir_all(&self.config.socket_dir)
            .await
            .map_err(|e| format!("Failed to create socket dir: {}", e))?;

        // Convert Docker image to rootfs if not cached
        let rootfs = super::image::ensure_rootfs(&deployment.image, &self.config.rootfs_dir)
            .await
            .map_err(|e| format!("Failed to prepare rootfs for image '{}': {}", deployment.image, e))?;

        // Parse resource limits
        let (vcpus, memory_mb) = parse_resources(deployment);

        // Start cloud-hypervisor process
        let _child = Command::new(&self.config.binary_path)
            .arg("--api-socket")
            .arg(socket.to_str().unwrap_or_default())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start cloud-hypervisor: {}", e))?;

        // Wait for the socket to be available
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        if !socket.exists() {
            return Err("cloud-hypervisor socket not available after 5s".to_string());
        }

        let client =
            CloudHypervisorClient::new(socket.to_str().unwrap_or_default());

        // Set up networking
        let net_config = super::network::setup_network(instance_id, &deployment.namespace)
            .await
            .map_err(|e| format!("Failed to setup network: {}", e))?;

        // Create VM configuration
        let vm_config = VmConfig {
            payload: PayloadConfig {
                kernel: Some(self.config.kernel_path.clone()),
                cmdline: Some("console=ttyS0 root=/dev/vda rw".to_string()),
                firmware: None,
                initramfs: None,
            },
            cpus: Some(CpuConfig {
                boot_vcpus: vcpus,
                max_vcpus: vcpus,
            }),
            memory: Some(MemoryConfig {
                size: (memory_mb as u64) * 1024 * 1024,
            }),
            disks: Some(vec![DiskConfig {
                path: rootfs.to_str().unwrap_or_default().to_string(),
                readonly: Some(false),
            }]),
            net: Some(vec![NetConfig {
                tap: Some(net_config.tap_name),
                ip: None,
                mask: None,
                mac: None,
            }]),
            serial: Some(ConsoleConfig {
                mode: "tty".to_string(),
            }),
            console: Some(ConsoleConfig {
                mode: "off".to_string(),
            }),
        };

        // Create and boot the VM
        client
            .create_vm(&vm_config)
            .await
            .map_err(|e| format!("Failed to create VM: {}", e))?;

        client
            .boot_vm()
            .await
            .map_err(|e| format!("Failed to boot VM: {}", e))?;

        info!(
            "Cloud Hypervisor VM {} started for deployment {}",
            instance_id, deployment.id
        );

        Ok(())
    }

    /// Stop and remove a VM instance.
    async fn stop_vm(&self, instance_id: &str) -> bool {
        let socket = self.socket_path(instance_id);
        let socket_str = socket.to_str().unwrap_or_default();

        if !socket.exists() {
            debug!("Socket {} does not exist, VM already stopped", socket_str);
            return true;
        }

        let client = CloudHypervisorClient::new(socket_str);

        // Shutdown the VM
        if let Err(e) = client.shutdown_vm().await {
            warn!("Failed to shutdown VM {}: {}", instance_id, e);
        }

        // Delete the VM
        if let Err(e) = client.delete_vm().await {
            warn!("Failed to delete VM {}: {}", instance_id, e);
        }

        // Clean up the socket file
        if let Err(e) = tokio::fs::remove_file(&socket).await {
            debug!("Failed to remove socket {}: {}", socket_str, e);
        }

        // Tear down networking
        super::network::teardown_network(instance_id).await;

        info!("Cloud Hypervisor VM {} stopped", instance_id);
        true
    }
}

fn parse_resources(deployment: &Deployment) -> (u32, u32) {
    let mut vcpus = 1u32;
    let mut memory_mb = 256u32;

    if let Some(ref resources) = deployment.resources {
        if let Some(ref limits) = resources.limits {
            if let Some(ref cpu) = limits.cpu {
                if let Ok(nano) = crate::models::deployments::parse_cpu_string(cpu) {
                    vcpus = std::cmp::max(1, (nano / 1_000_000_000) as u32);
                }
            }
            if let Some(ref mem) = limits.memory {
                if let Ok(bytes) = crate::models::deployments::parse_memory_string(mem) {
                    memory_mb = std::cmp::max(128, (bytes / (1024 * 1024)) as u32);
                }
            }
        }
    }

    (vcpus, memory_mb)
}

#[async_trait]
impl RuntimeLifecycle for CloudHypervisorLifecycle {
    async fn apply(
        &self,
        mut deployment: Deployment,
        resolved_mounts: Vec<ResolvedMount>,
    ) -> Deployment {
        if deployment.status == DeploymentStatus::Deleted {
            // Stop all instances
            let instances = {
                let map = self.instances.read().await;
                map.get(&deployment.id).cloned().unwrap_or_default()
            };
            for instance_id in &instances {
                self.stop_vm(instance_id).await;
            }
            self.instances.write().await.remove(&deployment.id);
            deployment.instances.clear();
            return deployment;
        }

        let current_count = deployment.instances.len();
        let target_count = deployment.replicas as usize;

        if current_count < target_count {
            // Scale up: create one VM per cycle
            let instance_id = format!(
                "ch-{}-{}",
                &deployment.id[..8.min(deployment.id.len())],
                crate::runtime::docker::tiny_id()
            );

            match self
                .start_vm_process(&instance_id, &deployment, &resolved_mounts)
                .await
            {
                Ok(()) => {
                    deployment.instances.push(instance_id.clone());
                    self.instances
                        .write()
                        .await
                        .entry(deployment.id.clone())
                        .or_default()
                        .push(instance_id);

                    if deployment.status == DeploymentStatus::Creating
                        || deployment.status == DeploymentStatus::Pending
                    {
                        deployment.status = DeploymentStatus::Running;
                    }
                }
                Err(e) => {
                    error!(
                        "Failed to start Cloud Hypervisor VM for deployment {}: {}",
                        deployment.id, e
                    );
                    deployment.restart_count += 1;
                    deployment.status = DeploymentStatus::Failed;
                }
            }
        } else if current_count > target_count {
            // Scale down: remove one VM
            if let Some(instance_id) = deployment.instances.first().cloned() {
                self.stop_vm(&instance_id).await;
                deployment.instances.remove(0);
                if let Some(list) = self.instances.write().await.get_mut(&deployment.id) {
                    list.retain(|id| id != &instance_id);
                }
            }
        }

        deployment
    }

    async fn list_instances(&self, deployment_id: String, _status: &str) -> Vec<String> {
        let map = self.instances.read().await;
        map.get(&deployment_id).cloned().unwrap_or_default()
    }

    async fn remove_instance(&self, instance_id: String) -> bool {
        self.stop_vm(&instance_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_resources_defaults() {
        let deployment = Deployment {
            id: "test".to_string(),
            created_at: String::new(),
            updated_at: None,
            status: DeploymentStatus::Creating,
            restart_count: 0,
            namespace: "default".to_string(),
            name: "test".to_string(),
            image: "nginx:latest".to_string(),
            config: None,
            runtime: "cloud-hypervisor".to_string(),
            kind: "worker".to_string(),
            replicas: 1,
            command: vec![],
            instances: vec![],
            labels: std::collections::HashMap::new(),
            environment: std::collections::HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: vec![],
            resources: None,
            image_digest: None,
            pending_events: vec![],
            parent_id: None,
        };

        let (vcpus, memory_mb) = parse_resources(&deployment);
        assert_eq!(vcpus, 1);
        assert_eq!(memory_mb, 256);
    }

    #[test]
    fn parse_resources_with_limits() {
        let deployment = Deployment {
            id: "test".to_string(),
            created_at: String::new(),
            updated_at: None,
            status: DeploymentStatus::Creating,
            restart_count: 0,
            namespace: "default".to_string(),
            name: "test".to_string(),
            image: "nginx:latest".to_string(),
            config: None,
            runtime: "cloud-hypervisor".to_string(),
            kind: "worker".to_string(),
            replicas: 1,
            command: vec![],
            instances: vec![],
            labels: std::collections::HashMap::new(),
            environment: std::collections::HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: vec![],
            resources: Some(crate::models::deployments::Resource {
                limits: Some(crate::models::deployments::ResourceSpec {
                    cpu: Some("2".to_string()),
                    memory: Some("512Mi".to_string()),
                }),
                requests: None,
            }),
            image_digest: None,
            pending_events: vec![],
            parent_id: None,
        };

        let (vcpus, memory_mb) = parse_resources(&deployment);
        assert_eq!(vcpus, 2);
        assert_eq!(memory_mb, 512);
    }
}
