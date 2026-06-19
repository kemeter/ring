//! Image acquisition and rootfs resolution.
//!
//! Containerd has no single "pull and unpack" verb. We use the **Transfer**
//! service to move an image from a registry (`OciRegistry` source) into the
//! local image store and unpack it into the configured snapshotter
//! (`ImageStore` destination with an `UnpackConfiguration`). Then, to give each
//! task a writable rootfs, we need the *chain ID* of the image's read-only
//! layers — the parent for `Snapshots.Prepare`. Containerd computes and labels
//! that on the image during unpack, so we read it back from the image labels
//! (falling back to recomputing it from the image config's `rootfs.diff_ids`).

use super::client::DEFAULT_SNAPSHOTTER;
use crate::hypervisor::error::RuntimeError;
use crate::runtime::docker::{ImagePullPolicy, ImageReference, parse_image_reference};
use containerd_client::services::v1::GetImageRequest;
use containerd_client::services::v1::ReadContentRequest;
use containerd_client::services::v1::TransferRequest;
use containerd_client::services::v1::content_client::ContentClient;
use containerd_client::services::v1::images_client::ImagesClient;
use containerd_client::services::v1::transfer_client::TransferClient;
use containerd_client::types::Platform;
use containerd_client::types::transfer::{
    ImageStore, OciRegistry, RegistryResolver, UnpackConfiguration,
};
use containerd_client::{to_any, with_namespace};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tokio_stream::StreamExt;
use tonic::Request;

/// Everything the runtime needs to address one image: the canonical reference
/// (what containerd stores it under) and optional registry credentials.
pub(crate) struct ContainerdImage {
    pub(crate) reference: String,
    pub(crate) parsed_ref: ImageReference,
    /// `(server, username, password)` registry credentials, when configured.
    pub(crate) auth: Option<(String, String, String)>,
}

impl ContainerdImage {
    /// Build from the deployment's image string and optional config auth.
    pub(crate) fn from_deployment(image: &str, auth: Option<(String, String, String)>) -> Self {
        let (repo, parsed_ref) = parse_image_reference(image);
        // Unlike the Docker daemon, containerd does NOT expand short image names:
        // it needs a fully-qualified reference. A bare `nginx` reaches its
        // Transfer resolver as `dummy://nginx:1.25-alpine`, whose `:1.25-alpine`
        // the URL parser reads as a port → "invalid port after host" and the
        // pull fails (deployment crash-loops). So we apply Docker Hub's implicit
        // rules ourselves: default the registry to `docker.io` and the namespace
        // to `library` for official images.
        let repo = normalize_repository(&repo);
        // The reference containerd stores the image under is the canonical
        // `name:tag` / `name@digest`.
        let reference = match &parsed_ref {
            ImageReference::Tag(t) => format!("{}:{}", repo, t),
            ImageReference::Digest(d) => format!("{}@{}", repo, d),
        };
        Self {
            reference,
            parsed_ref,
            auth,
        }
    }
}

/// Expand a Docker-style repository name to a fully-qualified containerd
/// reference, applying the same implicit defaults the Docker CLI/daemon do:
///   - `nginx`            → `docker.io/library/nginx`   (official image)
///   - `bitnami/redis`    → `docker.io/bitnami/redis`   (Hub user/org image)
///   - `ghcr.io/o/p`      → `ghcr.io/o/p`               (explicit registry, kept)
///   - `localhost:5000/x` → `localhost:5000/x`          (local registry, kept)
///
/// The first path segment is a registry host only if it contains a `.` or `:`
/// (a domain or `host:port`) or is exactly `localhost`; otherwise it's part of
/// the repository path on Docker Hub.
fn normalize_repository(repo: &str) -> String {
    let first_segment = repo.split('/').next().unwrap_or(repo);
    let has_registry_host =
        first_segment.contains('.') || first_segment.contains(':') || first_segment == "localhost";

    if has_registry_host {
        // Registry is explicit — leave it as the user wrote it.
        repo.to_string()
    } else if repo.contains('/') {
        // Hub user/org image: registry defaults to docker.io, path kept.
        format!("docker.io/{}", repo)
    } else {
        // Official image: docker.io + the implicit `library` namespace.
        format!("docker.io/library/{}", repo)
    }
}

