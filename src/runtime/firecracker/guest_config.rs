//! The guest config disk of a microVM booted from a squashfs: its environment
//! and volume mounts, applied by `ring-init` in place of cloud-init (see the
//! `guest_config` module of the `ring-init` crate for the format).

use serde::Serialize;

use crate::hypervisor::cloud_init::{GuestMount, MountTransport};
use crate::models::deployments::{Deployment, EnvValue};

#[derive(Serialize)]
struct GuestConfig<'a> {
    env: Vec<(&'a str, &'a str)>,
    mounts: Vec<Mount<'a>>,
}

#[derive(Serialize)]
struct Mount<'a> {
    device: &'a str,
    destination: &'a str,
    read_only: bool,
}

/// The disk content: JSON padded with zero bytes to whole 512-byte sectors.
pub(crate) fn disk_content(
    deployment: &Deployment,
    mounts: &[GuestMount],
) -> Result<Vec<u8>, String> {
    let mut env = Vec::with_capacity(deployment.environment.len());
    for (key, value) in &deployment.environment {
        match value {
            EnvValue::Plain(v) => env.push((key.as_str(), v.as_str())),
            // Secrets are resolved by the scheduler before a runtime sees the
            // deployment; one left here would otherwise vanish silently.
            EnvValue::SecretRef { .. } => {
                return Err(format!(
                    "unresolved secretRef for '{}' reached the VM runtime",
                    key
                ));
            }
        }
    }
    // A stable order, so the same deployment always produces the same disk.
    env.sort();

    let mounts = mounts
        .iter()
        .filter(|m| matches!(m.transport, MountTransport::Block))
        .map(|m| Mount {
            device: &m.source,
            destination: &m.destination,
            read_only: m.read_only,
        })
        .collect();

    let mut content = serde_json::to_vec(&GuestConfig { env, mounts })
        .map_err(|e| format!("cannot encode the guest config: {}", e))?;
    content.resize(content.len().div_ceil(512).max(1) * 512, 0);
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn deployment(env: &[(&str, EnvValue)]) -> Deployment {
        let mut deployment: Deployment = serde_json::from_value(serde_json::json!({
            "id": "dep-1", "created_at": "now", "status": "running",
            "namespace": "ns", "name": "app", "image": "/images/app.squashfs",
            "runtime": "firecracker", "kind": "worker", "replicas": 1,
            "command": [], "instances": [], "labels": {}, "environment": {},
            "volumes": "[]", "restart_count": 0
        }))
        .unwrap_or_else(|e| panic!("fixture: {e}"));
        deployment.environment = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect::<HashMap<_, _>>();
        deployment
    }

    #[test]
    fn the_disk_holds_the_env_and_block_mounts_padded_to_sectors() {
        let deployment = deployment(&[
            ("B", EnvValue::Plain("2".to_string())),
            ("A", EnvValue::Plain("it's".to_string())),
        ]);
        let mounts = vec![GuestMount::block(
            "/dev/vdc".to_string(),
            "/data".to_string(),
            true,
        )];

        let disk = disk_content(&deployment, &mounts).unwrap();
        assert_eq!(disk.len() % 512, 0);
        let end = disk.iter().position(|&b| b == 0).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&disk[..end]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "env": [["A", "it's"], ["B", "2"]],
                "mounts": [{"device": "/dev/vdc", "destination": "/data", "read_only": true}]
            })
        );
    }

    #[test]
    fn an_unresolved_secret_is_refused() {
        let deployment = deployment(&[(
            "TOKEN",
            EnvValue::SecretRef {
                secret_ref: "api-token".to_string(),
            },
        )]);
        let err = disk_content(&deployment, &[]).unwrap_err();
        assert!(err.contains("TOKEN"), "{err}");
    }
}
