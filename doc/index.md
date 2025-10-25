# Ring

Ring is a simple and declarative container orchestrator that allows you to deploy and manage your containerized applications with ease.

![Ring Logo](assets/ring-logo.png){ width="200" }

## What is Ring?

Ring is a lightweight alternative to Kubernetes and Docker Swarm, designed to provide essential container orchestration features without the complexity. It allows you to describe your deployments declaratively and takes care of maintaining the desired state of your applications.

!!! tip "Quick Start"
    New to Ring? Start with the [installation guide](installation.md) then follow the [getting started guide](getting-started/index.md)!

## Key Features

=== "🚀 Declarative Deployments"

    Describe your services with simple YAML or JSON files and let Ring handle the rest.

    ```yaml
    deployments:
      web-app:
        name: web-app
        image: "nginx:latest"
        replicas: 3
        namespace: production
    ```

=== "🔌 API-First Design"

    Everything is controllable via REST API. Perfect for CI/CD integration and automation.

    ```bash
    curl -X POST http://localhost:3030/deployments \
      -H "Authorization: Bearer $TOKEN" \
      -d @deployment.json
    ```

=== "📦 Multi-Runtime Support"

    Currently supports Docker with more runtimes planned for the future.

    ```yaml
    deployments:
      app:
        runtime: docker
        image: "myapp:latest"
    ```

=== "🏷️ Namespace Isolation"

    Organize your applications by environment or team with automatic network isolation.

    ```yaml
    deployments:
      app:
        namespace: production  # Isolated network
        replicas: 5
    ```

## Perfect for

Ring is ideal for:

- **Development environments**: Simple orchestration for teams
- **Web applications**: Service deployment with external load balancing
- **Microservices**: Managing medium-scale microservice architectures
- **CI/CD**: Automated deployment in your pipelines
- **Docker Compose migration**: Progressive transition to a more robust solution

## Quick Comparison

| Feature | Ring | Docker Compose | Kubernetes |
|---------|------|----------------|------------|
| Complexity | Low | Very Low | High |
| State Management | ✅ | ❌ | ✅ |
| REST API | ✅ | ❌ | ✅ |
| Multi-node | ❌ | ❌ | ✅ |
| Learning Curve | Gentle | Very Gentle | Steep |

## Quick Installation

=== "From source"

    ```bash
    git clone https://github.com/kemeter/ring.git
    cd ring
    cargo build --release
    sudo cp target/release/ring /usr/local/bin/
    ring init
    ```

=== "Package managers"

    ```bash
    # Package managers will be supported in future releases
    # For now, compile from source
    ```

## Your First Deployment

Once Ring is installed, create your first deployment:

```yaml title="nginx-demo.yaml"
deployments:
  nginx-demo:
    name: nginx-demo
    runtime: docker
    image: "nginx:latest"
    replicas: 1
```

Deploy it:

```bash
ring apply -f nginx-demo.yaml
```

Check the status:

```bash
ring deployment list
```

That's it! Your Nginx server is now running and managed by Ring.

## Architecture Overview

<div class="grid cards" markdown>

-   **Ring Server**

    Central orchestration engine that manages deployments and exposes the REST API.

-   **Docker Runtime**

    Currently uses Docker as the container runtime. Creates isolated networks per namespace.

-   **SQLite Database**

    Stores deployment state, user information, and configuration locally.

-   **REST API**

    Complete API access to all Ring functionality. Powers both CLI and external integrations.

</div>

## Support and Community

!!! info "Need Help?"

    - **Questions**: [GitHub Discussions](https://github.com/kemeter/ring/discussions)
    - **Bugs**: [GitHub Issues](https://github.com/kemeter/ring/issues)
    - **Documentation**: This documentation
    - **Source code**: [GitHub Repository](https://github.com/kemeter/ring)
    - **Commercial support**: [Alpacode.fr](https://alpacode.fr)

---

**Ready to get started?** Follow the [installation guide](installation.md)! 🚀