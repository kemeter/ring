# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed (breaking)
- **`POST /deployments` now uses RFC 7807** with the same shape as `POST /users` (`application/problem+json`, `violations[]` with `property_path`, `message`, `code`). Existing 400/422 responses with `{"message": "..."}` body are replaced. Codes for the rules already in place:
  - `deployment.runtime.unsupported` — runtime must be one of: docker, cloud-hypervisor
  - `deployment.command.cloud_hypervisor_unsupported`
  - `deployment.image.cloud_hypervisor_requires_absolute_path`
  - `deployment.network.host_runtime_unsupported`
  - `deployment.ports.host_network_conflict`
  - `deployment.replicas.host_network_conflict`

  **New rules** (previously not validated, the manifest applied and broke at runtime):
  - `deployment.ports.published.out_of_range` / `deployment.ports.target.out_of_range` — port 0 is reserved
  - `deployment.ports.published.duplicate` — two entries publishing the same host port
  - `deployment.ports.replicas_conflict` + `deployment.replicas.ports_conflict` — publishing host ports with `replicas > 1` causes inter-replica collisions
  - `deployment.replicas.job_must_be_one` — `kind: job` is one-shot
  - `deployment.health_checks.job_readiness_unsupported` — readiness checks only gate rolling updates
  - `deployment.environment.key.invalid` — env var names must match `[A-Za-z_][A-Za-z0-9_]*`
  - `deployment.resources.{limits,requests}.{cpu,memory}.invalid` — invalid quantity string
  - `deployment.config.image_pull_policy.unsupported` — must be `Always`, `IfNotPresent` or `Never`

  `property_path` follows JSONPath conventions for nested collections: `ports[0].published`, `volumes[2].source`, `resources.limits.cpu`.

- **Validation errors on `POST /users` and `PUT /users/{id}` now use RFC 7807.** The 422 response shape changed from the bare `validator`-derived `{"errors": <map>}` to `application/problem+json`:

  ```json
  {
    "type": "about:blank",
    "title": "Validation failed",
    "status": 422,
    "detail": "username: must be 2 to 50 characters\npassword: must be 8 to 128 characters",
    "violations": [
      { "property_path": "username", "message": "must be 2 to 50 characters", "code": "user.username.length" },
      { "property_path": "password", "message": "must be 8 to 128 characters", "code": "user.password.length" }
    ]
  }
  ```

  Every violation carries a stable `code` slug (e.g. `user.username.format`) that clients can branch on without parsing the human message. All applicable rules run on every request — the response lists every failure in one shot instead of stopping at the first.

  Username format is now `[a-zA-Z0-9][a-zA-Z0-9._-]*` (2-50 chars), matching GitHub-style conventions for human-facing identifiers. Password rules unchanged (8-128 chars).

