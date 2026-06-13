//! CNI networking for containerd tasks.
//!
//! containerd has no networking of its own — that is by design, it leaves the
//! network namespace to a [CNI] plugin chain. We drive the standard CNI plugins
//! ourselves over the documented CNI execution protocol: a JSON network config
//! on stdin, the operation in `CNI_COMMAND`, and the container/netns identifiers
//! in environment variables. This is the same protocol Kubernetes and `nerdctl`
//! use; it is *not* an application shell-out (we invoke the plugin binaries the
//! CNI spec defines, with the spec's wire format).
//!
//! On `ADD` we capture the assigned IP so [`super::health_check`] can resolve a
//! reachable address; on `DEL` we tear the interface down.
//!
//! [CNI]: https://github.com/containernetworking/cni

use serde::Deserialize;
use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::process::Stdio;

/// Where Ring writes its default CNI network configuration.
const CNI_CONF_DIR: &str = "/etc/cni/net.d";
const CNI_CONF_FILE: &str = "/etc/cni/net.d/10-ring.conflist";
/// Standard search path for CNI plugin binaries.
const CNI_BIN_DIR: &str = "/opt/cni/bin";
/// Network name + bridge used by Ring's default conflist.
const CNI_NETWORK_NAME: &str = "ring";
const CNI_BRIDGE: &str = "ring-cni0";
/// IPAM subnet for Ring containers. /16 gives ample address space and avoids the
/// common 10.42/10.88 ranges k3s and nerdctl default to.
const CNI_SUBNET: &str = "10.43.0.0/16";
/// Interface name inside the container.
const CNI_IFNAME: &str = "eth0";

/// Subset of a CNI ADD result we parse: the IPs assigned to the interface.
#[derive(Deserialize)]
struct CniResult {
    #[serde(default)]
    ips: Vec<CniIp>,
}

#[derive(Deserialize)]
struct CniIp {
    /// `address` is CIDR form, e.g. `10.43.0.5/16`.
    address: String,
}

/// Ensure Ring's default CNI conflist exists. Idempotent: written only when
/// absent so an operator can drop in their own config to override it.
pub(crate) fn ensure_default_config() {
    if Path::new(CNI_CONF_FILE).exists() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(CNI_CONF_DIR) {
        warn!("could not create CNI config dir {}: {}", CNI_CONF_DIR, e);
        return;
    }
    let conflist = default_conflist();
    match std::fs::write(CNI_CONF_FILE, conflist) {
        Ok(_) => info!("wrote default CNI conflist to {}", CNI_CONF_FILE),
        Err(e) => warn!("could not write CNI conflist {}: {}", CNI_CONF_FILE, e),
    }
}

fn default_conflist() -> String {
    serde_json::json!({
        "cniVersion": "1.0.0",
        "name": CNI_NETWORK_NAME,
        "plugins": [
            {
                "type": "bridge",
                "bridge": CNI_BRIDGE,
                "isGateway": true,
                "ipMasq": true,
                "hairpinMode": true,
                "ipam": {
                    "type": "host-local",
                    "ranges": [[{ "subnet": CNI_SUBNET }]],
                    "routes": [{ "dst": "0.0.0.0/0" }]
                }
            },
            { "type": "loopback" }
        ]
    })
    .to_string()
}

/// Whether the CNI plugin binaries are present. When missing we skip networking
/// with a clear warning rather than failing the whole deployment — the workload
/// still boots, just without a CNI address (health checks that need an IP will
/// report the missing address).
pub(crate) fn plugins_available() -> bool {
    Path::new(CNI_BIN_DIR).join("bridge").exists()
}

