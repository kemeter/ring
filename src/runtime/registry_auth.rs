//! Resolving registry credentials from the host's Docker config, shared by all
//! container runtimes (Docker, Podman, containerd).
//!
//! Today a deployment must inline `server`/`username`/`password` in its config,
//! which then lives in cleartext in the database and the API. This module lets a
//! deployment instead opt into reading credentials from the operator's host
//! Docker config (`~/.docker/config.json` and friends) — the secret never
//! leaves the host.
//!
//! The opt-in is a two-flag handshake, by design:
//!   - the **server** authorizes it (`use_host_registry_auth` per runtime), and
//!   - the **deployment** activates it (`config.use_host_auth`).
//!
//! Both are required: a deployment asking for host auth on a server that didn't
//! authorize it fails fast with [`HostAuthError::NotAuthorized`].
//!
//! The config file format (the `auths`/`credsStore`/`credHelpers` schema) is the
//! same across Docker, Podman and nerdctl/containerd — only the default location
//! differs. We resolve the standard Docker location by default and let the
//! operator point [`resolve_host_auth`] at any path via `host_registry_config`,
//! which covers Podman's `containers/auth.json` and the "daemon runs as a
//! different user than the one who logged in" case without per-runtime code.

use std::fs::File;
use std::io::BufReader;

use docker_credential::{CredentialRetrievalError, DockerCredential};

/// Why resolving host registry credentials failed. Each variant maps to an
/// actionable operator-facing message at the runtime boundary.
#[derive(Debug)]
pub(crate) enum HostAuthError {
    /// The deployment asked for host auth but the server didn't authorize it
    /// (`use_host_registry_auth` is off for this runtime).
    NotAuthorized,
    /// The host config file is missing or unreadable.
    ConfigUnavailable(String),
    /// The config was read but holds no credential for this registry host.
    NoEntryForRegistry(String),
    /// A credential helper / store was configured but failed, or returned a
    /// shape we can't use (e.g. an identity token rather than user/password).
    HelperFailed(String),
}

impl std::fmt::Display for HostAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostAuthError::NotAuthorized => write!(
                f,
                "deployment requested use_host_auth but the runtime is not \
                 authorized to use host registry credentials — set \
                 use_host_registry_auth = true under the runtime's \
                 [server.runtime.*] config"
            ),
            HostAuthError::ConfigUnavailable(detail) => write!(
                f,
                "host registry config is unavailable ({detail}) — check the file \
                 exists and is readable by the Ring daemon's user, or set \
                 host_registry_config to its path"
            ),
            HostAuthError::NoEntryForRegistry(host) => write!(
                f,
                "no host credential found for registry '{host}' — run \
                 `docker login {host}` as the Ring daemon's user, or set \
                 host_registry_config"
            ),
            HostAuthError::HelperFailed(detail) => {
                write!(f, "host credential helper failed: {detail}")
            }
        }
    }
}

/// Decide whether host auth should be used, given the server authorization and
/// the per-deployment activation. Returns `Ok(true)` to resolve host creds,
/// `Ok(false)` to keep the inline path, and `Err(NotAuthorized)` when the
/// deployment activated it but the server didn't authorize it.
///
/// Pure so it can be unit-tested and shared identically by every runtime.
pub(crate) fn decide_host_auth(authorized: bool, activated: bool) -> Result<bool, HostAuthError> {
    match (authorized, activated) {
        (_, false) => Ok(false),
        (true, true) => Ok(true),
        (false, true) => Err(HostAuthError::NotAuthorized),
    }
}

/// Extract the registry host from an image reference, applying the same implicit
/// defaults the Docker CLI uses:
///   - `nginx`            → `docker.io`
///   - `bitnami/redis`    → `docker.io`
///   - `ghcr.io/o/p:v1`   → `ghcr.io`
///   - `registry.io:5000/x` → `registry.io:5000`
///   - `localhost:5000/x` → `localhost:5000`
///
/// The first path segment is a registry host only if it contains a `.` or `:`
/// (a domain or `host:port`) or is exactly `localhost`; otherwise it is part of
/// the repository path on Docker Hub. This mirrors the rule in
/// `containerd::image::normalize_repository`.
pub(crate) fn registry_host_for(image: &str) -> String {
    let first_segment = image.split('/').next().unwrap_or(image);
    let has_registry_host =
        first_segment.contains('.') || first_segment.contains(':') || first_segment == "localhost";

    if has_registry_host {
        first_segment.to_string()
    } else {
        "docker.io".to_string()
    }
}