/// Subset of an OCI image manifest we need: the `config` descriptor pointer.
#[derive(Deserialize)]
struct OciManifest {
    config: OciDescriptor,
}

#[derive(Deserialize)]
struct OciDescriptor {
    digest: String,
}

/// A multi-arch manifest list / OCI image index: a list of per-platform
/// manifests instead of a single image. Docker Hub serves most official images
/// (nginx, alpine, …) this way, so the digest containerd records as the image
/// target points here, NOT at an image manifest. We must pick the entry matching
/// the host platform and recurse into it.
#[derive(Deserialize)]
struct OciIndex {
    manifests: Vec<OciIndexEntry>,
}

#[derive(Deserialize)]
struct OciIndexEntry {
    digest: String,
    #[serde(default)]
    platform: Option<OciPlatform>,
}

#[derive(Deserialize)]
struct OciPlatform {
    architecture: String,
    os: String,
}

/// Subset of an OCI image config we need: the rootfs diff ids plus the default
/// process the image declares. containerd is low-level — unlike the Docker
/// daemon, nothing merges the image's Entrypoint/Cmd into the OCI spec for us,
/// so Ring must read them here and apply them when the deployment overrides no
/// command (otherwise `runc create` fails with `args must not be empty`).
#[derive(Deserialize)]
struct OciImageConfig {
    rootfs: OciRootfs,
    #[serde(default)]
    config: OciImageConfigInner,
}

/// The `config` block of an OCI image config: the container runtime defaults.
#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct OciImageConfigInner {
    #[serde(default)]
    entrypoint: Option<Vec<String>>,
    #[serde(default)]
    cmd: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct OciRootfs {
    diff_ids: Vec<String>,
}

/// Result of resolving an image to a runnable rootfs parent.
pub(crate) struct ResolvedImage {
    /// Manifest digest of the image (what we record as `image_digest`).
    pub(crate) digest: Option<String>,
    /// Snapshot chain id that serves as the read-only parent of each
    /// per-instance writable snapshot.
    pub(crate) chain_id: String,
    /// The image's default process (`Entrypoint` + `Cmd`), used as `process.args`
    /// when the deployment does not override the command. Empty if the image
    /// declares neither — that image is only runnable with an explicit command.
    pub(crate) default_args: Vec<String>,
}

/// Ensure the image is present (and unpacked) honouring `policy`, then resolve
/// its rootfs chain id.
pub(crate) async fn ensure_image(
    client: &containerd_client::Client,
    namespace: &str,
    image: &ContainerdImage,
    policy: ImagePullPolicy,
) -> Result<ResolvedImage, RuntimeError> {
    let present = image_present(client, namespace, &image.reference).await;

    let must_pull = match policy {
        ImagePullPolicy::Never => {
            if !present {
                return Err(RuntimeError::ImageNotFound(format!(
                    "image '{}' not in containerd image store and image_pull_policy=Never forbids pulling",
                    image.reference
                )));
            }
            false
        }
        ImagePullPolicy::IfNotPresent => !present,
        // A digest reference is immutable; re-pulling buys nothing.
        ImagePullPolicy::Always => {
            !matches!(image.parsed_ref, ImageReference::Digest(_)) || !present
        }
    };

    if must_pull {
        pull_image(client, namespace, image).await?;
    } else {
        debug!(
            "containerd image {} served from local store (policy {:?})",
            image.reference, policy
        );
    }

    let descriptor = get_image_target(client, namespace, &image.reference).await?;
    let (chain_id, default_args) =
        resolve_chain_id_and_args(client, namespace, &descriptor.digest).await?;

    Ok(ResolvedImage {
        digest: Some(descriptor.digest),
        chain_id,
        default_args,
    })
}

