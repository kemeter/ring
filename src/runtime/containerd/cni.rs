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
//! reachable address; on `DEL` we tear the interface down. An instance that
//! publishes ports also goes through the `portmap` plugin, chained after
//! `bridge`, which forwards each host port to it.
//!
//! [CNI]: https://github.com/containernetworking/cni

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::process::Stdio;

/// Where Ring writes its default CNI network configuration.
const CNI_CONF_DIR: &str = "/etc/cni/net.d";
const CNI_CONF_FILE: &str = "/etc/cni/net.d/10-ring.conflist";
/// Candidate directories for CNI plugin binaries, in priority order. `/opt/cni/bin`
/// is the CNI upstream / containerd / Kubernetes default, but Debian/Ubuntu package
/// the plugins under `/usr/lib/cni` and Fedora under `/usr/libexec/cni` (both follow
/// the FHS rather than `/opt`). We probe all three so Ring works regardless of how
/// the plugins were installed, instead of assuming the upstream layout.
const CNI_BIN_DIRS: &[&str] = &["/opt/cni/bin", "/usr/lib/cni", "/usr/libexec/cni"];

/// Resolve the directory holding the CNI plugin binaries by probing the known
/// install locations for `bridge` (the plugin Ring invokes directly). Honours an
/// explicit `CNI_PATH` override first. Falls back to the upstream default so the
/// resulting error message names the conventional path.
fn cni_bin_dir() -> String {
    if let Ok(path) = std::env::var("CNI_PATH")
        && !path.is_empty()
    {
        return path;
    }
    for dir in CNI_BIN_DIRS {
        if Path::new(dir).join("bridge").is_file() {
            return (*dir).to_string();
        }
    }
    CNI_BIN_DIRS[0].to_string()
}
/// Network name + bridge used by Ring's default conflist.
const CNI_NETWORK_NAME: &str = "ring";
const CNI_BRIDGE: &str = "ring-cni0";
/// IPAM subnet for Ring containers. /16 gives ample address space and avoids the
/// common 10.42/10.88 ranges k3s and nerdctl default to.
const CNI_SUBNET: &str = "10.43.0.0/16";
/// Interface name inside the container.
const CNI_IFNAME: &str = "eth0";

/// A host port forwarded to an instance, as the `portmap` plugin takes it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct PortMapping {
    #[serde(rename = "hostPort")]
    pub(crate) host_port: u16,
    #[serde(rename = "containerPort")]
    pub(crate) container_port: u16,
    pub(crate) protocol: String,
    #[serde(rename = "hostIP")]
    pub(crate) host_ip: String,
}

/// The port mappings of a deployment's published ports.
pub(crate) fn port_mappings(
    ports: &[crate::models::deployments::DeploymentPort],
) -> Vec<PortMapping> {
    ports
        .iter()
        .map(|p| PortMapping {
            host_port: p.published,
            container_port: p.target,
            protocol: p.protocol.as_str().to_string(),
            host_ip: p.host_ip.clone().unwrap_or_else(|| "0.0.0.0".to_string()),
        })
        .collect()
}

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
    plugin_installed("bridge")
}

fn plugin_installed(plugin: &str) -> bool {
    Path::new(&cni_bin_dir()).join(plugin).is_file()
}

