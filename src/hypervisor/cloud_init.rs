//! cloud-init NoCloud datasource generation for VM runtimes.
//!
//! Shared by every KVM-backed runtime (Cloud Hypervisor and Firecracker): the
//! format (NoCloud) and the ISO output are standard, so each VM runtime attaches
//! the same `cidata.iso` as an extra drive. The implementation has zero
//! runtime-specific code — callers pass a `Deployment`, the virtio-fs mounts to
//! perform in-guest, and an optional static network config.
//!
//! Builds a small disk image (kept as `cidata.iso` for callers) that the guest
//! mounts at boot via cloud-init's NoCloud datasource. It contains:
//!
//! - `meta-data`  — minimal, just the instance-id (required by the spec)
//! - `user-data`  — cloud-config YAML that writes `/etc/ring/env` and a
//!   systemd drop-in so every service inherits the variables
//!
//! The disk is attached read-only as a second drive in `VmConfig.disks`. The
//! guest must have cloud-init installed (true for every standard cloud image:
//! Ubuntu Cloud, Fedora Cloud, Debian Cloud, Cirros, ...).
//!
//! The image is an ext4 filesystem labelled `CIDATA`, built with `mke2fs -d`
//! (e2fsprogs, userspace — no mount, no root). NoCloud accepts any labelled
//! filesystem; ext4 is chosen because minimal guest kernels (e.g. the
//! Firecracker CI vmlinux) ship neither iso9660 nor vfat — only ext4, which is
//! what the root disk uses — so an ISO/FAT datasource would be unmountable.

use crate::hypervisor::error::RuntimeError;
use crate::models::deployments::{Deployment, EnvValue};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// How the guest reaches a mount's backing storage.
///
/// Cloud Hypervisor shares host directories over **virtio-fs** (the `tag` is the
/// virtiofsd mount tag). Firecracker has no virtio-fs, so it attaches each
/// volume as a **virtio-block** device and the guest mounts the resulting
/// `/dev/vdX` as an ext4 filesystem. The cloud-init renderer emits the right
/// fstab/`mount` directives for either.
pub(crate) enum MountTransport {
    /// `source` is a virtiofs tag, mounted with `-t virtiofs`.
    Virtiofs,
    /// `source` is a block device path (e.g. `/dev/vdb`), mounted with `-t ext4`.
    Block,
}

/// One mount the guest needs to perform at boot. The host side (virtiofsd +
/// socket for virtio-fs, or the attached block device for virtio-block) is set
/// up before calling [`build_cidata_iso`]; this struct only carries what
/// cloud-init needs to mount it inside the VM.
pub(crate) struct GuestMount {
    /// The mount source as the guest sees it: a virtiofs tag, or a block device
    /// path like `/dev/vdb`.
    pub source: String,
    pub destination: String,
    pub read_only: bool,
    pub transport: MountTransport,
}

impl GuestMount {
    /// A virtio-fs mount (Cloud Hypervisor): `tag` is the virtiofsd mount tag.
    pub fn virtiofs(tag: String, destination: String, read_only: bool) -> Self {
        Self {
            source: tag,
            destination,
            read_only,
            transport: MountTransport::Virtiofs,
        }
    }

    /// A virtio-block mount (Firecracker): `device` is the guest block device
    /// path (e.g. `/dev/vdb`), mounted as ext4.
    pub fn block(device: String, destination: String, read_only: bool) -> Self {
        Self {
            source: device,
            destination,
            read_only,
            transport: MountTransport::Block,
        }
    }
}

/// Static network config Ring asks the guest to apply on its primary NIC.
/// All values come from `hypervisor::host_net::InstanceNet`, propagated through
/// the runtime layer so cloud-init can write a netplan/networkd dropin.
pub(crate) struct GuestNet {
    pub guest_ip: String,
    pub host_ip: String,
    pub prefix_len: u8,
    #[allow(dead_code)] // Rendered into the NoCloud network config; not read back.
    pub mac: String,
}

/// Build a NoCloud cidata ISO from the deployment's environment map,
/// virtio-fs mounts and (optionally) a static network configuration,
/// returning its path. The caller is responsible for cleaning the file up
/// when the VM stops.
pub(crate) async fn build_cidata_iso(
    instance_id: &str,
    deployment: &Deployment,
    mounts: &[GuestMount],
    net: Option<&GuestNet>,
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
                    "unresolved secretRef for '{}' reached the VM runtime",
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

    let user_data = render_user_data(&envs, mounts, net);
    let meta_data = render_meta_data(instance_id);

    tokio::fs::write(staging.join("user-data"), user_data)
        .await
        .map_err(RuntimeError::Io)?;
    tokio::fs::write(staging.join("meta-data"), meta_data)
        .await
        .map_err(RuntimeError::Io)?;

    // Build the NoCloud datasource as an ext4 image rather than ISO9660.
    // NoCloud accepts any labelled filesystem, but minimal guest kernels (e.g.
    // the Firecracker CI vmlinux) ship neither iso9660 nor vfat — only ext4 (it's
    // what the root disk uses), so an ISO/FAT cidata fails to mount and the guest
    // never gets its network config. The file keeps the `.cidata.iso` name for
    // callers that reference it, but its content is ext4 labelled CIDATA.
    let img_path = output_dir.join(format!("{}.cidata.iso", instance_id));
    if img_path.exists() {
        let _ = tokio::fs::remove_file(&img_path).await;
    }
    let img_str = img_path
        .to_str()
        .ok_or_else(|| RuntimeError::Other(format!("non-UTF-8 cidata path: {:?}", img_path)))?;

    let result = build_ext4_cidata(img_str, &staging).await;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    result?;

    Ok(img_path)
}

