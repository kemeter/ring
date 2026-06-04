# Frequently asked questions

Common questions about Ring.

## General

### What is Ring?

Ring is a lightweight workload orchestrator. It runs as a single binary, persists state in SQLite, and reconciles deployments against Docker (or Cloud Hypervisor microVMs, in alpha). You describe what you want in YAML; Ring keeps it that way.

### How is Ring different from Kubernetes?

| Aspect       | Ring                                    | Kubernetes                               |
|--------------|-----------------------------------------|------------------------------------------|
| Complexity   | Simple, single binary                   | Complex, many control-plane components   |
| Scope        | Essential single-node orchestration     | Full platform (service mesh, CRDs, etc.) |
| Target       | Small/medium workloads                  | Large clusters, complex requirements     |
| Manifest     | Compact YAML                            | Verbose YAML across many resources       |

### How is Ring different from Docker Compose?

Ring goes further than Compose by reconciling state continuously, exposing a REST API, and supporting namespaces, scaling, secrets, and authentication. Compose is a one-shot tool; Ring keeps your deployments in the desired state until you say otherwise.

### Is Ring production-ready?

Ring is suitable for small to medium single-node production workloads. It is not designed for:

- Multi-node clusters (not yet supported)
- Workloads requiring a full service mesh
- HA setups across machines

## Installation and configuration

### What are the system requirements?

- Docker (recent version)
- Linux (x86_64 or arm64). macOS works for development; Windows requires WSL2.
- Rust 1.85 or later when compiling from source (Ring uses edition 2024).

### How do I install Ring on Ubuntu?

```bash
# Install Docker
sudo apt update
sudo apt install docker.io
sudo usermod -aG docker $USER  # then log out and back in

# Install Rust 1.85+ via rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install build dependencies
sudo apt install libssl-dev pkg-config

# Compile and install
git clone https://github.com/kemeter/ring.git
cd ring
cargo build --release
sudo cp target/release/ring /usr/local/bin/

# Initialize the config directory
ring init
```

### Can I use Podman instead of Docker?

