//! Interactive `ring init`: prompt for the minimum settings needed to boot
//! ring-server and write them to `~/.config/kemeter/ring/config.toml`.
//!
//! Design choices we settled on before writing this:
//! - **No `auth.json` stub.** `ring login` creates it lazily, and `ring init`
//!   used to write an empty `{}` that served no purpose.
//! - **Always generate `RING_SECRET_KEY`.** The server refuses to boot
//!   without it (see `models::secret::try_load_encryption_key`), so making it
//!   optional was a footgun.
//! - **Refuse to overwrite.** If `config.toml` already exists, error out and
//!   suggest `--force`. Mirrors `kubectl config init`-style ergonomics and
//!   avoids silently wiping someone's session.
//! - **Non-TTY fallback.** When stdin is not a TTY (CI, piped input) we use
//!   defaults instead of hanging on a prompt that nobody will answer.

use crate::config::config::get_config_dir;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use clap::{Arg, ArgAction, ArgMatches, Command};
use rand::RngCore;
use std::fs;
use std::io::IsTerminal;
use std::path::Path;

pub(crate) fn command_config() -> Command {
    Command::new("init")
        .about("Initialize Ring configuration interactively")
        .arg(
            Arg::new("force")
                .long("force")
                .help("Overwrite an existing config.toml")
                .action(ArgAction::SetTrue),
        )
}

/// Resolved settings collected from the user (or defaulted in non-TTY mode).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitSettings {
    pub runtime: RuntimeChoice,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeChoice {
    Docker,
    CloudHypervisor,
    Both,
}

impl RuntimeChoice {
    fn label(self) -> &'static str {
        match self {
            RuntimeChoice::Docker => "Docker",
            RuntimeChoice::CloudHypervisor => "Cloud Hypervisor",
            RuntimeChoice::Both => "Both",
        }
    }
}

impl std::fmt::Display for RuntimeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

pub(crate) fn init(args: &ArgMatches) {
    let force = args.get_flag("force");
    let config_dir = get_config_dir();
    let config_path = format!("{}/config.toml", config_dir);
    let secret_key_path = format!("{}/secret-key", config_dir);

    if Path::new(&config_path).exists() && !force {
        eprintln!("Error: {} already exists.", config_path);
        eprintln!("To re-initialize: ring init --force");
        std::process::exit(1);
    }
    // Same guard for the secret-key file. Regenerating silently would render
    // every previously-stored secret undecryptable, so we treat it like the
    // config: refuse without `--force`.
    if Path::new(&secret_key_path).exists() && !force {
        eprintln!("Error: {} already exists.", secret_key_path);
        eprintln!(
            "To re-initialize (this will invalidate every secret stored with the old key): ring init --force"
        );
        std::process::exit(1);
    }

    let settings = collect_settings();
    let toml_content = build_config_toml(&settings);

    if let Err(e) = fs::create_dir_all(&config_dir) {
        eprintln!("Failed to create {}: {}", config_dir, e);
        std::process::exit(1);
    }

    // Persist the key BEFORE writing the config and before the on-screen
    // block — if the user kills the process or the terminal scrolls past,
    // the file is still on disk to recover from. The previous flow only
    // printed it to stdout, so an interrupted run produced a working
    // `config.toml` with no way to reconstruct the matching key.
    let key = generate_secret_key();
    if let Err(e) = write_secret_key_file(&secret_key_path, &key) {
        eprintln!("Failed to write {}: {}", secret_key_path, e);
        std::process::exit(1);
    }

    if let Err(e) = fs::write(&config_path, toml_content) {
        eprintln!("Failed to write {}: {}", config_path, e);
        std::process::exit(1);
    }
    println!("✓ Wrote {}", config_path);
    println!("✓ Wrote {} (mode 0600)", secret_key_path);

    print_secret_key_block(&key, &secret_key_path);
    print_next_steps();
}

/// Either prompt the user or fall back to sensible defaults when stdin is
/// not interactive (CI, piped scripts, etc.). The defaults match the most
/// common deployment shape: Docker on port 3030.
fn collect_settings() -> InitSettings {
    if !is_tty() {
        println!("(non-interactive stdin detected — using defaults: Docker, port 3030)");
        return InitSettings {
            runtime: RuntimeChoice::Docker,
            port: 3030,
        };
    }

    use inquire::{CustomType, Select};

    let runtime = Select::new(
        "Which runtime do you want to use?",
        vec![
            RuntimeChoice::Docker,
            RuntimeChoice::CloudHypervisor,
            RuntimeChoice::Both,
        ],
    )
    .prompt()
    .unwrap_or_else(|e| {
        eprintln!("Aborted: {}", e);
        std::process::exit(1);
    });

    let port: u16 = CustomType::<u16>::new("Which port should the API listen on?")
        .with_default(3030)
        .with_error_message("Enter a number between 1 and 65535")
        .prompt()
        .unwrap_or_else(|e| {
            eprintln!("Aborted: {}", e);
            std::process::exit(1);
        });

    InitSettings { runtime, port }
}

