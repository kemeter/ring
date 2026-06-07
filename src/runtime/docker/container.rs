use super::{DockerImage, ImagePullPolicy, ImageReference, parse_image_reference, tiny_id};
use crate::hypervisor::error::RuntimeError;
use crate::models::deployments::{
    Deployment, EnvValue, NetworkMode, parse_cpu_string, parse_memory_string,
};
use crate::models::health_check::HealthCheck;
use crate::models::volume::ResolvedMount;
use bollard::{
    Docker,
    auth::DockerCredentials,
    models::{
        ContainerCreateBody, EndpointSettings, HealthConfig, HostConfig, Mount, MountTypeEnum,
        MountVolumeOptions, MountVolumeOptionsDriverConfig, NetworkConnectRequest,
        NetworkCreateRequest, PortBinding,
    },
    query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, InspectNetworkOptionsBuilder,
        RemoveContainerOptionsBuilder, StartContainerOptionsBuilder, StopContainerOptionsBuilder,
    },
};
use futures::StreamExt;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

fn build_user_config(
    deployment_config: &Option<crate::models::deployments::DeploymentConfig>,
) -> Option<String> {
    let user = deployment_config.as_ref()?.user.as_ref()?;
    match (user.id, user.group) {
        (Some(uid), Some(gid)) => Some(format!("{}:{}", uid, gid)),
        (Some(uid), None) => Some(uid.to_string()),
        _ => None,
    }
}

fn get_privileged_config(
    deployment_config: &Option<crate::models::deployments::DeploymentConfig>,
) -> Option<bool> {
    deployment_config
        .as_ref()
        .and_then(|c| c.user.as_ref())
        .and_then(|u| u.privileged)
}

/// Pick the digest entry whose repository prefix matches `repo`. Docker stores
/// one `RepoDigest` per registry an image is tagged with, so blindly taking
/// the first entry could pin a digest from a sibling tag (e.g. `nginx` and
/// `myregistry/nginx` pointing at the same content). We match on the part
/// before `@` so a deployment of `myregistry/nginx:1.25` doesn't end up
/// recorded with a `docker.io/library/nginx@sha256:...` digest.
fn extract_digest(repo_digests: &Option<Vec<String>>, repo: &str) -> Option<String> {
    let entries = repo_digests.as_ref()?;
    if let Some(digest) = entries.iter().find_map(|d| {
        let (entry_repo, digest) = d.split_once('@')?;
        (entry_repo == repo).then(|| digest.to_string())
    }) {
        return Some(digest);
    }

    // Fallback: no entry matched the requested repo. This is expected when
    // Docker normalizes the name (`nginx` → `docker.io/library/nginx`), but
    // it can also signal a registry/repo mismatch worth investigating, so we
    // warn rather than silently substitute.
    let fallback = entries
        .iter()
        .find_map(|d| d.split_once('@').map(|(_, digest)| digest.to_string()));
    if fallback.is_some() {
        warn!(
            "No RepoDigest matched '{}'; using first available digest. Entries: {:?}",
            repo, entries
        );
    }
    fallback
}

/// Translate the first readiness HC of `type: command` into a Docker
/// `HEALTHCHECK`, so the proxy (Traefik / Sozune) can gate traffic via the
/// container's `Status: healthy` flag — without that translation, the proxy
/// would route to the new container as soon as it is `Running`, which is
/// before the application is actually ready.
///
/// Only `command` is translated: Docker's healthcheck takes a shell command,
/// it has no native TCP/HTTP probe. A `tcp` or `http` readiness HC is still
/// honoured by Ring's own scheduler-side gating, but the proxy won't see it.
/// This is documented as a limitation; users who need proxy-aware TCP/HTTP
/// readiness can wrap the probe in a shell command (`curl -fsS …`).
fn build_health_config(health_checks: &[HealthCheck]) -> Option<HealthConfig> {
    let hc = health_checks
        .iter()
        .filter(|hc| hc.is_readiness())
        .find(|hc| matches!(hc, HealthCheck::Command { .. }))?;

    let command = match hc {
        HealthCheck::Command { command, .. } => command.clone(),
        _ => return None,
    };

    // Docker expects nanoseconds. Fall back to safe defaults if the user's
    // duration string is malformed — the Ring scheduler-side gate will still
    // run with the same values, so a misconfiguration doesn't go unnoticed.
    let to_nanos = |s: &str| -> i64 {
        HealthCheck::parse_duration(s)
            .ok()
            .and_then(|d| i64::try_from(d.as_nanos()).ok())
            .unwrap_or(0)
    };

    Some(HealthConfig {
        test: Some(vec!["CMD-SHELL".to_string(), command]),
        interval: Some(to_nanos(hc.interval())),
        timeout: Some(to_nanos(hc.timeout())),
        retries: Some(hc.threshold() as i64),
        start_period: None,
        start_interval: None,
    })
}

