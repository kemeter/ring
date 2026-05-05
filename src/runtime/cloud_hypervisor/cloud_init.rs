//! cloud-init NoCloud datasource generation for VM runtimes.
//!
//! Currently only used by Cloud Hypervisor, but the format (NoCloud) and the
//! ISO output are standard — Firecracker and any future VM runtime can attach
//! the same ISO as a second drive. When that day comes, lift this file to
//! `src/runtime/cloud_init.rs` and update the `use` paths in CH (and the new
//! runtime). The implementation has zero CH-specific code.
//!
//! Builds a small ISO image (`cidata.iso`) that the guest mounts at boot via
//! cloud-init's NoCloud datasource. The ISO contains:
//!
//! - `meta-data`  — minimal, just the instance-id (required by the spec)
//! - `user-data`  — cloud-config YAML that writes `/etc/ring/env` and a
//!   systemd drop-in so every service inherits the variables
//!
//! The disk is attached read-only as a second drive in `VmConfig.disks`. The
//! guest must have cloud-init installed (true for every standard cloud image:
//! Ubuntu Cloud, Fedora Cloud, Debian Cloud, Cirros, ...).
//!
//! The ISO is built with `xorriso` because it's the most universally available
//! tool that can produce ISO9660 with a custom volume label (NoCloud requires
//! `CIDATA`). `cloud-localds` from the `cloud-utils` package is the more
//! ergonomic alternative but is not always installed.

use crate::models::deployments::{Deployment, EnvValue};
use crate::runtime::error::RuntimeError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// One virtio-fs mount the guest needs to perform at boot. The host side
/// (virtiofsd, socket) is set up before calling [`build_cidata_iso`]; this
/// struct only carries what cloud-init needs to mount it inside the VM.
pub(crate) struct GuestMount {
    pub tag: String,
    pub destination: String,
    pub read_only: bool,
}

