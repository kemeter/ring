//! The initramfs a microVM boots with when its image is a squashfs.
//!
//! It holds `ring-init` as `/init`, the empty directories it mounts on, and a
//! `/dev/console` node so its messages reach the serial console before devtmpfs
//! is mounted. Ring builds it from the `ring-init` binary configured on the
//! host, as a `newc` cpio archive (the format the kernel unpacks), and caches
//! it next to the microVM sockets under a name derived from the binary's
//! content: replacing the binary builds a new one. Firecracker loads the
//! initramfs into the guest's memory when the microVM starts, so archives of
//! a previous binary are only needed by a start in progress, and are removed
//! once they are old enough to be past it.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::hypervisor::error::RuntimeError;

/// Directories `ring-init` mounts on.
const DIRECTORIES: [&str; 6] = ["dev", "proc", "sys", "lower", "upper", "newroot"];

/// Build the initramfs for `init_binary` in `cache_dir` unless it is already
/// there, and return its path.
pub(crate) fn ensure(init_binary: &Path, cache_dir: &Path) -> Result<PathBuf, RuntimeError> {
    let binary = std::fs::read(init_binary).map_err(|e| {
        RuntimeError::VmStartFailed(format!(
            "cannot read ring-init at '{}' ({}): a squashfs image boots through it, see [server.runtime.firecracker] init_path",
            init_binary.display(),
            e
        ))
    })?;

    let digest = Sha256::digest(&binary);
    let path = cache_dir.join(format!("ring-init-{:x}.cpio", digest));
    if path.exists() {
        return Ok(path);
    }

    std::fs::create_dir_all(cache_dir)?;
    // Written under a name of its own and renamed into place, so a microVM
    // never boots a half-written archive, and two writers never share a file:
    // the rename is atomic, and the last one simply replaces an identical
    // archive.
    let partial = cache_dir.join(format!(
        "ring-init-{:x}.cpio.{}.partial",
        digest,
        uuid::Uuid::new_v4()
    ));
    let written =
        std::fs::write(&partial, archive(&binary)).and_then(|()| std::fs::rename(&partial, &path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(e.into());
    }

    remove_stale(cache_dir, &path);
    Ok(path)
}

/// How long an archive is kept after it stopped being the current one: far
/// longer than a microVM takes to load it, which happens when it starts.
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Remove the archives and leftover partial files other than `current` that
/// are older than [`STALE_AFTER`]. Best-effort: a file that cannot be removed
/// is tried again next time.
fn remove_stale(cache_dir: &Path, current: &Path) {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let ours = name.starts_with("ring-init-")
            && (name.ends_with(".cpio") || name.ends_with(".partial"));
        let old_enough = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= STALE_AFTER);
        if ours && old_enough && path != current {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// A `newc` cpio archive holding `init_binary` as `/init`.
fn archive(init_binary: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut inode = 1;
    let mut next_inode = || {
        inode += 1;
        inode
    };

    for dir in DIRECTORIES {
        entry(&mut out, next_inode(), dir, 0o040755, 0, &[]);
    }
    // Character device 5:1, the console.
    entry(
        &mut out,
        next_inode(),
        "dev/console",
        0o020600,
        (5 << 8) | 1,
        &[],
    );
    entry(&mut out, next_inode(), "init", 0o100755, 0, init_binary);
    entry(&mut out, 0, "TRAILER!!!", 0, 0, &[]);
    out
}

/// Append one `newc` entry. `rdev` packs the device major in its high byte and
/// minor in its low byte, for device nodes only.
fn entry(out: &mut Vec<u8>, inode: u32, name: &str, mode: u32, rdev: u32, data: &[u8]) {
    let fields = [
        inode,
        mode,
        0, // uid
        0, // gid
        if mode & 0o040000 != 0 { 2 } else { 1 },
        0, // mtime
        data.len() as u32,
        0, // device major
        0, // device minor
        rdev >> 8,
        rdev & 0xff,
        name.len() as u32 + 1,
        0, // check
    ];
    out.extend_from_slice(b"070701");
    for field in fields {
        out.extend_from_slice(format!("{:08x}", field).as_bytes());
    }
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    pad(out);
    out.extend_from_slice(data);
    pad(out);
}

/// `newc` aligns every header and every file body on 4 bytes.
fn pad(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// Whether the file at `path` is a squashfs image, from its magic number.
pub(crate) fn is_squashfs(path: &Path) -> bool {
    use std::io::Read;

    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && &magic == b"hsqs"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ring-initramfs-{}-{}", name, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Ask the system `cpio` to list the archive, as the kernel would unpack it.
    fn list(archive: &Path) -> Option<String> {
        let output = std::process::Command::new("cpio")
            .args(["-itv", "--quiet"])
            .stdin(std::fs::File::open(archive).ok()?)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[test]
    fn the_archive_holds_init_its_mount_points_and_the_console() {
        let dir = temp_dir("content");
        let binary = dir.join("ring-init");
        std::fs::write(&binary, b"#!fake ring-init").unwrap();

        let path = ensure(&binary, &dir.join("cache")).unwrap();
        let Some(listing) = list(&path) else {
            eprintln!("skipping: no cpio to read the archive back");
            return;
        };

        for dir in DIRECTORIES {
            assert!(
                listing
                    .lines()
                    .any(|l| l.starts_with('d') && l.ends_with(&format!(" {}", dir))),
                "{dir} missing from:\n{listing}"
            );
        }
        assert!(
            listing
                .lines()
                .any(|l| l.starts_with("crw") && l.ends_with(" dev/console")),
            "no console node in:\n{listing}"
        );
        assert!(
            listing
                .lines()
                .any(|l| l.starts_with("-rwxr-xr-x") && l.ends_with(" init")),
            "no executable init in:\n{listing}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_init_is_stored_verbatim() {
        let archive = archive(b"0123456789");
        let start = archive
            .windows(5)
            .position(|w| w == b"init\0")
            .expect("init entry");
        // Header (110) + "init\0" (5) padded to 4 → 116; the body follows.
        let body = start - 110 + 116;
        assert_eq!(&archive[body..body + 10], b"0123456789");
        assert!(archive.len().is_multiple_of(4));
    }

    #[test]
    fn a_new_binary_gets_a_new_archive() {
        let dir = temp_dir("cache");
        let binary = dir.join("ring-init");

        std::fs::write(&binary, b"v1").unwrap();
        let first = ensure(&binary, &dir).unwrap();
        assert_eq!(ensure(&binary, &dir).unwrap(), first, "cached");

        std::fs::write(&binary, b"v2").unwrap();
        let second = ensure(&binary, &dir).unwrap();
        assert_ne!(second, first);
        assert!(
            first.exists(),
            "the previous one stays for running microVMs"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archives_of_a_previous_binary_are_removed_once_stale() {
        let dir = temp_dir("stale");
        let binary = dir.join("ring-init");
        let old_archive = dir.join("ring-init-old.cpio");
        let old_partial = dir.join("ring-init-old.cpio.x.partial");
        let recent = dir.join("ring-init-recent.cpio");
        let unrelated = dir.join("vm.ext4");
        for file in [&old_archive, &old_partial, &recent, &unrelated] {
            std::fs::write(file, b"x").unwrap();
        }
        let long_ago = std::time::SystemTime::now() - STALE_AFTER * 2;
        for file in [&old_archive, &old_partial, &unrelated] {
            std::fs::File::options()
                .write(true)
                .open(file)
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        }

        std::fs::write(&binary, b"current").unwrap();
        let current = ensure(&binary, &dir).unwrap();

        assert!(current.exists());
        assert!(!old_archive.exists(), "stale archive kept");
        assert!(!old_partial.exists(), "stale partial file kept");
        assert!(recent.exists(), "may still be loading into a starting VM");
        assert!(unrelated.exists(), "not ours to remove");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_binary_names_the_setting() {
        let dir = temp_dir("missing");
        let err = ensure(&dir.join("nope"), &dir).unwrap_err().to_string();
        assert!(err.contains("init_path"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn squashfs_is_recognised_by_its_magic() {
        let dir = temp_dir("magic");
        let squashfs = dir.join("image.squashfs");
        let ext4 = dir.join("image.ext4");
        std::fs::write(&squashfs, b"hsqs\x00\x00\x00\x00").unwrap();
        std::fs::write(&ext4, vec![0u8; 2048]).unwrap();

        assert!(is_squashfs(&squashfs));
        assert!(!is_squashfs(&ext4));
        assert!(!is_squashfs(&dir.join("absent")));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