/// Decide whether to serve the image from the local cache instead of going
/// to the registry. Pure decision so it can be unit-tested without a Docker
/// daemon.
///
/// We prefer the cache when:
///   - no registry credentials were provided (`has_auth == false`), or
///   - the policy is `IfNotPresent` (explicitly opts into the cache), or
///   - the reference is a `Digest` (immutable content — re-pulling is a
///     no-op).
///
/// The first clause is the design rule, not a fallback: Kemeter only sends
/// registry credentials when a private registry is actually needed. When it
/// sends none, that's deliberate — Ring serves the image already present
/// locally, *whatever the policy*, including `Always`. (`Always` can only
/// mean "re-check the registry every reconcile" when there *is* a registry
/// to check; with no registry configured there is nothing to re-check, so
/// the local image is authoritative.) This is also what kept things working
/// before 1790c8d, which inadvertently turned this nominal path into an
/// anonymous pull of a private image and crash-looped prod.
///
/// When credentials *are* present, strict `Always` is honoured and we hit
/// the registry on every reconcile.
pub(crate) fn prefer_local_cache(
    policy: ImagePullPolicy,
    reference: &ImageReference,
    has_auth: bool,
) -> bool {
    !has_auth
        || matches!(policy, ImagePullPolicy::IfNotPresent)
        || matches!(reference, ImageReference::Digest(_))
}

/// Classify a Docker image-pull error into a `RuntimeError` whose message
/// names the likely cause *and* the fix. Bollard surfaces registry failures
/// as opaque strings; left untouched they reach the operator as
/// `Failed to pull image '…': <bollard dump>`, which says what failed but not
/// why or what to do. We match on the substrings Docker/registries emit and
/// rewrite them into one actionable line. The `image` argument lets the
/// "registry unreachable" case name the host the operator must check.
///
/// Order matters: a missing image (404) is distinct from an unreachable or
/// auth-refusing registry, so it's matched first and kept as `ImageNotFound`.
fn classify_pull_error(msg: &str, image: &str) -> RuntimeError {
    let lower = msg.to_lowercase();

    if lower.contains("404") || lower.contains("not found") || lower.contains("manifest unknown") {
        return RuntimeError::ImageNotFound(msg.to_string());
    }

    if lower.contains("unauthorized")
        || lower.contains("authentication required")
        || lower.contains("denied")
        || lower.contains("forbidden")
    {
        return RuntimeError::ImagePullFailed(format!(
            "registry authentication failed for '{image}' — check config.server, \
             config.username and config.password (original error: {msg})"
        ));
    }

    if lower.contains("connection refused")
        || lower.contains("no such host")
        || lower.contains("dial tcp")
        || lower.contains("timeout")
        || lower.contains("i/o timeout")
        || lower.contains("connection reset")
    {
        return RuntimeError::ImagePullFailed(format!(
            "cannot reach the registry for '{image}' — is it up and the registry \
             host correct? (original error: {msg})"
        ));
    }

    RuntimeError::ImagePullFailed(msg.to_string())
}

async fn pull_image(
    docker: Docker,
    image_config: DockerImage,
    policy: ImagePullPolicy,
) -> Result<Option<String>, RuntimeError> {
    let image_name = image_config.full_ref();
    let repo = image_config.name.clone();

    // `Never` is handled by the caller (which fails fast on cache miss) and
    // never reaches this function; the assertion pins the contract.
    debug_assert!(
        !matches!(policy, ImagePullPolicy::Never),
        "pull_image must not be called with Never policy"
    );

    let has_auth = image_config.auth.is_some();
    let prefer_cache = prefer_local_cache(policy, &image_config.reference, has_auth);

    if prefer_cache {
        if let Ok(inspect) = docker.inspect_image(&image_name).await {
            // Nominal path when no registry is configured: the locally
            // present image is authoritative. Not a degraded mode.
            debug!("Docker image {} served from local cache", image_name);
            return Ok(extract_digest(&inspect.repo_digests, &repo));
        }
        // Not in cache: attempt the pull anyway. With no credentials this is
        // an anonymous pull — it still succeeds for a public image, and
        // otherwise produces a clear registry error.
        debug!("Docker image {} not in local cache, pulling...", image_name);
    } else {
        info!("Pull docker image: {} (policy: Always)", image_name);
    }

    // `from_image` is the repository (with optional registry host), the
    // second field is either a tag or a digest. Bollard accepts the digest
    // form here directly.
    let (tag_param, digest_param) = match &image_config.reference {
        ImageReference::Tag(t) => (Some(t.as_str()), None),
        ImageReference::Digest(d) => (None, Some(d.as_str())),
    };
    let mut builder = CreateImageOptionsBuilder::new().from_image(&repo);
    if let Some(t) = tag_param {
        builder = builder.tag(t);
    }
    // Bollard's `CreateImageOptions` exposes digest via the same `tag` field
    // in practice (Docker's HTTP API accepts `tag=sha256:...`). The
    // distinction matters for the from_image value, which must not embed the
    // digest itself.
    if let Some(d) = digest_param {
        builder = builder.tag(d);
    }
    let create_image_options = builder.build();

    let credentials = image_config
        .auth
        .map(|(server, username, password)| DockerCredentials {
            username: Some(username),
            password: Some(password),
            serveraddress: Some(server),
            ..Default::default()
        });

    let mut stream = docker.create_image(Some(create_image_options), None, credentials);

    while let Some(pull_result) = stream.next().await {
        if let Err(e) = pull_result {
            let error_msg = e.to_string();
            error!("Docker image pull error: {}", error_msg);

            // Bail on the first error instead of draining the rest of the
            // stream — the previous version kept iterating to find the "last"
            // error, which delayed the failure and could mask the root cause
            // behind a later io error. `classify_pull_error` rewrites registry
            // failures into an actionable message (auth / unreachable / 404).
            return Err(classify_pull_error(&error_msg, &image_name));
        }
    }

    match docker.inspect_image(&image_name).await {
        Ok(inspect) => {
            info!("Docker successfully pulled image {}", image_name);
            Ok(extract_digest(&inspect.repo_digests, &repo))
        }
        Err(e) => {
            error!(
                "Docker image {} still not available after pull: {}",
                image_name, e
            );
            Err(RuntimeError::ImageNotFound(format!(
                "Image {} not available after pull",
                image_name
            )))
        }
    }
}