/// Build a NoCloud cidata ISO from the deployment's environment map and
/// virtio-fs mounts, returning its path. The caller is responsible for
/// cleaning the file up when the VM stops.
pub(crate) async fn build_cidata_iso(
    instance_id: &str,
    deployment: &Deployment,
    mounts: &[GuestMount],
    output_dir: &Path,
) -> Result<PathBuf, RuntimeError> {
    // Filter to plain values. Any unresolved SecretRef at this stage is a
    // scheduler bug — secrets must already be resolved before reaching the
    // runtime (same contract as the Docker runtime).
    let mut envs: Vec<(String, String)> = Vec::with_capacity(deployment.environment.len());
    for (key, value) in &deployment.environment {
        match value {
            EnvValue::Plain(v) => envs.push((key.clone(), v.clone())),
            EnvValue::SecretRef { .. } => {
                return Err(RuntimeError::Other(format!(
                    "unresolved secretRef for '{}' reached the cloud-hypervisor runtime",
                    key
                )));
            }
        }
    }

    // Stage the two files cloud-init's NoCloud expects on the ISO root.
    let staging = output_dir.join(format!("{}.cidata", instance_id));
    if staging.exists() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
    }
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(RuntimeError::Io)?;

    let user_data = render_user_data(&envs, mounts);
    let meta_data = render_meta_data(instance_id);

    tokio::fs::write(staging.join("user-data"), user_data)
        .await
        .map_err(RuntimeError::Io)?;
    tokio::fs::write(staging.join("meta-data"), meta_data)
        .await
        .map_err(RuntimeError::Io)?;

    let iso_path = output_dir.join(format!("{}.cidata.iso", instance_id));
    if iso_path.exists() {
        let _ = tokio::fs::remove_file(&iso_path).await;
    }

    let staging_str = staging
        .to_str()
        .ok_or_else(|| RuntimeError::Other(format!("non-UTF-8 staging path: {:?}", staging)))?;
    let iso_str = iso_path
        .to_str()
        .ok_or_else(|| RuntimeError::Other(format!("non-UTF-8 iso path: {:?}", iso_path)))?;

    // xorriso flags: -as mkisofs gives us the classic mkisofs CLI surface.
    // -volid CIDATA is mandatory for the NoCloud datasource.
    let output = Command::new("xorriso")
        .args([
            "-as",
            "mkisofs",
            "-output",
            iso_str,
            "-volid",
            "CIDATA",
            "-joliet",
            "-rock",
            staging_str,
        ])
        .output()
        .await
        .map_err(|e| {
            RuntimeError::Other(format!(
                "xorriso not available: {} (install xorriso package)",
                e
            ))
        })?;

    // The staging dir is no longer needed once the ISO is built.
    let _ = tokio::fs::remove_dir_all(&staging).await;

    if !output.status.success() {
        return Err(RuntimeError::Other(format!(
            "xorriso failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(iso_path)
}

fn render_meta_data(instance_id: &str) -> String {
    // instance-id is the only mandatory NoCloud field. local-hostname is a
    // nice-to-have so that `hostname` inside the VM matches the Ring instance.
    format!(
        "instance-id: {id}\nlocal-hostname: {id}\n",
        id = instance_id
    )
}

fn render_user_data(envs: &[(String, String)], mounts: &[GuestMount]) -> String {
    // Two-pronged delivery to cover both worlds:
    //   1. /etc/ring/env in KEY=value form, sourced by a systemd drop-in
    //      (`EnvironmentFile=`) so any unit using `[Service]` picks it up.
    //   2. /etc/profile.d/ring-env.sh so interactive shells / scripts started
    //      manually (e.g. `ssh user@vm` then run a binary) also see them.
    //
    // Values are quoted to survive whitespace, '#', '=' and other surprises.
    let mut env_file = String::new();
    let mut profile_script = String::from("# Auto-generated by Ring — do not edit.\n");
    for (k, v) in envs {
        env_file.push_str(&format!("{}={}\n", k, shell_quote(v)));
        profile_script.push_str(&format!("export {}={}\n", k, shell_quote(v)));
    }

    // The systemd drop-in targets the unit users typically run their app from.
    // We pick `multi-user.target.wants/*` indirectly via a global drop-in on
    // `service.d/` which applies to every system service.
    let dropin = "[Service]\nEnvironmentFile=-/etc/ring/env\n";

    // YAML-escape the file contents by base64-encoding them. cloud-init
    // supports `encoding: b64` for write_files, which removes any indentation
    // / quoting hazard.
    let env_b64 = base64_encode(&env_file);
    let profile_b64 = base64_encode(&profile_script);
    let dropin_b64 = base64_encode(dropin);

    // cloud-init's `mounts:` module owns fstab; entries become persistent
    // mounts honoured at every boot. We also emit explicit `mount` commands
    // in `runcmd` so the share is available *immediately* after first boot
    // (cloud-init runs `mounts` before `runcmd`, but the order isn't a
    // contract we want to lean on for "is the dir there yet").
    let mut mounts_block = String::new();
    let mut runcmd_lines = String::from("  - [systemctl, daemon-reload]\n");
    if !mounts.is_empty() {
        mounts_block.push_str("mounts:\n");
        for m in mounts {
            let opts = if m.read_only { "ro" } else { "defaults" };
            // [device, mountpoint, fstype, opts, dump, fsck_pass]
            mounts_block.push_str(&format!(
                "  - [\"{tag}\", \"{dest}\", \"virtiofs\", \"{opts}\", \"0\", \"0\"]\n",
                tag = m.tag,
                dest = m.destination,
                opts = opts,
            ));
            runcmd_lines.push_str(&format!(
                "  - [mkdir, -p, \"{dest}\"]\n  - [mount, -t, virtiofs, -o, \"{opts}\", \"{tag}\", \"{dest}\"]\n",
                tag = m.tag,
                dest = m.destination,
                opts = opts,
            ));
        }
    }

    format!(
        "#cloud-config
write_files:
  - path: /etc/ring/env
    permissions: '0600'
    encoding: b64
    content: {env_b64}
  - path: /etc/profile.d/ring-env.sh
    permissions: '0644'
    encoding: b64
    content: {profile_b64}
  - path: /etc/systemd/system/service.d/ring-env.conf
    permissions: '0644'
    encoding: b64
    content: {dropin_b64}
{mounts_block}runcmd:
{runcmd_lines}"
    )
}

/// Single-quote a value for use in `KEY='value'` lines, escaping inner single
/// quotes the bash way: `'\''`.
fn shell_quote(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('\'');
    for ch in v.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Tiny standalone base64 encoder so the runtime stays free of an extra
/// crate just for this. Uses the standard alphabet with padding.
fn base64_encode(input: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let b = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(ALPHABET[((b >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((b >> 12) & 0x3f) as usize] as char);
        out.push(ALPHABET[((b >> 6) & 0x3f) as usize] as char);
        out.push(ALPHABET[(b & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let b = (bytes[i] as u32) << 16;
        out.push(ALPHABET[((b >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((b >> 12) & 0x3f) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let b = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(ALPHABET[((b >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((b >> 12) & 0x3f) as usize] as char);
        out.push(ALPHABET[((b >> 6) & 0x3f) as usize] as char);
        out.push('=');
    }
    out
}

/// Suppress unused-import warning until the loader uses HashMap directly.
#[allow(dead_code)]
fn _suppress_unused() {
    let _ = HashMap::<String, String>::new();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_plain() {
        assert_eq!(shell_quote("simple"), "'simple'");
    }

    #[test]
    fn shell_quote_with_quote() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode(""), "");
        assert_eq!(base64_encode("f"), "Zg==");
        assert_eq!(base64_encode("fo"), "Zm8=");
        assert_eq!(base64_encode("foo"), "Zm9v");
        assert_eq!(base64_encode("foob"), "Zm9vYg==");
        assert_eq!(base64_encode("fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode("foobar"), "Zm9vYmFy");
    }

    #[test]
    fn user_data_contains_b64_env_payload() {
        let envs = vec![("FOO".to_string(), "bar".to_string())];
        let yaml = render_user_data(&envs, &[]);
        assert!(yaml.starts_with("#cloud-config"));
        assert!(yaml.contains("/etc/ring/env"));
        assert!(!yaml.contains("EnvironmentFile=-/etc/ring/env")); // dropin is base64'd
        // The plain "FOO='bar'" string must be base64-encoded inside the YAML.
        let expected = base64_encode("FOO='bar'\n");
        assert!(yaml.contains(&expected), "payload not in yaml: {}", yaml);
    }

    #[test]
    fn user_data_emits_mounts_and_runcmd_for_virtiofs() {
        let mounts = vec![
            GuestMount {
                tag: "bind-0".to_string(),
                destination: "/data".to_string(),
                read_only: false,
            },
            GuestMount {
                tag: "cfg-1".to_string(),
                destination: "/etc/app".to_string(),
                read_only: true,
            },
        ];
        let yaml = render_user_data(&[], &mounts);
        // fstab entries via cloud-init `mounts:` module
        assert!(yaml.contains("mounts:"));
        assert!(yaml.contains("\"bind-0\", \"/data\", \"virtiofs\", \"defaults\""));
        assert!(yaml.contains("\"cfg-1\", \"/etc/app\", \"virtiofs\", \"ro\""));
        // Immediate mount via runcmd
        assert!(yaml.contains("[mkdir, -p, \"/data\"]"));
        assert!(yaml.contains("[mount, -t, virtiofs, -o, \"defaults\", \"bind-0\", \"/data\"]"));
        assert!(yaml.contains("[mount, -t, virtiofs, -o, \"ro\", \"cfg-1\", \"/etc/app\"]"));
    }

    #[test]
    fn user_data_omits_mounts_block_when_empty() {
        let yaml = render_user_data(&[], &[]);
        assert!(!yaml.contains("mounts:"));
        assert!(!yaml.contains("virtiofs"));
    }

    #[test]
    fn meta_data_includes_instance_id() {
        let md = render_meta_data("ch-deadbeef");
        assert!(md.contains("instance-id: ch-deadbeef"));
        assert!(md.contains("local-hostname: ch-deadbeef"));
    }

    fn xorriso_or_skip(test: &str) -> bool {
        if std::process::Command::new("xorriso")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping {}: xorriso not installed", test);
            return true;
        }
        false
    }

    fn empty_deployment(id: &str) -> crate::models::deployments::Deployment {
        crate::models::deployments::Deployment {
            id: id.to_string(),
            created_at: String::new(),
            updated_at: None,
            status: crate::models::deployments::DeploymentStatus::Creating,
            restart_count: 0,
            namespace: "default".to_string(),
            name: "test".to_string(),
            image: "ubuntu.raw".to_string(),
            config: None,
            runtime: "cloud-hypervisor".to_string(),
            kind: "worker".to_string(),
            replicas: 1,
            command: vec![],
            instances: vec![],
            labels: std::collections::HashMap::new(),
            environment: std::collections::HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: vec![],
            resources: None,
            image_digest: None,
            ports: vec![],
            pending_events: vec![],
            parent_id: None,
        }
    }

    #[tokio::test]
    async fn build_cidata_iso_with_mounts_writes_real_iso() {
        if xorriso_or_skip("build_cidata_iso_with_mounts_writes_real_iso") {
            return;
        }

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("ring-cidata-{}-{}", std::process::id(), nanos));
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let dep = empty_deployment("ch-cidata-test");
        let mounts = vec![GuestMount {
            tag: "bind-0".to_string(),
            destination: "/data".to_string(),
            read_only: false,
        }];

        let iso = build_cidata_iso("ch-cidata-test", &dep, &mounts, &dir)
            .await
            .expect("xorriso should produce an ISO");

        let meta = tokio::fs::metadata(&iso).await.unwrap();
        assert!(meta.is_file(), "iso should be a file");
        assert!(meta.len() > 0, "iso should not be empty");
        // ISO9660 magic "CD001" lives at offset 0x8001 in the volume descriptor.
        let bytes = tokio::fs::read(&iso).await.unwrap();
        assert!(bytes.len() > 0x8006);
        assert_eq!(&bytes[0x8001..0x8006], b"CD001", "missing ISO9660 magic");
        // The volume label is at offset 0x8028, padded to 32 bytes — we want
        // CIDATA so cloud-init's NoCloud datasource picks it up.
        let label = std::str::from_utf8(&bytes[0x8028..0x8028 + 6]).unwrap();
        assert_eq!(label, "CIDATA");

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn build_cidata_iso_skips_when_nothing_to_emit() {
        if xorriso_or_skip("build_cidata_iso_skips_when_nothing_to_emit") {
            return;
        }
        // No env, no mounts: build_cidata_iso still produces an ISO (callers
        // decide whether to invoke it). What matters is that the function
        // doesn't crash on an empty input.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ring-cidata-empty-{}-{}",
            std::process::id(),
            nanos
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let dep = empty_deployment("ch-cidata-empty");
        let iso = build_cidata_iso("ch-cidata-empty", &dep, &[], &dir)
            .await
            .expect("empty cidata should still build");
        assert!(iso.exists());

        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