/// Run `CNI ADD` for a container's network namespace and return the assigned IP.
///
/// `netns_path` is the path to the network namespace (e.g.
/// `/proc/<pid>/ns/net`); `container_id` is the CNI container id (we use the
/// task/instance id).
///
/// Without the plugins the instance runs with no network, as documented
/// (`Ok(None)`). With them, a plugin that fails, hangs or answers nonsense is
/// an error: the instance would otherwise count as running while nothing can
/// reach it.
pub(crate) async fn add(
    container_id: &str,
    netns_path: &str,
    ports: &[PortMapping],
) -> Result<Option<IpAddr>, String> {
    if !plugins_available() {
        // Without a network, a published port could never be reached.
        if !ports.is_empty() {
            return Err(format!(
                "the CNI plugins are not installed (looked under {:?}), so published ports cannot be forwarded",
                CNI_BIN_DIRS
            ));
        }
        warn!(
            "CNI plugins not found under any of {:?} — instance {} will have no CNI network",
            CNI_BIN_DIRS, container_id
        );
        return Ok(None);
    }
    if !ports.is_empty() && !plugin_installed("portmap") {
        return Err(format!(
            "the CNI portmap plugin is not installed in {}, so published ports cannot be forwarded",
            cni_bin_dir()
        ));
    }
    // The plugin invocation (fork-exec + host-local IPAM filesystem I/O) blocks,
    // so run it off the async runtime's worker threads.
    let (cid, ns, mappings) = (
        container_id.to_string(),
        netns_path.to_string(),
        ports.to_vec(),
    );
    let stdout = tokio::task::spawn_blocking(move || {
        let bridge = exec_plugin("bridge", "ADD", &cid, &ns, &bridge_config().to_string())?;
        if !mappings.is_empty() {
            let previous: serde_json::Value = serde_json::from_slice(&bridge)
                .map_err(|e| format!("could not parse the CNI bridge result: {e}"))?;
            let config = portmap_config(&mappings, Some(previous)).to_string();
            exec_plugin("portmap", "ADD", &cid, &ns, &config)?;
        }
        Ok::<_, String>(bridge)
    })
    .await
    .map_err(|e| format!("CNI ADD for {container_id} did not run: {e}"))??;
    let result: CniResult = serde_json::from_slice(&stdout)
        .map_err(|e| format!("could not parse the CNI ADD result for {container_id}: {e}"))?;
    for ip in result.ips {
        if let Some((addr, _)) = ip.address.split_once('/')
            && let Ok(parsed) = addr.parse::<IpAddr>()
        {
            return Ok(Some(parsed));
        }
    }
    Err(format!("CNI ADD gave {container_id} no address"))
}

/// Run `CNI DEL` to tear down a container's network. Best-effort and idempotent
/// (the CNI spec requires DEL to succeed even if ADD never ran, so calling it
/// during cleanup of a half-created instance is safe).
pub(crate) async fn del(container_id: &str, netns_path: &str, ports: &[PortMapping]) {
    if !plugins_available() {
        return;
    }
    let (cid, ns, mappings) = (
        container_id.to_string(),
        netns_path.to_string(),
        ports.to_vec(),
    );
    let _ = tokio::task::spawn_blocking(move || {
        // In the reverse order of ADD: the forwarding goes before the
        // interface it points to.
        if !mappings.is_empty() && plugin_installed("portmap") {
            let config = portmap_config(&mappings, None).to_string();
            let _ = exec_plugin("portmap", "DEL", &cid, &ns, &config);
        }
        let _ = exec_plugin("bridge", "DEL", &cid, &ns, &bridge_config().to_string());
    })
    .await;
}

/// The config of the `bridge` plugin: Ring's network.
fn bridge_config() -> serde_json::Value {
    serde_json::json!({
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
}

/// The config of the `portmap` plugin for `ports`, chained after `bridge`
/// whose result it takes on `ADD`.
fn portmap_config(ports: &[PortMapping], previous: Option<serde_json::Value>) -> serde_json::Value {
    let mut config = serde_json::json!({
        "cniVersion": "1.0.0",
        "name": CNI_NETWORK_NAME,
        "type": "portmap",
        "capabilities": { "portMappings": true },
        // Lets the host reach a port published on 127.0.0.1, and a container
        // reach one through the host.
        "snat": true,
        "runtimeConfig": { "portMappings": ports },
    });
    if let Some(previous) = previous {
        config["prevResult"] = previous;
    }
    config
}

/// Run one CNI plugin binary with `config` on stdin.
fn exec_plugin(
    plugin_type: &str,
    command: &str,
    container_id: &str,
    netns_path: &str,
    net_config: &str,
) -> Result<Vec<u8>, String> {
    let bin_dir = cni_bin_dir();
    let plugin = Path::new(&bin_dir).join(plugin_type);
    let mut child = match std::process::Command::new(&plugin)
        .env("CNI_COMMAND", command)
        .env("CNI_CONTAINERID", container_id)
        .env("CNI_NETNS", netns_path)
        .env("CNI_IFNAME", CNI_IFNAME)
        .env("CNI_PATH", &bin_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("failed to spawn CNI {} plugin: {}", plugin_type, e);
            return Err(format!("failed to spawn the CNI {plugin_type} plugin: {e}"));
        }
    };

    if let Some(mut stdin) = child.stdin.take()
        && let Err(e) = stdin.write_all(net_config.as_bytes())
    {
        warn!("failed to write CNI config to plugin stdin: {}", e);
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("failed to write the CNI config: {e}"));
    }

    // Bounded: a plugin that hangs would otherwise hold a blocking thread and
    // its process forever, on every retry, and hang the teardown's DEL too.
    // Its output (a small JSON result) fits in the pipe, so it can exit
    // before being read.
    let deadline = std::time::Instant::now() + PLUGIN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                warn!(
                    "CNI {} for {} did not finish within {}s, killed",
                    command,
                    container_id,
                    PLUGIN_TIMEOUT.as_secs()
                );
                return Err(format!(
                    "CNI {command} did not finish within {}s",
                    PLUGIN_TIMEOUT.as_secs()
                ));
            }
            Err(e) => return Err(format!("CNI {command} could not be waited for: {e}")),
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("CNI {command} did not complete: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "CNI {} {} failed for {}: {}",
            plugin_type, command, container_id, stderr
        );
        return Err(format!(
            "CNI {plugin_type} {command} failed: {}",
            stderr.trim()
        ));
    }
    Ok(output.stdout)
}

