# Ring

A simple container orchestrator with declarative service deployment using containers.

## Why Ring?

Ring was created as a lightweight alternative to Kubernetes and Docker Swarm, providing only the essential features needed for container orchestration.

## Key Features

- **Declarative Deployments**: Specify your service requirements through configuration files
- **Smart State Management**: Automatically compute differences between desired and current state (similar to Terraform)
- **High Availability**: Automatic container restart and replication
- **RESTful HTTP API**: Easy integration with your existing tools
- **Docker Engine Backend**: Leveraging proven Docker technology

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
# or
cargo run init
```

### 2. Start Server

```bash
ring server start
# or
cargo run server start
```

### 3. Login

```bash
ring login --username admin --password changeme
# or
cargo run login --username admin --password changeme
```

### 4. Deploy

#### Using YAML
```bash
ring apply -f examples/nginx.yaml
# or
cargo run apply -f examples/nginx.yaml
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
ring deployment:list
# or
cargo run deployment list
```

#### Inspect Deployment
```bash
ring deployment inspect <deployment_id>
# or
cargo run deployment inspect <deployment_id>
```
