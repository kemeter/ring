# Roadmap

## Security

- [x] Secrets management ([#51](https://github.com/kemeter/ring/issues/51))
  - Encrypted storage (AES-256-GCM)
  - `secretRef` in deployment environment
  - CLI and API for secrets CRUD
- [x] `RING_TOKEN` env var to bypass `auth.json`
- [x] Configurable CORS origins allowlist
- [x] Username and password input validation
- [x] Prevent users from deleting their own account
- [x] Whitelist allowed filter columns and hide internal errors from API responses

## Observability

- [x] Follow deployment logs ([#22](https://github.com/kemeter/ring/issues/22))
- [x] Health checks ([#2](https://github.com/kemeter/ring/issues/2))
- [x] Prometheus metrics endpoint, including unhealthy deployments by namespace and status
- [x] OpenTelemetry export over OTLP (traces, metrics, logs)
- [x] `RING_LOG_FORMAT=json` structured console logs
- [x] Webhooks (push notifications on deployment events)

## Resource Management

- [x] Resource limits & requests ([#17](https://github.com/kemeter/ring/issues/17))

## Deployment Strategy

- [x] Rolling update with `parent_id` coordination
- [x] Fix CrashLoopBackOff container cleanup on delete
- [x] Detect container crashes via Docker events (bounded restart loop)
- [x] Capture `image_digest` on image pull
- [x] Namespaces as first-class resource with auto-creation
- [x] YAML `namespaces` section in apply config files

## Volumes

- [x] Config volume temp file cleanup
- [x] Named volume cleanup on deployment delete
- [x] Volume driver support (nfs, etc.)
- [x] Reject unknown volume types

## Runtime

- [x] Abstract scheduler behind `RuntimeLifecycle` trait
- [x] Resolve volumes in scheduler (`ResolvedMount`) for runtime-agnostic mounts
- [x] Single Docker instance injection (configurable host)
- [x] Podman runtime (beta), including `network.mode host`
- [x] containerd runtime (beta) ([#21](https://github.com/kemeter/ring/issues/21))
- [x] Cloud Hypervisor runtime (alpha)
- [x] Firecracker microVM runtime (experimental) — boot/delete, ports, restart, console logs + rotation, metrics, `kind: job`, ext4 volumes
- [x] `ring-agent` in-guest companion (vsock) backing `type: command` health checks on Cloud Hypervisor and Firecracker
- [ ] Selectable Firecracker rootfs format (`rootfs_format`): monolithic ext4 (simple, copied per instance) and a read-only erofs store + per-VM writable overlay (denser — one shared store across VMs, smaller per-instance footprint)

### MicroVM hardening (Cloud Hypervisor / Firecracker)

- [x] Wire the error `classifier` into the CH, Firecracker and containerd runtimes so terminal failures stop burning restart cycles
- [ ] Symmetric host NAT: outbound NAT is Firecracker-only today
- [ ] Symmetric memory admission control (`check_host_memory`): Cloud Hypervisor / containerd only today
- [ ] Re-evaluate `needs_vsock` outside of boot time (adding a `command` health check to a running deployment has no effect)
- [ ] Deterministic `/30` allocation collisions between instances (known v1 limitation)
- [ ] `instance_address` consistency: Firecracker returns `None` without a tap, Cloud Hypervisor always returns an IP (probes time out instead of failing clearly)
- [ ] Ship a documented way to inject `ring-agent` into guest images
- [ ] Honour the `status` filter in `list_instances` on Cloud Hypervisor and Firecracker

## CLI

- [x] Granular exit codes (auth, connection, not-found, conflict)
- [x] `deployment logs` flags: `--follow`, `--tail`, `--since`, `--container`
- [x] `apply` support for `command`, `resources`, `health_checks`
- [x] `namespace prune` inactive-only by default with `--all` flag
- [x] `doctor` command to check runtime dependencies
- [x] `deployment health-checks` command
- [x] `--output json` for `deployment list` and `inspect`

## Dashboard

- [x] List views for deployments, volumes, webhooks, users and tokens on a shared `ResourceTable`
- [x] Deployment log viewer component
- [x] Shared detail table cards

## Documentation

- [x] Marketing site and doc portal (Aplos)

## Quality & CI

- [x] CI workflow with clippy and formatting checks
- [x] E2E test scenarios (create/delete, bind volume, TCP health check, rolling update, replicas convergence, crash loop, scale-down, Cloud Hypervisor boot/delete)
- [x] Error path tests for not-found on deployments, secrets, configs
- [x] Propagate errors with `thiserror` instead of silently swallowing
- [x] Graceful API bind error handling (no panic)
- [x] Graceful shutdown on SIGTERM/SIGINT
- [x] E2E suites for containerd (6 scenarios) and Firecracker (9 scenarios)
- [ ] Add the `firecracker` suite to the default `tests/e2e/run.sh` set and clean up its leftover processes and taps between tests
- [ ] Run the e2e suites in CI (none of them run today, only `cargo test`)
