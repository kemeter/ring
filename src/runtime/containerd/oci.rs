//! OCI runtime spec generation.
//!
//! containerd's runc shim consumes a standard [OCI runtime spec] (the same
//! `config.json` runc reads) carried as the container's `spec` field. There is
//! no convenience builder in the gRPC API: we construct the spec ourselves and
//! wrap the JSON in a protobuf [`Any`] with the runtime-spec type url that the
//! shim recognises.
//!
//! This is a deliberately minimal-but-correct spec modeled on runc's default
//! (`runc spec`): a Linux process with the standard namespaces, the canonical
//! pseudo-filesystem mounts, and the deployment's process args / env / cwd.
//!
//! [OCI runtime spec]: https://github.com/opencontainers/runtime-spec

use crate::models::deployments::{Deployment, EnvValue};
use crate::models::volume::ResolvedMount;
use prost_types::Any;
use serde_json::{Value, json};

/// The type url the runc shim matches to decode the container spec. This is the
/// fixed identifier containerd registers for the OCI runtime spec; it is not a
/// generated prost type, so we set it on a raw `Any` by hand.
const RUNTIME_SPEC_TYPE_URL: &str = "types.containerd.io/opencontainers/runtime-spec/1/Spec";

/// Build the OCI runtime spec for a deployment instance and pack it as an `Any`.
///
/// `rootfs_path` is the absolute host path the snapshot mounts resolve to — but
/// when the rootfs is delivered as task mounts (the snapshot path), the runc
/// shim mounts them onto `root.path` itself, so we keep the conventional
/// relative `rootfs` here and let the task `rootfs` mounts populate it.
pub(crate) fn build_spec(
    deployment: &Deployment,
    resolved_mounts: &[ResolvedMount],
    config_files: &[(String, String)],
    image_default_args: &[String],
) -> Any {
    let spec = build_spec_value(
        deployment,
        resolved_mounts,
        config_files,
        image_default_args,
    );
    // `spec` is an in-memory serde_json::Value built from owned data, so
    // serialisation cannot fail in practice; expect() over unwrap_or_default()
    // so a truly impossible failure surfaces loudly instead of silently
    // shipping an empty spec that the shim would reject with an opaque error.
    let bytes = serde_json::to_vec(&spec).expect("OCI spec Value must serialize");
    Any {
        type_url: RUNTIME_SPEC_TYPE_URL.to_string(),
        value: bytes,
    }
}

/// Pure spec construction, split out so it can be unit-tested without gRPC.
pub(crate) fn build_spec_value(
    deployment: &Deployment,
    resolved_mounts: &[ResolvedMount],
    config_files: &[(String, String)],
    image_default_args: &[String],
) -> Value {
    // The deployment command overrides the image default. When the deployment
    // gives no command, fall back to the image's Entrypoint+Cmd: containerd is
    // low-level and (unlike the Docker daemon) does NOT merge image defaults
    // into the OCI spec, so an empty `process.args` makes `runc create` fail
    // with `args must not be empty`.
    let args = if deployment.command.is_empty() {
        image_default_args.to_vec()
    } else {
        deployment.command.clone()
    };

    let mut env: Vec<String> =
        vec!["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string()];
    for (key, value) in deployment.environment.iter() {
        if let EnvValue::Plain(v) = value {
            env.push(format!("{}={}", key, v));
        }
    }

    let (uid, gid) = deployment
        .config
        .as_ref()
        .and_then(|c| c.user.as_ref())
        .map(|u| (u.id.unwrap_or(0), u.group.unwrap_or(0)))
        .unwrap_or((0, 0));

    let mut mounts = default_mounts();
    mounts.extend(spec_mounts_from_resolved(resolved_mounts, config_files));

    // `args` is resolved above (deployment command, else image default). It can
    // still be empty if the image declares neither Entrypoint nor Cmd — such an
    // image is only runnable with an explicit command, and runc will reject it
    // with a clear `args must not be empty`. We set `args` only when non-empty
    // to keep the spec clean.
    let mut process = json!({
        "terminal": false,
        "user": { "uid": uid, "gid": gid },
        "env": env,
        "cwd": "/",
        "capabilities": default_capabilities(),
        "rlimits": [
            { "type": "RLIMIT_NOFILE", "hard": 1024, "soft": 1024 }
        ],
        "noNewPrivileges": true,
    });
    if !args.is_empty() {
        process["args"] = json!(args);
    }

    json!({
        "ociVersion": "1.0.2-dev",
        "process": process,
        "root": { "path": "rootfs", "readonly": false },
        "hostname": deployment.name,
        "mounts": mounts,
        "linux": {
            "namespaces": default_namespaces(),
            "maskedPaths": masked_paths(),
            "readonlyPaths": readonly_paths(),
        }
    })
}

/// The canonical pseudo-filesystem mounts runc installs by default.
fn default_mounts() -> Vec<Value> {
    vec![
        json!({ "destination": "/proc", "type": "proc", "source": "proc" }),
        json!({
            "destination": "/dev", "type": "tmpfs", "source": "tmpfs",
            "options": ["nosuid", "strictatime", "mode=755", "size=65536k"]
        }),
        json!({
            "destination": "/dev/pts", "type": "devpts", "source": "devpts",
            "options": ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620", "gid=5"]
        }),
        json!({
            "destination": "/dev/shm", "type": "tmpfs", "source": "shm",
            "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"]
        }),
        json!({
            "destination": "/dev/mqueue", "type": "mqueue", "source": "mqueue",
            "options": ["nosuid", "noexec", "nodev"]
        }),
        json!({
            "destination": "/sys", "type": "sysfs", "source": "sysfs",
            "options": ["nosuid", "noexec", "nodev", "ro"]
        }),
        json!({
            "destination": "/sys/fs/cgroup", "type": "cgroup", "source": "cgroup",
            "options": ["nosuid", "noexec", "nodev", "relatime", "ro"]
        }),
    ]
}

