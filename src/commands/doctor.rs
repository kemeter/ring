use crate::config::config::{Config, get_config_dir};
use clap::{ArgMatches, Command};
use std::process::Command as ShellCommand;

pub(crate) fn command_config() -> Command {
    Command::new("doctor").about("Check system dependencies for configured runtimes")
}

pub(crate) struct Check {
    name: String,
    passed: bool,
    detail: String,
}

impl Check {
    fn ok(name: &str, detail: &str) -> Self {
        Self {
            name: name.to_string(),
            passed: true,
            detail: detail.to_string(),
        }
    }

    fn fail(name: &str, detail: &str) -> Self {
        Self {
            name: name.to_string(),
            passed: false,
            detail: detail.to_string(),
        }
    }
}

fn check_binary(name: &str, version_flag: &str) -> Check {
    match ShellCommand::new(name).arg(version_flag).output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            Check::ok(name, &version)
        }
        _ => Check::fail(name, "not found in PATH"),
    }
}

fn check_file(name: &str, path: &str) -> Check {
    if std::path::Path::new(path).exists() {
        Check::ok(name, path)
    } else {
        Check::fail(name, &format!("not found at {}", path))
    }
}

fn check_kvm() -> Check {
    let path = "/dev/kvm";
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(_) => Check::ok("KVM", "/dev/kvm accessible"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Check::fail("KVM", "/dev/kvm not found")
        }
        Err(e) => Check::fail(
            "KVM",
            &format!(
                "/dev/kvm not accessible: {} (try: sudo usermod -aG kvm $USER)",
                e
            ),
        ),
    }
}

/// Verify the cloud-hypervisor binary has the network capabilities it needs
/// to create TAP interfaces. Without these the VM dies at boot with
/// `ConfigureTap PermissionDenied` and the only fix is `setcap`.
fn check_capabilities(binary: &str) -> Check {
    // Resolve the binary to an absolute path: getcap won't search $PATH.
    let resolved = match ShellCommand::new("which").arg(binary).output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => {
            return Check::fail(
                "Capabilities",
                &format!("cannot resolve '{}' in PATH", binary),
            );
        }
    };

    let output = match ShellCommand::new("getcap").arg(&resolved).output() {
        Ok(o) => o,
        Err(_) => {
            return Check::fail(
                "Capabilities",
                "'getcap' not found (install libcap2-bin / libcap)",
            );
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let has_net_admin = stdout.contains("cap_net_admin");
    let has_net_raw = stdout.contains("cap_net_raw");

    let fix_hint = format!("run: sudo setcap cap_net_admin,cap_net_raw+ep {}", resolved);

    if has_net_admin && has_net_raw {
        Check::ok(
            "Capabilities",
            &format!("cap_net_admin,cap_net_raw set on {}", resolved),
        )
    } else if !has_net_admin && !has_net_raw {
        Check::fail(
            "Capabilities",
            &format!(
                "missing cap_net_admin and cap_net_raw on {} ({})",
                resolved, fix_hint
            ),
        )
    } else {
        let missing = if has_net_admin {
            "cap_net_raw"
        } else {
            "cap_net_admin"
        };
        Check::fail(
            "Capabilities",
            &format!("missing {} on {} ({})", missing, resolved, fix_hint),
        )
    }
}

fn check_docker() -> Vec<Check> {
    vec![check_binary("docker", "--version")]
}

/// Firecracker boots a kernel directly (no firmware step like Cloud
/// Hypervisor), so its dependencies are: the `firecracker` binary, an
/// uncompressed `vmlinux` kernel, `/dev/kvm`, and the same TAP network
/// capabilities CH needs (Ring sets up the tap in-process via
/// `CAP_NET_ADMIN`/`CAP_NET_RAW`). Firecracker is experimental — these checks
/// only run when the runtime is explicitly enabled.
fn check_firecracker(config: &Config) -> Vec<Check> {
    let mut checks = Vec::new();

    let binary = config
        .server
        .runtime
        .firecracker
        .binary_path
        .as_deref()
        .unwrap_or("firecracker");
    checks.push(check_binary(binary, "--version"));

    checks.push(check_kvm());
    checks.push(check_capabilities(binary));

    let default_kernel = format!("{}/firecracker/vmlinux", get_config_dir());
    let kernel = config
        .server
        .runtime
        .firecracker
        .kernel_path
        .as_deref()
        .unwrap_or(&default_kernel);
    checks.push(check_file("Kernel", kernel));

    // Port forwarding is socat-based, same as CH.
    checks.push(check_socat());

    checks
}

fn check_cloud_hypervisor(config: &Config) -> Vec<Check> {
    let mut checks = Vec::new();

    let binary = config
        .server
        .runtime
        .cloud_hypervisor
        .binary_path
        .as_deref()
        .unwrap_or("cloud-hypervisor");
    checks.push(check_binary(binary, "--version"));

    checks.push(check_kvm());
    checks.push(check_capabilities(binary));
    checks.push(check_xorriso());
    checks.push(check_socat());

    let default_firmware = format!("{}/cloud-hypervisor/vmlinux", get_config_dir());
    let firmware = config
        .server
        .runtime
        .cloud_hypervisor
        .firmware_path
        .as_deref()
        .unwrap_or(&default_firmware);
    checks.push(check_file("Firmware", firmware));

    checks.push(check_virtiofsd());

    checks
}

/// xorriso is invoked to build the cloud-init NoCloud cidata ISO when a
/// deployment ships environment variables. Without it `environment: { ... }`
/// silently degrades into "VM boots without those vars set."
fn check_xorriso() -> Check {
    match ShellCommand::new("xorriso").arg("-version").output() {
        Ok(out) if out.status.success() => Check::ok(
            "xorriso",
            "available (used to build cloud-init cidata ISOs)",
        ),
        _ => Check::fail(
            "xorriso",
            "not found — environment variables won't be injected into VMs (apt install xorriso / dnf install xorriso)",
        ),
    }
}

/// socat is spawned (one process per port mapping) to forward `0.0.0.0:<published>`
/// on the host to `<guest_ip>:<target>` inside the VM. Without it, deployments
/// with `ports:` boot but stay unreachable from the host.
fn check_socat() -> Check {
    match ShellCommand::new("socat").arg("-V").output() {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stderr)
                .lines()
                .chain(String::from_utf8_lossy(&out.stdout).lines())
                .find(|l| l.contains("socat version"))
                .unwrap_or("")
                .trim()
                .to_string();
            Check::ok(
                "socat",
                if version.is_empty() {
                    "available (used to forward host ports to VM guest IPs)"
                } else {
                    &version
                },
            )
        }
        _ => Check::fail(
            "socat",
            "not found — `ports:` on cloud-hypervisor deployments won't be reachable from the host (apt install socat / dnf install socat)",
        ),
    }
}

