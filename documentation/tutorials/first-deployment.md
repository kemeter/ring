# Your first deployment

You'll deploy nginx with Ring, reach it on `localhost:8080`, scale it to three replicas, and clean up. Time required: about 10 minutes.

This tutorial assumes Ring is installed, the server is running, and you've logged in. If not, complete [Install and run Ring](/documentation/tutorials/install-and-run) first.

## 1. Write the manifest

Create a file called `nginx.yaml` anywhere on disk:

```yaml
deployments:
  nginx-demo:
    name: nginx-demo
    namespace: default
    runtime: docker
    image: "nginx:1.25"
    replicas: 1
    ports:
      - { published: 8080, target: 80 }
```

What each field does:

- `name`, `namespace`: together they identify this deployment. Same name in a different namespace is a different deployment.
- `runtime: docker` uses the Docker runtime
- `image: "nginx:1.25"` is the Docker image to run. Pin to a specific tag in production
- `replicas: 1` means one running container
- `ports` publishes container port 80 on host port 8080

## 2. Apply

```bash
ring apply -f nginx.yaml
```

Expected output:

```
Processing deployment 'nginx-demo'
Deployment 'nginx-demo' created

Summary:
  Successful: 1
```

`ring apply` only **submits** the desired state. The scheduler then picks it up on its next tick (default: every 10 seconds), pulls the image if it isn't cached, and starts the container.

## 3. Verify

List your deployments:

```bash
ring deployment list
```

You should see something like:

```text
ID                                    NAME         STATUS    REPLICAS
6a3f8d2c-9b41-4c8e-bf3d-7e2a5d18f9c0  nginx-demo   running   1/1
```

Hit the running container:

```bash
curl http://localhost:8080
```

You should see the nginx welcome page HTML. If you get a connection refused, the scheduler hasn't ticked yet, so wait 10 seconds.

> **Important: IDs vs names.** Most `ring deployment` subcommands (`events`, `logs`, `delete`, …) take the deployment's **ID** (the UUID in the first column above), not its name. Copy the ID from `ring deployment list` and use it in the commands below. We'll write `<ID>` as a placeholder.

To watch the lifecycle as it happens:

```bash
ring deployment events <ID> --follow
```

## 4. Scale to three replicas

Open `nginx.yaml` and change `replicas: 1` to `replicas: 3`:

```yaml
deployments:
  nginx-demo:
    name: nginx-demo
    namespace: default
    runtime: docker
    image: "nginx:1.25"
    replicas: 3                # was 1
    ports:
      - { published: 8080, target: 80 }
```

Re-apply:

```bash
ring apply -f nginx.yaml
```

> **Note**: with `replicas > 1` and a `ports:` entry, only one container can actually bind host port 8080. Docker rejects the others with `bind: address already in use`. That's expected; for production, you'd put a reverse proxy in front. For this tutorial, we'll accept the noise; the test below still works.

Wait one scheduler tick, then check:

```bash
ring deployment list
docker ps --filter "label=ring_deployment" --filter "name=default_nginx-demo"
```

You should see three containers, one for each replica (one will be in a restart loop due to the port conflict; that's fine for the tutorial).

## 5. Watch the logs

```bash
ring deployment logs <ID>
```

You'll see the nginx access logs from all replicas combined. To stream:

```bash
ring deployment logs <ID> --follow
```

## 6. Clean up

Delete the deployment (using its ID):

```bash
ring deployment delete <ID>
```

Confirm the containers are gone:

```bash
ring deployment list
docker ps --filter "label=ring_deployment"
```

Both should be empty.

## What you learned

- A manifest in YAML describes what you want; Ring reconciles toward it on every tick
- `ring apply` submits, the scheduler executes
- Status flows: `creating` → `running` (workers stay there) or `completed` / `failed` (jobs)
- Deleting a deployment tears down its containers
- Replicas with a single published port hit Docker's port conflict, since real production routing belongs to a reverse proxy

## What's next

You now know Ring's core workflow. Pick a how-to guide for whatever feature you need next:

- [Deploy with secrets](/documentation/how-to/deploy-with-secrets): encrypted env-var injection
- [Configure health checks](/documentation/how-to/configure-health-checks): TCP / HTTP / command probes
- [Perform a rolling update](/documentation/how-to/perform-rolling-update): zero-downtime deploys
- [Run a job](/documentation/how-to/run-a-job): one-shot tasks like migrations