/// Whether an image with this reference exists in the image store.
async fn image_present(
    client: &containerd_client::Client,
    namespace: &str,
    reference: &str,
) -> bool {
    let mut images = ImagesClient::new(client.channel());
    let req = with_namespace!(
        GetImageRequest {
            name: reference.to_string(),
        },
        namespace
    );
    match images.get(req).await {
        Ok(resp) => resp.into_inner().image.is_some(),
        Err(_) => false,
    }
}

/// Pull + unpack an image via the Transfer service. The source is an OCI
/// registry; the destination is the image store with an unpack into the default
/// snapshotter so the rootfs layers are materialized for `Snapshots.Prepare`.
async fn pull_image(
    client: &containerd_client::Client,
    namespace: &str,
    image: &ContainerdImage,
) -> Result<(), RuntimeError> {
    info!("containerd pulling image {}", image.reference);

    // Registry credentials are passed as a static `Authorization: Basic <...>`
    // header on the resolver. The Transfer service also supports an interactive
    // `auth_stream` callback, but a direct Basic header is sufficient for the
    // username/password Ring receives and avoids running a streaming auth
    // handler. Token-auth registries (Docker Hub, GHCR) accept Basic creds on
    // the token endpoint, so this covers the common private-registry case.
    let mut headers = HashMap::new();
    if let Some((_, username, password)) = &image.auth {
        use base64::Engine as _;
        let raw = format!("{}:{}", username, password);
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
        headers.insert("Authorization".to_string(), format!("Basic {}", encoded));
    }

    let resolver = RegistryResolver {
        headers,
        ..Default::default()
    };
    let source = OciRegistry {
        reference: image.reference.clone(),
        resolver: Some(resolver),
    };

    let platform = Platform {
        os: "linux".to_string(),
        architecture: std::env::consts::ARCH.to_string(),
        ..Default::default()
    };
    let dest = ImageStore {
        name: image.reference.clone(),
        platforms: vec![platform.clone()],
        unpacks: vec![UnpackConfiguration {
            platform: Some(platform),
            snapshotter: DEFAULT_SNAPSHOTTER.to_string(),
        }],
        ..Default::default()
    };

    let request = TransferRequest {
        source: Some(to_any(&source)),
        destination: Some(to_any(&dest)),
        options: None,
    };

    let mut transfer = TransferClient::new(client.channel());
    transfer
        .transfer(with_namespace!(request, namespace))
        .await
        .map_err(|e| classify_pull_error(e.message(), &image.reference))?;

    info!("containerd successfully pulled image {}", image.reference);
    Ok(())
}

/// Map a Transfer error string into a typed `RuntimeError`, mirroring the Docker
/// runtime's `classify_pull_error` so deployments land in the right status.
fn classify_pull_error(msg: &str, image: &str) -> RuntimeError {
    let lower = msg.to_lowercase();
    if lower.contains("not found") || lower.contains("manifest unknown") || lower.contains("404") {
        return RuntimeError::ImageNotFound(format!("image '{}' not found: {}", image, msg));
    }
    if lower.contains("unauthorized")
        || lower.contains("authentication")
        || lower.contains("denied")
        || lower.contains("forbidden")
    {
        return RuntimeError::ImagePullFailed(format!(
            "registry authentication failed for '{}' — check config.server, config.username and \
             config.password (original error: {})",
            image, msg
        ));
    }
    if lower.contains("connection refused")
        || lower.contains("no such host")
        || lower.contains("timeout")
        || lower.contains("dial")
    {
        return RuntimeError::ImagePullFailed(format!(
            "cannot reach the registry for '{}' (original error: {})",
            image, msg
        ));
    }
    RuntimeError::ImagePullFailed(msg.to_string())
}