/// Translate Ring's resolved mounts into OCI bind mounts. Named/bind volumes
/// become host bind mounts; content/config/secret mounts are written to host
/// temp files by the caller and bind-mounted read-only (the `config_files` pairs
/// are `(host_path, destination)`).
fn spec_mounts_from_resolved(
    resolved_mounts: &[ResolvedMount],
    config_files: &[(String, String)],
) -> Vec<Value> {
    let mut out = Vec::new();
    for m in resolved_mounts {
        match m {
            ResolvedMount::Bind {
                source,
                destination,
                read_only,
            } => {
                out.push(bind_mount(source, destination, *read_only));
            }
            ResolvedMount::Named {
                name,
                destination,
                read_only,
                ..
            } => {
                // Named volumes are materialized under a host directory keyed by
                // the volume name. Bind that directory into the container.
                let source = format!("/var/lib/ring/volumes/{}", name);
                out.push(bind_mount(&source, destination, *read_only));
            }
            // Content mounts are handled via `config_files` (written to disk by
            // the lifecycle before spec construction).
            ResolvedMount::Content { .. } => {}
        }
    }
    for (host_path, destination) in config_files {
        out.push(bind_mount(host_path, destination, true));
    }
    out
}

fn bind_mount(source: &str, destination: &str, read_only: bool) -> Value {
    let mut options = vec!["rbind".to_string()];
    options.push(if read_only {
        "ro".to_string()
    } else {
        "rw".to_string()
    });
    json!({
        "destination": destination,
        "type": "bind",
        "source": source,
        "options": options,
    })
}

/// Standard Linux namespaces for an isolated container. Network is its own
/// namespace so CNI can wire it up; sharing the host network would defeat the
/// CNI bridge model.
fn default_namespaces() -> Vec<Value> {
    vec![
        json!({ "type": "pid" }),
        json!({ "type": "ipc" }),
        json!({ "type": "uts" }),
        json!({ "type": "mount" }),
        json!({ "type": "network" }),
    ]
}

/// runc's default capability set for an unprivileged container.
fn default_capabilities() -> Value {
    let caps = vec![
        "CAP_CHOWN",
        "CAP_DAC_OVERRIDE",
        "CAP_FSETID",
        "CAP_FOWNER",
        "CAP_MKNOD",
        "CAP_NET_RAW",
        "CAP_SETGID",
        "CAP_SETUID",
        "CAP_SETFCAP",
        "CAP_SETPCAP",
        "CAP_NET_BIND_SERVICE",
        "CAP_SYS_CHROOT",
        "CAP_KILL",
        "CAP_AUDIT_WRITE",
    ];
    json!({
        "bounding": caps,
        "effective": caps,
        "permitted": caps,
    })
}

fn masked_paths() -> Vec<&'static str> {
    vec![
        "/proc/acpi",
        "/proc/asound",
        "/proc/kcore",
        "/proc/keys",
        "/proc/latency_stats",
        "/proc/timer_list",
        "/proc/timer_stats",
        "/proc/sched_debug",
        "/sys/firmware",
        "/proc/scsi",
    ]
}

