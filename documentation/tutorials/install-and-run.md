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

The release build takes a few minutes the first time. If `ring --version` doesn't work after install, the binary isn't on your `PATH` — check `which ring`.

## 2. Generate the encryption key

Ring encrypts every secret it stores at rest with AES-256-GCM. The server refuses to start without an encryption key. Generate one now:

```bash
export RING_SECRET_KEY="$(openssl rand -base64 32)"
```

Keep this value safe. Lose it and every secret you create becomes unrecoverable; leak it and every secret is compromised. For this tutorial, an exported shell variable is fine — for production, put it in a `systemd EnvironmentFile=`, Vault, or your secret manager of choice. See [Secrets and encryption](/documentation/concepts/secrets-encryption) for the threat model.

## 3. Initialize the config

```bash
ring init
```

This creates `~/.config/kemeter/ring/` and writes an empty `auth.json`. It **does not** create the database or seed the admin user — that happens the first time the server runs. The command produces no output on success.

## 4. Start the server

```bash
ring server start
```

On first start, Ring:

- Binds to `<local-IP>:3030` by default (typically your LAN IP; falls back to `127.0.0.1`)
- Runs SQLite migrations to create `ring.db` in the current directory
- Seeds the default admin user: username `admin`, password `changeme`

Leave this terminal running. You should see log output if you set `RUST_LOG=info` before starting:

```bash
RUST_LOG=info ring server start
```

Open a second terminal for the rest of the tutorial.

## 5. Verify the API is up

```bash
curl http://localhost:3030/healthz
```

Expected output:

```json
{"state":"UP"}
```

If you get a connection refused, check the server's startup log — Ring prints the actual bind address (which might be your LAN IP, not `localhost`).

## 6. Log in

```bash
ring login --username admin --password changeme
```

The CLI stores the resulting token in `~/.config/kemeter/ring/auth.json`. Every subsequent command (`ring apply`, `ring deployment list`, …) reads the token from there.

You should now change the default password:

```bash
ring user update --password "your-secure-password"
```

The default `admin/changeme` credentials only work until the password is changed.

## 7. Sanity check

```bash
ring deployment list
```

You should see an empty list — no deployments yet, but the command worked, which proves the CLI is authenticated against your server.

## What's next

You have a working Ring installation:

- ✅ Binary installed
- ✅ Encryption key configured
- ✅ Server running
- ✅ Admin user authenticated

Continue with [Your first deployment](/documentation/tutorials/first-deployment) to actually run a workload — we'll deploy nginx and curl it on `localhost:8080` in about 10 minutes.

## Troubleshooting

**`Failed to connect to Docker daemon`** — the daemon isn't running, or your user isn't in the `docker` group. Run `sudo systemctl start docker`, and if needed `sudo usermod -aG docker $USER` then log out and back in.

**`Port 3030 already in use`** — another process owns the port. Either stop it (`sudo ss -tlnp | grep 3030` to find the PID) or change Ring's port in `~/.config/kemeter/ring/config.toml`. The full file requires `current`, `host`, `api`, and `user`:

```toml
[contexts.default]
current = true
host = "127.0.0.1"

api.scheme = "http"
api.port = 3031

user.salt = "your-salt"
```

See [reference: config.toml](/documentation/reference/config-toml) for every field.

**Anything else** — run `ring doctor`. It checks Docker connectivity, the encryption key, and Cloud Hypervisor prerequisites if you've configured that runtime.

For running Ring as a managed service (systemd, Docker Compose), see [how-to: run Ring as a service](/documentation/how-to/run-as-service).
