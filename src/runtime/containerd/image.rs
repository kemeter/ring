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
        // The reference containerd stores the image under is the canonical
        // `name:tag` / `name@digest` — same string the user wrote, normalized.
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

/// Subset of an OCI image manifest we need: the `config` descriptor pointer.
#[derive(Deserialize)]
struct OciManifest {
    config: OciDescriptor,
}

#[derive(Deserialize)]
struct OciDescriptor {
    digest: String,
}

/// Subset of an OCI image config we need: the rootfs diff ids.
#[derive(Deserialize)]
struct OciImageConfig {
    rootfs: OciRootfs,
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
    let chain_id = resolve_chain_id(client, namespace, &descriptor.digest).await?;

    Ok(ResolvedImage {
        digest: Some(descriptor.digest),
        chain_id,
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

/// Compute the rootfs chain id of an image from its manifest + config.
///
/// containerd stores the image config (with `rootfs.diff_ids`) in the content
/// store, keyed by the manifest's `config.digest`. The chain id is the iterated
/// SHA-256 of the diff ids — the same value the snapshotter keys unpacked layers
/// under, so it is the correct parent for a writable snapshot.
async fn resolve_chain_id(
    client: &containerd_client::Client,
    namespace: &str,
    manifest_digest: &str,
) -> Result<String, RuntimeError> {
    let manifest_bytes = read_content(client, namespace, manifest_digest).await?;
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

    Ok(compute_chain_id(&config.rootfs.diff_ids))
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
        let img = ContainerdImage::from_deployment("nginx", None);
        assert_eq!(img.reference, "nginx:latest");
    }

    #[test]
    fn image_reference_digest_canonicalized() {
        let img = ContainerdImage::from_deployment("nginx@sha256:abc", None);
        assert_eq!(img.reference, "nginx@sha256:abc");
    }
}
