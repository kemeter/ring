mod container;
pub(crate) mod docker_lifecycle;
mod health_check;
mod instances;
mod lifecycle;
mod logs;
mod stats;

use crate::hypervisor::error::RuntimeError;
use bollard::Docker;

/// How an image was addressed in the manifest. A `tag` is mutable on the
/// registry side (`my/app:latest` can be re-pushed); a `digest` pins an
/// immutable content hash. Docker's pull API takes them through different
/// query parameters, and `IfNotPresent` semantics differ (a digest reference
/// hits the same content forever, so re-pulling buys nothing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ImageReference {
    Tag(String),
    Digest(String),
}

pub(crate) struct DockerImage {
    pub name: String,
    pub reference: ImageReference,
    pub auth: Option<(String, String, String)>,
}

impl DockerImage {
    /// Reassemble the canonical `name[:tag|@digest]` string Docker expects in
    /// `inspect_image`. We can't just keep the original input because we need
    /// to be able to query the local cache and the registry independently.
    pub fn full_ref(&self) -> String {
        match &self.reference {
            ImageReference::Tag(tag) => format!("{}:{}", self.name, tag),
            ImageReference::Digest(digest) => format!("{}@{}", self.name, digest),
        }
    }
}

/// Parse a Docker image reference of the form:
///   - `name`              → `(name, Tag("latest"))`
///   - `name:tag`          → `(name, Tag(tag))`
///   - `host[:port]/path[:tag]` → `(host[:port]/path, Tag(tag|"latest"))`
///   - `name@sha256:...`   → `(name, Digest("sha256:..."))`
///
/// The naive `split_once(':')` we used before mistook the port of a
/// `registry.example:5000/foo/bar:v1` reference for a tag, and choked on
/// digest references entirely. The rule: a `:` only introduces a tag if it
/// appears after the last `/` (i.e. in the final path component); a `@`
/// always introduces a digest.
pub(crate) fn parse_image_reference(image: &str) -> (String, ImageReference) {
    // Digest takes precedence: `name@sha256:...` is unambiguous because `@`
    // never appears in a registry host or repository path.
    if let Some((name, digest)) = image.split_once('@') {
        return (name.to_string(), ImageReference::Digest(digest.to_string()));
    }

    // Split on `/` to isolate the last path component, then look for a tag
    // separator only inside it. This protects against `host:port/path` being
    // misread as `host:(port/path)`.
    let last_slash = image.rfind('/');
    let last_component_start = last_slash.map(|i| i + 1).unwrap_or(0);
    let last_component = &image[last_component_start..];

    if let Some(colon_in_last) = last_component.rfind(':') {
        let split_at = last_component_start + colon_in_last;
        let name = image[..split_at].to_string();
        let tag = image[split_at + 1..].to_string();
        return (name, ImageReference::Tag(tag));
    }

    (image.to_string(), ImageReference::Tag("latest".to_string()))
}

/// Pull policy enum derived from the validated string (PR #89 enforces
/// case-sensitive `Always`/`IfNotPresent`/`Never` at the API). We still parse
/// case-insensitively at the runtime boundary because legacy seed data and
/// tests historically used lowercase, and being strict here would crash
/// rather than recover gracefully when fed a value that slipped past
/// validation (e.g. seed migration, manual DB edit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImagePullPolicy {
    Always,
    IfNotPresent,
    Never,
}

impl ImagePullPolicy {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "never" => Self::Never,
            "ifnotpresent" => Self::IfNotPresent,
            "always" => Self::Always,
            // Anything else slipped past PR #89's API validation (manual DB
            // edit, an older row created before the validator landed, etc.).
            // Surface it so operators can fix the source rather than have us
            // silently substitute `Always` and pretend everything is fine.
            other => {
                warn!(
                    "Unknown image_pull_policy '{}' — falling back to Always. \
                     Accepted values: Always, IfNotPresent, Never.",
                    other
                );
                Self::Always
            }
        }
    }
}

impl From<bollard::errors::Error> for RuntimeError {
    fn from(err: bollard::errors::Error) -> Self {
        let err_msg = err.to_string();
        if err_msg.contains("404")
            || err_msg.contains("not found")
            || err_msg.contains("manifest unknown")
        {
            RuntimeError::ImageNotFound(err_msg)
        } else {
            RuntimeError::Other(err_msg)
        }
    }
}

/// Connect to Docker at `host` *and confirm the daemon actually answers*,
/// returning the client only if it does.
///
/// `connect`/`connect_with_host` merely build a bollard client — they succeed
/// even when no daemon is listening, because the socket connection is lazy.
/// That makes them unsuitable for a best-effort "is Docker available?" gate at
/// startup: we'd register a runtime that 500s on the first deployment. A
/// `ping()` round-trip is the cheapest call that proves the daemon is up and
/// speaking the API, so it gates registering the runtime.
pub(crate) async fn connect_and_verify(host: &str) -> Result<Docker, RuntimeError> {
    let docker = connect_with_host(host)?;
    docker.ping().await.map_err(|e| {
        RuntimeError::Other(format!(
            "Docker daemon at {} did not respond to ping: {}",
            host, e
        ))
    })?;
    Ok(docker)
}

