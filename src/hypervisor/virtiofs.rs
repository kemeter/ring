//! virtio-fs daemon supervision.
//!
//! Lives at `hypervisor::` (not under `cloud_hypervisor::`) because the host-side
//! protocol is the same for any virtio-capable hypervisor: spawn a `virtiofsd`
//! process pinned to a Unix socket and a host directory, then hand the socket
//! to the VMM. Cloud Hypervisor consumes it via `VmConfig.fs[*].socket`;
//! Firecracker (≥ 1.7) exposes an equivalent surface. Whoever calls
//! [`spawn_virtiofsd`] keeps the returned [`VirtiofsMount`] alive for the
//! lifetime of the VM — its `Drop` impl kills the daemon and removes the
//! socket file.
//!
//! The daemon binary is discovered the same way `ring doctor` does it:
//! `/usr/libexec/virtiofsd` first, then `/usr/lib/qemu/virtiofsd`. Override
//! by exporting `RING_VIRTIOFSD` to a custom path.

use crate::hypervisor::error::RuntimeError;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};

const VIRTIOFSD_CANDIDATES: &[&str] = &["/usr/libexec/virtiofsd", "/usr/lib/qemu/virtiofsd"];

/// Resolve the virtiofsd binary path, preferring the env override.
pub(crate) fn locate_virtiofsd() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RING_VIRTIOFSD") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    VIRTIOFSD_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

/// A live virtio-fs share. Holds the daemon child so dropping it tears down
/// the share. The VMM reads from `socket_path` and the guest mounts via `tag`.
pub(crate) struct VirtiofsMount {
    pub tag: String,
    pub socket_path: PathBuf,
    pub destination: String,
    pub read_only: bool,
    /// Owned: dropping the mount kills the daemon.
    child: Child,
}

impl VirtiofsMount {
    pub fn socket_path_str(&self) -> Result<&str, RuntimeError> {
        self.socket_path.to_str().ok_or_else(|| {
            RuntimeError::Other(format!("non-UTF-8 socket path: {:?}", self.socket_path))
        })
    }
}

impl Drop for VirtiofsMount {
    fn drop(&mut self) {
        // start_kill() sends SIGKILL synchronously; the OS will reap. We don't
        // .wait() here because Drop is sync and we don't want to block.
        let _ = self.child.start_kill();
        // virtiofsd unlinks the socket itself when a vhost-user client
        // connects, so it may not exist by the time we get here — both
        // cleanups are best-effort. The .pid sidecar file is on us:
        // virtiofsd does not remove it on exit.
        let _ = std::fs::remove_file(&self.socket_path);
        let pid_path = self.socket_path.with_extension(
            self.socket_path
                .extension()
                .map(|e| {
                    let mut s = e.to_os_string();
                    s.push(".pid");
                    s
                })
                .unwrap_or_else(|| std::ffi::OsString::from("pid")),
        );
        let _ = std::fs::remove_file(&pid_path);
    }
}

