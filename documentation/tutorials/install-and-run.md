# Install and run Ring

By the end of this tutorial you'll have Ring installed, the server running, and a confirmed `200 OK` from its API. Time required: about 15 minutes.

## Prerequisites

- A Linux or macOS machine
- Docker installed and the daemon running (`docker ps` works for your user)

Quick check:

```bash
docker --version
```

If `docker ps` fails with a permission error, add yourself to the docker group: `sudo usermod -aG docker $USER`, then log out and back in.

## 1. Install Ring

### From a pre-built binary (recommended, Linux x86_64)

Each release attaches a pre-built `ring` binary for `x86_64-unknown-linux-gnu`. Grab the latest one:

```bash
TAG=$(curl -s https://api.github.com/repos/kemeter/ring/releases/latest | grep -oP '"tag_name": "\K[^"]+')
curl -L "https://github.com/kemeter/ring/releases/download/${TAG}/ring-${TAG}-x86_64-unknown-linux-gnu.tar.gz" \
  | tar -xz
sudo install -m 0755 "ring-${TAG}-x86_64-unknown-linux-gnu/ring" /usr/local/bin/ring
ring --version
```

The release archive also contains a `.sha256` companion if you want to verify the download.

### From source

If you're on macOS, ARM Linux, or want the latest unreleased changes, build from source. Requires Rust 1.85 or later ([rustup](https://rustup.rs/)) and OpenSSL headers:

```bash
# Debian / Ubuntu
sudo apt update && sudo apt install libssl-dev pkg-config

# Fedora / RHEL
sudo dnf install openssl-devel

# macOS
brew install openssl pkg-config
```

Then clone and build:

```bash
git clone https://github.com/kemeter/ring.git
cd ring
cargo build --release
sudo install -m 0755 target/release/ring /usr/local/bin/ring
ring --version
```

The release build takes a few minutes the first time. If `ring --version` doesn't work after install, the binary isn't on your `PATH`; check `which ring`.

## 2. Initialize the config

```bash
ring init
```

Ring will prompt you for two things:

- **Which runtime to use**: Docker, Podman, Cloud Hypervisor, Firecracker (experimental), or both (Docker + Cloud Hypervisor)
- **Which port the API should listen on**, which defaults to `3030`

It then writes `~/.config/kemeter/ring/config.toml`, persists a freshly generated `RING_SECRET_KEY` to `~/.config/kemeter/ring/secret-key` (mode `0600`), and prints the same key on stdout for convenience:

```
$ ring init
? Which runtime do you want to use? Docker
? Which port should the API listen on? 3030
✓ Wrote /home/you/.config/kemeter/ring/config.toml
✓ Wrote /home/you/.config/kemeter/ring/secret-key (mode 0600)

────────────────────────────────────────────────────────────────────────
  IMPORTANT — export this key before starting the server:

    export RING_SECRET_KEY="aBc123…=="

  Without it, `ring server start` will refuse to boot.
  Without it, secrets stored on disk become unrecoverable.

  Also saved to: /home/you/.config/kemeter/ring/secret-key
  Treat that file like a private key: chmod 0600, never commit, never back up unencrypted.
────────────────────────────────────────────────────────────────────────

→ Pre-flight checks (ring doctor):

Server
  [+] RING_SECRET_KEY: set, decodes to a 32-byte AES-256 key

Docker
  [+] docker: Docker version 28.5.0, build 887030f

→ Next steps:
  1. Export the key above
  2. ring server start             # first boot creates the admin user (admin/changeme)
  3. ring login -u admin -p changeme
  4. ring user update --password "<your password>"  # rotate the default password
```

At the end, `ring init` runs the same diagnostics as [`ring doctor`](/documentation/reference/cli) on the runtime you just selected, so you find out *now* that Docker isn't running, KVM is missing, or a kernel image is absent, instead of at your first `ring apply`. A failing check (`[-]`) is only a warning: `init` already wrote your config, so it still exits `0`. Fix the flagged items and re-run `ring doctor` to confirm. Only the selected runtime is checked, so a Docker-only init won't nag about Cloud Hypervisor dependencies.

Copy the `export RING_SECRET_KEY=...` line into your shell, or pin it to a `systemd EnvironmentFile=` for a production install. The on-disk copy under `~/.config/kemeter/ring/secret-key` is a recovery aid if the terminal scrolls past or the install is interrupted (**it is not a substitute for setting the environment variable**), since `ring server start` only reads `RING_SECRET_KEY` from the environment. Lose the key (file deleted *and* env forgotten) and every secret you store becomes unrecoverable; leak it and every secret is compromised. See [Secrets and encryption](/documentation/concepts/secrets-encryption) for the threat model.

