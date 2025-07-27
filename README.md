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
