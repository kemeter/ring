# Examples

Real-world Ring manifests for common use cases. Every example uses the formats Ring's deserializer actually accepts: structured volume objects, map-style labels, `secretRef` for sensitive environment values.

## Simple web apps

### Static site with Nginx

```yaml
# static-website.yaml
deployments:
  static-site:
    name: static-site
    namespace: web
    runtime: docker
    image: "nginx:1.21-alpine"
    replicas: 2

    volumes:
      - type: bind
        source: /var/www/html
        destination: /usr/share/nginx/html
        driver: local
        permission: ro

    labels:
      app: static-website
      "traefik.enable": "true"
      "traefik.http.routers.static.rule": "Host(`www.example.com`)"
```

### Node.js API

```yaml
# nodejs-app.yaml
deployments:
  nodejs-api:
    name: nodejs-api
    namespace: backend
    runtime: docker
    image: "node:18-alpine"
    replicas: 3

    environment:
      NODE_ENV: "production"
      PORT: "3000"
      DATABASE_URL:
        secretRef: "database-url"
      JWT_SECRET:
        secretRef: "jwt-secret"

    volumes:
      - type: bind
        source: /opt/nodejs-api
        destination: /usr/src/app
        driver: local
        permission: ro

    command: ["npm", "start"]

    labels:
      app: nodejs-api
      tier: backend
```

## Databases

### PostgreSQL with persistence

```yaml
# postgres.yaml
deployments:
  postgres-db:
    name: postgres-db
    namespace: database
    runtime: docker
    image: "postgres:13"
    replicas: 1

    environment:
      POSTGRES_DB: "myapp"
      POSTGRES_USER: "appuser"
      POSTGRES_PASSWORD:
        secretRef: "postgres-password"
      PGDATA: "/var/lib/postgresql/data/pgdata"

    volumes:
      - type: bind
        source: /var/lib/ring/postgres/data
        destination: /var/lib/postgresql/data
        driver: local
        permission: rw
      - type: bind
        source: /var/lib/ring/postgres/backup
        destination: /backup
        driver: local
        permission: rw

    labels:
      app: postgres
      type: database
      backup: enabled
```

Create the secret before applying:

```bash
ring secret create postgres-password -n database -v "secure-pg-password"
```

### Redis cache

```yaml
# redis.yaml
deployments:
  redis-cache:
    name: redis-cache
    namespace: cache
    runtime: docker
    image: "redis:7-alpine"
    replicas: 1

    environment:
      REDIS_PASSWORD:
        secretRef: "redis-password"

    volumes:
      - type: bind
        source: /var/lib/ring/redis
        destination: /data
        driver: local
        permission: rw

    command: ["redis-server", "--requirepass", "$REDIS_PASSWORD", "--appendonly", "yes"]

    labels:
      app: redis
      type: cache
```

> The `command:` interpolation `$REDIS_PASSWORD` is resolved at apply time from the **shell** environment of `ring apply`, not from the deployment's environment. To inject a Ring-managed secret into the command, run `ring apply` in a shell where `REDIS_PASSWORD` is set, or use `--env-file`.

## Configs and named volumes

Ring supports three volume types: `bind` (host path), `volume` (named Docker volume), and `config` (mount a file from a `ring config` entry).

### Named volume

```yaml
deployments:
  app:
    name: app
    namespace: default
    runtime: docker
    image: "myapp:latest"
    replicas: 1

    volumes:
      - type: volume
        source: app-data           # named Docker volume
        destination: /data
        driver: local              # or "nfs"
        permission: rw
```

### Config-mounted file

First create the config:

```bash
ring config create nginx-config -n web -f ./custom.conf
```

Then mount it:

```yaml
deployments:
  nginx:
    name: nginx
    namespace: web
    runtime: docker
    image: "nginx:1.21"
    replicas: 1

    volumes:
      - type: config
        source: nginx-config       # config name in the same namespace
        destination: /etc/nginx/conf.d/custom.conf
        driver: local
        permission: ro
```

## WordPress + MySQL

