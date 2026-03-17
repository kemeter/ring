# Installation

This guide walks you through installing Ring on your system.

## Prerequisites

Ring requires:

- **Docker**: [Official installation guide](https://docs.docker.com/get-docker/)
- **Rust** (for compilation): [Rust installation](https://rustup.rs/)

!!! tip "Quick verification"
    ```bash
    docker --version
    rustc --version  # If compiling from source
    ```

## Installing Ring

### Option 1: Pre-compiled Binary (Recommended)

*Note: Pre-compiled binaries will be available soon.*

### Option 2: Compile from Source

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

# Compile Ring
cargo build --release

# Install the binary
sudo cp target/release/ring /usr/local/bin/
```

### Option 3: Docker (Development)

```bash
# Build Ring Docker image
docker build -t ring .

# Run Ring in a container
docker run -d \
  --name ring-server \
  -p 3030:3030 \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v $(pwd)/data:/app/data \
  ring
```

!!! warning "Docker Socket"
    When running Ring in Docker, mounting the Docker socket allows Ring to manage containers on the host.

## Verification

### Check Ring Installation

```bash
# Check Ring version
ring --version

# Verify Docker access
docker ps
```

### Test Ring Server

```bash
# Initialize Ring
ring init

# Start Ring server
ring server start
```

The server should start on `http://localhost:3030`.

### Health Check

```bash
# Test the API
curl http://localhost:3030/healthz
# Expected: {"state":"UP"}
```

## Initial Configuration

### Initialize the Database

```bash
ring init
```

This command:
- Creates the SQLite database (`ring.db`)
- Creates the default admin user (`admin` / `changeme`)

### Start the Server

```bash
ring server start
```

Ring server will:
- Listen on port `3030` by default
- Use the SQLite database in the current directory
- Log activity to the console

### Login

```bash
ring login --username admin --password changeme
```

!!! tip "Change Default Password"
    For security, change the default admin password immediately:
    ```bash
    ring user update --username admin --password "your-secure-password"
    ```

## Running as a Service

### systemd (Linux)

Create a service file:

```bash
sudo tee /etc/systemd/system/ring.service > /dev/null <<EOF
[Unit]
Description=Ring Container Orchestrator
After=docker.service
Requires=docker.service

[Service]
Type=simple
User=root
WorkingDirectory=/opt/ring
Environment=RING_SECRET_KEY=your-base64-encoded-key
ExecStart=/usr/local/bin/ring server start
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF
```

Enable and start:

```bash
# Create working directory
sudo mkdir -p /opt/ring
sudo chown $(whoami):$(whoami) /opt/ring

# Initialize Ring in the service directory
cd /opt/ring
ring init

# Enable and start service
sudo systemctl enable ring
sudo systemctl start ring
sudo systemctl status ring
```

### Docker Compose

```yaml title="docker-compose.yml"
version: '3.8'

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

## Configuration Options

### Environment Variables

- `RING_DATABASE_PATH`: Path to SQLite database file (default: `./ring.db`)
- `RING_DB_POOL_SIZE`: Maximum database connections in the pool (default: `5`)
- `RING_CONFIG_DIR`: Configuration directory path (default: `~/.config/kemeter/ring`)
- `RING_SECRET_KEY`: Encryption key for secrets management (32 bytes, base64-encoded). Required to use the secrets feature.
- `RING_APPLY_TIMEOUT`: Timeout in seconds for apply operations (default: `300`)
- `RING_SCHEDULER_INTERVAL`: Scheduler check interval in seconds (overrides config.toml value, default: `10`)

#### Generating a secret key

```bash
# Generate a random 32-byte key, base64-encoded
openssl rand -base64 32
```

Set it before starting the server:

```bash
export RING_SECRET_KEY="your-base64-encoded-key"
ring server start
```

!!! warning "Key Management"
    Store this key securely. If lost, all encrypted secrets become unrecoverable. If compromised, rotate it and recreate all secrets.

### Command Line Options

The Ring server currently uses default settings. Configuration options will be expanded in future versions.

## Troubleshooting

### Common Issues

#### "Failed to connect to Docker daemon"

**Cause**: Docker is not running or user lacks permissions.

**Solution**:
```bash
# Start Docker
sudo systemctl start docker

# Add user to docker group
sudo usermod -aG docker $USER
# Then logout and login again
```

#### "Permission denied" on `/var/run/docker.sock`

**Cause**: User not in docker group.

**Solution**:
```bash
sudo usermod -aG docker $USER
# Logout and login again
```

#### "Port 3030 already in use"

**Cause**: Another service is using port 3030.

**Solution**:
```bash
# Find the process using port 3030
sudo ss -tlnp | grep 3030

# Stop the conflicting service or use a different port
# (Port configuration will be available in future versions)
```

### Logs and Debugging

```bash
# Check Ring server logs (if running as service)
sudo journalctl -u ring -f

# Test Docker connectivity
docker ps

# Verify Ring database
ls -la ring.db
```

## Next Steps

Now that Ring is installed:

1. Follow the [Getting Started guide](getting-started/index.md)
2. Create your [first deployment](getting-started/first-deployment.md)
3. Explore [examples](examples.md)

## Uninstallation

### Remove Ring Binary

```bash
sudo rm /usr/local/bin/ring
```

### Remove Data

```bash
# Remove database and data
rm -rf ring.db

# Remove service (if installed)
sudo systemctl stop ring
sudo systemctl disable ring
sudo rm /etc/systemd/system/ring.service
sudo systemctl daemon-reload
```