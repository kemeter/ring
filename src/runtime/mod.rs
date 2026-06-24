//! Concrete runtime implementations. Each runtime drives deployment instances
//! on a different backend (containers via Docker/Podman, microVMs via Cloud
//! Hypervisor/Firecracker). The shared host-side plumbing and the common
//! runtime contract live in the sibling [`crate::hypervisor`] module.

pub(crate) mod cloud_hypervisor;
pub(crate) mod containerd;
pub(crate) mod docker;
pub(crate) mod firecracker;
pub(crate) mod podman;
pub(crate) mod registry_auth;