```yaml
# wordpress.yaml
deployments:
  wordpress:
    name: wordpress
    namespace: cms
    runtime: docker
    image: "wordpress:latest"
    replicas: 2

    environment:
      WORDPRESS_DB_HOST: "mysql.database:3306"
      WORDPRESS_DB_NAME: "wordpress"
      WORDPRESS_DB_USER: "wp_user"
      WORDPRESS_DB_PASSWORD:
        secretRef: "wp-db-password"

    volumes:
      - type: bind
        source: /var/lib/ring/wordpress/wp-content
        destination: /var/www/html/wp-content
        driver: local
        permission: rw

    labels:
      app: wordpress
      type: cms

  mysql:
    name: mysql
    namespace: database
    runtime: docker
    image: "mysql:8.0"
    replicas: 1

    environment:
      MYSQL_ROOT_PASSWORD:
        secretRef: "mysql-root-password"
      MYSQL_DATABASE: "wordpress"
      MYSQL_USER: "wp_user"
      MYSQL_PASSWORD:
        secretRef: "wp-db-password"

    volumes:
      - type: bind
        source: /var/lib/ring/mysql
        destination: /var/lib/mysql
        driver: local
        permission: rw

    labels:
      app: mysql
      type: database
```

> Cross-namespace networking is not automatic in Ring — `mysql.database` resolves only if both deployments share a namespace, or if MySQL is reachable through external routing. For the example above, put both deployments in the same namespace.

## Microservices

```yaml
# microservices.yaml
deployments:
  frontend:
    name: frontend
    namespace: microservices
    runtime: docker
    image: "nginx:alpine"
    replicas: 2

    volumes:
      - type: bind
        source: /opt/frontend/dist
        destination: /usr/share/nginx/html
        driver: local
        permission: ro
      - type: bind
        source: /opt/frontend/nginx.conf
        destination: /etc/nginx/nginx.conf
        driver: local
        permission: ro

    labels:
      app: frontend
      tier: presentation

  api-gateway:
    name: api-gateway
    namespace: microservices
    runtime: docker
    image: "nginx:alpine"
    replicas: 2

    volumes:
      - type: bind
        source: /opt/gateway/nginx.conf
        destination: /etc/nginx/nginx.conf
        driver: local
        permission: ro

    labels:
      app: api-gateway
      tier: gateway

  user-service:
    name: user-service
    namespace: microservices
    runtime: docker
    image: "mycompany/user-service:v1.2.0"
    replicas: 3

    environment:
      DATABASE_URL:
        secretRef: "user-db-url"
      JWT_SECRET:
        secretRef: "jwt-secret"
      SERVICE_PORT: "8001"

    labels:
      app: user-service
      tier: backend
      service: users

  order-service:
    name: order-service
    namespace: microservices
    runtime: docker
    image: "mycompany/order-service:v1.1.5"
    replicas: 2

    environment:
      DATABASE_URL:
        secretRef: "order-db-url"
      REDIS_URL: "redis://redis-cache:6379"
      SERVICE_PORT: "8002"

    labels:
      app: order-service
      tier: backend
      service: orders
```

## Private image registry

```yaml
# enterprise-app.yaml
deployments:
  internal-app:
    name: internal-app
    namespace: enterprise
    runtime: docker
    image: "registry.company.com/internal/myapp:v2.1.0"
    replicas: 5

    config:
      server: "registry.company.com"
      username: "registry-user"
      password: "$REGISTRY_PASSWORD"
      image_pull_policy: "Always"

    environment:
      APP_ENV: "production"
      LOG_LEVEL: "info"
      DB_HOST: "db.company.internal"
      DB_NAME: "production_db"
      DB_USER: "app_user"
      DB_PASSWORD:
        secretRef: "app-db-password"

    labels:
      app: internal-app
      environment: production
      team: platform
```

`$REGISTRY_PASSWORD` is interpolated by `ring apply` from your shell environment (or from a file passed with `--env-file`).

## Multiple environments in one file

```yaml
# multi-env.yaml
namespaces:
  development:
    name: development
  staging:
    name: staging
  production:
    name: production

deployments:
  dev-api:
    name: api
    namespace: development
    runtime: docker
    image: "myapp:dev"
    replicas: 1

    environment:
      NODE_ENV: "development"
      LOG_LEVEL: "debug"

    config:
      image_pull_policy: "Always"

    labels:
      app: api
      environment: development

  staging-api:
    name: api
    namespace: staging
    runtime: docker
    image: "myapp:staging"
    replicas: 2

    environment:
      NODE_ENV: "staging"
      LOG_LEVEL: "info"
      DATABASE_URL:
        secretRef: "staging-database-url"

    config:
      image_pull_policy: "IfNotPresent"

    labels:
      app: api
      environment: staging

  prod-api:
    name: api
    namespace: production
    runtime: docker
    image: "myapp:v1.5.2"
    replicas: 5

    environment:
      NODE_ENV: "production"
      LOG_LEVEL: "warn"
      DATABASE_URL:
        secretRef: "production-database-url"

    config:
      image_pull_policy: "IfNotPresent"

    labels:
      app: api
      environment: production
      criticality: high
```

