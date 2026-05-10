mod client;
// NOTE: cloud_init lives here for now because CH is the only VM runtime that
// uses it. When Firecracker (or any other VM runtime) lands, lift this module
// to `src/runtime/cloud_init.rs` and adjust the `use` paths — the contents are
// runtime-agnostic on purpose. See comment at the top of cloud_init.rs.
mod cloud_init;
mod console_logs;
mod lifecycle;
mod stats;

pub(crate) use lifecycle::{CloudHypervisorLifecycle, CloudHypervisorRuntimeConfig};
