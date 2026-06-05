//! Podman runtime support.
//!
//! Podman exposes a Docker-compatible REST API via `podman system service`,
//! which means the existing `bollard`-based `DockerLifecycle` can drive it
//! almost unchanged — the transport and most of the request/response shapes
//! are identical. This module's job is therefore narrow: resolve the right
//! socket (rootless-first) and hand a connected `bollard::Docker` to
//! `DockerLifecycle`. There is intentionally no parallel `PodmanLifecycle`
//! struct; duplicating the container/logs/stats/health code would be pure
//! drift waiting to happen.
//!
//! # Why rootless-first
//!
//! Rootless is Podman's default and headline mode, and it's the case that
//! actually exercises Ring's volume abstraction: user namespaces remap the
//! container's UID/GID, so a file written inside the container has a
//! different owner on the host. Bind mounts, named-volume ownership, and the
//! `o=sync` durability story all behave differently than under Docker-root.
//! If volumes work rootless, root falls out almost for free; the reverse is
//! not true.
//!
//! # Known semantic deltas (where bollard does NOT save us)
//!
//! These are tracked here as the real work, to be validated against a live
//! Podman by a `tests/e2e/t*.sh` harness (cargo tests can't catch them):
//!
//! 1. **Events require a running service.** Unlike the always-present Docker
//!    daemon, Podman's event stream only flows while `podman system service`
//!    is up. The orphan-volume reaper (which consumes container `die`/`remove`
//!    events) silently stops reaping if the service is down — we must detect
//!    and surface that rather than appear healthy.
//! 2. **No central daemon / fork-exec lifecycle.** Containers can outlive any
//!    supervising process, so "is this instance actually running?" may need a
//!    reconcile pass rather than trusting daemon-held state.
//! 3. **Partial API compatibility.** Some endpoints/fields bollard expects in
//!    Docker's shape are a subset or slightly different under Podman's
//!    emulated API version. To be discovered endpoint-by-endpoint at runtime.

/// Resolve the Podman API socket host, rootless-first.
///
/// Resolution order:
///   1. `RING_PODMAN_HOST` — explicit override (`unix://…` or `tcp://…`).
///   2. `DOCKER_HOST` — honoured because `podman system service` users
///      frequently point it at the Podman socket for tooling compatibility.
///   3. Rootless default: `unix:///run/user/$UID/podman/podman.sock`.
///   4. Root default: `unix:///run/podman/podman.sock`.
///
/// Used as the default for `[server.runtime.podman] host`. The connection
/// itself is made by `commands/server.rs` via `docker::connect_and_verify`,
/// since Podman speaks the Docker wire protocol.
pub(crate) fn resolve_socket_host() -> String {
    if let Ok(explicit) = std::env::var("RING_PODMAN_HOST") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    if let Ok(docker_host) = std::env::var("DOCKER_HOST") {
        let trimmed = docker_host.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    // Rootless default. `XDG_RUNTIME_DIR` is the canonical source for the
    // per-user runtime dir; fall back to `/run/user/$UID` when it's unset
    // (common under bare systemd user sessions and cron).
    if let Some(rootless) = rootless_socket_path() {
        return rootless;
    }

    // Root default — last resort, matches Podman's system-wide service.
    "unix:///run/podman/podman.sock".to_string()
}

/// Build the rootless socket host string from the environment, or `None` if
/// neither `XDG_RUNTIME_DIR` nor `UID` can be determined.
fn rootless_socket_path() -> Option<String> {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        let trimmed = xdg.trim_end_matches('/');
        if !trimmed.is_empty() {
            return Some(format!("unix://{}/podman/podman.sock", trimmed));
        }
    }

    if let Ok(uid) = std::env::var("UID") {
        let trimmed = uid.trim();
        if !trimmed.is_empty() {
            return Some(format!("unix:///run/user/{}/podman/podman.sock", trimmed));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests mutate process-global env vars, so they must not run
    // concurrently with each other. A shared mutex serialises them.
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_clean_env<F: FnOnce()>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved: Vec<(&str, Option<String>)> =
            ["RING_PODMAN_HOST", "DOCKER_HOST", "XDG_RUNTIME_DIR", "UID"]
                .iter()
                .map(|k| (*k, std::env::var(k).ok()))
                .collect();
        for (k, _) in &saved {
            unsafe { std::env::remove_var(k) };
        }
        f();
        for (k, v) in saved {
            match v {
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }

    #[test]
    fn explicit_override_wins() {
        with_clean_env(|| {
            unsafe { std::env::set_var("RING_PODMAN_HOST", "tcp://10.0.0.1:8080") };
            unsafe { std::env::set_var("DOCKER_HOST", "unix:///should/be/ignored.sock") };
            assert_eq!(resolve_socket_host(), "tcp://10.0.0.1:8080");
        });
    }

    #[test]
    fn docker_host_used_when_no_explicit() {
        with_clean_env(|| {
            unsafe { std::env::set_var("DOCKER_HOST", "unix:///run/user/1000/podman/podman.sock") };
            assert_eq!(
                resolve_socket_host(),
                "unix:///run/user/1000/podman/podman.sock"
            );
        });
    }

    #[test]
    fn rootless_default_from_xdg_runtime_dir() {
        with_clean_env(|| {
            unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000") };
            assert_eq!(
                resolve_socket_host(),
                "unix:///run/user/1000/podman/podman.sock"
            );
        });
    }

    #[test]
    fn rootless_default_from_uid_fallback() {
        with_clean_env(|| {
            unsafe { std::env::set_var("UID", "1234") };
            assert_eq!(
                resolve_socket_host(),
                "unix:///run/user/1234/podman/podman.sock"
            );
        });
    }

    #[test]
    fn root_default_as_last_resort() {
        with_clean_env(|| {
            assert_eq!(resolve_socket_host(), "unix:///run/podman/podman.sock");
        });
    }

    #[test]
    fn empty_override_is_ignored() {
        with_clean_env(|| {
            unsafe { std::env::set_var("RING_PODMAN_HOST", "   ") };
            unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000") };
            assert_eq!(
                resolve_socket_host(),
                "unix:///run/user/1000/podman/podman.sock"
            );
        });
    }
}
