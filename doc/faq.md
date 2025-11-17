# Frequently Asked Questions (FAQ)

This page answers the most common questions about Ring.

## General Questions

### What exactly is Ring?

Ring is a lightweight container orchestrator that allows you to deploy and manage containerized applications declaratively. It's inspired by Kubernetes but with a much simpler approach and reduced learning curve.

### How does Ring differ from Kubernetes?

| Aspect | Ring | Kubernetes |
|--------|------|------------|
| **Complexity** | Simple, minimal configuration | Complex, steep learning curve |
| **Size** | Single binary, lightweight | Complete distribution with many components |
| **Scope** | Essential orchestration | Complete platform with service mesh, etc. |
| **Target** | SMBs, development, simple applications | Large enterprises, complex use cases |
| **Configuration** | Simple YAML | Complex YAML with many resources |

### How does Ring differ from Docker Compose?

Ring goes further than Docker Compose by offering:

- **State management**: Ring maintains desired state and automatically restarts containers
- **REST API**: Integration with external systems
- **Multi-namespace**: Logical isolation of environments
- **Scaling**: Replica management
- **Authentication**: User management system

### Is Ring ready for production?

Ring is suitable for small to medium-sized production environments. It is not recommended for:

- Critical applications requiring complex high availability
- Multi-node clusters (not yet supported)
- Use cases requiring advanced service mesh

## Installation and Configuration

### What are the system requirements?

**Required:**
- Docker Engine (recent version)
- Linux, macOS or Windows with WSL2

**For compilation:**
- Rust 1.70+
- OpenSSL (development)

### How to install Ring on Ubuntu?

```bash
# Install Docker
sudo apt update
sudo apt install docker.io
sudo usermod -aG docker $USER

# Install Rust (for compilation)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install dependencies
sudo apt install libssl-dev pkg-config

# Compile Ring
git clone https://github.com/kemeter/ring.git
cd ring
cargo build --release
sudo cp target/release/ring /usr/local/bin/

# Initialize Ring
ring init
```

### Can I use Ring with Podman instead of Docker?

Currently, Ring only supports Docker. Podman support might be added in the future.

### How to configure Ring to listen on a different port?

```bash
# Start with custom port
ring server start --port 8080

# Or via environment variable
export RING_PORT=8080
ring server start
```

## Usage and Deployments

### How to specify a specific image version?

Always specify an explicit version in production:

```yaml
deployments:
  app:
    image: "nginx:1.21.6"  # ✅ Specific version
    # image: "nginx:latest"  # ❌ Avoid latest in production
```

### How to manage sensitive secrets?

Ring supports several approaches:

```yaml
# ✅ Environment variables (recommended)
secrets:
  DATABASE_PASSWORD: "$DB_PASSWORD"

# ✅ External secret files
volumes:
  - "/secure/secrets:/app/secrets:ro"

# ❌ Avoid hardcoded secrets
secrets:
  PASSWORD: "password123"  # Bad practice
```

### How to do load balancing?

Ring doesn't do built-in load balancing. Use an external proxy:

**With Traefik:**
```yaml
deployments:
  app:
    replicas: 3
    labels:
      - "traefik.enable=true"
      - "traefik.http.routers.app.rule=Host(`app.example.com`)"
```

**With Nginx:**
```nginx
upstream ring_app {
    server localhost:32768;  # Container port 1
    server localhost:32769;  # Container port 2
    server localhost:32770;  # Container port 3
}

server {
    listen 80;
    server_name app.example.com;
    location / {
        proxy_pass http://ring_app;
    }
}
```

### How to persist data?

Use bind mounts for persistence:

```yaml
deployments:
  database:
    volumes:
      # Persistent data on host
      - "/var/lib/ring/postgres:/var/lib/postgresql/data"

      # Backups
      - "/backup/postgres:/backup"
```

### How to handle database migrations?

Several approaches are possible:

**1. Separate migration job:**
```yaml
deployments:
  migration:
    name: migration
    image: "myapp:latest"
    replicas: 1
    command: ["npm", "run", "migrate"]
    # Remove after manual migration
```

**2. Migration at startup:**
```yaml
deployments:
  app:
    command: ["sh", "-c", "npm run migrate && npm start"]
```

## Troubleshooting

### "Failed to connect to Docker daemon"

**Possible causes:**
- Docker is not started
- Insufficient permissions
- Docker socket inaccessible