/// Create an ext4 image labelled `CIDATA` containing the staged user-data and
/// meta-data, using `mke2fs -d` — no mounting, no root.
///
/// ext4 is chosen over ISO9660/vfat because it's the one filesystem every Linux
/// guest kernel can mount (it's what the root disk uses). Minimal microVM
/// kernels (e.g. the Firecracker CI vmlinux) frequently ship neither iso9660
/// nor vfat, leaving the NoCloud datasource unreadable; ext4 always works.
/// NoCloud locates the datasource by the `CIDATA` label regardless of fs type.
async fn build_ext4_cidata(img_str: &str, staging: &Path) -> Result<(), RuntimeError> {
    let staging_str = staging
        .to_str()
        .ok_or_else(|| RuntimeError::Other(format!("non-UTF-8 staging path: {:?}", staging)))?;

    // Ship a pinned mke2fs.conf next to the staging dir. Some hosts carry an
    // /etc/mke2fs.conf newer than their mke2fs binary, defining ext4 features
    // (orphan_file, metadata_csum_seed) the binary rejects with a misleading
    // "Invalid filesystem option set". Pointing MKE2FS_CONFIG at our own file
    // makes the build reproducible and avoids that mismatch.
    let conf_path = staging.with_extension("mke2fs.conf");
    let conf = "[defaults]\n\
        \tbase_features = sparse_super,large_file,filetype,resize_inode,dir_index,ext_attr\n\
        \tdefault_mntopts = acl,user_xattr\n\
        \tblocksize = 1024\n\
        \tinode_size = 256\n\
        [fs_types]\n\
        \text4 = {\n\
        \t\tfeatures = has_journal,extent,huge_file,flex_bg,metadata_csum,64bit,dir_nlink,extra_isize\n\
        \t}\n";
    tokio::fs::write(&conf_path, conf)
        .await
        .map_err(RuntimeError::Io)?;

    // -d seeds the fs from the staging dir at creation (userspace, no mount).
    // 1 MiB holds the two small text files comfortably. -L sets the CIDATA label
    // NoCloud probes for. -F since the target is a regular file, -q for quiet.
    let mkfs = Command::new("mke2fs")
        .env("MKE2FS_CONFIG", &conf_path)
        .args([
            "-q",
            "-F",
            "-t",
            "ext4",
            "-L",
            "CIDATA",
            "-d",
            staging_str,
            img_str,
            "1M",
        ])
        .output()
        .await
        .map_err(|e| {
            RuntimeError::Other(format!("mke2fs not available: {} (install e2fsprogs)", e))
        })?;
    let _ = tokio::fs::remove_file(&conf_path).await;
    if !mkfs.status.success() {
        return Err(RuntimeError::Other(format!(
            "mke2fs cidata failed: {}",
            String::from_utf8_lossy(&mkfs.stderr).trim()
        )));
    }

    Ok(())
}

fn render_meta_data(instance_id: &str) -> String {
    // instance-id is the only mandatory NoCloud field. local-hostname is a
    // nice-to-have so that `hostname` inside the VM matches the Ring instance.
    format!(
        "instance-id: {id}\nlocal-hostname: {id}\n",
        id = instance_id
    )
}

