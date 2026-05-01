# Installation

This guide walks you through installing Ring on your system.

## Prerequisites

Ring requires:

- **Docker** — [official installation guide](https://docs.docker.com/get-docker/)
- **Rust 1.85 or later** (for compilation) — Ring uses edition 2024. Install via [rustup](https://rustup.rs/).

Quick verification:

```bash
docker --version
rustc --version  # if compiling from source
```

## Installing Ring

### Option 1: pre-compiled binary

Pre-compiled binaries are not yet published. Compile from source for now.

### Option 2: compile from source

```bash
# Clone the repository
git clone https://github.com/kemeter/ring.git
cd ring

# Install system dependencies (Ubuntu/Debian)
sudo apt update
sudo apt install libssl-dev pkg-config

# Install system dependencies (CentOS/RHEL)
sudo yum install openssl-devel

# Install system dependencies (macOS with Homebrew)
brew install openssl pkg-config

# Compile Ring (needs Rust 1.85+)
cargo build --release

# Install the binary
sudo cp target/release/ring /usr/local/bin/
```

### Option 3: Docker (development)

```bash
docker build -t ring .

docker run -d \
  --name ring-server \
  -p 3030:3030 \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v $(pwd)/data:/app/data \
  ring
```

> **Docker socket warning** — mounting `/var/run/docker.sock` gives Ring full control over the host's Docker daemon. Treat the Ring container as privileged.

## Verification

```bash
ring --version
docker ps
```

## Initial setup

### 1. Initialize the config directory

```bash
ring init
```

This creates `~/.config/kemeter/ring/` (or `$RING_CONFIG_DIR` if set) and writes an empty `auth.json`. **It does not create the database or seed the admin user** — that happens on the first `ring server start`.

### 2. Start the server

```bash
ring server start
```

On first start, the server:

- Listens on `127.0.0.1:3030` (configurable via `config.toml`)
- Runs SQLite migrations to create `ring.db` in the working directory (override with `RING_DATABASE_PATH`)
- Seeds the default admin user `admin` / `changeme`
- Logs to stdout (set `RUST_LOG=info` for visibility)

### 3. Check the API is up

```bash
curl http://localhost:3030/healthz
# {"state":"UP"}
```

### 4. Log in

```bash
ring login --username admin --password changeme
```

The token is stored in `~/.config/kemeter/ring/auth.json` and reused by subsequent CLI commands.

> **Change the default password immediately.** `ring user update --password "your-secure-password"` updates the currently authenticated user (whose token is in `auth.json`). The `--username` flag also lets you rename the admin account at the same time.

## Running as a service

### systemd (Linux)

Create a service file:

```bash
sudo tee /etc/systemd/system/ring.service > /dev/null <<EOF
[Unit]
Description=Ring container orchestrator
After=docker.service
Requires=docker.service

[Service]
Type=simple
User=root
WorkingDirectory=/opt/ring
Environment=RING_SECRET_KEY=your-base64-encoded-key
Environment=RUST_LOG=info
ExecStart=/usr/local/bin/ring server start
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF
```

Enable and start:

```bash
sudo mkdir -p /opt/ring
sudo chown $(whoami):$(whoami) /opt/ring

cd /opt/ring
ring init

sudo systemctl enable ring
sudo systemctl start ring
sudo systemctl status ring
```

### Docker Compose

```yaml
# compose.yaml
services:
  ring:
    build: .
    ports:
      - "3030:3030"
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - ./data:/app/data
    restart: unless-stopped
    environment:
      - RING_DATABASE_PATH=/app/data/ring.db
      - RING_SECRET_KEY=${RING_SECRET_KEY}
```

## Configuration

### config.toml

Ring reads `~/.config/kemeter/ring/config.toml` (or `$RING_CONFIG_DIR/config.toml`) for client and server settings. See the [CLI reference](/documentation/reference/cli) for the full schema. The bind address and port live there.

### Environment variables

- `RING_DATABASE_PATH` — path to the SQLite database file (default: `./ring.db`)
- `RING_DB_POOL_SIZE` — maximum SQLite connections in the pool (default: `5`)
- `RING_CONFIG_DIR` — config directory path (default: `~/.config/kemeter/ring`)
- `RING_SECRET_KEY` — 32-byte base64-encoded encryption key for the secrets feature. **Required** to create or read secrets — without it, the server returns `500` on secret endpoints.
- `RING_APPLY_TIMEOUT` — timeout in seconds for a single scheduler `apply` cycle (default: `300`)
- `RING_SCHEDULER_INTERVAL` — scheduler tick interval in seconds (overrides `scheduler.interval` in `config.toml`)
- `RUST_LOG` — log level (e.g. `RUST_LOG=info` or `RUST_LOG=ring=debug`)

#### Generating the secret key

```bash
openssl rand -base64 32
```

Export it before starting the server:

```bash
export RING_SECRET_KEY="your-base64-encoded-key"
ring server start
```

> **Key management** — store this key securely. If lost, encrypted secrets become unrecoverable. If leaked, rotate it and recreate every secret.

## Troubleshooting

### "Failed to connect to Docker daemon"

Docker is not running, or the user lacks permissions.

```bash
sudo systemctl start docker
sudo usermod -aG docker $USER  # then log out and back in
```

### "Permission denied" on `/var/run/docker.sock`

User is not in the `docker` group.

```bash
sudo usermod -aG docker $USER  # then log out and back in
```

### "Port 3030 already in use"

Another service is bound to 3030. Change the port in `config.toml`:

```toml
[contexts.default]
api.port = 3031
```

Or find and stop the conflicting process:

```bash
sudo ss -tlnp | grep 3030
```

### Run `ring doctor`

`ring doctor` checks Docker connectivity and Cloud Hypervisor prerequisites (binary, KVM, firmware, virtiofsd). Use it as a first-step diagnostic.

### Logs

```bash
# Service mode
sudo journalctl -u ring -f

# Foreground
RUST_LOG=info ring server start
```

## Next steps

1. [Getting started](/documentation/getting-started/overview)
2. [Your first deployment](/documentation/getting-started/first-deployment)
3. [Examples](/documentation/guides/examples)

## Uninstall

```bash
# Remove binary
sudo rm /usr/local/bin/ring

# Remove data
rm -rf ring.db ring.db-shm ring.db-wal
rm -rf ~/.config/kemeter/ring

# Remove systemd service
sudo systemctl stop ring
sudo systemctl disable ring
sudo rm /etc/systemd/system/ring.service
sudo systemctl daemon-reload
```
