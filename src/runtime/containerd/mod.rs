//! Containerd runtime: drive deployment instances on a host's `containerd`
//! daemon **through its native gRPC API**, with no `ctr`/`nerdctl` shell-out and
//! no Docker daemon in between.
//!
//! # Why native gRPC
//!
//! Ring already drives Docker (and Docker-compatible Podman) over the Docker
//! HTTP API via `bollard`. Containerd sits one layer below that: it is the
//! container supervisor Docker itself delegates to. Talking to it directly buys
//! us a smaller trusted surface (no `dockerd`), the same daemon Kubernetes
//! (k3s/RKE2) already runs, and an API that is stable and versioned. The cost is
//! that containerd is *low level* — it has no opinion about pull policy,
//! networking, or a "run this image" convenience verb. Each of those is a
//! distinct gRPC service we orchestrate by hand:
//!
//! - **Transfer** — pull an image from a registry into the content store and
//!   unpack it into a snapshot (`transfer.rs`/`image.rs`).
//! - **Images / Content** — resolve the pulled image's config to compute the
//!   rootfs *chain ID*, which is the parent for a writable snapshot.
//! - **Snapshots** — `Prepare` a per-instance writable rootfs from that chain
//!   ID and return the mounts the task needs.
//! - **Containers** — register the container metadata object, carrying the OCI
//!   runtime spec (`oci.rs`) as a protobuf `Any`.
//! - **Tasks** — create + start the actual process from the container, kill and
//!   delete it on teardown, exec health probes, and read cgroup metrics.
//! - **CNI** — containerd does *not* do networking; we drive the standard CNI
//!   plugins ourselves (`cni.rs`).
//!
//! The shape of a deployment (N replicas, env, command, labels, registry auth,
//! pull policy) is identical to the Docker runtime — this module reproduces that
//! semantics on top of the lower-level primitives.

pub(crate) mod client;
pub(crate) mod cni;
pub(crate) mod health_check;
pub(crate) mod image;
pub(crate) mod instances;
pub(crate) mod lifecycle;
pub(crate) mod logs;
pub(crate) mod oci;
pub(crate) mod stats;

pub(crate) use client::{ContainerdLifecycle, ContainerdRuntimeConfig};

/// Label key under which every Ring-managed container records its owning
/// deployment id. Mirrors the Docker runtime's `ring_deployment` label so
/// `list_instances`/`remove_instance` can filter by deployment without keeping
/// external state. Containerd's filter syntax matches on `labels."<key>"`.
pub(crate) const RING_DEPLOYMENT_LABEL: &str = "ring_deployment";

/// Generate a short random suffix for instance ids, matching the Docker
/// runtime's `tiny_id` so container names read the same across runtimes.
pub(crate) fn tiny_id() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    format!("{:08x}", rng.random::<u32>())
}