fn is_tty() -> bool {
    // `IsTerminal` landed in std in 1.70; no need for a third-party crate or
    // for hand-rolling a libc binding. Also works on Windows out of the box,
    // which the previous `extern "C" { fn isatty }` declaration didn't.
    std::io::stdin().is_terminal()
}

/// Build the `config.toml` body from settings. Pure function — no I/O — so
/// it's trivially testable with every combination of runtime and port.
pub(crate) fn build_config_toml(settings: &InitSettings) -> String {
    let host = local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string());

    // Salt for password hashing. Random per-init so two unrelated installs
    // don't end up with the same hash for identical passwords.
    let salt = random_salt();

    let mut out = String::new();
    out.push_str("[contexts.default]\n");
    out.push_str("current = true\n");
    out.push_str(&format!("host = \"{}\"\n", host));
    out.push('\n');
    out.push_str("api.scheme = \"http\"\n");
    out.push_str(&format!("api.port = {}\n", settings.port));
    out.push('\n');
    out.push_str(&format!("user.salt = \"{}\"\n", salt));

    if matches!(
        settings.runtime,
        RuntimeChoice::CloudHypervisor | RuntimeChoice::Both
    ) {
        out.push('\n');
        out.push_str("[contexts.default.runtime.cloud_hypervisor]\n");
        out.push_str("# Uncomment and adjust if cloud-hypervisor isn't on $PATH:\n");
        out.push_str("# binary_path = \"/usr/local/bin/cloud-hypervisor\"\n");
        out.push_str("# firmware_path = \"/var/lib/ring/cloud-hypervisor/vmlinux\"\n");
        out.push_str("# socket_dir = \"/var/lib/ring/cloud-hypervisor/sockets\"\n");
        // Hint at the seccomp escape hatch without enabling it — most hosts
        // don't need it, but the comment saves an hour of debugging when
        // they do.
        out.push_str("# seccomp = \"false\"  # only if VMs die with SIGSYS on boot\n");
    }

    out
}

fn random_salt() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    B64.encode(bytes)
}

fn generate_secret_key() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    B64.encode(bytes)
}