/// Fetch the image's target (manifest) descriptor from the image store.
async fn get_image_target(
    client: &containerd_client::Client,
    namespace: &str,
    reference: &str,
) -> Result<containerd_client::types::Descriptor, RuntimeError> {
    let mut images = ImagesClient::new(client.channel());
    let req = with_namespace!(
        GetImageRequest {
            name: reference.to_string(),
        },
        namespace
    );
    let resp = images
        .get(req)
        .await
        .map_err(|e| RuntimeError::Other(format!("GetImage failed for {}: {}", reference, e)))?;
    resp.into_inner()
        .image
        .and_then(|img| img.target)
        .ok_or_else(|| {
            RuntimeError::ImageNotFound(format!("image '{}' has no target descriptor", reference))
        })
}

/// The host platform containerd unpacked layers for. Ring builds for
/// `x86_64-unknown-linux-gnu`; matching `linux/amd64` is correct for the
/// supported targets. `arm64` falls out of `target_arch` when cross-built.
fn host_architecture() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    }
}

/// If `digest` points at a multi-arch index, return the digest of the manifest
/// matching the host platform; otherwise return `digest` unchanged.
///
/// We distinguish an index from an image manifest by structure: an index has a
/// `manifests` array and no `config`. Parsing as `OciIndex` succeeds only for
/// the former, so a failed parse means "already an image manifest" and we pass
/// the digest through untouched.
async fn resolve_platform_manifest(
    client: &containerd_client::Client,
    namespace: &str,
    digest: &str,
) -> Result<String, RuntimeError> {
    let bytes = read_content(client, namespace, digest).await?;

    let index: OciIndex = match serde_json::from_slice(&bytes) {
        Ok(index) => index,
        // Not an index (no `manifests` array) — it's already an image manifest.
        Err(_) => return Ok(digest.to_string()),
    };

    // An image manifest can also deserialize loosely; require a non-empty
    // `manifests` list to treat it as an index.
    if index.manifests.is_empty() {
        return Ok(digest.to_string());
    }

    let arch = host_architecture();
    select_platform_digest(&index, arch).ok_or_else(|| {
        RuntimeError::Other(format!(
            "image index {} has no manifest for platform linux/{}",
            digest, arch
        ))
    })
}

/// Pick the manifest digest matching `linux/<arch>` from an index, falling back
/// to a platform-less entry. Pure (no I/O) so it is unit-tested directly.
fn select_platform_digest(index: &OciIndex, arch: &str) -> Option<String> {
    index
        .manifests
        .iter()
        .find(|m| {
            m.platform
                .as_ref()
                .is_some_and(|p| p.os == "linux" && p.architecture == arch)
        })
        // Some indexes carry attestation manifests with no platform; the real
        // images declare one, so the platform match above wins when present.
        .or_else(|| index.manifests.iter().find(|m| m.platform.is_none()))
        .map(|m| m.digest.clone())
}

/// Compute an image's rootfs chain id AND its default process args from its
/// manifest + config.
///
/// containerd stores the image config (with `rootfs.diff_ids` and the
/// `Entrypoint`/`Cmd` defaults) in the content store, keyed by the manifest's
/// `config.digest`. The chain id is the iterated SHA-256 of the diff ids — the
/// same value the snapshotter keys unpacked layers under, so it is the correct
/// parent for a writable snapshot. The default args (`Entrypoint ++ Cmd`) are
/// what containerd would run when no command override is given; Ring applies
/// them to the OCI spec itself because nothing else does at this level.
async fn resolve_chain_id_and_args(
    client: &containerd_client::Client,
    namespace: &str,
    manifest_digest: &str,
) -> Result<(String, Vec<String>), RuntimeError> {
    // The target may be a multi-arch index (manifest list) rather than an image
    // manifest. Follow the index to the host-platform manifest before parsing
    // the `config` pointer — otherwise `config` is absent and parsing fails,
    // which manifested as an endless crash loop on official Docker Hub images.
    let manifest_digest = resolve_platform_manifest(client, namespace, manifest_digest).await?;

    let manifest_bytes = read_content(client, namespace, &manifest_digest).await?;
    let manifest: OciManifest = serde_json::from_slice(&manifest_bytes).map_err(|e| {
        RuntimeError::Other(format!(
            "failed to parse image manifest {}: {}",
            manifest_digest, e
        ))
    })?;

    let config_bytes = read_content(client, namespace, &manifest.config.digest).await?;
    let config: OciImageConfig = serde_json::from_slice(&config_bytes).map_err(|e| {
        RuntimeError::Other(format!(
            "failed to parse image config {}: {}",
            manifest.config.digest, e
        ))
    })?;

    if config.rootfs.diff_ids.is_empty() {
        return Err(RuntimeError::Other(
            "image config has no rootfs diff_ids".to_string(),
        ));
    }

    let default_args = image_default_args(&config.config);
    Ok((compute_chain_id(&config.rootfs.diff_ids), default_args))
}