/// Provision named volumes explicitly before they are mounted, instead of
/// letting Docker auto-create them on first mount. Auto-created volumes carry
/// no labels, so they are untraceable (which deployment owns them?) and
/// unprunable. Here we tag the volume with Ring labels so it can be attributed
/// and reaped deliberately.
///
/// We deliberately set no `driver_opts`. The obvious idea — `o=sync` on the
/// `local` driver for crash durability — does not work: the `local` driver
/// treats `o` as `mount(8)` options, which require a `type` and `device` too,
/// so `o=sync` alone is rejected with `400 missing required option: "device"`.
/// Honouring it would mean turning the named volume into a host bind-mount,
/// changing its semantics entirely. Durability for a plain named volume is a
/// property of the host filesystem under `/var/lib/docker/volumes` and the
/// workload's own fsync discipline, not a per-volume driver opt — so we leave
/// it to the operator's driver configuration.
///
/// `create_volume` is idempotent in the Docker Engine: creating a volume whose
/// name already exists returns the existing volume rather than erroring, so this
/// is safe to call on every (re)deploy. Existing volumes keep their original
/// options, which is the intended "preserve data across deploys" behaviour.
async fn ensure_named_volume(
    docker: &Docker,
    name: &str,
    driver: &str,
    namespace: &str,
    deployment_name: &str,
) -> Result<(), RuntimeError> {
    let driver = if driver.is_empty() { "local" } else { driver };

    let mut labels = HashMap::new();
    labels.insert("ring.managed".to_string(), "true".to_string());
    labels.insert("ring.namespace".to_string(), namespace.to_string());
    labels.insert("ring.deployment".to_string(), deployment_name.to_string());

    let request = bollard::models::VolumeCreateRequest {
        name: Some(name.to_string()),
        driver: Some(driver.to_string()),
        labels: Some(labels),
        ..Default::default()
    };

    match docker.create_volume(request).await {
        Ok(_) => {
            debug!("Ensured named volume '{}' (driver={})", name, driver);
            Ok(())
        }
        Err(e) => Err(RuntimeError::InstanceCreationFailed(format!(
            "failed to provision named volume '{}': {}",
            name, e
        ))),
    }
}

