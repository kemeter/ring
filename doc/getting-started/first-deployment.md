# Your First Deployment

Now that Ring is configured, let's deploy your first application! We'll use a simple Nginx web server.

## Method 1: Deployment with YAML File

### Creating the Configuration File

Create a file called `my-first-app.yaml`:

```yaml title="my-first-app.yaml"
deployments:
  nginx-demo:
    name: nginx-demo
    namespace: default
    runtime: docker
    kind: worker  # Long-running service (default)
    image: "nginx:latest"
    replicas: 1
    labels:
      - "app=nginx"
      - "version=latest"
```

### Deployment

```bash
ring apply -f my-first-app.yaml
```

You should see:

```
✅ Deployment nginx-demo created successfully
📦 Container nginx-demo-1 starting
🚀 Deployment nginx-demo is now running
```

## Method 2: Deployment via REST API

### Getting the Token

```bash
# The token is automatically saved after login
# You can check it worked by listing deployments
ring deployment list
```

### Sending the Request

```bash
curl -X POST http://localhost:3030/deployments \
  -H "Authorization: Bearer YOUR_TOKEN_HERE" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "nginx-api",
    "runtime": "docker",
    "namespace": "default",
    "kind": "worker",
    "replicas": 1,
    "image": "nginx:latest",
    "labels": {},
    "secrets": {},
    "volumes": []
  }'
```

## Verifying the Deployment

### List Deployments

```bash
ring deployment list
```

Expected result:
```
┌─────────────┬───────────┬───────────┬─────────┬────────────┐
│ ID          │ Name      │ Namespace │ Replicas│ Status     │
├─────────────┼───────────┼───────────┼─────────┼────────────┤
│ nginx-demo  │ nginx-demo│ default   │ 1       │ Running    │
└─────────────┴───────────┴───────────┴─────────┴────────────┘
```

### Detailed Inspection

```bash
ring deployment inspect nginx-demo
```

### Verification with Docker

```bash
# List Ring containers
docker ps --filter "label=ring_deployment"
```

## Testing the Application

### Identifying the Port

```bash
# Find the port exposed by the container
docker port $(docker ps -q --filter "label=ring.deployment=nginx-demo")
```

### HTTP Test

```bash
# Test the application (replace PORT with the found port)
curl http://localhost:PORT
```

You should see the default Nginx welcome page!

## Scaling the Application

### Modifying the YAML File

Edit `my-first-app.yaml` to increase the number of replicas:

```yaml title="my-first-app.yaml"
deployments:
  nginx-demo:
    name: nginx-demo
    namespace: default
    runtime: docker
    image: "nginx:latest"
    replicas: 3  # ← Changed from 1 to 3
    labels:
      - "app=nginx"
      - "version=latest"
```

### Redeployment

```bash
ring apply -f my-first-app.yaml
```

Ring will automatically:
- Detect the difference (1 → 3 replicas)
- Create 2 additional containers
- Maintain the desired state

### Verification

```bash
ring deployment list
docker ps --filter "label=ring.deployment=nginx-demo"
```

## Adding Advanced Configuration

### Example with Volumes and Environment Variables

```yaml title="nginx-advanced.yaml"
deployments:
  nginx-advanced:
    name: nginx-advanced
    namespace: production
    runtime: docker
    image: "nginx:1.21"
    replicas: 2

    # Environment variables
    secrets:
      NGINX_PORT: "80"
      CUSTOM_CONFIG: "production"

    # Volume mounts
    volumes:
      - "/tmp/nginx-logs:/var/log/nginx"

    # Labels for identification
    labels:
      - "app=nginx"
      - "environment=production"
      - "version=1.21"

    # Image configuration
    config:
      image_pull_policy: "IfNotPresent"
```

```bash
ring apply -f nginx-advanced.yaml
```

## Monitoring the Application

### Real-time Logs

```bash
# Follow deployment logs
ring deployment logs nginx-demo --follow
```

### Deployment Events

```bash
# View event history
ring deployment events nginx-demo
```

## Cleanup

### Deleting a Deployment

```bash
ring deployment delete nginx-demo
```

### Verification

```bash
# Verify the deployment has been deleted
ring deployment list

# Verify containers have been stopped
docker ps --filter "label=ring.deployment=nginx-demo"
```

## Summary

🎉 **Congratulations!** You have:

- ✅ Deployed your first application with Ring
- ✅ Learned to use YAML files and the REST API
- ✅ Tested automatic scaling
- ✅ Explored monitoring and logs
- ✅ Cleaned up your resources

## Next Steps

Now that you master the basics, you can:

- Explore more [advanced examples](../examples.md)
- Learn complete [deployment management](managing-deployments.md)
- Discover the [REST API](../api-reference.md)

## Best Practices

!!! tip "Tips"
    - Use **namespaces** to organize your environments
    - Always specify an explicit **image version** in production
    - Add descriptive **labels** to facilitate management
    - Use **secrets** for sensitive information
    - Test your deployments in a development environment first