fn check_virtiofsd() -> Check {
    let candidates = [
        "virtiofsd",
        "/usr/libexec/virtiofsd",
        "/usr/lib/qemu/virtiofsd",
    ];
    for path in &candidates {
        if let Ok(output) = std::process::Command::new(path).arg("--version").output()
            && output.status.success()
        {
            let version = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            return Check::ok("virtiofsd", &format!("{} ({})", version, path));
        }
    }
    Check::fail("virtiofsd", "not found (apt install virtiofsd)")
}

/// Server-level checks that apply regardless of which runtime is in use.
/// Today: `RING_SECRET_KEY` validation. Anything that touches a secret
/// (deployment env vars with `secretRef`, `POST /secrets`, ...) panics
/// when the key is missing or malformed; surface it here so operators
/// catch it before the first `ring apply`.
fn check_server() -> Vec<Check> {
    vec![match crate::models::secret::try_load_encryption_key() {
        Ok(_) => Check::ok("RING_SECRET_KEY", "set, decodes to a 32-byte AES-256 key"),
        Err(e) => Check::fail("RING_SECRET_KEY", &e),
    }]
}

/// Collect every diagnostic group. `Server` checks always run; runtime checks
/// only run for runtimes enabled in the config, so `ring init --runtime docker`
/// doesn't drown the operator in irrelevant Cloud Hypervisor failures. When no
/// runtime is enabled (e.g. a stub config) we fall back to checking all of them
/// — that matches the old behaviour and is the most useful default for a bare
/// `ring doctor`.
pub(crate) fn collect_checks(config: &Config) -> Vec<(&'static str, Vec<Check>)> {
    let docker_enabled = config.server.runtime.docker.enabled;
    let podman_enabled = config.server.runtime.podman.enabled;
    let ch_enabled = config.server.runtime.cloud_hypervisor.enabled;
    let fc_enabled = config.server.runtime.firecracker.enabled;
    let none_enabled = !docker_enabled && !podman_enabled && !ch_enabled && !fc_enabled;

    let mut groups: Vec<(&'static str, Vec<Check>)> = vec![("Server", check_server())];

    // Podman speaks the Docker API, so its host-side dependency is the same
    // `docker`/`podman` CLI presence check we already have for Docker.
    if docker_enabled || podman_enabled || none_enabled {
        groups.push(("Docker", check_docker()));
    }
    if ch_enabled || none_enabled {
        groups.push(("Cloud Hypervisor", check_cloud_hypervisor(config)));
    }
    // Firecracker is experimental, so it's only diagnosed when explicitly
    // enabled — never as part of the bare `ring doctor` fallback.
    if fc_enabled {
        groups.push(("Firecracker", check_firecracker(config)));
    }

    groups
}

/// Print a collected set of checks. Returns `true` if any check failed, so the
/// caller decides what a failure means: `ring doctor` exits non-zero, while
/// `ring init` only warns (init itself succeeded).
pub(crate) fn report_checks(groups: &[(&str, Vec<Check>)]) -> bool {
    let mut has_failure = false;
    for (runtime, checks) in groups {
        println!("{}", runtime);
        for check in checks {
            let icon = if check.passed { "+" } else { "-" };
            println!("  [{}] {}: {}", icon, check.name, check.detail);
            if !check.passed {
                has_failure = true;
            }
        }
        println!();
    }
    has_failure
}

pub(crate) fn execute(_args: &ArgMatches, config: Config) {
    let groups = collect_checks(&config);
    let has_failure = report_checks(&groups);

    if has_failure {
        std::process::exit(1);
    }
}
