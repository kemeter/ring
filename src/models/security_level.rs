//! What a deployment may ask of the host, set per namespace.
//!
//! The three levels follow Kubernetes' Pod Security Standards:
//!
//! - `privileged` allows everything, the host included;
//! - `baseline` refuses what reaches the host: a privileged container, the
//!   host's network, and a bind mount of a host path;
//! - `restricted` also requires a container to run as a user other than root.
//!
//! A namespace without a level of its own gets the server's default,
//! `[server.admission] default`. `[server.admission] allowed_host_paths` lets
//! `baseline` and `restricted` bind a host path under one of its prefixes,
//! for a platform that mounts storage it manages into its tenants.
//!
//! The level is checked when a deployment is submitted and again before the
//! scheduler starts an instance, so lowering a namespace's level also stops
//! its deployments from starting new instances that it no longer allows.

use crate::models::deployments::{Deployment, NetworkMode};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path};
use std::str::FromStr;

#[derive(
    Serialize, Deserialize, sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default,
)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub(crate) enum SecurityLevel {
    Privileged,
    #[default]
    Baseline,
    Restricted,
}

impl SecurityLevel {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Privileged => "privileged",
            Self::Baseline => "baseline",
            Self::Restricted => "restricted",
        }
    }
}

impl fmt::Display for SecurityLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SecurityLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "privileged" => Ok(Self::Privileged),
            "baseline" => Ok(Self::Baseline),
            "restricted" => Ok(Self::Restricted),
            other => Err(format!(
                "unknown security level '{other}' (expected privileged, baseline or restricted)"
            )),
        }
    }
}

/// A host directory that `baseline` and `restricted` let a bind volume mount,
/// with everything below it. Absolute and without `.` or `..`, and never `/`,
/// which would allow every path: that is `privileged`.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(try_from = "String")]
pub(crate) struct HostPathPrefix(String);

impl TryFrom<String> for HostPathPrefix {
    type Error = String;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        if !is_plain_absolute(&path) {
            return Err(format!(
                "allowed host path '{path}' must be absolute, without '.' or '..'"
            ));
        }
        if Path::new(&path).parent().is_none() {
            return Err(
                "allowed host path '/' would allow every path; set the namespace to privileged instead"
                    .to_string(),
            );
        }
        Ok(Self(path))
    }
}

impl HostPathPrefix {
    /// Whether `source` is this directory or lies below it. Compared component
    /// by component, so `/srv/data` does not hold `/srv/database`, and a source
    /// with `..` is never held, as it could climb out.
    fn holds(&self, source: &str) -> bool {
        is_plain_absolute(source) && Path::new(source).starts_with(&self.0)
    }
}

