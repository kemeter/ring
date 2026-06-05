//! Firecracker microVM runtime.
//!
//! Boots each deployment instance as a dedicated Firecracker microVM, driven via
//! Firecracker's REST API over a per-VM Unix control socket. Structurally a
//! sibling of `cloud_hypervisor`: same KVM-backed-VM model and the same shared
//! helpers (`host_net`, `port_forwarder`, `virtiofs`, `vsock_client`), but a
//! different VMM and a different API shape (a sequence of resource PUTs instead
//! of CH's monolithic `vm.create`).

mod client;
mod lifecycle;

pub(crate) use lifecycle::{FirecrackerLifecycle, FirecrackerRuntimeConfig};