## Workers vs jobs

### Worker (default)

Long-running services with replica management.

```yaml
deployments:
  web-server:
    name: web-server
    namespace: default
    runtime: docker
    kind: worker            # default; can be omitted
    image: "nginx:latest"
    replicas: 3
```

### Job

One-shot task. Always one instance, no respawn after exit.

```yaml
deployments:
  migration:
    name: migration
    namespace: default
    runtime: docker
    kind: job
    image: "myapp:latest"
    replicas: 1
    command: ["npm", "run", "migrate"]
```

## Mixed workers and a scheduler

```yaml
# workers.yaml
deployments:
  web-api:
    name: web-api
    namespace: workers
    runtime: docker
    image: "myapp:latest"
    replicas: 3

    environment:
      ROLE: "web"
      PORT: "8000"
      REDIS_URL: "redis://redis-cache:6379"
      DATABASE_URL:
        secretRef: "database-url"

    labels:
      app: myapp
      component: web

  background-worker:
    name: background-worker
    namespace: workers
    runtime: docker
    image: "myapp:latest"
    replicas: 2

    environment:
      ROLE: "worker"
      REDIS_URL: "redis://redis-cache:6379"
      DATABASE_URL:
        secretRef: "database-url"
      WORKER_CONCURRENCY: "4"

    command: ["python", "worker.py"]

    labels:
      app: myapp
      component: worker

  scheduler:
    name: scheduler
    namespace: workers
    runtime: docker
    image: "myapp:latest"
    replicas: 1

    environment:
      ROLE: "scheduler"
      REDIS_URL: "redis://redis-cache:6379"
      DATABASE_URL:
        secretRef: "database-url"

    command: ["python", "scheduler.py"]

    labels:
      app: myapp
      component: scheduler
```

## Monitoring stack

```yaml
# monitoring.yaml
deployments:
  prometheus:
    name: prometheus
    namespace: monitoring
    runtime: docker
    image: "prom/prometheus:latest"
    replicas: 1

    volumes:
      - type: bind
        source: /opt/monitoring/prometheus.yml
        destination: /etc/prometheus/prometheus.yml
        driver: local
        permission: ro

  grafana:
    name: grafana
    namespace: monitoring
    runtime: docker
    image: "grafana/grafana:latest"
    replicas: 1

    environment:
      GF_SECURITY_ADMIN_PASSWORD:
        secretRef: "grafana-admin-password"
```

Ring itself exposes a health endpoint:

```bash
curl http://localhost:3030/healthz
# {"state":"UP"}
```

## Patterns

### Labels for service discovery

Labels are a key/value map. They flow into Docker container labels, so any tool that reads container labels (Traefik, Prometheus relabel, custom scripts) can use them.

```yaml
deployments:
  app:
    labels:
      app: myapp
      component: frontend
      version: v1.2.3
      environment: production
      team: web
      monitoring: prometheus
      backup: enabled
      criticality: high
      "traefik.enable": "true"
      "traefik.http.routers.app.rule": "Host(`app.example.com`)"
```

Quote keys that contain dots — YAML treats them as strings only when quoted.

### Secrets

Sensitive values should be created as Ring secrets and referenced by name. They are stored AES-256-GCM-encrypted and decrypted only at deployment time.

```yaml
deployments:
  secure-app:
    namespace: production
    environment:
      NODE_ENV: "production"
      LOG_LEVEL: "info"
      PORT: "3000"

      DATABASE_PASSWORD:
        secretRef: "database-password"
      JWT_SECRET:
        secretRef: "jwt-secret"
      API_KEY:
        secretRef: "external-api-key"
```

Create them once:

```bash
ring secret create database-password -n production -v "$DATABASE_PASSWORD"
ring secret create jwt-secret -n production -v "$JWT_SECRET"
ring secret create external-api-key -n production -v "$EXTERNAL_API_KEY"
```

The server must be started with `RING_SECRET_KEY` set; without it, all secret operations return `500 Internal Server Error`.

### Health checks for rolling updates

Adding a health check is what unlocks the zero-downtime rolling update path. See [managing deployments](getting-started/managing-deployments.md#rolling-update-zero-downtime) for the full lifecycle.

```yaml
deployments:
  app:
    name: app
    namespace: production
    runtime: docker
    image: "myapp:v1.2.3"
    replicas: 3

    health_checks:
      - type: http
        url: "http://localhost:8080/health"
        interval: "10s"
        timeout: "5s"
        threshold: 3
        on_failure: restart
```

Health-check duration suffixes: `ms` and `s`. Larger suffixes (`m`, `h`) are not parsed.
