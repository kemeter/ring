# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
