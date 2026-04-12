use crate::config::config::Config;
use clap::{ArgMatches, Command};
use std::process::Command as ShellCommand;

pub(crate) fn command_config() -> Command {
    Command::new("doctor").about("Check system dependencies for configured runtimes")
}

struct Check {
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
    match std::fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(_) => Check::ok("KVM", "/dev/kvm accessible"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Check::fail("KVM", "/dev/kvm not found")
        }
        Err(e) => Check::fail(
            "KVM",
            &format!("/dev/kvm not accessible: {} (try: sudo usermod -aG kvm $USER)", e),
        ),
    }
}

fn check_docker() -> Vec<Check> {
    vec![check_binary("docker", "--version")]
}

fn check_cloud_hypervisor(config: &Config) -> Vec<Check> {
    let mut checks = Vec::new();

    let binary = config
        .runtime
        .cloud_hypervisor
        .binary_path
        .as_deref()
        .unwrap_or("cloud-hypervisor");
    checks.push(check_binary(binary, "--version"));

    checks.push(check_kvm());

    let default_firmware = format!(
        "{}/cloud-hypervisor/vmlinux",
        crate::config::config::get_config_dir()
    );
    let firmware = config
        .runtime
        .cloud_hypervisor
        .firmware_path
        .as_deref()
        .unwrap_or(&default_firmware);
    checks.push(check_file("Firmware", firmware));

    checks.push(check_virtiofsd());

    checks
}

fn check_virtiofsd() -> Check {
    let candidates = [
        "virtiofsd",
        "/usr/libexec/virtiofsd",
        "/usr/lib/qemu/virtiofsd",
    ];
    for path in &candidates {
        if let Ok(output) = std::process::Command::new(path).arg("--version").output() {
            if output.status.success() {
                let version = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                return Check::ok("virtiofsd", &format!("{} ({})", version, path));
            }
        }
    }
    Check::fail("virtiofsd", "not found (apt install virtiofsd)")
}

pub(crate) fn execute(_args: &ArgMatches, config: Config) {
    let mut all_checks: Vec<(&str, Vec<Check>)> = Vec::new();

    all_checks.push(("Docker", check_docker()));
    all_checks.push(("Cloud Hypervisor", check_cloud_hypervisor(&config)));

    let mut has_failure = false;

    for (runtime, checks) in &all_checks {
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

    if has_failure {
        std::process::exit(1);
    }
}