- **`DeploymentStatus` is now snake_case in the JSON API and DB.** Previously the lifecycle states (`pending`, `running`, …) were lowercase while the error states (`CrashLoopBackOff`, `ImagePullBackOff`, …) were PascalCase — the mismatch silently dropped rows from string-matching filters elsewhere in the code (root cause of PR #84). All variants now share the same convention. Mapping for external consumers:
  - `CrashLoopBackOff` → `crash_loop_back_off`
  - `ImagePullBackOff` → `image_pull_back_off`
  - `CreateContainerError` → `create_container_error`
  - `NetworkError` → `network_error`
  - `ConfigError` → `config_error`
  - `FileSystemError` → `file_system_error`
  - `Error` → `error` (unchanged shape, lowercased)

  Migration `20220101000015_snake_case_deployment_status.sql` rewrites existing rows. Update any script that does `jq '.status == "CrashLoopBackOff"'` or similar.

  Event `reason` strings (`ImagePullBackOff`, `InstanceCreationFailed`, …) stay PascalCase — those are event labels, not statuses.

## [0.8.0] - 2026-05-12

### Added
- **Cloud Hypervisor — readiness gate**: scheduler-side `is_ready_to_drain` with per-health-check anti-flap window. Rolling updates wait for the new instance to be ready before draining the parent. Includes Docker `HEALTHCHECK` translation so the same gate applies to both runtimes (PR #72).
- **Cloud Hypervisor — `kind: job`**: dispatch worker/job in `apply`, boot one VM, mark `Completed` on guest shutdown. E2E `t21_job_kind.sh` validates the full lifecycle including artifact cleanup.
- **Cloud Hypervisor — command health checks**: in-guest `ring-agent` over AF_VSOCK port 2375 reads the real exit code (PR #69).
- **Cloud Hypervisor — full stats parity**: CPU and memory from `/proc/<vmm-pid>/{stat,status}` (PR #70), then network from `/sys/class/net/<tap>/statistics/*` (swapped host↔guest), threads from `/proc/<vmm-pid>/status`, disk I/O from `/proc/<vmm-pid>/io` when accessible (PR #78). Disk I/O degrades gracefully to zero on hardened hosts because CH clears `PR_SET_DUMPABLE`.
- **Cloud Hypervisor — console log rotation**: size-based rotation with a 60s sweep, configurable via `[runtime.cloud_hypervisor].max_console_log_bytes` / `max_console_log_backups` (defaults 10 MiB × 3 backups). `ring deployment logs --tail N` reads through rotated backups (PR #77).
- **Cloud Hypervisor — port conflict detection**: pre-check `TcpListener::bind` before VM boot, emit `PortAllocationFailed` event and `CrashLoopBackOff` after `MAX_RESTART_COUNT` — same semantics as Docker Compose.
- **Cloud Hypervisor — `ring doctor` socat check**: verify `socat` presence when port mapping is requested.
- **Docker — host network mode**: `network_mode: host` field on Docker deployments, with migration `20220101000014_network_mode.sql` and `documentation/how-to/use-host-network.md`.
- **Scheduler — configurable anti-flap window**: `min_healthy_time` per health check variant (TCP/HTTP/Command), default 10s, scheduler picks the max across readiness HCs.
- **API — config filtering**: `GET /configs?name=...`.
- **API — `ForceReplace` event**: emitted when a rolling update is skipped (PR #71).
- **Log level classification**: extended `classify_log` for kernel (`<N>` syslog priority, `BUG:`/`Oops:`/`Kernel panic`), cloud-init/systemd (`ERROR`/`WARN`/`INFO:`/`DEBUG`), and bracketed firmware markers (`[INFO]`/`[WARN]`/`[ERROR]`/`[DEBUG]`). Runtime-agnostic — benefits both Docker and CH.
- **Documentation restructure to Diátaxis**: tutorials, how-to, reference, concepts, help. Sozune integration added as recommended HTTP proxy.
- **Pre-built release binaries**: GitHub Actions workflow now attaches `ring` (x86_64-unknown-linux-gnu) and `ring-agent` (x86_64-unknown-linux-musl, static) tarballs to each tagged release.

### Changed
- Health checks (Docker + CH) migrated to a shared `probe` module.
- E2E tests split into `tests/e2e/docker/` and `tests/e2e/cloud-hypervisor/` with a `run.sh` orchestrator.
- Cloud Hypervisor stats and logs documented as **Supported** in the parity table (no longer "partial").

### Fixed
- Cloud Hypervisor cleans up half-created VMs on boot failure.
- Cloud Hypervisor retries on transient boot failures with exponential backoff and typed errors.
- Scheduler emits docker-events at level `warning` (was `info`).
- Anti-flap window no longer re-arms every scheduler cycle (PR #72).
- Docker `command` health check now honors the exit code (PR #72).
- `handle_rolling_update` no longer spawns/kills in a loop when the parent finishes draining (PR #72).
- CLI `apply` serialises the `readiness` flag through to the API (PR #72).
- `RING_SECRET_KEY` is validated at startup and surfaced in `ring doctor`.
- Config loader falls back to `current = true` context when the requested name does not match.
- OpenSSL CVEs (Dependabot high + moderate) addressed via `cargo upgrade`.

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
