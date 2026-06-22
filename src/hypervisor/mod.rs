//! Host-side plumbing shared across the VM runtimes (Cloud Hypervisor,
//! Firecracker) plus the common runtime contract (lifecycle trait, error
//! types, health probes, resource checks). Kept out of [`crate::runtime`] so
//! that `runtime` holds only the concrete runtime implementations.

pub(crate) mod cloud_init;
pub(crate) mod console_logs;
pub(crate) mod error;
pub(crate) mod health_probes;
pub(crate) mod host_nat;
pub(crate) mod host_net;
pub(crate) mod lifecycle_trait;
#[cfg(test)]
pub(crate) mod mock;
pub(crate) mod port_forwarder;
pub(crate) mod resources;
pub(crate) mod stats;
pub(crate) mod tap;
pub(crate) mod types;
pub(crate) mod virtiofs;
pub(crate) mod volume_image;
pub(crate) mod vsock_client;
