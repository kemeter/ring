# Run Ring as a service

For anything beyond local development, run `ring server` under a process manager so it survives reboots, restarts on crash, and gets its environment from a managed file rather than your shell.

Two common shapes:

## systemd

Create `/etc/systemd/system/ring.service`:

```toml
[Unit]
Description=Ring container orchestrator
After=docker.service
Requires=docker.service

[Service]
Type=simple
User=root
WorkingDirectory=/opt/ring
EnvironmentFile=/etc/ring/env
ExecStart=/usr/local/bin/ring server start
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
```

Put your environment variables in `/etc/ring/env` (mode `0600`, owned by root):

```bash
RING_SECRET_KEY=your-base64-encoded-key
RING_DATABASE_PATH=/opt/ring/ring.db
RUST_LOG=info
```

Prepare the working directory and init the config:

```bash
sudo mkdir -p /opt/ring
sudo chown $(whoami):$(whoami) /opt/ring
cd /opt/ring
ring init
```

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable ring
sudo systemctl start ring
sudo systemctl status ring
```

Follow the logs:

```bash
sudo journalctl -u ring -f
```

### Why `EnvironmentFile` rather than `Environment=`

The systemd `Environment=` directive bakes the value into the unit file, which usually ends up in version control. A separate `EnvironmentFile=` keeps `RING_SECRET_KEY` out of git and lets you `chmod 0600` it.

## Docker Compose

If Ring itself runs as a container:

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

> **Docker socket warning.** Mounting `/var/run/docker.sock` gives the Ring container full control over the host's Docker daemon — equivalent to root on the host. Treat the Ring container as privileged.

Set `RING_SECRET_KEY` in your shell or a `.env` file (`.env` next to `compose.yaml`, gitignored).

```bash
echo "RING_SECRET_KEY=$(openssl rand -base64 32)" > .env
chmod 600 .env
docker compose up -d
```

## Update the binary

```bash
sudo systemctl stop ring
sudo cp target/release/ring /usr/local/bin/ring
sudo systemctl start ring
```

If you've changed the binary in a way that adds new SQLite migrations, they run automatically on the next start. The database is opened in WAL mode — a normal restart doesn't lose any committed state.

## Uninstall

```bash
sudo systemctl stop ring
sudo systemctl disable ring
sudo rm /etc/systemd/system/ring.service
sudo systemctl daemon-reload

sudo rm /usr/local/bin/ring
rm -rf /opt/ring/ring.db /opt/ring/ring.db-shm /opt/ring/ring.db-wal
rm -rf ~/.config/kemeter/ring
```

## See also

- [Tutorial: install and run](/documentation/tutorials/install-and-run) — first install
- [How-to: observe and debug](/documentation/how-to/observe-and-debug) — logs and event streams
- [Reference: environment variables](/documentation/reference/environment-variables)
