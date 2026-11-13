# Namespace security levels

A deployment can ask for more than its own container: a privileged container, the host's network, a directory of the host. Any of these gives it the host, and with it every other namespace. A namespace's **security level** decides whether its deployments may ask for them. The levels follow Kubernetes' [Pod Security Standards](https://kubernetes.io/docs/concepts/security/pod-security-standards/).

| Level | What it refuses |
|---|---|
| `privileged` | Nothing. |
| `baseline` | `config.user.privileged: true`, `network.mode: host`, and `bind` volumes. |
| `restricted` | What `baseline` refuses, and a container running as root: `config.user.id` must be set to a user other than `0`. |

`baseline` is the default. Named volumes, configs, secrets and published `ports` are allowed at every level.

`restricted` checks the user on the container runtimes only (Docker, Podman, containerd). A Cloud Hypervisor or Firecracker guest runs its own kernel, so its user says nothing about the host. Ring cannot read the user an image declares, so `config.user.id` is required even when the image already runs as a non-root user.

## Where a namespace gets its level

A namespace without a level of its own follows the server's default, `[server.admission] default` in [`config.toml`](/documentation/reference/config-toml#server-admission).

```toml
[server.admission]
default = "baseline"
```

An admin sets a namespace's own level, or resets it to the default:

```bash
ring namespace update production --security-level privileged
ring namespace update production --security-level default
```

The dashboard's **Namespaces** page does the same, and `ring namespace list` shows each namespace's level. Only an admin can change a level, and only from a session or a token valid for every namespace: a token limited to `tenant-a` cannot open `tenant-a` to the host.

Upgrading to the version that introduced levels pins every namespace already running a privileged container, a host-network deployment or a bind volume to `privileged`, so nothing stops. See [UPGRADE.md](https://github.com/kemeter/ring/blob/main/UPGRADE.md).

## When the level is checked

**On submission.** `ring apply` and `POST /deployments` refuse a deployment its namespace does not allow, with a `422` naming each field at fault. Nothing is stored, and a namespace that did not exist is not created.

```
$ ring apply -f agent.yaml
Processing deployment 'agent'
Unable to apply deployment 'agent': Validation failed (422)
  * volumes[0].type: a bind volume mounts a path of the host, which the baseline level of namespace 'tenant-a' does not allow; use a named volume instead
```

**Before each new instance.** When a namespace's level is lowered, its deployments keep their running instances, but none of them starts a new instance the level no longer allows. Such a deployment backs off with the reason `spec_rejected` and an `admission_denied` event. It is retried at most every 5 minutes, so raising the level again is picked up without a new `ring apply`.

## See also

- [Manifest reference: `config`](/documentation/reference/manifest#config)
- [Manifest reference: `volumes`](/documentation/reference/manifest#volumes)
- [How-to: use host network mode](/documentation/how-to/use-host-network)
