use tokio::process::Command;

/// Set up networking for a Cloud Hypervisor VM instance.
///
/// Creates a TAP device and bridges it to the namespace network bridge.
/// Returns the TAP device name and the IP assigned to the VM.
pub(crate) async fn setup_network(
    instance_id: &str,
    namespace: &str,
) -> Result<NetworkConfig, String> {
    let tap_name = format!("tap-{}", &instance_id[..12.min(instance_id.len())]);
    let bridge_name = format!("ring_{}", namespace);

    // Create TAP device
    run_command("ip", &["tuntap", "add", &tap_name, "mode", "tap"]).await?;
    run_command("ip", &["link", "set", &tap_name, "up"]).await?;

    // Ensure bridge exists
    if run_command("ip", &["link", "show", &bridge_name]).await.is_err() {
        run_command("ip", &["link", "add", &bridge_name, "type", "bridge"]).await?;
        run_command("ip", &["link", "set", &bridge_name, "up"]).await?;
    }

    // Attach TAP to bridge
    run_command("ip", &["link", "set", &tap_name, "master", &bridge_name]).await?;

    info!(
        "Network setup complete: tap={}, bridge={}",
        tap_name, bridge_name
    );

    Ok(NetworkConfig {
        tap_name,
        bridge_name,
    })
}

/// Tear down networking for a VM instance.
pub(crate) async fn teardown_network(instance_id: &str) {
    let tap_name = format!("tap-{}", &instance_id[..12.min(instance_id.len())]);

    if let Err(e) = run_command("ip", &["link", "delete", &tap_name]).await {
        debug!("Failed to delete TAP {}: {}", tap_name, e);
    }
}

pub(crate) struct NetworkConfig {
    pub tap_name: String,
    pub bridge_name: String,
}

async fn run_command(cmd: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("Failed to run {} {:?}: {}", cmd, args, e))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("{} {:?} failed: {}", cmd, args, stderr))
    }
}