pub(crate) async fn create_container(
    deployment: &mut Deployment,
    docker: &Docker,
    resolved_mounts: &[crate::models::volume::ResolvedMount],
) -> Result<(), RuntimeError> {
    debug!("Create container for deployment id: {}", &deployment.id);

    // Admission control before any expensive work (image pull, network create):
    // if the host can't hold the requested memory, the container would only get
    // OOM-killed at runtime (or, with no limit, take the host down with it).
    // Fail here with an actionable message instead.
    crate::hypervisor::resources::check_host_memory(deployment)?;

    let (name, reference) = parse_image_reference(&deployment.image);

    let mut image_config = DockerImage {
        name,
        reference,
        auth: None,
    };

    if let Some(config) = &deployment.config
        && let (Some(server), Some(username), Some(password)) =
            (&config.server, &config.username, &config.password)
    {
        image_config.auth = Some((server.clone(), username.clone(), password.clone()));
    }

    let policy = deployment
        .config
        .as_ref()
        .map(|c| ImagePullPolicy::parse(&c.image_pull_policy))
        .unwrap_or(ImagePullPolicy::Always);

    match policy {
        ImagePullPolicy::Never => {
            // Contract: never reach out to a registry. If the image isn't
            // already cached, surface a clear `ImageNotFound` that mentions
            // the policy — otherwise the operator sees an opaque bollard 404
            // and has no way to tell whether the registry is unreachable or
            // the policy itself blocked the pull.
            let image_name = image_config.full_ref();
            match docker.inspect_image(&image_name).await {
                Ok(inspect) => {
                    deployment.image_digest =
                        extract_digest(&inspect.repo_digests, &image_config.name);
                }
                Err(_) => {
                    return Err(RuntimeError::ImageNotFound(format!(
                        "image '{}' not in local cache and image_pull_policy=Never forbids pulling",
                        image_name
                    )));
                }
            }
        }
        ImagePullPolicy::Always | ImagePullPolicy::IfNotPresent => {
            let digest = pull_image(docker.clone(), image_config, policy).await?;
            deployment.image_digest = digest;
        }
    }

    let use_host_network = matches!(
        deployment.network.as_ref().map(|n| n.mode),
        Some(NetworkMode::Host)
    );

    let network_name = format!("ring_{}", deployment.namespace);
    if !use_host_network {
        create_network(docker.clone(), network_name.clone()).await?;
    }

    let temporary_id = tiny_id();
    let container_name = format!(
        "{}_{}_{}",
        &deployment.namespace, &deployment.name, temporary_id
    );

    let mut labels = HashMap::new();
    labels.insert("ring_deployment".to_string(), deployment.id.clone());
    for (key, value) in deployment.labels.iter() {
        labels.insert(key.clone(), value.clone());
    }

    let envs: Vec<String> = deployment.environment
        .iter()
        .filter_map(|(key, env_value)| {
            match env_value {
                EnvValue::Plain(v) => Some(format!("{}={}", key, v)),
                EnvValue::SecretRef { .. } => {
                    // SecretRef should be resolved before reaching the runtime
                    error!("Unresolved secretRef for key '{}' - this should have been resolved before calling create_container", key);
                    None
                }
            }
        })
        .collect();

    let mut mounts: Vec<Mount> = vec![];
    for resolved in resolved_mounts {
        // Provision named volumes explicitly (labels + durability) before the
        // mount, rather than relying on Docker's implicit on-first-mount create.
        if let ResolvedMount::Named { name, driver, .. } = resolved {
            ensure_named_volume(
                docker,
                name,
                driver,
                &deployment.namespace,
                &deployment.name,
            )
            .await?;
        }
        mounts.push(create_mount_from_resolved(resolved, &deployment.id).await?);
    }

    let user_config = build_user_config(&deployment.config);
    let privileged_config = get_privileged_config(&deployment.config);

    let port_bindings: HashMap<String, Option<Vec<PortBinding>>> = deployment
        .ports
        .iter()
        .map(|p| {
            let key = format!("{}/{}", p.target, p.protocol.as_str());
            let binding = PortBinding {
                host_ip: Some(p.host_ip.clone().unwrap_or_else(|| "0.0.0.0".to_string())),
                host_port: Some(p.published.to_string()),
            };
            (key, Some(vec![binding]))
        })
        .collect();

    // ExposedPorts must mirror the PortBindings keys so Docker actually spawns
    // the docker-proxy and installs the DNAT iptables rule. Without this field
    // the binding in HostConfig is silently ignored (no proxy, port never open).
    // Computed before `port_bindings` is moved into `host_config` below.
    let exposed_ports: Option<Vec<String>> = if port_bindings.is_empty() {
        None
    } else {
        Some(port_bindings.keys().cloned().collect())
    };

    let host_config = HostConfig {
        mounts: Some(mounts),
        privileged: privileged_config,
        network_mode: if use_host_network {
            Some("host".to_string())
        } else {
            None
        },
        port_bindings: if port_bindings.is_empty() {
            None
        } else {
            Some(port_bindings)
        },
        nano_cpus: deployment
            .resources
            .as_ref()
            .and_then(|r| r.limits.as_ref())
            .and_then(|l| l.cpu.as_ref())
            .and_then(|cpu| parse_cpu_string(cpu).ok()),
        memory: deployment
            .resources
            .as_ref()
            .and_then(|r| r.limits.as_ref())
            .and_then(|l| l.memory.as_ref())
            .and_then(|m| parse_memory_string(m).ok()),
        memory_reservation: deployment
            .resources
            .as_ref()
            .and_then(|r| r.requests.as_ref())
            .and_then(|req| req.memory.as_ref())
            .and_then(|m| parse_memory_string(m).ok()),
        ..Default::default()
    };

    let config = ContainerCreateBody {
        image: Some(deployment.image.clone()),
        cmd: Some(deployment.command.clone()),
        env: Some(envs),
        labels: Some(labels),
        host_config: Some(host_config),
        user: user_config,
        healthcheck: build_health_config(&deployment.health_checks),
        exposed_ports,
        ..Default::default()
    };

    let options = CreateContainerOptionsBuilder::new()
        .name(&container_name)
        .build();

    match docker.create_container(Some(options), config).await {
        Ok(container) => {
            debug!("Docker create container {:?}", container.id);
            deployment.instances.push(container.id.to_string());

            if !use_host_network {
                let endpoint_config = EndpointSettings {
                    aliases: Some(vec![deployment.name.clone(), container_name.clone()]),
                    ..Default::default()
                };

                let connect_request = NetworkConnectRequest {
                    container: container.id.clone(),
                    endpoint_config: Some(endpoint_config),
                };

                if let Err(e) = docker.connect_network(&network_name, connect_request).await {
                    // Same orphan guard as `start_container` below: Docker
                    // accepted `create` (the container exists, in `Created`
                    // state) but the follow-up call failed. Without this
                    // cleanup, the orphan would be picked up by
                    // `list_instances` on the next reconciliation tick as
                    // if it were a healthy instance, masking the need to
                    // retry and accumulating one stale container per failed
                    // attempt.
                    remove_container(docker.clone(), container.id.clone()).await;
                    // Also drop the id from in-memory instances so the
                    // caller doesn't carry a reference to a container that
                    // no longer exists.
                    deployment.instances.pop();
                    return Err(RuntimeError::InstanceCreationFailed(format!(
                        "Docker failed to connect to network: {}",
                        e
                    )));
                }
            }

            let start_options = StartContainerOptionsBuilder::new().build();
            if let Err(e) = docker
                .start_container(&container.id, Some(start_options))
                .await
            {
                // Docker accepted `create` but `start` failed — leaving the
                // container in `Created` state. Remove it before returning
                // the error so the next retry isn't shadowed by an orphan
                // container that's neither running nor counted toward
                // restart_count.
                remove_container(docker.clone(), container.id.clone()).await;
                deployment.instances.pop();
                return Err(RuntimeError::InstanceCreationFailed(format!(
                    "Docker failed to start container: {}",
                    e
                )));
            }

            info!(
                "Docker container {} created and started successfully",
                container_name
            );
            Ok(())
        }
        Err(e) => {
            error!("Docker failed to create container: {}", e);
            Err(RuntimeError::from(e))
        }
    }
}