/// An absolute path with no `.` or `..` component.
fn is_plain_absolute(path: &str) -> bool {
    path.starts_with('/')
        && !path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

/// One thing a deployment asks for that its namespace's level does not allow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Breach {
    /// The manifest field at fault, e.g. `volumes[0].type`.
    pub(crate) field: String,
    /// A stable code to branch on.
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

/// The runtimes that run the workload as a process on the host. The VM
/// runtimes boot their own kernel, so the user inside the guest says nothing
/// about the host.
fn runs_on_the_host_kernel(runtime: &str) -> bool {
    matches!(runtime, "docker" | "podman" | "containerd")
}

/// Everything in `deployment` that `level` does not allow, empty when it is
/// admitted. A bind volume whose source lies under one of `allowed_host_paths`
/// is allowed at every level.
pub(crate) fn breaches(
    deployment: &Deployment,
    level: SecurityLevel,
    allowed_host_paths: &[HostPathPrefix],
) -> Vec<Breach> {
    let mut breaches = Vec::new();
    if level == SecurityLevel::Privileged {
        return breaches;
    }

    let user = deployment.config.as_ref().and_then(|c| c.user.as_ref());

    if user.and_then(|u| u.privileged) == Some(true) {
        breaches.push(Breach {
            field: "config.user.privileged".to_string(),
            code: "deployment.config.user.privileged.forbidden",
            message: format!(
                "a privileged container has full access to the host, which the {level} level of namespace '{}' does not allow",
                deployment.namespace
            ),
        });
    }

    if deployment
        .network
        .as_ref()
        .is_some_and(|n| n.mode == NetworkMode::Host)
    {
        breaches.push(Breach {
            field: "network.mode".to_string(),
            code: "deployment.network.mode.forbidden",
            message: format!(
                "host networking shares the host's network stack, which the {level} level of namespace '{}' does not allow",
                deployment.namespace
            ),
        });
    }

    // Stored as JSON. A row that does not parse is refused by the volume
    // resolution later on; there is nothing to admit or refuse here.
    let volumes: Vec<serde_json::Value> =
        serde_json::from_str(&deployment.volumes).unwrap_or_default();
    for (index, volume) in volumes.iter().enumerate() {
        if volume.get("type").and_then(|t| t.as_str()) != Some("bind") {
            continue;
        }
        let source = volume.get("source").and_then(|s| s.as_str()).unwrap_or("");
        if allowed_host_paths.iter().any(|prefix| prefix.holds(source)) {
            continue;
        }
        breaches.push(Breach {
            field: format!("volumes[{index}].type"),
            code: "deployment.volumes.bind.forbidden",
            message: format!(
                "a bind volume mounts a path of the host, which the {level} level of namespace '{}' does not allow; use a named volume instead",
                deployment.namespace
            ),
        });
    }

    if level == SecurityLevel::Restricted && runs_on_the_host_kernel(&deployment.runtime) {
        match user.and_then(|u| u.id) {
            Some(0) => breaches.push(Breach {
                field: "config.user.id".to_string(),
                code: "deployment.config.user.id.root_forbidden",
                message: format!(
                    "the restricted level of namespace '{}' does not allow running as root (user id 0)",
                    deployment.namespace
                ),
            }),
            Some(_) => {}
            None => breaches.push(Breach {
                field: "config.user.id".to_string(),
                code: "deployment.config.user.id.required",
                message: format!(
                    "the restricted level of namespace '{}' requires a non-root user id, as the image's default user may be root",
                    deployment.namespace
                ),
            }),
        }
    }

    breaches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::deployments::{
        DeploymentConfig, DeploymentStatus, NetworkConfig, UserConfig,
    };
    use std::collections::HashMap;

    fn deployment() -> Deployment {
        Deployment {
            id: "dep-0001".to_string(),
            created_at: String::new(),
            updated_at: None,
            status: DeploymentStatus::Pending,
            reason: None,
            message: None,
            restart_count: 0,
            namespace: "tenant-a".to_string(),
            name: "web".to_string(),
            image: "alpine:3".to_string(),
            config: None,
            runtime: "docker".to_string(),
            kind: "worker".to_string(),
            replicas: 1,
            command: vec![],
            instances: vec![],
            labels: HashMap::new(),
            environment: HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: vec![],
            resources: None,
            autoscale: None,
            desired_replicas: None,
            image_digest: None,
            ports: vec![],
            pending_events: vec![],
            parent_id: None,
            network: None,
            restart: None,
        }
    }

    fn with_user(id: Option<u32>, privileged: Option<bool>) -> Deployment {
        let config: DeploymentConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        Deployment {
            config: Some(DeploymentConfig {
                user: Some(UserConfig {
                    id,
                    group: None,
                    privileged,
                }),
                ..config
            }),
            ..deployment()
        }
    }

    fn codes(deployment: &Deployment, level: SecurityLevel) -> Vec<&'static str> {
        breaches(deployment, level, &[])
            .iter()
            .map(|b| b.code)
            .collect()
    }

    /// A deployment asking for every host-level option.
    fn reaching_the_host() -> Deployment {
        Deployment {
            network: Some(NetworkConfig {
                mode: NetworkMode::Host,
            }),
            volumes: r#"[{"type":"volume","source":"data","destination":"/data","driver":"local","permission":"rw"},
                         {"type":"bind","source":"/var/run/docker.sock","destination":"/var/run/docker.sock","driver":"local","permission":"rw"}]"#
                .to_string(),
            ..with_user(Some(0), Some(true))
        }
    }

    #[test]
    fn privileged_admits_everything() {
        assert!(breaches(&reaching_the_host(), SecurityLevel::Privileged, &[]).is_empty());
    }

    #[test]
    fn baseline_refuses_each_way_to_the_host() {
        let found = breaches(&reaching_the_host(), SecurityLevel::Baseline, &[]);
        let fields: Vec<&str> = found.iter().map(|b| b.field.as_str()).collect();
        assert_eq!(
            fields,
            ["config.user.privileged", "network.mode", "volumes[1].type"]
        );
    }

    #[test]
    fn baseline_admits_a_plain_deployment_running_as_root() {
        assert!(codes(&deployment(), SecurityLevel::Baseline).is_empty());
        assert!(codes(&with_user(Some(0), Some(false)), SecurityLevel::Baseline).is_empty());
    }

    #[test]
    fn restricted_requires_a_non_root_user() {
        assert_eq!(
            codes(&deployment(), SecurityLevel::Restricted),
            ["deployment.config.user.id.required"]
        );
        assert_eq!(
            codes(&with_user(Some(0), None), SecurityLevel::Restricted),
            ["deployment.config.user.id.root_forbidden"]
        );
        assert!(codes(&with_user(Some(1000), None), SecurityLevel::Restricted).is_empty());
    }

    #[test]
    fn restricted_also_refuses_what_baseline_refuses() {
        let found = codes(&reaching_the_host(), SecurityLevel::Restricted);
        assert!(found.contains(&"deployment.config.user.privileged.forbidden"));
        assert!(found.contains(&"deployment.volumes.bind.forbidden"));
        assert!(found.contains(&"deployment.config.user.id.root_forbidden"));
    }

    #[test]
    fn restricted_leaves_the_user_of_a_virtual_machine_alone() {
        let vm = Deployment {
            runtime: "cloud-hypervisor".to_string(),
            ..deployment()
        };
        assert!(codes(&vm, SecurityLevel::Restricted).is_empty());
    }

    fn binding(source: &str) -> Deployment {
        Deployment {
            volumes: serde_json::json!([{
                "type": "bind", "source": source, "destination": "/data",
                "driver": "local", "permission": "rw"
            }])
            .to_string(),
            ..deployment()
        }
    }

    fn allowed(paths: &[&str]) -> Vec<HostPathPrefix> {
        paths
            .iter()
            .map(|p| HostPathPrefix::try_from(p.to_string()).unwrap())
            .collect()
    }

    #[test]
    fn a_bind_under_an_allowed_host_path_is_admitted_at_every_level() {
        let zerofs = allowed(&["/mnt/zerofs"]);
        for source in ["/mnt/zerofs", "/mnt/zerofs/acme/files", "/mnt/zerofs/acme/"] {
            for level in [SecurityLevel::Baseline, SecurityLevel::Restricted] {
                let found: Vec<_> = breaches(&binding(source), level, &zerofs)
                    .into_iter()
                    .map(|b| b.code)
                    .collect();
                assert!(
                    !found.contains(&"deployment.volumes.bind.forbidden"),
                    "{source} at {level}"
                );
            }
        }
    }

    #[test]
    fn a_bind_that_climbs_out_or_merely_shares_a_prefix_is_refused() {
        let zerofs = allowed(&["/mnt/zerofs"]);
        for source in [
            "/mnt/zerofs/../../var/run/docker.sock",
            "/mnt/zerofs/acme/../../../etc",
            "/mnt/zerofs/./acme",
            "/mnt/zerofsx",
            "/mnt",
            "mnt/zerofs/acme",
            "",
        ] {
            assert_eq!(
                breaches(&binding(source), SecurityLevel::Baseline, &zerofs)
                    .into_iter()
                    .map(|b| b.code)
                    .collect::<Vec<_>>(),
                ["deployment.volumes.bind.forbidden"],
                "{source}"
            );
        }
    }

    #[test]
    fn an_allowed_host_path_is_absolute_plain_and_not_the_root() {
        for path in ["/mnt/zerofs", "/srv/data/"] {
            assert!(HostPathPrefix::try_from(path.to_string()).is_ok(), "{path}");
        }
        for path in ["/", "//", "mnt/zerofs", "/mnt/../etc", "/mnt/./zerofs", ""] {
            assert!(
                HostPathPrefix::try_from(path.to_string()).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn levels_round_trip() {
        for level in [
            SecurityLevel::Privileged,
            SecurityLevel::Baseline,
            SecurityLevel::Restricted,
        ] {
            assert_eq!(level.as_str().parse::<SecurityLevel>(), Ok(level));
            assert_eq!(
                serde_json::to_string(&level).unwrap(),
                format!("\"{level}\"")
            );
        }
        assert!("root".parse::<SecurityLevel>().is_err());
    }
}