/// Resolve `(username, password)` for `registry_host` from the host registry
/// config. When `config_path` is `Some`, that exact file is read; otherwise the
/// standard Docker resolution applies (`$DOCKER_CONFIG` then
/// `~/.docker/config.json`). Credential helpers and stores
/// (`credHelpers`/`credsStore`) are honored in both cases.
pub(crate) fn resolve_host_auth(
    registry_host: &str,
    config_path: Option<&str>,
) -> Result<(String, String), HostAuthError> {
    // Docker Hub creds are conventionally keyed under the legacy v1 endpoint,
    // not the bare `docker.io` host, so try both forms for Hub.
    let lookups: Vec<String> = if registry_host == "docker.io" {
        vec![
            "https://index.docker.io/v1/".to_string(),
            registry_host.to_string(),
        ]
    } else {
        vec![registry_host.to_string()]
    };

    let mut last_err = HostAuthError::NoEntryForRegistry(registry_host.to_string());
    for key in &lookups {
        match get_credential(key, config_path) {
            Ok(DockerCredential::UsernamePassword(user, pass)) => return Ok((user, pass)),
            Ok(DockerCredential::IdentityToken(_)) => {
                return Err(HostAuthError::HelperFailed(
                    "registry returned an identity token; only username/password \
                     credentials are supported"
                        .to_string(),
                ));
            }
            Err(e) => last_err = map_retrieval_error(e, registry_host),
        }
    }
    Err(last_err)
}

/// Run the crate's lookup against either an explicit file or the default
/// Docker location.
fn get_credential(
    server: &str,
    config_path: Option<&str>,
) -> Result<DockerCredential, CredentialRetrievalError> {
    match config_path {
        Some(path) => {
            let file = File::open(path).map_err(|_| CredentialRetrievalError::ConfigReadError)?;
            docker_credential::get_credential_from_reader(BufReader::new(file), server)
        }
        None => docker_credential::get_credential(server),
    }
}

fn map_retrieval_error(err: CredentialRetrievalError, registry_host: &str) -> HostAuthError {
    match err {
        CredentialRetrievalError::ConfigNotFound | CredentialRetrievalError::ConfigReadError => {
            HostAuthError::ConfigUnavailable(err.to_string())
        }
        CredentialRetrievalError::NoCredentialConfigured => {
            HostAuthError::NoEntryForRegistry(registry_host.to_string())
        }
        other => HostAuthError::HelperFailed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_host_official_image_defaults_to_hub() {
        assert_eq!(registry_host_for("nginx"), "docker.io");
    }

    #[test]
    fn registry_host_hub_org_image_defaults_to_hub() {
        assert_eq!(registry_host_for("bitnami/redis"), "docker.io");
    }

    #[test]
    fn registry_host_explicit_domain_kept() {
        assert_eq!(registry_host_for("ghcr.io/o/p:v1"), "ghcr.io");
    }

    #[test]
    fn registry_host_with_port_kept() {
        assert_eq!(
            registry_host_for("registry.example:5000/foo/bar"),
            "registry.example:5000"
        );
    }

    #[test]
    fn registry_host_localhost_kept() {
        assert_eq!(registry_host_for("localhost:5000/x"), "localhost:5000");
    }

    #[test]
    fn decide_not_activated_keeps_inline() {
        assert!(matches!(decide_host_auth(false, false), Ok(false)));
        assert!(matches!(decide_host_auth(true, false), Ok(false)));
    }

    #[test]
    fn decide_authorized_and_activated_uses_host() {
        assert!(matches!(decide_host_auth(true, true), Ok(true)));
    }

    #[test]
    fn decide_activated_but_unauthorized_errors() {
        assert!(matches!(
            decide_host_auth(false, true),
            Err(HostAuthError::NotAuthorized)
        ));
    }
}