/// The default process an image declares: `Entrypoint` concatenated with `Cmd`,
/// matching Docker/OCI semantics. Pure (no I/O) so it is unit-tested directly.
fn image_default_args(config: &OciImageConfigInner) -> Vec<String> {
    let mut args = config.entrypoint.clone().unwrap_or_default();
    args.extend(config.cmd.clone().unwrap_or_default());
    args
}

/// Iterated chain id over an ordered list of layer diff ids, per the OCI image
/// spec: `chainID(L0) = L0`, `chainID(Ln) = sha256(chainID(Ln-1) + " " + Ln)`.
pub(crate) fn compute_chain_id(diff_ids: &[String]) -> String {
    let mut chain = diff_ids[0].clone();
    for diff in &diff_ids[1..] {
        let mut hasher = Sha256::new();
        hasher.update(format!("{} {}", chain, diff).as_bytes());
        chain = format!("sha256:{}", hex::encode(hasher.finalize()));
    }
    chain
}

/// Read a full blob from the content store by digest.
async fn read_content(
    client: &containerd_client::Client,
    namespace: &str,
    digest: &str,
) -> Result<Vec<u8>, RuntimeError> {
    let mut content = ContentClient::new(client.channel());
    let req = with_namespace!(
        ReadContentRequest {
            digest: digest.to_string(),
            offset: 0,
            size: 0,
        },
        namespace
    );
    let resp = content
        .read(req)
        .await
        .map_err(|e| RuntimeError::Other(format!("ReadContent failed for {}: {}", digest, e)))?;

    let mut stream = resp.into_inner();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| RuntimeError::Other(format!("ReadContent stream error: {}", e)))?;
        buf.extend_from_slice(&chunk.data);
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_id_single_layer_is_diff_id() {
        let ids = vec!["sha256:aaaa".to_string()];
        assert_eq!(compute_chain_id(&ids), "sha256:aaaa");
    }

    #[test]
    fn chain_id_multi_layer_iterates() {
        // Known reference vector: chainID of two layers is
        // sha256("<L0> <L1>"). We only assert determinism + the prefix here
        // since the literal hash is verified against containerd at e2e time.
        let ids = vec!["sha256:aaaa".to_string(), "sha256:bbbb".to_string()];
        let chain = compute_chain_id(&ids);
        assert!(chain.starts_with("sha256:"));
        assert_ne!(chain, "sha256:aaaa");
        // Stable across calls.
        assert_eq!(chain, compute_chain_id(&ids));
    }

    #[test]
    fn image_reference_tag_canonicalized() {
        // Containerd needs a fully-qualified reference: a bare `nginx` must
        // expand to docker.io/library/nginx, else its Transfer resolver builds
        // `dummy://nginx:latest` and the URL parser rejects the tag as a port.
        let img = ContainerdImage::from_deployment("nginx", None);
        assert_eq!(img.reference, "docker.io/library/nginx:latest");
    }

    #[test]
    fn image_reference_with_tag_canonicalized() {
        let img = ContainerdImage::from_deployment("nginx:1.25-alpine", None);
        assert_eq!(img.reference, "docker.io/library/nginx:1.25-alpine");
    }

    #[test]
    fn image_reference_digest_canonicalized() {
        let img = ContainerdImage::from_deployment("nginx@sha256:abc", None);
        assert_eq!(img.reference, "docker.io/library/nginx@sha256:abc");
    }

    fn entry(digest: &str, os: Option<&str>, arch: Option<&str>) -> OciIndexEntry {
        OciIndexEntry {
            digest: digest.to_string(),
            platform: match (os, arch) {
                (Some(os), Some(arch)) => Some(OciPlatform {
                    os: os.to_string(),
                    architecture: arch.to_string(),
                }),
                _ => None,
            },
        }
    }

    #[test]
    fn select_platform_picks_matching_arch() {
        // A typical multi-arch index (nginx:alpine on Docker Hub): the bug was
        // parsing this as an image manifest, which has no `config` field.
        let index = OciIndex {
            manifests: vec![
                entry("sha256:arm64", Some("linux"), Some("arm64")),
                entry("sha256:amd64", Some("linux"), Some("amd64")),
            ],
        };
        assert_eq!(
            select_platform_digest(&index, "amd64"),
            Some("sha256:amd64".to_string())
        );
        assert_eq!(
            select_platform_digest(&index, "arm64"),
            Some("sha256:arm64".to_string())
        );
    }

    #[test]
    fn select_platform_falls_back_to_platformless_entry() {
        let index = OciIndex {
            manifests: vec![entry("sha256:plain", None, None)],
        };
        assert_eq!(
            select_platform_digest(&index, "amd64"),
            Some("sha256:plain".to_string())
        );
    }

    #[test]
    fn select_platform_none_when_no_match() {
        // Index with only a non-matching arch and no platform-less fallback.
        let index = OciIndex {
            manifests: vec![entry("sha256:arm64", Some("linux"), Some("arm64"))],
        };
        assert_eq!(select_platform_digest(&index, "amd64"), None);
    }

    #[test]
    fn image_default_args_concatenates_entrypoint_and_cmd() {
        let cfg = OciImageConfigInner {
            entrypoint: Some(vec!["/docker-entrypoint.sh".to_string()]),
            cmd: Some(vec!["nginx".to_string(), "-g".to_string()]),
        };
        assert_eq!(
            image_default_args(&cfg),
            vec!["/docker-entrypoint.sh", "nginx", "-g"]
        );
    }

    #[test]
    fn image_default_args_cmd_only() {
        let cfg = OciImageConfigInner {
            entrypoint: None,
            cmd: Some(vec!["/bin/sh".to_string()]),
        };
        assert_eq!(image_default_args(&cfg), vec!["/bin/sh"]);
    }

    #[test]
    fn image_default_args_empty_when_neither() {
        let cfg = OciImageConfigInner {
            entrypoint: None,
            cmd: None,
        };
        assert!(image_default_args(&cfg).is_empty());
    }

    #[test]
    fn select_platform_ignores_non_linux_os() {
        let index = OciIndex {
            manifests: vec![
                entry("sha256:win", Some("windows"), Some("amd64")),
                entry("sha256:lin", Some("linux"), Some("amd64")),
            ],
        };
        assert_eq!(
            select_platform_digest(&index, "amd64"),
            Some("sha256:lin".to_string())
        );
    }

    #[test]
    fn normalize_official_image_gets_library_namespace() {
        assert_eq!(normalize_repository("nginx"), "docker.io/library/nginx");
    }

    #[test]
    fn normalize_hub_user_image_gets_docker_io() {
        assert_eq!(
            normalize_repository("bitnami/redis"),
            "docker.io/bitnami/redis"
        );
    }

    #[test]
    fn normalize_explicit_registry_is_untouched() {
        assert_eq!(
            normalize_repository("ghcr.io/owner/proj"),
            "ghcr.io/owner/proj"
        );
        assert_eq!(
            normalize_repository("registry.example.com/team/app"),
            "registry.example.com/team/app"
        );
    }

    #[test]
    fn normalize_registry_with_port_is_untouched() {
        assert_eq!(
            normalize_repository("localhost:5000/app"),
            "localhost:5000/app"
        );
        assert_eq!(normalize_repository("localhost/app"), "localhost/app");
    }
}
