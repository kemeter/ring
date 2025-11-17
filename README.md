# Ring

A simple container orchestrator with declarative service deployment using containers.

## Why Ring?

Ring was created as a lightweight alternative to Kubernetes and Docker Swarm, providing only the essential features needed for container orchestration.

## Key Features

- **Declarative Deployments**: Specify your service requirements through YAML or JSON configuration files
- **Smart State Management**: Automatically compute differences between desired and current state (similar to Terraform)
- **High Availability**: Automatic container restart and replication with configurable replica counts
- **RESTful HTTP API**: Easy integration with your existing tools and CI/CD pipelines
- **Docker Engine Backend**: Leveraging proven Docker technology
- **Multi-namespace Support**: Organize deployments across different namespaces
- **Volume Management**: Support for persistent volumes and bind mounts
- **Secret Management**: Secure handling of environment variables and secrets
- **User Management**: Role-based access control with user authentication
- **Container Registry Authentication**: Support for private Docker registries
- **Flexible Image Pull Policies**: Configure when to pull container images (Always, IfNotPresent)
- **Container Logs Access**: Stream and inspect container logs
- **Network Isolation**: Create isolated container networks per namespace
- **Health Checks**: Monitor container health with TCP, HTTP, and command-based checks

*Note*: Ring does not handle load balancing, as it's not within its scope.

## Prerequisites

- Rust (for building)
- OpenSSL (v0.9.90):
    - **Ubuntu/Debian**: `sudo apt install librust-openssl-sys-dev`
    - **Fedora**: `sudo dnf install openssl-devel`
    - **Arch Linux**: `sudo pacman -S openssl`
    - **macOS**: `brew install openssl`
    - **Windows**: Download and install OpenSSL from [https://slproweb.com/products/Win32OpenSSL.html](https://slproweb.com/products/Win32OpenSSL.html)

## Installation

```bash
cargo build
```

## Getting Started

### 1. Initialize

```bash
ring init
```

### 2. Start Server

```bash
ring server start
```

### 3. Login

```bash
ring login --username admin --password changeme
```

### 4. Deploy

#### Using YAML
```bash
ring apply -f examples/nginx.yaml
```

#### Using HTTP API
1. Get your token:
```bash
ring config user-token
```

2. Make the request:
```bash
http POST localhost:3030/deployments bearer -A bearer -a <your_token> < examples/nginx.json
```

### 5. Manage Deployments

#### List Deployments
```bash
ring deployment list
```

#### Inspect Deployment
```bash
ring deployment inspect <deployment_id>
```

#### View Container Logs
```bash
ring deployment logs <deployment_id>
```

#### Delete Deployment
```bash
ring deployment delete <deployment_id>
```

## Advanced Configuration

### Deployment Configuration Options

Ring supports comprehensive deployment configuration through YAML or JSON files:

```yaml
deployments:
  my-app:
    name: my-app
    runtime: docker
    image: "nginx:1.19.5"
    namespace: production
    replicas: 3
    config:
      image_pull_policy: "IfNotPresent"  # or "Always"
      username: "registry-user"         # for private registries
      password: "registry-password"
      user:
        id: 1000                        # run as specific user ID
        group: 1000                     # run as specific group ID
        privileged: false               # security settings
    volumes:
      - "/host/path:/container/path"    # bind mounts
      - "/data:/app/data"
    secrets:
      DATABASE_URL: "postgres://user:pass@host:5432/db"
      API_KEY: "$API_KEY"               # environment variable substitution
    labels:
      - "traefik.enable=true"           # custom labels for service discovery
      - "traefik.http.routers.app.rule=Host(`app.example.com`)"
    health_checks:
      - type: tcp
        port: 80                        # check if port is accessible
        interval: "10s"                 # check every 10 seconds
        timeout: "5s"                   # timeout after 5 seconds
        threshold: 3                    # fail after 3 consecutive failures
        on_failure: "restart"           # action: restart, stop, or alert
      - type: http
        url: "http://localhost:8080/"   # HTTP endpoint to check
        interval: "30s"
        timeout: "10s"
        threshold: 2
        on_failure: "alert"
      - type: command
        command: "nginx -t"             # custom health check command
        interval: "60s"
        timeout: "15s"
        threshold: 1
        on_failure: "stop"
```

### User Management

#### Create Users
```bash
ring user create --username <username> --password <password>
```

#### List Users
```bash
ring user list
```

#### Update User Password
```bash
ring user update --username <username> --password <new_password>
```

#### Delete User
```bash
ring user delete --username <username>
```

### Configuration Management

#### List Configurations
```bash
ring config list
```

#### Inspect Configuration
```bash
ring config inspect <config_key>
```

#### Delete Configuration
```bash
ring config delete <config_key>
```

### Namespace Management

#### Prune Namespace (Clean up unused resources)
```bash
ring namespace prune <namespace>
```

### Node Information

#### Get Node Status
```bash
ring node get
```

## Health Checks

Ring supports comprehensive container health monitoring with three types of health checks:

### Health Check Types

- **TCP Check**: Verifies that a specific port is accessible
- **HTTP Check**: Makes HTTP requests to validate service availability  
- **Command Check**: Executes custom commands inside containers for application-specific health validation

### Important Notes

✅ **Health checks run per container instance** from the host using container IPs. This means:

- **For HTTP checks**: Use `localhost` in URLs (e.g., `http://localhost:80/health`) - Ring replaces with each container's IP
- **For TCP checks**: Use internal container ports (e.g., port 80) - Ring connects to each container's IP  
- **For Command checks**: Commands execute inside each container (using `docker exec`)

### Configuration

```yaml
health_checks:
  - type: tcp
    port: 80                    # Container internal port
    interval: "10s"             # How often to check
    timeout: "5s"               # Request timeout  
    threshold: 3                # Failures before triggering action
    on_failure: "restart"       # Action: restart, stop, alert
    
  - type: http  
    url: "http://localhost:80/health"    # Container internal URL
    interval: "30s"
    timeout: "10s"
    threshold: 2
    on_failure: "alert"
    
  - type: command
    command: "nginx -t"                 # Runs inside each container  
    interval: "60s"  
    timeout: "15s"
    threshold: 1
    on_failure: "stop"
```

### Failure Actions

- **restart**: Remove the failed container instance (scheduler automatically recreates it)
- **stop**: Delete the deployment (stops and removes all containers)  
- **alert**: Log an alert event (visible in `ring deployment events`)

### How It Works

Each health check runs **per container instance** using real container IPs:
- Ring inspects each container to get its IP address (e.g., `172.17.0.3`)
- HTTP: `http://localhost:80/health` becomes `http://172.17.0.3:80/health`  
- TCP: Connects directly to `172.17.0.3:80`
- Deployment with 3 replicas = 3 × health checks per config
- Failed instance gets removed and automatically recreated
- Maintains high availability by keeping healthy instances running
- Blocking operations (HTTP/TCP) run in separate threads to avoid blocking the scheduler
- Database cleanup: keeps max 50 checks per deployment + 7 days retention

### Monitoring Health Checks

View health check results and events:

```bash
# View deployment events (includes health check alerts)
ring deployment events <deployment_id>
```