/// Spawn a virtiofsd that exports `shared_dir` over `socket_path` with the
/// given `tag`. Returns once the socket file appears on disk (up to ~5s).
///
/// `shared_dir` must already exist. `socket_path`'s parent must already exist.
///
/// `durable` selects the cache policy. virtiofsd's default (`auto`) caches data
/// with timeouts, so a guest write can linger in the daemon's page cache and be
/// lost on an unclean host shutdown. For persistent writable volumes we pass
/// `--cache never` so every guest write goes through to the host filesystem.
/// Non-durable shares (read-only binds, rendered config files) keep the faster
/// default — they hold no data the operator expects to survive a crash.
pub(crate) async fn spawn_virtiofsd(
    binary_path: &Path,
    shared_dir: &Path,
    socket_path: &Path,
    tag: &str,
    destination: &str,
    read_only: bool,
    durable: bool,
) -> Result<VirtiofsMount, RuntimeError> {
    if !shared_dir.exists() {
        return Err(RuntimeError::Other(format!(
            "virtiofs shared dir does not exist: {:?}",
            shared_dir
        )));
    }

    // Stale socket from a previous run would make virtiofsd refuse to bind.
    if socket_path.exists() {
        let _ = tokio::fs::remove_file(socket_path).await;
    }

    let socket_str = socket_path
        .to_str()
        .ok_or_else(|| RuntimeError::Other(format!("non-UTF-8 socket path: {:?}", socket_path)))?;
    let shared_str = shared_dir
        .to_str()
        .ok_or_else(|| RuntimeError::Other(format!("non-UTF-8 shared dir: {:?}", shared_dir)))?;

    // sandbox=none: virtiofsd refuses to chroot when not running as root.
    // Ring is expected to run as a service user with CAP_NET_ADMIN/RAW for CH;
    // it is not running as root, so chroot sandboxing is unavailable.
    let mut command = Command::new(binary_path);
    command
        .arg("--socket-path")
        .arg(socket_str)
        .arg("--shared-dir")
        .arg(shared_str)
        .arg("--sandbox")
        .arg("none")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    if read_only {
        command.arg("--readonly");
    }

    if durable {
        // `never` == no virtiofsd-side data caching: writes are not held back,
        // matching the durability contract of a synchronous mount.
        command.arg("--cache").arg("never");
    }

    let child = command.spawn().map_err(|e| {
        RuntimeError::Other(format!(
            "failed to spawn virtiofsd at {:?}: {}",
            binary_path, e
        ))
    })?;

    // Wait for the socket to appear (virtiofsd creates it once it's ready).
    for _ in 0..50 {
        if socket_path.exists() {
            return Ok(VirtiofsMount {
                tag: tag.to_string(),
                socket_path: socket_path.to_path_buf(),
                destination: destination.to_string(),
                read_only,
                child,
            });
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // Socket never appeared: the daemon either died or hung. Kill it and
    // surface what we can from stderr.
    let mut mount = VirtiofsMount {
        tag: tag.to_string(),
        socket_path: socket_path.to_path_buf(),
        destination: destination.to_string(),
        read_only,
        child,
    };
    let _ = mount.child.start_kill();
    Err(RuntimeError::Other(
        "virtiofsd did not create its socket within 5s".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Mutex;

    /// `RING_VIRTIOFSD` is process-global, but `tokio::test`s run concurrently
    /// in one process. Without serialization, `locate_respects_env_override`
    /// can set/remove the override (and delete its fake binary) in the middle
    /// of another test's `locate_virtiofsd()` call, making `spawn_virtiofsd`
    /// chase a path that has already vanished. Every test that reads or writes
    /// the override must hold this lock for the whole window it depends on it.
    ///
    /// A `tokio::sync::Mutex` (not `std`) so the async tests can keep the guard
    /// held across their `.await` points without tripping `await_holding_lock`.
    static ENV_GUARD: Mutex<()> = Mutex::const_new(());

    #[test]
    fn locate_respects_env_override() {
        // Sync test, no tokio runtime: take the lock without awaiting.
        let _guard = ENV_GUARD.blocking_lock();

        let tmp = std::env::temp_dir().join("ring-virtiofs-test-fake");
        std::fs::write(&tmp, b"#!/bin/sh\nexit 0\n").unwrap();
        // SAFETY: ENV_GUARD serializes every test that touches RING_VIRTIOFSD,
        // so no other test reads the env while we mutate it here.
        unsafe {
            std::env::set_var("RING_VIRTIOFSD", &tmp);
        }
        let found = locate_virtiofsd();
        unsafe {
            std::env::remove_var("RING_VIRTIOFSD");
        }
        std::fs::remove_file(&tmp).ok();
        assert_eq!(found.as_deref(), Some(tmp.as_path()));
    }

    /// Build a per-test scratch dir under the OS temp dir. Cleaned up by
    /// the caller via `std::fs::remove_dir_all`.
    fn scratch_dir(label: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ring-virtiofs-{}-{}-{}", label, pid, nanos));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Skip-or-run pattern: returns the resolved virtiofsd path, or `None`
    /// when the binary is unavailable (CI sandboxes, devs without it).
    fn virtiofsd_or_skip(test_name: &str) -> Option<PathBuf> {
        match locate_virtiofsd() {
            Some(p) => Some(p),
            None => {
                eprintln!(
                    "skipping {}: virtiofsd not installed (apt install virtiofsd)",
                    test_name
                );
                None
            }
        }
    }

    #[tokio::test]
    async fn spawn_creates_socket_and_drop_kills_daemon() {
        let _guard = ENV_GUARD.lock().await;

        let Some(virtiofsd) = virtiofsd_or_skip("spawn_creates_socket_and_drop_kills_daemon")
        else {
            return;
        };

        let dir = scratch_dir("spawn-ok");
        let shared = dir.join("share");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("hello.txt"), b"hi").unwrap();
        let socket = dir.join("vfs.sock");

        let mount = spawn_virtiofsd(&virtiofsd, &shared, &socket, "tag-x", "/mnt/x", false, true)
            .await
            .expect("spawn should succeed");

        assert!(socket.exists(), "socket file should exist after spawn");
        assert_eq!(mount.tag, "tag-x");
        assert_eq!(mount.destination, "/mnt/x");
        assert!(!mount.read_only);

        drop(mount);

        // After Drop, the socket file is removed best-effort and the daemon
        // is killed. Give the OS a beat to reap.
        for _ in 0..20 {
            if !socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            !socket.exists(),
            "socket should be removed after Drop, still present at {:?}",
            socket
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn spawn_fails_when_shared_dir_missing() {
        let _guard = ENV_GUARD.lock().await;

        // No need for the real binary — this errors out before spawning.
        let dir = scratch_dir("missing-share");
        let result = spawn_virtiofsd(
            Path::new("/usr/libexec/virtiofsd"),
            &dir.join("does-not-exist"),
            &dir.join("vfs.sock"),
            "tag-y",
            "/mnt/y",
            false,
            false,
        )
        .await;
        assert!(result.is_err(), "expected error for missing shared dir");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn spawn_removes_stale_socket() {
        let _guard = ENV_GUARD.lock().await;

        let Some(virtiofsd) = virtiofsd_or_skip("spawn_removes_stale_socket") else {
            return;
        };

        let dir = scratch_dir("stale-socket");
        let shared = dir.join("share");
        std::fs::create_dir_all(&shared).unwrap();
        let socket = dir.join("vfs.sock");
        // Plant a stale regular file at the socket path. Without cleanup,
        // virtiofsd's bind() would fail with EADDRINUSE.
        std::fs::write(&socket, b"stale").unwrap();

        let mount = spawn_virtiofsd(&virtiofsd, &shared, &socket, "tag-z", "/mnt/z", true, false)
            .await
            .expect("spawn should overwrite the stale file");
        assert!(mount.read_only);
        drop(mount);

        std::fs::remove_dir_all(&dir).ok();
    }
}