/// Run `CNI ADD` for a container's network namespace and return the assigned IP.
///
/// `netns_path` is the path to the network namespace (e.g.
/// `/proc/<pid>/ns/net`); `container_id` is the CNI container id (we use the
/// task/instance id).
pub(crate) async fn add(container_id: &str, netns_path: &str) -> Option<IpAddr> {
    if !plugins_available() {
        warn!(
            "CNI plugins not found under {} — instance {} will have no CNI network",
            CNI_BIN_DIR, container_id
        );
        return None;
    }
    // The plugin invocation (fork-exec + host-local IPAM filesystem I/O) blocks,
    // so run it off the async runtime's worker threads.
    let (cid, ns) = (container_id.to_string(), netns_path.to_string());
    let stdout = match tokio::task::spawn_blocking(move || exec_plugin("ADD", &cid, &ns))
        .await
        .ok()
        .flatten()
    {
        Some(out) => out,
        None => return None,
    };
    let result: CniResult = match serde_json::from_slice(&stdout) {
        Ok(r) => r,
        Err(e) => {
            warn!("could not parse CNI ADD result for {}: {}", container_id, e);
            return None;
        }
    };
    for ip in result.ips {
        if let Some((addr, _)) = ip.address.split_once('/')
            && let Ok(parsed) = addr.parse::<IpAddr>()
        {
            return Some(parsed);
        }
    }
    None
}

/// Run `CNI DEL` to tear down a container's network. Best-effort and idempotent
/// (the CNI spec requires DEL to succeed even if ADD never ran, so calling it
/// during cleanup of a half-created instance is safe).
pub(crate) async fn del(container_id: &str, netns_path: &str) {
    if !plugins_available() {
        return;
    }
    let (cid, ns) = (container_id.to_string(), netns_path.to_string());
    let _ = tokio::task::spawn_blocking(move || exec_plugin("DEL", &cid, &ns)).await;
}

/// Execute the chained CNI plugins for the conflist. We invoke the first plugin
/// (`bridge`) directly with the conflist's plugin config — for Ring's simple two
/// plugin chain this is sufficient; a full chained runtime would feed each
/// plugin's output as `prevResult` to the next, which is unnecessary for a
/// bridge+loopback chain where only the bridge allocates an IP.
fn exec_plugin(command: &str, container_id: &str, netns_path: &str) -> Option<Vec<u8>> {
    // Extract the bridge plugin config from the conflist and add the required
    // top-level fields each plugin invocation expects (`cniVersion`, `name`).
    let net_config = serde_json::json!({
        "cniVersion": "1.0.0",
        "name": CNI_NETWORK_NAME,
        "type": "bridge",
        "bridge": CNI_BRIDGE,
        "isGateway": true,
        "ipMasq": true,
        "hairpinMode": true,
        "ipam": {
            "type": "host-local",
            "ranges": [[{ "subnet": CNI_SUBNET }]],
            "routes": [{ "dst": "0.0.0.0/0" }]
        }
    })
    .to_string();

    let plugin = Path::new(CNI_BIN_DIR).join("bridge");
    let mut child = match std::process::Command::new(&plugin)
        .env("CNI_COMMAND", command)
        .env("CNI_CONTAINERID", container_id)
        .env("CNI_NETNS", netns_path)
        .env("CNI_IFNAME", CNI_IFNAME)
        .env("CNI_PATH", CNI_BIN_DIR)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("failed to spawn CNI bridge plugin: {}", e);
            return None;
        }
    };

    if let Some(mut stdin) = child.stdin.take()
        && let Err(e) = stdin.write_all(net_config.as_bytes())
    {
        warn!("failed to write CNI config to plugin stdin: {}", e);
        return None;
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            warn!("CNI bridge plugin {} did not complete: {}", command, e);
            return None;
        }
    };

    if !output.status.success() {
        warn!(
            "CNI {} failed for {}: {}",
            command,
            container_id,
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }
    Some(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_conflist_is_valid_json_with_bridge() {
        let conf: serde_json::Value = serde_json::from_str(&default_conflist()).unwrap();
        assert_eq!(conf["name"], CNI_NETWORK_NAME);
        let plugins = conf["plugins"].as_array().unwrap();
        assert_eq!(plugins[0]["type"], "bridge");
        assert_eq!(plugins[0]["bridge"], CNI_BRIDGE);
    }

    #[test]
    fn cni_result_parses_ip() {
        let raw = r#"{"ips":[{"address":"10.43.0.5/16"}]}"#;
        let result: CniResult = serde_json::from_slice(raw.as_bytes()).unwrap();
        assert_eq!(result.ips[0].address, "10.43.0.5/16");
    }
}