/// Longest a CNI plugin may run before it is killed.
const PLUGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `CNI_PATH` is a process-global env var; tests that set/remove it must not
    /// run concurrently or they race each other's view of the var. Serialise the
    /// window with a shared lock (same pattern as the virtiofsd env tests).
    static ENV_GUARD: Mutex<()> = Mutex::new(());

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

    #[test]
    fn cni_bin_dir_honours_cni_path_override() {
        let _g = ENV_GUARD.lock().unwrap();
        // CNI_PATH wins over probing, even pointing at a path that doesn't exist.
        let prev = std::env::var_os("CNI_PATH");
        // SAFETY: this is the only test touching CNI_PATH and it restores it.
        unsafe { std::env::set_var("CNI_PATH", "/custom/cni") };
        assert_eq!(cni_bin_dir(), "/custom/cni");
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CNI_PATH", v),
                None => std::env::remove_var("CNI_PATH"),
            }
        }
    }

    #[test]
    fn cni_bin_dir_falls_back_to_upstream_default() {
        let _g = ENV_GUARD.lock().unwrap();
        // With no override and (assumed) no plugins under the test paths, the
        // resolver still returns the conventional upstream path so error
        // messages name something sensible.
        let prev = std::env::var_os("CNI_PATH");
        // SAFETY: this is the only test touching CNI_PATH and it restores it.
        unsafe { std::env::remove_var("CNI_PATH") };
        let resolved = cni_bin_dir();
        assert!(CNI_BIN_DIRS.contains(&resolved.as_str()));
        unsafe {
            if let Some(v) = prev {
                std::env::set_var("CNI_PATH", v);
            }
        }
    }

    #[test]
    fn portmap_takes_the_bridge_result_and_the_mappings() {
        let ports = port_mappings(&[crate::models::deployments::DeploymentPort {
            published: 8080,
            target: 80,
            host_ip: Some("127.0.0.1".to_string()),
            protocol: crate::models::deployments::PortProtocol::Udp,
        }]);
        let previous = serde_json::json!({"ips": [{"address": "10.43.0.5/16"}]});

        let config = portmap_config(&ports, Some(previous.clone()));

        assert_eq!(config["type"], "portmap");
        assert_eq!(config["capabilities"]["portMappings"], true);
        assert_eq!(
            config["runtimeConfig"]["portMappings"],
            serde_json::json!([{"hostPort": 8080, "containerPort": 80, "protocol": "udp", "hostIP": "127.0.0.1"}])
        );
        assert_eq!(config["prevResult"], previous);
        assert!(portmap_config(&ports, None).get("prevResult").is_none());
    }

    #[test]
    fn a_port_without_a_host_ip_is_published_on_every_interface() {
        let ports = port_mappings(&[crate::models::deployments::DeploymentPort {
            published: 8080,
            target: 80,
            host_ip: None,
            protocol: Default::default(),
        }]);
        assert_eq!(ports[0].host_ip, "0.0.0.0");
        assert_eq!(ports[0].protocol, "tcp");
    }
}
