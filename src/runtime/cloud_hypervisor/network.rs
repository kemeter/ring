use super::client::NetConfig;

/// Build network configuration for a Cloud Hypervisor VM.
///
/// Cloud Hypervisor creates TAP devices itself when given a net config
/// (requires CAP_NET_ADMIN on the cloud-hypervisor binary).
/// Ring does not need to manage TAP devices directly.
pub(crate) fn build_net_config() -> Vec<NetConfig> {
    vec![NetConfig {
        tap: None,
        ip: None,
        mask: None,
        mac: None,
    }]
}