async fn create_mount_from_resolved(
    resolved: &ResolvedMount,
    deployment_id: &str,
) -> Result<Mount, RuntimeError> {
    match resolved {
        ResolvedMount::Bind {
            source,
            destination,
            read_only,
        } => {
            let type_mount = if source.starts_with('/') {
                Some(MountTypeEnum::BIND)
            } else {
                Some(MountTypeEnum::VOLUME)
            };
            Ok(Mount {
                target: Some(destination.clone()),
                source: Some(source.clone()),
                typ: type_mount,
                read_only: Some(*read_only),
                ..Default::default()
            })
        }
        ResolvedMount::Named {
            name,
            destination,
            read_only,
            driver,
        } => {
            let volume_options = if !driver.is_empty() && driver != "local" {
                Some(MountVolumeOptions {
                    driver_config: Some(MountVolumeOptionsDriverConfig {
                        name: Some(driver.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            } else {
                None
            };
            Ok(Mount {
                target: Some(destination.clone()),
                source: Some(name.clone()),
                typ: Some(MountTypeEnum::VOLUME),
                read_only: Some(*read_only),
                volume_options,
                ..Default::default()
            })
        }
        ResolvedMount::Content {
            content,
            destination,
        } => {
            let temp_dir = format!("/tmp/ring_configs/{}", deployment_id);
            tokio::fs::create_dir_all(&temp_dir).await?;

            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            content.hash(&mut hasher);
            let hash = format!("{:x}", hasher.finish());
            let temp_file = format!("{}/{}", temp_dir, hash);
            if !tokio::fs::try_exists(&temp_file).await.unwrap_or(false) {
                tokio::fs::write(&temp_file, content).await?;
            }

            debug!(
                "Created temporary config file: {} -> {}",
                temp_file, destination
            );

            Ok(Mount {
                target: Some(destination.clone()),
                source: Some(temp_file),
                typ: Some(MountTypeEnum::BIND),
                read_only: Some(true),
                ..Default::default()
            })
        }
    }
}

pub(crate) async fn remove_container(docker: Docker, container_id: String) {
    let stop_options = StopContainerOptionsBuilder::new().build();

    match docker
        .stop_container(&container_id, Some(stop_options))
        .await
    {
        Ok(_) => debug!("Container {} stopped successfully", container_id),
        Err(e) => debug!("Error stopping container {}: {:?}", container_id, e),
    }

    // `v(true)` removes the *anonymous* volumes attached to this container —
    // the ones Docker auto-creates from an image's `VOLUME` directive and which
    // would otherwise pile up as untracked orphans across redeploys. Docker
    // never deletes *named* volumes via this flag, so Ring-managed and
    // operator-named data is preserved. This is a per-container, zero-blast-
    // radius cleanup, not a daemon-wide volume prune.
    let remove_options = RemoveContainerOptionsBuilder::new().v(true).build();
    match docker
        .remove_container(&container_id, Some(remove_options))
        .await
    {
        Ok(_) => info!("Container {} removed successfully", container_id),
        Err(e) => error!("Error removing container {}: {:?}", container_id, e),
    }
}

pub(crate) async fn remove_container_by_id(docker: &Docker, container_id: String) -> bool {
    let stop_options = StopContainerOptionsBuilder::new().build();
    let _ = docker
        .stop_container(&container_id, Some(stop_options))
        .await;

    // See `remove_container`: `v(true)` reaps anonymous volumes only; named
    // (Ring-managed / operator) volumes are untouched.
    let remove_options = RemoveContainerOptionsBuilder::new().v(true).build();
    match docker
        .remove_container(&container_id, Some(remove_options))
        .await
    {
        Ok(_) => {
            info!("Container {} removed successfully", container_id);
            true
        }
        Err(e) => {
            error!("Error removing container {}: {:?}", container_id, e);
            false
        }
    }
}

async fn create_network(docker: Docker, network_name: String) -> Result<(), RuntimeError> {
    debug!("Start Docker create network: {}", network_name);

    let inspect_options = InspectNetworkOptionsBuilder::new().build();
    match docker
        .inspect_network(&network_name, Some(inspect_options))
        .await
    {
        Ok(_) => {
            debug!("Docker network {} already exists", network_name);
            Ok(())
        }
        Err(_) => {
            info!("Docker create network: {}", network_name);

            let create_request = NetworkCreateRequest {
                name: network_name.clone(),
                ..Default::default()
            };

            match docker.create_network(create_request).await {
                Ok(info) => {
                    debug!("Network created: {:?}", info);
                    Ok(())
                }
                Err(e) => {
                    error!("Docker network create error: {}", e);
                    Err(RuntimeError::NetworkCreationFailed(format!(
                        "Failed to create network {}: {}",
                        network_name, e
                    )))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::deployments::UserConfig;

    #[test]
    fn test_build_user_config_with_uid_and_gid() {
        let config = Some(crate::models::deployments::DeploymentConfig {
            image_pull_policy: String::from("always"),
            server: None,
            username: None,
            password: None,
            user: Some(UserConfig {
                id: Some(1000),
                group: Some(1000),
                privileged: Some(false),
            }),
        });
        assert_eq!(build_user_config(&config), Some("1000:1000".to_string()));
    }

    #[test]
    fn test_build_user_config_with_uid_only() {
        let config = Some(crate::models::deployments::DeploymentConfig {
            image_pull_policy: String::from("always"),
            server: None,
            username: None,
            password: None,
            user: Some(UserConfig {
                id: Some(1000),
                group: None,
                privileged: Some(false),
            }),
        });
        assert_eq!(build_user_config(&config), Some("1000".to_string()));
    }

    #[test]
    fn test_build_user_config_none() {
        assert_eq!(build_user_config(&None), None);
    }

    #[test]
    fn extract_digest_none_when_repo_digests_missing() {
        assert_eq!(extract_digest(&None, "nginx"), None);
    }

    #[test]
    fn extract_digest_picks_matching_repo() {
        // Docker can attach the same image content to multiple repos; we must
        // pin to the one we actually pulled, not whichever happens to come
        // back first.
        let digests = Some(vec![
            "docker.io/library/nginx@sha256:aaa".to_string(),
            "ghcr.io/kemeter/nginx@sha256:bbb".to_string(),
        ]);
        assert_eq!(
            extract_digest(&digests, "ghcr.io/kemeter/nginx"),
            Some("sha256:bbb".to_string())
        );
    }

    #[test]
    fn extract_digest_falls_back_to_first_when_no_match() {
        // Docker may normalize names (`nginx` → `docker.io/library/nginx`).
        // The fallback preserves the previous behaviour rather than
        // returning None and losing the digest entirely.
        let digests = Some(vec!["docker.io/library/nginx@sha256:zzz".to_string()]);
        assert_eq!(
            extract_digest(&digests, "nginx"),
            Some("sha256:zzz".to_string())
        );
    }

    #[test]
    fn extract_digest_handles_malformed_entry() {
        // An entry without `@` should not panic; it's silently skipped and
        // the next valid one wins.
        let digests = Some(vec![
            "garbage_without_at_sign".to_string(),
            "nginx@sha256:ok".to_string(),
        ]);
        assert_eq!(
            extract_digest(&digests, "nginx"),
            Some("sha256:ok".to_string())
        );
    }

    #[test]
    fn test_get_privileged_config() {
        let config = Some(crate::models::deployments::DeploymentConfig {
            image_pull_policy: String::from("always"),
            server: None,
            username: None,
            password: None,
            user: Some(UserConfig {
                id: Some(0),
                group: Some(0),
                privileged: Some(true),
            }),
        });
        assert_eq!(get_privileged_config(&config), Some(true));
    }

    #[tokio::test]
    async fn test_bind_mount_from_resolved() {
        let resolved = ResolvedMount::Bind {
            source: "/host/path".to_string(),
            destination: "/container/path".to_string(),
            read_only: false,
        };
        let mount = create_mount_from_resolved(&resolved, "test-deployment")
            .await
            .unwrap();
        assert_eq!(mount.target, Some("/container/path".to_string()));
        assert_eq!(mount.source, Some("/host/path".to_string()));
        assert_eq!(mount.typ, Some(MountTypeEnum::BIND));
        assert_eq!(mount.read_only, Some(false));
    }

    #[tokio::test]
    async fn test_content_mount_from_resolved() {
        let resolved = ResolvedMount::Content {
            content: "server { listen 80; }".to_string(),
            destination: "/app/nginx.conf".to_string(),
        };
        let mount = create_mount_from_resolved(&resolved, "test-deployment")
            .await
            .unwrap();
        assert_eq!(mount.target, Some("/app/nginx.conf".to_string()));
        assert!(
            mount
                .source
                .unwrap()
                .contains("/tmp/ring_configs/test-deployment")
        );
        assert_eq!(mount.read_only, Some(true));
    }

    #[tokio::test]
    async fn test_named_volume_from_resolved() {
        let resolved = ResolvedMount::Named {
            name: "my-docker-volume".to_string(),
            destination: "/app/data".to_string(),
            read_only: false,
            driver: "local".to_string(),
        };
        let mount = create_mount_from_resolved(&resolved, "test-deployment")
            .await
            .unwrap();
        assert_eq!(mount.target, Some("/app/data".to_string()));
        assert_eq!(mount.source, Some("my-docker-volume".to_string()));
        assert_eq!(mount.typ, Some(MountTypeEnum::VOLUME));
        assert_eq!(mount.read_only, Some(false));
        assert!(mount.volume_options.is_none());
    }

    #[tokio::test]
    async fn test_named_volume_with_nfs_driver() {
        let resolved = ResolvedMount::Named {
            name: "shared".to_string(),
            destination: "/mnt".to_string(),
            read_only: true,
            driver: "nfs".to_string(),
        };
        let mount = create_mount_from_resolved(&resolved, "test-deployment")
            .await
            .unwrap();
        let driver_name = mount.volume_options.unwrap().driver_config.unwrap().name;
        assert_eq!(driver_name, Some("nfs".to_string()));
    }

    #[test]
    fn extract_digest_none_when_no_at_sign() {
        // Single entry without `@` is malformed → no digest can be recovered.
        let repo_digests = Some(vec!["nginx:latest".to_string()]);
        assert_eq!(extract_digest(&repo_digests, "nginx"), None);
    }

    #[test]
    fn extract_digest_none_when_empty_vec() {
        assert_eq!(extract_digest(&Some(vec![]), "nginx"), None);
    }

    use crate::models::health_check::FailureAction;

    #[test]
    fn build_health_config_returns_none_when_no_readiness_hc() {
        let hcs = vec![HealthCheck::Command {
            command: "echo ok".to_string(),
            interval: "10s".to_string(),
            timeout: "5s".to_string(),
            threshold: 3,
            on_failure: FailureAction::Alert,
            readiness: false,
            min_healthy_time: None,
        }];
        assert!(build_health_config(&hcs).is_none());
    }

    #[test]
    fn build_health_config_returns_none_for_tcp_readiness() {
        // TCP readiness gates Ring scheduling but does not become a Docker
        // HEALTHCHECK — Docker's healthcheck takes a shell command only.
        let hcs = vec![HealthCheck::Tcp {
            port: 80,
            interval: "10s".to_string(),
            timeout: "5s".to_string(),
            threshold: 3,
            on_failure: FailureAction::Alert,
            readiness: true,
            min_healthy_time: None,
        }];
        assert!(build_health_config(&hcs).is_none());
    }

    #[test]
    fn build_health_config_returns_none_for_http_readiness() {
        let hcs = vec![HealthCheck::Http {
            url: "http://localhost/health".to_string(),
            interval: "10s".to_string(),
            timeout: "5s".to_string(),
            threshold: 3,
            on_failure: FailureAction::Alert,
            readiness: true,
            min_healthy_time: None,
        }];
        assert!(build_health_config(&hcs).is_none());
    }

    #[test]
    fn build_health_config_translates_command_readiness() {
        let hcs = vec![HealthCheck::Command {
            command: "test -f /var/run/kemeter/ready".to_string(),
            interval: "10s".to_string(),
            timeout: "5s".to_string(),
            threshold: 3,
            on_failure: FailureAction::Alert,
            readiness: true,
            min_healthy_time: None,
        }];
        let cfg = build_health_config(&hcs).expect("should translate");
        assert_eq!(
            cfg.test,
            Some(vec![
                "CMD-SHELL".to_string(),
                "test -f /var/run/kemeter/ready".to_string(),
            ])
        );
        assert_eq!(cfg.interval, Some(10_000_000_000));
        assert_eq!(cfg.timeout, Some(5_000_000_000));
        assert_eq!(cfg.retries, Some(3));
    }

    #[test]
    fn build_health_config_picks_first_command_readiness_when_mixed() {
        let hcs = vec![
            HealthCheck::Tcp {
                port: 80,
                interval: "10s".to_string(),
                timeout: "5s".to_string(),
                threshold: 3,
                on_failure: FailureAction::Alert,
                readiness: true,
                min_healthy_time: None,
            },
            HealthCheck::Command {
                command: "/usr/local/bin/ready.sh".to_string(),
                interval: "15s".to_string(),
                timeout: "3s".to_string(),
                threshold: 5,
                on_failure: FailureAction::Alert,
                readiness: true,
                min_healthy_time: None,
            },
        ];
        let cfg = build_health_config(&hcs).expect("should translate the command HC");
        assert_eq!(cfg.test.as_ref().unwrap()[1], "/usr/local/bin/ready.sh");
    }

    // --- prefer_local_cache: no private registry configured means we serve
    //     the local image first. This is the nominal design, not a fallback. ---

    #[test]
    fn prefer_cache_no_registry_serves_local_first_any_policy() {
        // Design rule: when Kemeter sends no registry credentials (auth=None)
        // — because none is needed — Ring serves the cached image first,
        // whatever the policy. Not a degraded mode: this is the intended
        // behaviour. Always must not force a registry round-trip here.
        let r = ImageReference::Tag("9809f6b1".to_string());
        assert!(prefer_local_cache(ImagePullPolicy::Always, &r, false));
        assert!(prefer_local_cache(ImagePullPolicy::IfNotPresent, &r, false));
    }

    #[test]
    fn prefer_cache_ifnotpresent_uses_cache_even_with_registry() {
        // IfNotPresent always opts into the cache, registry or not.
        let r = ImageReference::Tag("9809f6b1".to_string());
        assert!(prefer_local_cache(ImagePullPolicy::IfNotPresent, &r, true));
    }

    #[test]
    fn prefer_cache_digest_reference_uses_cache() {
        // A digest is immutable content — re-pulling buys nothing, so we
        // prefer the cache even under Always and even with a registry.
        let r = ImageReference::Digest("sha256:deadbeef".to_string());
        assert!(prefer_local_cache(ImagePullPolicy::Always, &r, true));
        assert!(prefer_local_cache(ImagePullPolicy::Always, &r, false));
    }

    #[test]
    fn prefer_cache_always_with_registry_goes_to_registry() {
        // A private registry *was* configured: honour strict Always and hit
        // the registry on every reconcile.
        let r = ImageReference::Tag("9809f6b1".to_string());
        assert!(!prefer_local_cache(ImagePullPolicy::Always, &r, true));
    }

    #[test]
    fn classify_pull_error_missing_image_stays_not_found() {
        // 404 / manifest unknown is a missing image, not an unreachable or
        // auth-refusing registry — it must keep the ImageNotFound variant so
        // the deployment lands in ImagePullBackOff with the right reason.
        let e = classify_pull_error(
            "manifest unknown: manifest unknown",
            "registry.example.com/app:v1",
        );
        assert!(matches!(e, RuntimeError::ImageNotFound(_)));

        let e = classify_pull_error("received unexpected HTTP status: 404 Not Found", "app:v1");
        assert!(matches!(e, RuntimeError::ImageNotFound(_)));
    }

    #[test]
    fn classify_pull_error_auth_names_the_credentials_fix() {
        for raw in [
            "unauthorized: authentication required",
            "denied: requested access to the resource is denied",
            "pull access denied, repository does not exist or may require authorization",
        ] {
            let e = classify_pull_error(raw, "private.io/app:v1");
            match e {
                RuntimeError::ImagePullFailed(msg) => {
                    assert!(
                        msg.contains("config.username") && msg.contains("config.password"),
                        "auth error should point at the credential config, got: {msg}"
                    );
                    assert!(msg.contains("private.io/app:v1"), "should name the image");
                }
                other => panic!("expected ImagePullFailed, got {other:?}"),
            }
        }
    }

    #[test]
    fn classify_pull_error_unreachable_names_the_registry() {
        for raw in [
            "dial tcp 10.0.0.1:5000: connect: connection refused",
            "dial tcp: lookup registry.invalid: no such host",
            "Get \"https://registry.invalid/v2/\": net/http: request canceled (Client.Timeout exceeded)",
        ] {
            let e = classify_pull_error(raw, "registry.invalid/app:v1");
            match e {
                RuntimeError::ImagePullFailed(msg) => {
                    assert!(
                        msg.contains("cannot reach the registry"),
                        "unreachable error should say so, got: {msg}"
                    );
                    assert!(
                        msg.contains("registry.invalid/app:v1"),
                        "should name the image"
                    );
                }
                other => panic!("expected ImagePullFailed, got {other:?}"),
            }
        }
    }

    #[test]
    fn classify_pull_error_unknown_keeps_the_original_detail() {
        // An error we don't recognise must still surface its detail rather
        // than being swallowed — operator clarity beats a tidy generic string.
        let e = classify_pull_error("some unexpected daemon hiccup", "app:v1");
        match e {
            RuntimeError::ImagePullFailed(msg) => {
                assert!(msg.contains("some unexpected daemon hiccup"));
            }
            other => panic!("expected ImagePullFailed, got {other:?}"),
        }
    }
}