Not currently. Ring uses the [bollard](https://crates.io/crates/bollard) crate to talk to a Docker-compatible socket. Podman support is not on the roadmap.

### How do I run Ring on a different port?

Edit `~/.config/kemeter/ring/config.toml`:

```toml
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = 8080            # change here
```

Then restart `ring server start`.

## Usage

### Should I pin image tags?

Yes. Always pin to a specific tag in production:

```yaml
deployments:
  app:
    image: "nginx:1.25.3"   # good
    # image: "nginx:latest" # bad — moving target
```

### How do I manage sensitive secrets?

Use Ring's encrypted secrets, not plain values in the manifest:

```yaml
environment:
  DATABASE_PASSWORD:
    secretRef: "database-password"
```

Create the secret first:

```bash
ring secret create database-password -n production -v "$(openssl rand -base64 24)"
```

The server must be started with `RING_SECRET_KEY` set (a base64-encoded 32-byte key).

### How do I do load balancing?

Ring does not include a built-in load balancer. Two common patterns:

**Traefik via container labels:**

```yaml
deployments:
  app:
    replicas: 3
    labels:
      app: web
      "traefik.enable": "true"
      "traefik.http.routers.app.rule": "Host(`app.example.com`)"
```

**Nginx upstream block (manual):**

```
upstream ring_app {
    server 127.0.0.1:32768;
    server 127.0.0.1:32769;
    server 127.0.0.1:32770;
}

server {
    listen 80;
    server_name app.example.com;
    location / {
        proxy_pass http://ring_app;
    }
}
```

### How do I persist data?

Use a `bind` volume:

```yaml
deployments:
  database:
    volumes:
      - type: bind
        source: /var/lib/ring/postgres
        destination: /var/lib/postgresql/data
        driver: local
        permission: rw
      - type: bind
        source: /backup/postgres
        destination: /backup
        driver: local
        permission: rw
```

Or a named Docker volume:

```yaml
volumes:
  - type: volume
    source: postgres-data
    destination: /var/lib/postgresql/data
    driver: local
    permission: rw
```

### How do I handle database migrations?

**Separate migration job:**

```yaml
deployments:
  migration:
    name: migration
    namespace: production
    runtime: docker
    kind: job
    image: "myapp:latest"
    replicas: 1
    command: ["npm", "run", "migrate"]
```

**Run migrations at app startup:**

```yaml
deployments:
  app:
    command: ["sh", "-c", "npm run migrate && npm start"]
```

## Troubleshooting

### `ring doctor`

Always start with `ring doctor`. It checks Docker connectivity and Cloud Hypervisor prerequisites.

### "Failed to connect to Docker daemon"

```bash
sudo systemctl status docker
sudo systemctl start docker
sudo usermod -aG docker $USER  # then log out and back in
docker ps
```

### "Port already in use"

```bash
sudo ss -tlnp | grep 3030
```

Change `api.port` in `config.toml` and restart.

### "Authentication failed"

```bash
curl http://localhost:3030/healthz
ring login --username admin --password changeme
```

If the admin password was changed and you've forgotten it, you currently have to wipe `ring.db` and let the server re-seed `admin/changeme` on next start. There is no `ring init` reset path for the database.

### Containers won't start

```bash
ring deployment list
ring deployment logs <DEPLOYMENT_ID>
ring deployment events <DEPLOYMENT_ID>

# Check Docker directly
docker ps -a --filter "label=ring_deployment=<DEPLOYMENT_ID>"
docker logs <CONTAINER_ID>
```

Common causes: image not found, malformed `command`, missing host path for a `bind` volume, missing secret referenced via `secretRef`.

### "No space left on device"

```bash
docker system prune -f
docker image prune -a
docker volume prune
```

## Performance and scaling

### How many containers can Ring handle on one node?

Hundreds, depending on host CPU/RAM/disk and application weight. For larger deployments or multi-node, consider Kubernetes.

### How do I optimize performance?

```bash
# Raise Docker file-descriptor limits
echo '{"default-ulimits":{"nofile":{"Name":"nofile","Hard":65536,"Soft":65536}}}' \
  | sudo tee /etc/docker/daemon.json
sudo systemctl restart docker
```

```yaml
deployments:
  app:
    image: "node:18-alpine"   # use slim base images
    config:
      image_pull_policy: "IfNotPresent"
```

### Does Ring support clustering?

No. Ring is single-node. Multi-node clustering is not currently planned.

## Security

### How do I secure Ring in production?

Network — front Ring with a reverse proxy doing TLS termination, and restrict the API port to trusted networks:

```bash
# UFW: allow only the LAN to reach 3030
sudo ufw allow from 192.168.1.0/24 to any port 3030 proto tcp
```

> Don't add a blanket `sudo ufw deny 3030` after that. UFW evaluates rules in order, but a generic deny on the same port can mask the more specific allow depending on your policy. Use a default-deny policy and only allow trusted ranges.

Authentication — change the default admin password as soon as you log in:

```bash
ring user update --password "new-strong-password"
ring user create --username deployer --password "deployer-password"
```

Run as a dedicated user via systemd (`User=ring`, `Group=ring`).

### How do I manage TLS?

Ring doesn't terminate TLS. Use a reverse proxy:

```
server {
    listen 443 ssl http2;
    server_name ring.example.com;

    ssl_certificate /path/to/cert.pem;
    ssl_certificate_key /path/to/key.pem;

    location / {
        proxy_pass http://127.0.0.1:3030;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }
}
```

## Integration and automation

### CI/CD with Ring

**GitHub Actions** (using a long-lived token via `RING_TOKEN`):

```yaml
name: Deploy to Ring
on:
  push:
    branches: [main]

jobs:
  deploy:
    runs-on: ubuntu-latest
    env:
      RING_TOKEN: ${{ secrets.RING_TOKEN }}
    steps:
      - uses: actions/checkout@v4
      - name: Deploy
        run: ring apply -f deployment.yaml
```

**GitLab CI:**

```yaml
deploy:
  stage: deploy
  script:
    - export RING_TOKEN="$RING_API_TOKEN"
    - ring apply -f deployment.yaml
  only:
    - main
```

When `RING_TOKEN` is set, the CLI ignores `auth.json` and uses the token directly — no need to run `ring login` from the pipeline.

### How do I monitor Ring?

- Health: `GET /healthz` returns `{"state":"UP"}`
- Per-deployment: `ring deployment metrics <id>` (CPU/memory/network/disk per instance)
- Per-deployment events: `ring deployment events <id>` or stream via `--follow`
- Per-deployment health checks: `ring deployment health-checks <id>`
- Push notifications: subscribe to [outbound webhooks](../how-to/subscribe-to-events-with-webhooks.md) to receive deployment events (status changes, scaling, rollouts, errors) as signed HTTP POSTs

Ring does not yet expose Prometheus-format metrics.

## Support and community

- Documentation — this site
- Issues — [github.com/kemeter/ring/issues](https://github.com/kemeter/ring/issues)
- Discussions — [github.com/kemeter/ring/discussions](https://github.com/kemeter/ring/discussions)
- Commercial support — [Alpacode](https://alpacode.fr)

### How do I contribute?

1. Fork the repository
2. Create a feature branch
3. Add tests
4. Open a pull request

---

This FAQ didn't answer your question? [Open an issue](https://github.com/kemeter/ring/issues) or check the [discussions](https://github.com/kemeter/ring/discussions).
