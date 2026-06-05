# Troubleshooting

When something breaks, work outside-in: confirm Ring is responsive, then look at what Ring decided, then at what the application did. The full debugging flow is in [how-to: observe and debug](/documentation/how-to/observe-and-debug); this page collects the specific errors and their fixes.

## Server won't start

### "Failed to connect to Docker daemon"

The Docker daemon isn't running, or your user lacks permissions.

```bash
sudo systemctl start docker
sudo usermod -aG docker $USER          # then log out and back in
docker ps                              # should now work
```

### "Permission denied" on `/var/run/docker.sock`

Your user is not in the `docker` group.

```bash
sudo usermod -aG docker $USER          # then log out and back in
```

### "Port 3030 already in use"

Another process owns the port. Either stop it:

```bash
sudo ss -tlnp | grep 3030              # find the PID
```

Or change Ring's port in `~/.config/kemeter/ring/config.toml`. The schema requires `current`, `host`, and `api`:

```toml
[contexts.default]
current = true
host = "127.0.0.1"

api.scheme = "http"
api.port = 3031                        # was 3030
```

See [reference: config.toml](/documentation/reference/config-toml) for every field.

### "RING_SECRET_KEY is required" / exits with code 1

Ring refuses to start without `RING_SECRET_KEY`. Generate one and export it before starting:

```bash
export RING_SECRET_KEY="$(openssl rand -base64 32)"
ring server start
```

Validate without starting the server:

```bash
ring doctor
```

For a managed service, the key belongs in a `systemd EnvironmentFile=`, not in your interactive shell. See [how-to: run as a service](/documentation/how-to/run-as-service).

### Cloud Hypervisor VMs die with `SIGSYS` at boot

CH's default seccomp filter doesn't whitelist a syscall the boot path needs on some recent kernels. Symptom in the Ring log:

```
cloud-hypervisor process for ch-... exited with signal: 31 (SIGSYS) (core dumped)
stderr: ==== Possible seccomp violation ====
```

Workaround:

```toml
[server.runtime.cloud_hypervisor]
enabled = true
seccomp = "false"      # disable filter
# or
seccomp = "log"        # keep filter, only log violations
```

Leave `seccomp` unset in production unless you've actually hit this.

## Authentication fails

### "Invalid credentials" right after `ring init`

The default credentials are `admin` / `changeme`, **but only on the first server start before the password is changed**. If you've changed the password and forgotten it, the only path forward is to delete the user from the database and recreate it via `ring user create` — Ring has no password-reset workflow.

### "Unauthorized" on every command

The token in `~/.config/kemeter/ring/auth.json` is invalid. Log in again:

```bash
ring login --username admin --password "your-password"
```

### "Could not connect to localhost:3030"

Check that the server is listening on the address the CLI thinks it should:

```bash
curl http://localhost:3030/healthz             # should return {"state":"UP"}
```

Ring binds to its detected local IP by default (e.g. `192.168.1.x`), not `localhost`. The CLI uses whatever `host` value is in your `config.toml`. If they disagree, either change `host` to `127.0.0.1` for loopback-only, or point the CLI at the actual bind IP.

## Deployment problems

> For what each deployment status means and how a deployment moves between them, see [Deployment status lifecycle](/documentation/concepts/deployment-status-lifecycle).

### Stuck in `creating`

Look at the events first:

```bash
ring deployment events <DEPLOYMENT_ID> --level error
```

Most common causes:

- **`SecretResolutionError`** — a `secretRef` in `environment:` refers to a secret that doesn't exist in the deployment's namespace. Create the secret, or fix the manifest
- **`ImagePullBackOff`** — Docker can't pull the image. Wrong tag, missing credentials, network problem. Verify with `docker pull <image>` from the host
- **`InstanceCreationFailed`** — Docker rejected the container creation. Common subreasons: port conflict (`bind: address already in use`), invalid bind mount (source path missing), unsupported runtime option

### Stuck in `deleted`

A deployment shown as `deleted` should disappear within a scheduler cycle or two once its containers are gone — the row is purged automatically. If it lingers and `ring deployment events <ID>` keeps logging `secret_resolution_error` / `config_load_error` / `volume_resolution_error`, you are on a build prior to the fix where the scheduler tried to resolve a deployment's secrets/configs/volumes *before* tearing it down. A resource deleted alongside the deployment (e.g. the secret a `secretRef` pointed at) then made resolution fail every tick, so cleanup was never reached.

Current builds reconcile a `deleted` deployment straight to teardown and purge, ignoring secret/config/volume resolution. If you hit this on an older server, upgrade; the stuck row clears on the next cycle after restart.

### `image_pull_back_off`

```bash
ring deployment events <DEPLOYMENT_ID> --level error --limit 20
```

The event message names the likely cause and the fix, and keeps Docker's exact rejection in `(original error: …)` for the full detail. Three cases are distinguished:

- **`… not found …`** — the tag or digest doesn't exist in the registry (or `image_pull_policy: Never` and the image isn't cached locally). Check the image reference.
- **`registry authentication failed … — check config.server, config.username and config.password`** — the registry refused the credentials (or required some and none were sent). Fix the credentials below.
- **`cannot reach the registry … — is it up and the registry host correct?`** — a transport failure (connection refused, host not found, timeout). The registry host is wrong or down; verify with `docker pull <image>` from the host.

For private registries, set `config.server` / `config.username` / `config.password` in the manifest:

```yaml
config:
  server: "registry.company.com"
  username: "registry-user"
  password: "$REGISTRY_PASSWORD"
  image_pull_policy: "Always"
```