pub(crate) fn connect_with_host(host: &str) -> Result<Docker, RuntimeError> {
    if host.starts_with("unix://") {
        let socket_path = host.trim_start_matches("unix://");
        Docker::connect_with_socket(socket_path, 120, bollard::API_DEFAULT_VERSION).map_err(|e| {
            RuntimeError::Other(format!(
                "Failed to connect to Docker socket {}: {}",
                host, e
            ))
        })
    } else if host.starts_with("tcp://") {
        Docker::connect_with_http(host, 120, bollard::API_DEFAULT_VERSION).map_err(|e| {
            RuntimeError::Other(format!("Failed to connect to Docker at {}: {}", host, e))
        })
    } else {
        Docker::connect_with_local_defaults()
            .map_err(|e| RuntimeError::Other(format!("Failed to connect to Docker: {}", e)))
    }
}

pub(crate) fn tiny_id() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    format!("{:08x}", rng.random::<u32>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_image_reference_bare_name_defaults_to_latest() {
        let (name, r#ref) = parse_image_reference("nginx");
        assert_eq!(name, "nginx");
        assert_eq!(r#ref, ImageReference::Tag("latest".to_string()));
    }

    #[test]
    fn parse_image_reference_with_tag() {
        let (name, r#ref) = parse_image_reference("nginx:1.25");
        assert_eq!(name, "nginx");
        assert_eq!(r#ref, ImageReference::Tag("1.25".to_string()));
    }

    #[test]
    fn parse_image_reference_with_repo_path() {
        let (name, r#ref) = parse_image_reference("library/nginx:1.25");
        assert_eq!(name, "library/nginx");
        assert_eq!(r#ref, ImageReference::Tag("1.25".to_string()));
    }

    #[test]
    fn parse_image_reference_with_registry_host_no_port() {
        let (name, r#ref) = parse_image_reference("ghcr.io/kemeter/ring:v0.8.0");
        assert_eq!(name, "ghcr.io/kemeter/ring");
        assert_eq!(r#ref, ImageReference::Tag("v0.8.0".to_string()));
    }

    #[test]
    fn parse_image_reference_with_registry_port_keeps_port_in_name() {
        // Regression: the previous `split_once(':')` parser turned this into
        // `("registry.example", "5000/foo/bar:v1")` and Docker would refuse
        // the pull because the registry hostname was lost.
        let (name, r#ref) = parse_image_reference("registry.example:5000/foo/bar:v1");
        assert_eq!(name, "registry.example:5000/foo/bar");
        assert_eq!(r#ref, ImageReference::Tag("v1".to_string()));
    }

    #[test]
    fn parse_image_reference_with_registry_port_no_tag() {
        let (name, r#ref) = parse_image_reference("registry.example:5000/foo/bar");
        assert_eq!(name, "registry.example:5000/foo/bar");
        assert_eq!(r#ref, ImageReference::Tag("latest".to_string()));
    }

    #[test]
    fn parse_image_reference_digest() {
        let (name, r#ref) = parse_image_reference(
            "nginx@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        );
        assert_eq!(name, "nginx");
        assert_eq!(
            r#ref,
            ImageReference::Digest(
                "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                    .to_string()
            )
        );
    }

    #[test]
    fn parse_image_reference_digest_with_registry_port() {
        // Digest takes precedence over any `:` in the host part — `@` is the
        // unambiguous marker.
        let (name, r#ref) = parse_image_reference("registry.example:5000/foo/bar@sha256:deadbeef");
        assert_eq!(name, "registry.example:5000/foo/bar");
        assert_eq!(r#ref, ImageReference::Digest("sha256:deadbeef".to_string()));
    }

    #[test]
    fn full_ref_roundtrip_tag() {
        let img = DockerImage {
            name: "ghcr.io/kemeter/ring".to_string(),
            reference: ImageReference::Tag("v0.8.0".to_string()),
            auth: None,
        };
        assert_eq!(img.full_ref(), "ghcr.io/kemeter/ring:v0.8.0");
    }

    #[test]
    fn full_ref_roundtrip_digest() {
        let img = DockerImage {
            name: "nginx".to_string(),
            reference: ImageReference::Digest("sha256:abc".to_string()),
            auth: None,
        };
        assert_eq!(img.full_ref(), "nginx@sha256:abc");
    }

    #[test]
    fn pull_policy_parses_canonical_values() {
        assert_eq!(ImagePullPolicy::parse("Always"), ImagePullPolicy::Always);
        assert_eq!(
            ImagePullPolicy::parse("IfNotPresent"),
            ImagePullPolicy::IfNotPresent
        );
        assert_eq!(ImagePullPolicy::parse("Never"), ImagePullPolicy::Never);
    }

    #[test]
    fn pull_policy_parses_case_insensitively() {
        // Legacy seed data and existing test fixtures used lowercase.
        assert_eq!(ImagePullPolicy::parse("always"), ImagePullPolicy::Always);
        assert_eq!(
            ImagePullPolicy::parse("ifnotpresent"),
            ImagePullPolicy::IfNotPresent
        );
        assert_eq!(ImagePullPolicy::parse("NEVER"), ImagePullPolicy::Never);
    }

    #[test]
    fn pull_policy_unknown_falls_back_to_always() {
        // Defensive default: pulling fresh is the safe fallback compared to
        // accidentally using a stale cached image when an unknown policy
        // slipped past validation.
        assert_eq!(ImagePullPolicy::parse("Maybe"), ImagePullPolicy::Always);
        assert_eq!(ImagePullPolicy::parse(""), ImagePullPolicy::Always);
    }
}