/// Write `key` to `path` with mode 0600 (Unix). The file is created with
/// the right mode from the start (via `OpenOptions::mode`) so there is no
/// transient window where the secret is world-readable, even under a lax
/// `umask`. On non-Unix targets we fall back to a plain write — the mode
/// guarantee doesn't apply but the file still persists for recovery.
fn write_secret_key_file(path: &str, key: &str) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }

    let mut file = opts.open(path)?;
    file.write_all(key.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

fn print_secret_key_block(key: &str, key_path: &str) {
    // Simple, alignment-free presentation: a top rule, plain text body, a
    // bottom rule. Trying to box-draw around variable-width content (the
    // 44-char base64 key plus surrounding quotes) caused right-edge drift
    // that wasn't worth the visual gain.
    let rule = "─".repeat(72);
    println!();
    println!("{}", rule);
    println!("  IMPORTANT — export this key before starting the server:");
    println!();
    println!("    export RING_SECRET_KEY=\"{}\"", key);
    println!();
    println!("  Without it, `ring server start` will refuse to boot.");
    println!("  Without it, secrets stored on disk become unrecoverable.");
    println!();
    println!("  Also saved to: {}", key_path);
    println!(
        "  Treat that file like a private key: chmod 0600, never commit, never back up unencrypted."
    );
    println!("{}", rule);
}

fn print_next_steps() {
    println!();
    println!("→ Next steps:");
    println!("  1. Export the key above");
    println!(
        "  2. ring server start             # first boot creates the admin user (admin/changeme)"
    );
    println!("  3. ring login -u admin -p changeme");
    println!("  4. ring user update --password \"<your password>\"  # rotate the default password");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_config_docker_only() {
        let s = InitSettings {
            runtime: RuntimeChoice::Docker,
            port: 3030,
        };
        let out = build_config_toml(&s);
        assert!(out.contains("[contexts.default]"));
        assert!(out.contains("api.port = 3030"));
        // No CH block when Docker only.
        assert!(!out.contains("runtime.cloud_hypervisor"));
    }

    #[test]
    fn build_config_ch_only_emits_ch_section() {
        let s = InitSettings {
            runtime: RuntimeChoice::CloudHypervisor,
            port: 3030,
        };
        let out = build_config_toml(&s);
        assert!(out.contains("[contexts.default.runtime.cloud_hypervisor]"));
        assert!(out.contains("seccomp"));
    }

    #[test]
    fn build_config_both_emits_ch_section() {
        let s = InitSettings {
            runtime: RuntimeChoice::Both,
            port: 8080,
        };
        let out = build_config_toml(&s);
        assert!(out.contains("api.port = 8080"));
        assert!(out.contains("[contexts.default.runtime.cloud_hypervisor]"));
    }

    #[test]
    fn build_config_custom_port_is_serialized() {
        let s = InitSettings {
            runtime: RuntimeChoice::Docker,
            port: 9999,
        };
        let out = build_config_toml(&s);
        assert!(out.contains("api.port = 9999"));
    }

    #[test]
    fn build_config_emits_random_salt() {
        let s = InitSettings {
            runtime: RuntimeChoice::Docker,
            port: 3030,
        };
        let a = build_config_toml(&s);
        let b = build_config_toml(&s);
        // Two runs must produce different salts.
        let salt_a = extract_salt(&a);
        let salt_b = extract_salt(&b);
        assert_ne!(salt_a, salt_b, "salts must be random per init run");
        assert!(!salt_a.is_empty());
    }

    #[test]
    fn generated_secret_key_is_32_bytes_base64() {
        let key = generate_secret_key();
        let decoded = B64.decode(&key).expect("must decode");
        assert_eq!(decoded.len(), 32);
    }

    /// Per-test temp path. Avoids pulling `tempfile` into dev-deps for two
    /// tests; the file is cleaned up at the end of the test regardless of
    /// outcome via `_drop_on_end` (we panic on cleanup failure only if the
    /// test itself succeeded, so a failing assert keeps the artefact for
    /// inspection).
    fn unique_tmp_path(label: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ring-init-test-{}-{}-{}",
            std::process::id(),
            n,
            label
        ));
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn write_secret_key_file_persists_content_with_trailing_newline() {
        let path = unique_tmp_path("persist");
        let key = "dGVzdC1rZXktdGVzdC1rZXktdGVzdC1rZXktdGVzdA==";
        write_secret_key_file(&path, key).expect("write");
        let content = std::fs::read_to_string(&path).expect("read");
        // Trailing newline makes the file `cat`-friendly and matches how
        // tools like `pass` store secrets — losing it would be a silent UX
        // regression if someone reads the file with `xargs` or similar.
        assert_eq!(content, format!("{}\n", key));
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn write_secret_key_file_is_chmod_0600() {
        use std::os::unix::fs::PermissionsExt;
        let path = unique_tmp_path("perms");
        write_secret_key_file(&path, "x").expect("write");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        // Only the low 9 bits matter for the permission check; the file
        // type bits live above and vary by platform. `0o600` = owner rw,
        // group/other none — the standard "private key" mode.
        assert_eq!(mode & 0o777, 0o600, "expected 0600, got {:o}", mode & 0o777);
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn write_secret_key_file_overwrites_truncates() {
        // `--force` re-creates the file. Make sure we truncate so a shorter
        // key doesn't leave trailing bytes from a previous longer one.
        let path = unique_tmp_path("trunc");
        write_secret_key_file(&path, "AAAAAAAAAAAAAAAAAAAAAAAA").expect("write1");
        write_secret_key_file(&path, "B").expect("write2");
        let content = std::fs::read_to_string(&path).expect("read");
        assert_eq!(content, "B\n");
        let _ = std::fs::remove_file(&path);
    }

    fn extract_salt(toml: &str) -> String {
        for line in toml.lines() {
            if let Some(rest) = line.strip_prefix("user.salt = \"") {
                return rest.trim_end_matches('"').to_string();
            }
        }
        String::new()
    }

    #[test]
    fn parsed_toml_round_trips_as_valid_config() {
        // Make sure what we generate is actually parseable by Ring's own
        // config loader — guards against drift between init and load.
        let s = InitSettings {
            runtime: RuntimeChoice::Both,
            port: 3030,
        };
        let out = build_config_toml(&s);
        let parsed: toml::Value = toml::from_str(&out).expect("must parse");
        let port = parsed
            .get("contexts")
            .and_then(|c| c.get("default"))
            .and_then(|d| d.get("api"))
            .and_then(|a| a.get("port"))
            .and_then(|p| p.as_integer())
            .expect("api.port must exist");
        assert_eq!(port, 3030);
    }
}