See [how-to: deploy with secrets → private registry credentials](/documentation/how-to/deploy-with-secrets#private-registry-credentials-are-different).

### `crash_loop_back_off`

The container has crashed more than `MAX_RESTART_COUNT` times. Look at:

```bash
ring deployment events <DEPLOYMENT_ID> --level warning   # crashes show up here
ring deployment logs <DEPLOYMENT_ID> --tail 200          # what the app said
```

After fixing the root cause, re-apply the manifest. Ring resets the counter on a fresh apply.

### `insufficient_resources`

The host doesn't have enough free memory to honour the deployment's requested memory, so Ring refused to start it — *before* creating any container or booting any VM. The event names the gap:

```bash
ring deployment events <DEPLOYMENT_ID> --level error --limit 5
# insufficient host memory for 'web': needs 4096 MiB but only 1800 MiB is available — …
```

This status is **terminal** — Ring does not retry, because the memory isn't going to reappear on its own. Two ways out:

- Free memory on the host (stop other workloads), then re-apply the manifest.
- Lower the deployment's `resources.requests.memory` (or `resources.limits.memory` if no request is set) to fit, then re-apply.

The check compares against memory available *at that moment*; it's a guard against gross over-asks, not a precise reservation system. CPU is not gated — CPU overcommit is allowed.

### Health checks flap

```bash
ring deployment health-checks <DEPLOYMENT_ID>            # full history
ring deployment health-checks <DEPLOYMENT_ID> --latest   # one row per check
```

Common causes:

- **`timeout`** — the probe's `timeout` is shorter than the application's actual response time. Increase, or fix the slow path
- **`failed`** with HTTP — a 3xx response counts as failure (Ring doesn't follow redirects). Point the URL at the redirect target
- **`failed`** with TCP — the port isn't open inside the container yet. Either bump `threshold`, or use a TCP/HTTP check on an endpoint that signals real readiness
- **`failed`** flapping between hosts — `interval` is currently advisory; probes actually run once per scheduler tick (default 10s). Lower the tick (`RING_SCHEDULER_INTERVAL=2`) for tighter cadence

### Rolling update stuck on the new version

```bash
ring deployment list --status failed
ring deployment events <CHILD_ID> --level error
ring deployment health-checks <CHILD_ID>
ring deployment logs <CHILD_ID> --tail 200
```

If the child never goes healthy, Ring leaves the parent running. Fix the manifest and re-apply (creates a new child, ignores the failed one), or `ring deployment delete <CHILD_ID>` to clear it explicitly.

To roll back, set the image tag to the previous version and re-apply — Ring rolls forward to the older tag through the same rolling-update path.

### "Multiple active deployments share name+namespace"

A previous failed rollout left the parent and a stuck child both `running`. List the duplicates:

```bash
ring deployment list -n <NAMESPACE>
```

Delete the unwanted one with `ring deployment delete <ID>`. Ring falls back to immediate replacement until the duplicates are gone.

## Cloud Hypervisor specifics

### "Failed to create TAP"

Cloud Hypervisor needs `CAP_NET_ADMIN`:

```bash
sudo setcap cap_net_admin,cap_net_raw+ep $(which cloud-hypervisor)
getcap $(which cloud-hypervisor)
```

Re-run after every CH upgrade — `setcap` doesn't survive a new binary.

### VM boots but is unreachable on its published port

`socat` isn't installed on the host. Each `ports:` entry needs a socat forwarder; without it the VM boots fine but no host port is bound.

```bash
sudo apt install socat        # or dnf install socat
ring doctor                   # confirms socat presence
```

### `environment:` is empty inside the VM

Either `xorriso` isn't installed on the host, or the guest image doesn't ship cloud-init.

```bash
sudo apt install xorriso
ring doctor
```

Custom guest images from scratch (e.g. Buildroot) won't pick up env vars unless you add cloud-init or read `/etc/ring/env` yourself in your boot scripts.

### `command:` health check rejected

Either the in-guest `ring-agent` isn't running, or the agent's `cap_net_admin` / vsock module isn't enabled. See [how-to: deploy on Cloud Hypervisor → prerequisites](/documentation/how-to/deploy-on-cloud-hypervisor#prerequisites).

## Generic diagnostic flow

`ring doctor` is the first-line check — it verifies Docker connectivity, the encryption key, and Cloud Hypervisor prerequisites (binary, KVM, firmware, virtiofsd, xorriso, socat).

```bash
ring doctor
```

After that, the full debugging order from [how-to: observe and debug](/documentation/how-to/observe-and-debug):

```bash
curl http://localhost:3030/healthz                       # is Ring up?
ring deployment list                                     # what's the state?
ring deployment events <ID> --level error --limit 50     # what did Ring decide?
ring deployment health-checks <ID> --latest              # are probes passing?
ring deployment logs <ID> --tail 200                     # what did the app say?
ring deployment metrics <ID>                             # resource pressure?
```

Then, if you've ruled out Ring:

```bash
docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID"
docker logs <CONTAINER_ID>
docker inspect <CONTAINER_ID>
```

## Server logs

```bash
sudo journalctl -u ring -f                # systemd
RUST_LOG=info ring server start           # foreground
RUST_LOG=ring=debug ring server start     # all Ring components
RUST_LOG=ring::scheduler=debug ring server start    # one component
```

## See also

- [How-to: observe and debug](/documentation/how-to/observe-and-debug)
- [Reference: environment variables](/documentation/reference/environment-variables)
- [Reference: config.toml](/documentation/reference/config-toml)
- [FAQ](/documentation/help/faq)