fn render_user_data(
    envs: &[(String, String)],
    mounts: &[GuestMount],
    net: Option<&GuestNet>,
) -> String {
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

    // Static network configuration on the primary NIC. We avoid declarative
    // formats (netplan, NetworkManager keyfiles, systemd-networkd dropins)
    // because each Linux distro ships a different one — the only thing
    // every modern guest reliably ships is `ip` from iproute2. We assume
    // the kernel exposes the NIC under a predictable name; on Cloud
    // Hypervisor's virtio-net the name is `enp0s3` on Ubuntu/Debian, `eth0`
    // on Cirros and older minimal images. Try both.
    if let Some(n) = net {
        runcmd_lines.push_str(&format!(
            "  - [\"sh\", \"-c\", \"for i in enp0s3 ens3 eth0; do ip link show \\\"$i\\\" >/dev/null 2>&1 && IFACE=\\\"$i\\\" && break; done; ip addr add {guest}/{plen} dev \\\"$IFACE\\\"; ip link set \\\"$IFACE\\\" up; ip route add default via {host}\"]\n",
            guest = n.guest_ip,
            host = n.host_ip,
            plen = n.prefix_len,
        ));
    }

    if !mounts.is_empty() {
        mounts_block.push_str("mounts:\n");
        for m in mounts {
            let opts = if m.read_only { "ro" } else { "defaults" };
            let fstype = match m.transport {
                MountTransport::Virtiofs => "virtiofs",
                MountTransport::Block => "ext4",
            };
            // [device, mountpoint, fstype, opts, dump, fsck_pass]
            mounts_block.push_str(&format!(
                "  - [\"{src}\", \"{dest}\", \"{fstype}\", \"{opts}\", \"0\", \"0\"]\n",
                src = m.source,
                dest = m.destination,
                fstype = fstype,
                opts = opts,
            ));
            runcmd_lines.push_str(&format!(
                "  - [mkdir, -p, \"{dest}\"]\n  - [mount, -t, {fstype}, -o, \"{opts}\", \"{src}\", \"{dest}\"]\n",
                src = m.source,
                dest = m.destination,
                fstype = fstype,
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
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
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
        let yaml = render_user_data(&envs, &[], None);
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
            GuestMount::virtiofs("bind-0".to_string(), "/data".to_string(), false),
            GuestMount::virtiofs("cfg-1".to_string(), "/etc/app".to_string(), true),
        ];
        let yaml = render_user_data(&[], &mounts, None);
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
        let yaml = render_user_data(&[], &[], None);
        assert!(!yaml.contains("mounts:"));
        assert!(!yaml.contains("virtiofs"));
    }

    #[test]
    fn user_data_emits_network_config_when_provided() {
        let net = GuestNet {
            guest_ip: "10.42.5.6".to_string(),
            host_ip: "10.42.5.5".to_string(),
            prefix_len: 30,
            mac: "02:11:22:33:44:55".to_string(),
        };
        let yaml = render_user_data(&[], &[], Some(&net));
        assert!(yaml.contains("ip addr add 10.42.5.6/30"));
        assert!(yaml.contains("ip route add default via 10.42.5.5"));
        // The probe loop tries enp0s3 / ens3 / eth0 in turn.
        assert!(yaml.contains("enp0s3"));
        assert!(yaml.contains("eth0"));
    }

    #[test]
    fn user_data_omits_network_runcmd_when_no_net() {
        let yaml = render_user_data(&[], &[], None);
        assert!(!yaml.contains("ip addr add"));
        assert!(!yaml.contains("ip route add default"));
    }

    #[test]
    fn meta_data_includes_instance_id() {
        let md = render_meta_data("ch-deadbeef");
        assert!(md.contains("instance-id: ch-deadbeef"));
        assert!(md.contains("local-hostname: ch-deadbeef"));
    }

    fn ext4_tools_or_skip(test: &str) -> bool {
        for tool in ["mke2fs"] {
            if std::process::Command::new(tool)
                .arg("--help")
                .output()
                .is_err()
            {
                eprintln!(
                    "skipping {}: {} not installed (dosfstools/mtools)",
                    test, tool
                );
                return true;
            }
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
            network: None,
        }
    }

    #[tokio::test]
    async fn build_cidata_iso_with_mounts_writes_real_iso() {
        if ext4_tools_or_skip("build_cidata_iso_with_mounts_writes_real_iso") {
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
        let mounts = vec![GuestMount::virtiofs(
            "bind-0".to_string(),
            "/data".to_string(),
            false,
        )];

        let iso = build_cidata_iso("ch-cidata-test", &dep, &mounts, None, &dir)
            .await
            .expect("e2fsprogs should produce a cidata image");

        let meta = tokio::fs::metadata(&iso).await.unwrap();
        assert!(meta.is_file(), "cidata should be a file");
        assert!(meta.len() > 0, "cidata should not be empty");
        // The datasource is an ext4 image (not ISO9660): minimal guest kernels
        // ship neither iso9660 nor vfat, only ext4. The ext4 superblock starts
        // at byte 1024; its magic 0xEF53 (little-endian) sits at offset 0x38
        // within it (file offset 0x438), and the 16-byte volume label
        // (s_volume_name) at offset 0x78 (file offset 0x478).
        let bytes = tokio::fs::read(&iso).await.unwrap();
        assert!(bytes.len() > 0x488, "image too small to hold a superblock");
        assert_eq!(
            &bytes[0x438..0x43a],
            &[0x53, 0xef],
            "missing ext4 superblock magic"
        );
        // The volume label must be CIDATA so cloud-init's NoCloud datasource
        // picks it up. Trim the NUL padding from the 16-byte field.
        let label_bytes = &bytes[0x478..0x478 + 16];
        let label = std::str::from_utf8(label_bytes)
            .unwrap()
            .trim_end_matches('\0');
        assert_eq!(label, "CIDATA");

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn build_cidata_iso_skips_when_nothing_to_emit() {
        if ext4_tools_or_skip("build_cidata_iso_skips_when_nothing_to_emit") {
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
        let iso = build_cidata_iso("ch-cidata-empty", &dep, &[], None, &dir)
            .await
            .expect("empty cidata should still build");
        assert!(iso.exists());

        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
