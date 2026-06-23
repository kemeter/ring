//! Build ext4 images for Firecracker virtio-block volumes.
//!
//! Firecracker has no virtio-fs, so a Ring volume is realised as an ext4 image
//! attached to the microVM as an extra drive; the guest mounts the resulting
//! `/dev/vdX` at the requested destination (cloud-init wires that up). This
//! module produces those images entirely in userspace with `mke2fs` — no
//! mounting, no root:
//!
//! - [`build_ext4_from_dir`] seeds a fresh image from a host directory
//!   (`mke2fs -d`), used for `Bind` (host source dir) and `Content` (a single
//!   rendered file staged into a temp dir).
//! - [`create_empty_ext4`] makes an empty filesystem, used for a `Named`
//!   persistent volume the first time it is referenced.
//!
//! Both pin an `mke2fs.conf` for reproducibility — see the note in
//! [`write_pinned_conf`].

use crate::hypervisor::error::RuntimeError;
use std::path::Path;
use tokio::process::Command;

/// Write a pinned `mke2fs.conf` next to `img` and return its path. Some hosts
/// carry an `/etc/mke2fs.conf` newer than their `mke2fs` binary, defining ext4
/// features (orphan_file, metadata_csum_seed) the binary rejects with a
/// misleading "Invalid filesystem option set". Pointing `MKE2FS_CONFIG` at our
/// own file makes builds reproducible across hosts.
async fn write_pinned_conf(img: &Path) -> Result<std::path::PathBuf, RuntimeError> {
    let conf_path = img.with_extension("mke2fs.conf");
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
    Ok(conf_path)
}

async fn run_mke2fs(conf_path: &Path, args: &[&str]) -> Result<(), RuntimeError> {
    let out = Command::new("mke2fs")
        .env("MKE2FS_CONFIG", conf_path)
        .args(args)
        .output()
        .await
        .map_err(|e| {
            RuntimeError::Other(format!("mke2fs not available: {} (install e2fsprogs)", e))
        })?;
    let _ = tokio::fs::remove_file(conf_path).await;
    if !out.status.success() {
        return Err(RuntimeError::Other(format!(
            "mke2fs failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// Create an ext4 image at `img` seeded from the contents of `src_dir`
/// (`mke2fs -d`). `size_mib` must be large enough to hold the directory; the
/// caller sizes it. `label` becomes the filesystem label.
pub(crate) async fn build_ext4_from_dir(
    img: &Path,
    src_dir: &Path,
    size_mib: u64,
    label: &str,
) -> Result<(), RuntimeError> {
    let img_str = img
        .to_str()
        .ok_or_else(|| RuntimeError::Other(format!("non-UTF-8 image path: {:?}", img)))?;
    let src_str = src_dir
        .to_str()
        .ok_or_else(|| RuntimeError::Other(format!("non-UTF-8 source path: {:?}", src_dir)))?;
    if img.exists() {
        let _ = tokio::fs::remove_file(img).await;
    }
    let conf = write_pinned_conf(img).await?;
    let size = format!("{}M", size_mib.max(1));
    run_mke2fs(
        &conf,
        &[
            "-q", "-F", "-t", "ext4", "-L", label, "-d", src_str, img_str, &size,
        ],
    )
    .await
}

/// Create an empty ext4 image at `img` of `size_mib` MiB with `label`. Used for
/// a persistent `Named` volume on first use; subsequent boots reuse the file.
pub(crate) async fn create_empty_ext4(
    img: &Path,
    size_mib: u64,
    label: &str,
) -> Result<(), RuntimeError> {
    let img_str = img
        .to_str()
        .ok_or_else(|| RuntimeError::Other(format!("non-UTF-8 image path: {:?}", img)))?;
    if let Some(parent) = img.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(RuntimeError::Io)?;
    }
    let conf = write_pinned_conf(img).await?;
    let size = format!("{}M", size_mib.max(1));
    run_mke2fs(
        &conf,
        &["-q", "-F", "-t", "ext4", "-L", label, img_str, &size],
    )
    .await
}

/// Estimate an ext4 image size (MiB) that comfortably holds `bytes` of payload:
/// the payload rounded up plus filesystem overhead (journal + metadata), with a
/// sane floor. ext4's journal alone is a few MiB, so small volumes still need
/// headroom.
pub(crate) fn sizing_mib_for_bytes(bytes: u64) -> u64 {
    const FLOOR_MIB: u64 = 16;
    const OVERHEAD_MIB: u64 = 8;
    let payload_mib = bytes.div_ceil(1024 * 1024);
    (payload_mib + OVERHEAD_MIB).max(FLOOR_MIB)
}

/// Estimate the on-disk byte size of everything under `dir` (recursively): the
/// payload `mke2fs -d` will copy into the volume image. Used to size a `Bind`
/// volume image from its host source directory.
///
/// Counts regular files, symlinks (their own size, not the target — we never
/// follow links, so a dangling or out-of-tree link can't crash the walk or
/// inflate the estimate), and directory entries themselves (each costs a bit of
/// metadata). Under-counting here means `mke2fs -d` could fail on a too-small
/// image, so we err toward including everything we touch.
pub(crate) async fn dir_size_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&d).await {
            Ok(e) => e,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            // symlink_metadata: stat the entry itself, never the link target.
            let Ok(meta) = tokio::fs::symlink_metadata(&path).await else {
                continue;
            };
            let ft = meta.file_type();
            if ft.is_dir() {
                total += meta.len();
                stack.push(path);
            } else {
                // Regular file or symlink: count its own byte length.
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizing_has_floor_and_overhead() {
        assert_eq!(sizing_mib_for_bytes(0), 16);
        assert_eq!(sizing_mib_for_bytes(1), 16);
        // 20 MiB payload + 8 overhead = 28, above the floor.
        assert_eq!(sizing_mib_for_bytes(20 * 1024 * 1024), 28);
    }

    #[tokio::test]
    async fn build_from_dir_and_empty_produce_images() {
        // Only run where mke2fs exists (CI installs e2fsprogs).
        if Command::new("mke2fs").arg("-V").output().await.is_err() {
            eprintln!("skipping: mke2fs not available");
            return;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("ring-vol-{}-{}", std::process::id(), nanos));
        let src = base.join("src");
        tokio::fs::create_dir_all(&src).await.unwrap();
        tokio::fs::write(src.join("hello.txt"), b"payload")
            .await
            .unwrap();

        let from_dir = base.join("from-dir.ext4");
        build_ext4_from_dir(&from_dir, &src, 16, "DATA")
            .await
            .unwrap();
        assert!(from_dir.exists());
        let meta = tokio::fs::metadata(&from_dir).await.unwrap();
        assert!(meta.len() > 0);

        let empty = base.join("empty.ext4");
        create_empty_ext4(&empty, 16, "DATA").await.unwrap();
        assert!(empty.exists());

        tokio::fs::remove_dir_all(&base).await.ok();
    }

    #[tokio::test]
    async fn dir_size_sums_files_recursively() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ring-dsz-{}-{}", std::process::id(), nanos));
        tokio::fs::create_dir_all(dir.join("sub")).await.unwrap();
        tokio::fs::write(dir.join("a"), b"12345").await.unwrap();
        tokio::fs::write(dir.join("sub/b"), b"678").await.unwrap();
        // At least the file payload (5 + 3 = 8 bytes); directory entries add
        // their own (filesystem-dependent) metadata size on top.
        assert!(dir_size_bytes(&dir).await >= 8);
        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