**Solutions:**
```bash
# Check Docker
sudo systemctl status docker
sudo systemctl start docker

# User permissions
sudo usermod -aG docker $USER
# Then restart the session

# Test
docker ps
```

### "Port already in use"

**Cause:** Port 3030 is already in use.

**Solutions:**
```bash
# Find the process
sudo ss -tlnp | grep 3030

# Use a different port
ring server start --port 8080

# Or stop the other service
sudo systemctl stop service-using-3030
```

### "Authentication failed"

**Possible causes:**
- Wrong credentials
- Ring server not started
- Expired token

**Solutions:**
```bash
# Check the server
curl http://localhost:3030/healthz

# Retry connection
ring login --username admin --password changeme

# Reset if necessary
ring init
```

### Containers won't start

**Diagnostics:**
```bash
# Check deployments
ring deployment list

# Detailed logs
ring deployment logs problematic-app

# Events
ring deployment events problematic-app

# Check Docker directly
docker ps -a --filter "label=ring.deployment=problematic-app"
```

**Common causes:**
- Image not found
- Error in command
- Port conflicts
- Non-existent volumes

### "No space left on device"

**Solutions:**
```bash
# Clean up Docker
docker system prune -f

# Clean up unused images
docker image prune -a

# Clean up volumes
docker volume prune
```

## Performance and Scaling

### How many containers can Ring handle?

Ring can theoretically handle hundreds of containers on a machine, but it depends on:

- **System resources** (CPU, RAM, storage)
- **Application type** (lightweight vs heavy)
- **Disk and network I/O**

For large requirements, consider Kubernetes.

### How to optimize performance?

**System configuration:**
```bash
# Increase Docker limits
echo '{"default-ulimits":{"nofile":{"Name":"nofile","Hard":65536,"Soft":65536}}}' | sudo tee /etc/docker/daemon.json
sudo systemctl restart docker
```

**Image optimization:**
```yaml
deployments:
  app:
    # Use Alpine images when possible
    image: "node:16-alpine"

    config:
      # Avoid downloading images every time
      image_pull_policy: "IfNotPresent"
```

### Does Ring support clustering?

No, Ring is currently designed for a single node. Multi-node support is planned for future versions.

## Security

### How to secure Ring in production?

**Network:**
```bash
# Use a reverse proxy with TLS
# Restrict access to Ring port (3030)
sudo ufw allow from 192.168.1.0/24 to any port 3030
sudo ufw deny 3030
```

**Authentication:**
```bash
# Change admin password
ring user update --username admin --password "new-strong-password"

# Create dedicated users
ring user create --username deployer --password "deployer-password"
```

**System:**
```bash
# Run Ring with a dedicated user
sudo useradd -r -s /bin/false ring
sudo systemctl edit ring.service
# [Service]
# User=ring
# Group=ring
```

### How to manage TLS certificates?

Ring doesn't have built-in TLS support. Use a reverse proxy:

**With Nginx:**
```nginx
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

## Integration and Automation

### How to integrate Ring with CI/CD?

**GitHub Actions:**
```yaml
name: Deploy to Ring
on:
  push:
    branches: [main]

jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - name: Deploy
        run: |
          ring login --username ${{ secrets.RING_USER }} --password ${{ secrets.RING_PASSWORD }}
          ring apply -f deployment.yaml
```

**GitLab CI:**
```yaml
deploy:
  stage: deploy
  script:
    - ring login --username $RING_USER --password $RING_PASSWORD
    - ring apply -f deployment.yaml
  only:
    - main
```

### How to monitor Ring?

Ring doesn't expose built-in metrics. To monitor:

- **Ring itself**: Use the `/healthz` endpoint
- **Your applications**: Configure monitoring in your containers



## Support and Community

### Where to get help?

- **Documentation**: This documentation
- **GitHub Issues**: [github.com/kemeter/ring/issues](https://github.com/kemeter/ring/issues)
- **Discussions**: [github.com/kemeter/ring/discussions](https://github.com/kemeter/ring/discussions)

### How to contribute to Ring?

1. **Fork** the repository
2. **Create** a branch for your feature
3. **Test** your changes
4. **Submit** a pull request

### Does Ring have commercial support?

Yes, Ring has official commercial support provided by [Alpacode.fr](https://alpacode.fr). Community support is also available via GitHub.

---

**This FAQ didn't answer your question?**
Feel free to [open an issue](https://github.com/kemeter/ring/issues) or check the [discussions](https://github.com/kemeter/ring/discussions).