If `config.toml` or `secret-key` already exists, `ring init` refuses to overwrite without `--force`. Re-running with `--force` generates a brand-new key, so every secret stored under the old one becomes undecryptable, so use it only for fresh installs you don't mind wiping.

In CI or any non-interactive shell, `ring init` skips the prompts and falls back to defaults (Docker, port 3030).

### Scripting `ring init`

To configure a non-default runtime or port without a prompt (CI, Ansible, etc.), pass the flags explicitly:

```bash
ring init --runtime cloud-hypervisor --port 4030
```

- `--runtime`: `docker`, `podman`, `cloud-hypervisor`, `firecracker` (experimental), or `both` (Docker + Cloud Hypervisor).
- `--port`: the API port (default `3030`).

Flags take precedence over prompts and over the non-interactive defaults, and compose per-field: passing only `--port` still prompts for the runtime on a TTY (or defaults to Docker when there's no TTY), and vice versa.

## 3. Start the server

```bash
ring server start
```

On first start, Ring:

- Binds to `<local-IP>:3030` by default (typically your LAN IP; falls back to `127.0.0.1`)
- Runs SQLite migrations to create `ring.db` in the current directory
- Seeds the default admin user: username `admin`, password `changeme`

It prints a short banner telling you where it's reachable:

```text
  Ring 0.8.0  ready

  ➜  Local:     http://127.0.0.1:3030
  ➜  Network:   http://192.168.1.67:3030
  ➜  Dashboard: disabled (enable with --dashboard)
  ➜  Runtimes:  cloud-hypervisor, docker
```

- **Local** is the loopback address; **Network** is your machine's LAN IP, resolved automatically. Share it to reach Ring from another host.
- With `--dashboard`, the `Dashboard` line shows its URL (default `http://127.0.0.1:3031`).
- When `host` is set to a specific address (not `0.0.0.0`), only the matching line is shown.

Leave this terminal running. You should see log output if you set `RUST_LOG=info` before starting:

```bash
RUST_LOG=info ring server start
```

Open a second terminal for the rest of the tutorial.

## 4. Verify the API is up

```bash
curl http://localhost:3030/healthz
```

Expected output:

```json
{"state":"UP"}
```

If you get a connection refused, check the server's startup log, where Ring prints the actual bind address (which might be your LAN IP, not `localhost`).

## 5. Log in

```bash
ring login --username admin --password changeme
```

The CLI stores the resulting token in `~/.config/kemeter/ring/auth.json`. Every subsequent command (`ring apply`, `ring deployment list`, …) reads the token from there.

You should now change the default password:

```bash
ring user update --password "your-secure-password"
```

The default `admin/changeme` credentials only work until the password is changed.

## 6. Sanity check

```bash
ring deployment list
```

You should see an empty list: no deployments yet, but the command worked, which proves the CLI is authenticated against your server.

## What's next

You have a working Ring installation:

- ✅ Binary installed
- ✅ Encryption key configured
- ✅ Server running
- ✅ Admin user authenticated

Continue with [Your first deployment](/documentation/tutorials/first-deployment) to actually run a workload: we'll deploy nginx and curl it on `localhost:8080` in about 10 minutes.

## Troubleshooting

**`Failed to connect to Docker daemon`**: the daemon isn't running, or your user isn't in the `docker` group. Run `sudo systemctl start docker`, and if needed `sudo usermod -aG docker $USER` then log out and back in.

**`Port 3030 already in use`**: another process owns the port. Either stop it (`sudo ss -tlnp | grep 3030` to find the PID) or change Ring's port in `~/.config/kemeter/ring/config.toml`. The file requires `current`, `host`, and `api`:

```toml
[contexts.default]
current = true
host = "127.0.0.1"

api.scheme = "http"
api.port = 3031
```

See [reference: config.toml](/documentation/reference/config-toml) for every field.

**Anything else**: run `ring doctor`. It checks Docker connectivity, the encryption key, and Cloud Hypervisor prerequisites if you've configured that runtime.

For running Ring as a managed service (systemd, Docker Compose), see [how-to: run Ring as a service](/documentation/how-to/run-as-service).