fn readonly_paths() -> Vec<&'static str> {
    vec![
        "/proc/bus",
        "/proc/fs",
        "/proc/irq",
        "/proc/sys",
        "/proc/sysrq-trigger",
    ]
}

/// Build an OCI process spec for an exec probe (used by health checks). Wrapped
/// as an `Any` for `ExecProcessRequest.spec`.
pub(crate) fn build_exec_process_spec(args: Vec<String>) -> Any {
    let process = json!({
        "terminal": false,
        "user": { "uid": 0, "gid": 0 },
        "args": args,
        "env": [
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
        ],
        "cwd": "/",
        "capabilities": default_capabilities(),
        "noNewPrivileges": true,
    });
    Any {
        type_url: "types.containerd.io/opencontainers/runtime-spec/1/Process".to_string(),
        value: serde_json::to_vec(&process).expect("exec process spec Value must serialize"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::deployments::DeploymentStatus;
    use std::collections::HashMap;

    fn make_deployment() -> Deployment {
        Deployment {
            id: "dep-1".to_string(),
            created_at: "now".to_string(),
            updated_at: None,
            status: DeploymentStatus::Running,
            restart_count: 0,
            namespace: "default".to_string(),
            name: "web".to_string(),
            image: "nginx:latest".to_string(),
            config: None,
            runtime: "containerd".to_string(),
            kind: "worker".to_string(),
            replicas: 1,
            command: vec![
                "nginx".to_string(),
                "-g".to_string(),
                "daemon off;".to_string(),
            ],
            instances: vec![],
            labels: HashMap::new(),
            environment: HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: vec![],
            resources: None,
            image_digest: None,
            ports: vec![],
            pending_events: vec![],
            parent_id: None,
            network: None,
        }
    }

    #[test]
    fn spec_includes_command_args() {
        let mut d = make_deployment();
        d.environment
            .insert("FOO".to_string(), EnvValue::Plain("bar".to_string()));
        let spec = build_spec_value(&d, &[], &[], &[]);
        assert_eq!(spec["process"]["args"][0], "nginx");
        let env = spec["process"]["env"].as_array().unwrap();
        assert!(env.iter().any(|e| e == "FOO=bar"));
    }

    #[test]
    fn spec_falls_back_to_image_default_args_when_command_empty() {
        // No deployment command → the image's Entrypoint+Cmd must populate
        // process.args, or runc rejects the container with "args must not be
        // empty" (the containerd crash-loop bug on official images).
        let mut d = make_deployment();
        d.command.clear();
        let image_args = vec!["/docker-entrypoint.sh".to_string(), "nginx".to_string()];
        let spec = build_spec_value(&d, &[], &[], &image_args);
        assert_eq!(spec["process"]["args"][0], "/docker-entrypoint.sh");
        assert_eq!(spec["process"]["args"][1], "nginx");
    }

    #[test]
    fn spec_command_overrides_image_default_args() {
        // Deployment command wins over the image default.
        let d = make_deployment(); // command = ["nginx", "-g", "daemon off;"]
        let image_args = vec!["/docker-entrypoint.sh".to_string()];
        let spec = build_spec_value(&d, &[], &[], &image_args);
        assert_eq!(spec["process"]["args"][0], "nginx");
    }

    #[test]
    fn spec_omits_args_when_no_command_and_no_image_default() {
        // Neither deployment command nor image default → args omitted (runc will
        // surface a clear error rather than us shipping an empty array).
        let mut d = make_deployment();
        d.command.clear();
        let spec = build_spec_value(&d, &[], &[], &[]);
        assert!(spec["process"].get("args").is_none());
    }

    #[test]
    fn spec_has_network_namespace() {
        let d = make_deployment();
        let spec = build_spec_value(&d, &[], &[], &[]);
        let namespaces = spec["linux"]["namespaces"].as_array().unwrap();
        assert!(namespaces.iter().any(|n| n["type"] == "network"));
    }

    #[test]
    fn spec_binds_resolved_mounts() {
        let d = make_deployment();
        let mounts = vec![ResolvedMount::Bind {
            source: "/host/data".to_string(),
            destination: "/data".to_string(),
            read_only: true,
        }];
        let spec = build_spec_value(&d, &mounts, &[], &[]);
        let m = spec["mounts"].as_array().unwrap();
        assert!(
            m.iter()
                .any(|x| x["destination"] == "/data" && x["source"] == "/host/data")
        );
    }
}
