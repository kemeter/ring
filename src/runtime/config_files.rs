//! Where Ring writes the decrypted content of config and secret volumes before
//! a container runtime mounts it.
//!
//! The files hold plaintext secrets, so they live in a directory only Ring's
//! user can enter (`0700`), and preferably on a tmpfs so they never reach the
//! disk. The root is, in order:
//!
//! 1. `$RUNTIME_DIRECTORY/configs`, the runtime directory systemd creates for
//!    a unit with `RuntimeDirectory=` (a tmpfs under `/run`);
//! 2. `$XDG_RUNTIME_DIR/ring/configs`, the user's runtime directory (a tmpfs
//!    under `/run/user/<uid>`);
//! 3. `/var/lib/ring/configs` otherwise.
//!
//! The files themselves keep the default mode: a container that runs as
//! another user than Ring's must still be able to read what is mounted into
//! it, and the directory already keeps every other host user out.

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const PRIVATE_DIR_MODE: u32 = 0o700;

/// Root of every deployment's config files, resolved once per process.
pub(crate) fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        resolve_root(
            std::env::var_os("RUNTIME_DIRECTORY").as_deref(),
            std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
        )
    })
}

fn resolve_root(
    runtime_directory: Option<&std::ffi::OsStr>,
    xdg_runtime_dir: Option<&std::ffi::OsStr>,
) -> PathBuf {
    // systemd passes every directory of `RuntimeDirectory=`, separated by `:`.
    if let Some(first) = runtime_directory
        .and_then(|v| v.to_str())
        .and_then(|v| v.split(':').next())
        .filter(|v| !v.is_empty())
    {
        return PathBuf::from(first).join("configs");
    }
    if let Some(dir) = xdg_runtime_dir.filter(|v| !v.is_empty()) {
        return PathBuf::from(dir).join("ring").join("configs");
    }
    PathBuf::from("/var/lib/ring/configs")
}

/// Directory holding one deployment's config files.
pub(crate) fn deployment_dir(deployment_id: &str) -> PathBuf {
    root().join(deployment_id)
}

/// Create `deployment_dir(deployment_id)` if needed, with the root and it both
/// restricted to Ring's user, and return it.
pub(crate) async fn ensure_deployment_dir(deployment_id: &str) -> std::io::Result<PathBuf> {
    let dir = deployment_dir(deployment_id);
    let target = dir.clone();
    tokio::task::spawn_blocking(move || ensure_private_dir(root(), &target))
        .await
        .map_err(std::io::Error::other)??;
    Ok(dir)
}

/// Create `dir` and its missing parents with mode `0700`, then make sure
/// `root` and `dir` are `0700` even if an earlier version of Ring created
/// them more open.
fn ensure_private_dir(root: &Path, dir: &Path) -> std::io::Result<()> {
    create_private_dir(dir)?;
    restrict(root)
}

/// Create `dir` and its missing parents with mode `0700`, and restrict `dir`
/// to `0700` if it already existed with a more open mode. Also used for the
/// microVM runtimes' `socket_dir`, which stages the config files they share
/// with their guests.
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR_MODE)
        .create(dir)?;
    restrict(dir)
}

fn restrict(path: &Path) -> std::io::Result<()> {
    let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if mode != PRIVATE_DIR_MODE {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_DIR_MODE))?;
    }
    Ok(())
}

/// Remove a deployment's config files. A missing directory is not an error.
pub(crate) fn remove_deployment_dir(deployment_id: &str) -> std::io::Result<()> {
    match std::fs::remove_dir_all(deployment_dir(deployment_id)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn systemd_runtime_directory_wins() {
        assert_eq!(
            resolve_root(
                Some(OsStr::new("/run/ring:/run/other")),
                Some(OsStr::new("/run/user/1000"))
            ),
            PathBuf::from("/run/ring/configs")
        );
    }

    #[test]
    fn the_user_runtime_dir_comes_next() {
        assert_eq!(
            resolve_root(None, Some(OsStr::new("/run/user/1000"))),
            PathBuf::from("/run/user/1000/ring/configs")
        );
        assert_eq!(
            resolve_root(Some(OsStr::new("")), Some(OsStr::new("/run/user/1000"))),
            PathBuf::from("/run/user/1000/ring/configs")
        );
    }

    #[test]
    fn without_a_runtime_dir_it_falls_back_to_var_lib() {
        assert_eq!(
            resolve_root(None, None),
            PathBuf::from("/var/lib/ring/configs")
        );
        assert_eq!(
            resolve_root(None, Some(OsStr::new(""))),
            PathBuf::from("/var/lib/ring/configs")
        );
    }

    #[test]
    fn the_root_is_never_under_tmp() {
        assert!(!root().starts_with("/tmp"), "root is {:?}", root());
    }

    fn scratch(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ring-config-files-{}-{}",
            label,
            uuid::Uuid::new_v4()
        ))
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn created_directories_are_private() {
        let root = scratch("create");
        let dir = root.join("dep-1");

        ensure_private_dir(&root, &dir).unwrap();

        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&dir), 0o700);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn directories_left_open_are_tightened() {
        let root = scratch("tighten");
        let dir = root.join("dep-1");
        std::fs::create_dir_all(&dir).unwrap();
        for path in [&root, &dir] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        ensure_private_dir(&root, &dir).unwrap();

        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&dir), 0o700);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
