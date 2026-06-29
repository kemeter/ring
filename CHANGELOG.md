# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- containerd runtime over native gRPC: index/entrypoint resolution for multi-arch images, CNI networking, logs, command health checks (#144)
- Podman runtime, opt-in under `[server.runtime.podman]` (#139)
- Firecracker microVM runtime (experimental): boot, networking, outbound NAT, restart reconciliation (#142, #146, #147)
- Firecracker reaches Cloud Hypervisor parity for observability: serial console log read/stream (#167), per-instance CPU/memory/network/disk/pid metrics (#168), and copy-truncate console-log rotation (#169)
- Firecracker `kind: job`: boot one VM, mark `completed` when the guest reboots (which exits the VMM); artifacts reaped, terminal status sticky (#170)
- Firecracker volumes via virtio-block: bind/named/config/secret mounts realised as ext4 images attached as extra drives and mounted by cloud-init; named volumes persist, ephemeral ones are reaped
- Public Prometheus `/metrics` endpoint exposing inventory and queue gauges, plus background-refreshed per-deployment runtime resource usage (#164)
- `ring_unhealthy_deployments` metric broken down by namespace and status (#179)
- Pull private images using the server host's Docker credentials via `use_host_auth`, gated by a server-side `use_host_registry_auth` flag and `host_registry_config` path; resolved for Docker and containerd (#173)
- Pull private images via `image_pull_secret`, resolving a stored secret into registry credentials at schedule time; mutually exclusive with other credential sources (#174)
- `apply`: reference a config payload from an external file with `files:`, kept verbatim unless `interpolate` is set (#172)
- Scoped API tokens (PAT) with per-scope and per-namespace enforcement, plus `ring token` CLI (#134)
- Outbound webhooks with HMAC-signed delivery and a durable event queue, plus `ring webhook` CLI (#135)
- First-class Volume entity: `/volumes` CRUD and `ring volume` CLI (#112)
- `ring logout` and `POST /logout` to revoke the session; login now prompts for username/password when flags are omitted (#150, #154)
- `ring init` runs `ring doctor` pre-flight checks for the selected runtime; `--runtime firecracker` now supported (#148)
- Filter deployments by label (#129)
- UDP port forwarding via `ports[].protocol` (#133)
- Load `config.toml` from `--config` or `RING_CONFIG_FILE` (#145)

### Changed
- Runtimes are opt-in via a top-level `[server]` table; Docker is no longer enabled by default (#138)
- An instance only reaches `running` once its readiness checks pass (#136)
- Login session unified into the token table (`user.token` dropped) (#150)
- `apply` warns on fields the Cloud Hypervisor runtime silently ignores (#130)
- Extracted `auth` into its own config module (#137)
- Split `runtime`/`hypervisor` modules, migrated to `tracing`, dropped dead deps (#141, #143)
- `deployment list` resolves instances in one bulk runtime call instead of one per deployment (#180)

### Fixed
- Docker workers honour `MAX_RESTART_COUNT` and stop restarting once the bound is reached instead of looping indefinitely (#177)
- Health-check phase is keyed on the pre-cycle status so liveness checks run on the right instance state (#175)
- Firecracker allocates guest networking unconditionally (not only when a port is published) and connects to the base vsock socket for host-to-guest calls (#171)
- `apply` updates existing configs instead of skipping them
- Podman crash loops are now detected via reconciliation and reach `CrashLoopBackOff`; no more `event channel disconnected` log spam every tick (#159)
- `apply` manifests can set `user`/`group` again (deployment config typed as a struct) (#161)
- `Always` pull policy is honoured for credential-less (public) images (#162)
- Docker CPU percentage no longer reports near-zero from one-shot stats (#156)
- containerd probes CNI plugin dirs instead of assuming `/opt/cni/bin` (#158)
- `user list` renders rows with null `updated_at`/`login_at` instead of an empty table (#155)
- A server-only `config.toml` without `[contexts]` is now accepted (#153)
- Firecracker re-adopts instance networking and re-spawns port-forwarders across `ring-server` restarts (#151)
- Host-port deployments now recreate instead of rolling, avoiding the `port already allocated` loop (#140)
- Actionable error when a Cloud Hypervisor `command` health check can't reach `ring-agent` (#131)
- Cloud Hypervisor VMs survive a `ring-server` restart instead of being orphaned (#132)

## [0.9.0] - 2026-06-03

### Added
- Web dashboard (SvelteKit): overview, deployment detail with live metrics, health-check history, streamed logs and events, node page, read-only namespaces/secrets/configs, theme toggle (#91, #92, #117, #122, #123, #124, #127, #128)
- Host-memory admission control — deployments that don't fit go to a terminal `insufficient_resources` status instead of being OOM-killed
- `volumes: type: secret` — mount a secret as a read-only file (#107)
- `configs:` block inline in `ring apply`; publishable ports with `host_ip` (Docker + CH) (#105, #106)
- `ring init` — interactive setup (runtime + port, generates `RING_SECRET_KEY`), scriptable via `--runtime` / `--port` (#83, #125)
- `ring node get` — host information for the server's machine
- Delete multiple deployments in one command (#114)
- Startup banner showing API/dashboard URLs and registered runtimes (#121)
- Semantic CLI colours and aligned tables (ANSI dropped in pipes / `NO_COLOR` / `--output json`) (#101)

### Changed (breaking)
- All write endpoints (`/deployments`, `/users`, `/namespaces`, `/secrets`, `/configs`, `/login`) now return RFC 7807 `application/problem+json` with a `violations[]` array and stable `code` slugs; every failure is reported in one pass. Replaces the legacy `{"message"|"error"|"errors": …}` shapes (#88, #89, #90)
- `DeploymentStatus` is now snake_case everywhere in the JSON API and DB (e.g. `CrashLoopBackOff` → `crash_loop_back_off`); migration `20220101000015` rewrites existing rows. Update scripts matching the old PascalCase values (#85)
- CLI commands exit non-zero on failure with a categorised code (auth/connection/not-found/conflict) instead of silently exiting 0 (#99, #100, #116)

### Changed
- `ring apply` / `ring namespace create` / `ring secret create` render RFC 7807 violations with their property paths instead of a raw status line (#89)
- Failed Docker image pulls surface an actionable reason (auth refused / registry unreachable / not found) instead of a raw daemon dump (#95)

### Fixed
- Scheduler no longer gets stuck cleaning up a deleted deployment whose referenced secret/config/volume was removed (#118)
- `CreateContainerError` converges to `CrashLoopBackOff` instead of looping forever; orphan containers cleaned up on create/connect-network failure; `kind: job` create errors converge to `Failed` (#84, #86, #87)
- Secret names accept env-style uppercase + underscore (#103)

### Security
- IDOR fix on `PUT`/`DELETE /users/{id}` — a user could modify or delete another (#94)
- Unique random salt per password hash (was deterministic — identical passwords produced identical hashes) (#97)
- Single fail-closed auth middleware at the router; namespace-scoped audit log for create/update/delete (#96, #98)
- Dependency bumps closing OpenSSL CVEs (#115)

### Internal
- CI: clippy is now blocking (`-D warnings`); added a dashboard lint job (biome + svelte-check) (#119)
- Presentation helpers (`style`, `output`, `problem_json`) moved into a `cli` module; shared `OutputFormat` for `--output` (#126)

## [0.8.0] - 2026-05-12

### Added
- Cloud Hypervisor readiness gate: rolling updates wait for the new instance to be ready before draining the parent, with Docker `HEALTHCHECK` translation so the gate applies to both runtimes (#72, #73)
- Cloud Hypervisor `kind: job`: boot one VM, mark `Completed` on guest shutdown (#67)
- Cloud Hypervisor command health checks via in-guest `ring-agent` over vsock (real exit code) (#69)
- Cloud Hypervisor full stats parity: CPU, memory, network, threads and disk I/O (disk I/O degrades to zero on hardened hosts) (#70, #78)
- Cloud Hypervisor console log rotation (size-based, configurable) (#77)
- Cloud Hypervisor host-port conflict detection before VM boot (#74)
- Cloud Hypervisor `ring doctor` socat check (#68)
- Docker host network mode (`network_mode: host`) (#79)
- Configurable anti-flap window (`min_healthy_time`) per health check (#76)
- Filter configs by name (`GET /configs?name=...`)
- `ForceReplace` event when a rolling update is skipped (#71)
- Log level classification for kernel, cloud-init/systemd and firmware markers (#80)
- E2E coverage for Docker and Cloud Hypervisor scenarios (#66)
- Documentation restructured to Diátaxis; Sozune added as recommended HTTP proxy
- Pre-built release binaries (`ring` + `ring-agent`) attached to each tagged release (#81)

### Changed
- Health checks (Docker + CH) migrated to a shared `probe` module
- E2E tests split into `tests/e2e/docker/` and `tests/e2e/cloud-hypervisor/`
- Cloud Hypervisor stats and logs marked **Supported** in the parity table

### Fixed
- Cloud Hypervisor cleans up half-created VMs on boot failure and retries transient failures with exponential backoff
- Anti-flap window no longer re-arms every scheduler cycle; Docker `command` health check now honours the exit code (#72)
- `handle_rolling_update` no longer spawns/kills in a loop when the parent finishes draining (#72)
- `RING_SECRET_KEY` validated at startup and surfaced in `ring doctor`
- Config loader falls back to the `current = true` context when the requested name doesn't match
- OpenSSL CVEs (Dependabot) addressed

## [0.7.0] - 2026-04-17

### Added
- **Cloud Hypervisor runtime**: experimental lightweight VM runtime alongside Docker, with per-instance sparse disk copies, TAP networking managed via `CAP_NET_ADMIN`, socket-based instance discovery, logs, health checks, and stats.
- `runtime.cloud_hypervisor` configuration section with `firmware_path`.
- `ring doctor` command to check runtime dependencies.
- `ring deployment health-checks` CLI command.
- `--output json` flag on `deployment list` and `deployment inspect`.
- Multi-runtime scheduler dispatch.
- E2E test scenarios: shell-based create/delete, bind volume, TCP health check, rolling update, replicas convergence, Cloud Hypervisor boot/delete.

### Changed
- Unified `RuntimeInterface` into a single `RuntimeLifecycle` trait.
- Removed Docker from `AppState`; API now uses `RuntimeMap` for instance listing.
- API defaults to binding `0.0.0.0`.
- Cloud Hypervisor uses pre-built raw disk images instead of Docker-to-rootfs conversion.
- Cloud Hypervisor data directories moved to `~/.config/kemeter/ring`.

### Fixed
- Dropping a deployment now removes its containers regardless of status (including `exited`, `dead`, etc.).
- API bind errors are handled gracefully instead of panicking.
- Health check removal is runtime-aware.
- Per-instance disk copies, qcow2 fallback, VM state refresh.
- Instance discovery by scanning sockets instead of relying on in-memory state.
- Volumes and command health checks are rejected on the Cloud Hypervisor runtime (not yet supported).

## [0.6.0] - 2026-04-10

### Added
- Configurable CORS origins allowlist.
- Marketing site and documentation portal (`website/`) built with aplos.
- `RING_TOKEN` env var to bypass `auth.json`.
- Granular CLI exit codes (auth, connection, not-found, conflict).
- `--follow`, `--tail`, `--since`, `--container` flags on `deployment logs`.
- `command`, `resources`, `health_checks` fields supported in `apply`.
- `namespace prune` only removes inactive deployments by default, with `--all` flag.

### Changed
- Scheduler abstracted behind `RuntimeLifecycle` trait.
- Config volumes resolved in scheduler and passed as `ResolvedMount` to the runtime.
- Single injected Docker instance instead of reconnecting in every function.
- Async I/O for temp files; `&str` in model queries instead of `String`.

### Fixed
- Scheduler no longer overwrites `deleted` status set by the API.
- `user update` requires at least one field.

## [0.5.0] - 2026-04-07

### Added
- Rolling update strategy with `parent_id` coordination.
- CI workflow with clippy and formatting checks.
- Input validation on username and password.
- Prevent users from deleting their own account.

### Changed
- Scheduler loop split into smaller focused functions.
- Duplicated SQL filter logic extracted into `models::query` helper.
- Errors propagated via `thiserror` instead of being silently swallowed.
- `SCHEDULER_INTERVAL` env var renamed to `RING_SCHEDULER_INTERVAL`.

### Fixed
- Named volumes cleaned up on deployment delete, with driver config passed through.
- Unknown volume types rejected instead of silently falling back to config.
- Config volume temp files cleaned up; duplicates avoided.
- Container removal failures during rolling update handled.
- Containers of `CrashLoopBackOff` deployments deleted when removed.

## [0.4.0] - 2026-03-16

### Added
- Secrets management with AES-256-GCM encryption, `secretRef` support in deployment environment, warning when deleting a referenced secret.
- Namespace as a first-class resource with auto-creation on deployment; `namespaces` section supported in YAML config files.
- Real-time container resource usage metrics (API + CLI).
- K8s-style `limits`/`requests` structure for resource limits.
- `image_digest` field on Deployment, captured on image pull.
- Docker integration tests for the health checker.
- Migration from `rusqlite` to `sqlx` with async connection pooling.

### Changed
- Deployment `secrets` column renamed to `environment`.
- Status magic strings replaced by `DeploymentStatus` enum.
- `docker.rs` split into focused modules; error handling deduplicated.
- Apply timeout, health checker outcome pattern, and time-based cleanup in the scheduler.

### Fixed
- Separate SQLite connections for API and scheduler to prevent mutex contention freeze.
- Filter columns whitelisted; internal errors hidden from API responses.
- Server bind uses config host/port instead of hardcoded `0.0.0.0:3030`.
- Panic prevented when `--password` is omitted in `user update`.
- Incorrect `restart_count` increment on successful scale-up removed; HTTP health check timeout added.
- Namespace filter params actually sent by `config list`.
- Panic prevented on short container IDs in `list_instances_with_names`.

## [0.3.0] - 2026-02-19

Initial public release. Core feature set: Docker runtime, deployments (worker and job kinds), scheduler, API server, CLI, authentication, configs, volumes, HTTP/TCP health checks, container logs, resource limits (CPU/memory), and documentation.
