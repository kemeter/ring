# Practical Examples

This page presents concrete examples of using Ring for different types of applications and use cases.

## Simple Web Applications

### Static Web Server with Nginx

```yaml title="static-website.yaml"
deployments:
  static-site:
    name: static-site
    namespace: web
    runtime: docker
    image: "nginx:1.21-alpine"
    replicas: 2

    # Static content mounting
    volumes:
      - "/var/www/html:/usr/share/nginx/html:ro"

    labels:
      - "traefik.enable=true"
      - "traefik.http.routers.static.rule=Host(`www.example.com`)"
      - "app=static-website"
```

### Node.js Application

```yaml title="nodejs-app.yaml"
deployments:
  nodejs-api:
    name: nodejs-api
    namespace: backend
    runtime: docker
    image: "node:16-alpine"
    replicas: 3

    # Environment variables
    secrets:
      NODE_ENV: "production"
      PORT: "3000"
      DATABASE_URL: "$DATABASE_URL"
      JWT_SECRET: "$JWT_SECRET"

    # Source code mounting (for development)
    volumes:
      - "./app:/usr/src/app"
      - "/usr/src/app/node_modules"  # Anonymous volume for node_modules

    # Custom command
    command: ["npm", "start"]

    labels:
      - "app=nodejs-api"
      - "tier=backend"
```

## Databases

### PostgreSQL with Persistence

```yaml title="postgres.yaml"
deployments:
  postgres-db:
    name: postgres-db
    namespace: database
    runtime: docker
    image: "postgres:13"
    replicas: 1

    # PostgreSQL configuration
    secrets:
      POSTGRES_DB: "myapp"
      POSTGRES_USER: "appuser"
      POSTGRES_PASSWORD: "$POSTGRES_PASSWORD"
      PGDATA: "/var/lib/postgresql/data/pgdata"

    # Persistent storage
    volumes:
      - "/var/lib/ring/postgres/data:/var/lib/postgresql/data"
      - "/var/lib/ring/postgres/backup:/backup"

    labels:
      - "app=postgres"
      - "type=database"
      - "backup=enabled"
```

### Redis Cache

```yaml title="redis.yaml"
deployments:
  redis-cache:
    name: redis-cache
    namespace: cache
    runtime: docker
    image: "redis:6-alpine"
    replicas: 1

    # Redis configuration
    secrets:
      REDIS_PASSWORD: "$REDIS_PASSWORD"

    # Redis persistence
    volumes:
      - "/var/lib/ring/redis:/data"

    # Custom Redis configuration
    command: ["redis-server", "--requirepass", "$REDIS_PASSWORD", "--appendonly", "yes"]

    labels:
      - "app=redis"
      - "type=cache"
```

## Applications with Complex Configuration

### WordPress Application

```yaml title="wordpress.yaml"
configs:
  wp-config:
    namespace: cms
    name: "wordpress-config"
    data: |
      {
        "wp-config.php": "<?php\ndefine('WP_DEBUG', false);\ndefine('WP_CACHE', true);\n?>"
      }

deployments:
  wordpress:
    name: wordpress
    namespace: cms
    runtime: docker
    image: "wordpress:latest"
    replicas: 2

    secrets:
      WORDPRESS_DB_HOST: "mysql.database:3306"
      WORDPRESS_DB_NAME: "wordpress"
      WORDPRESS_DB_USER: "wp_user"
      WORDPRESS_DB_PASSWORD: "$WP_DB_PASSWORD"

    volumes:
      # Persistent WordPress content
      - "/var/lib/ring/wordpress/wp-content:/var/www/html/wp-content"
      # Custom configuration
      - type: config
        source: wordpress-config
        key: "wp-config.php"
        destination: /var/www/html/wp-config-custom.php
        driver: local

    labels:
      - "app=wordpress"
      - "type=cms"
      - "traefik.enable=true"
      - "traefik.http.routers.wp.rule=Host(`blog.example.com`)"

  mysql:
    name: mysql
    namespace: database
    runtime: docker
    image: "mysql:8.0"
    replicas: 1

    secrets:
      MYSQL_ROOT_PASSWORD: "$MYSQL_ROOT_PASSWORD"
      MYSQL_DATABASE: "wordpress"
      MYSQL_USER: "wp_user"
      MYSQL_PASSWORD: "$WP_DB_PASSWORD"

    volumes:
      - "/var/lib/ring/mysql:/var/lib/mysql"

    labels:
      - "app=mysql"
      - "type=database"
```

## Microservices

### Complete Microservices Architecture

```yaml title="microservices.yaml"
# Frontend
deployments:
  frontend:
    name: frontend
    namespace: microservices
    runtime: docker
    image: "nginx:alpine"
    replicas: 2

    volumes:
      - "./frontend/dist:/usr/share/nginx/html:ro"
      - "./frontend/nginx.conf:/etc/nginx/nginx.conf:ro"

    labels:
      - "app=frontend"
      - "tier=presentation"
      - "traefik.enable=true"
      - "traefik.http.routers.frontend.rule=Host(`app.example.com`)"

  # API Gateway
  api-gateway:
    name: api-gateway
    namespace: microservices
    runtime: docker
    image: "nginx:alpine"
    replicas: 2

    volumes:
      - "./gateway/nginx.conf:/etc/nginx/nginx.conf:ro"

    labels:
      - "app=api-gateway"
      - "tier=gateway"

  # User service
  user-service:
    name: user-service
    namespace: microservices
    runtime: docker
    image: "mycompany/user-service:v1.2.0"
    replicas: 3

    secrets:
      DATABASE_URL: "$USER_DB_URL"
      JWT_SECRET: "$JWT_SECRET"
      SERVICE_PORT: "8001"

    labels:
      - "app=user-service"
      - "tier=backend"
      - "service=users"

  # Order service
  order-service:
    name: order-service
    namespace: microservices
    runtime: docker
    image: "mycompany/order-service:v1.1.5"
    replicas: 2

    secrets:
      DATABASE_URL: "$ORDER_DB_URL"
      REDIS_URL: "redis://redis.cache:6379"
      SERVICE_PORT: "8002"

    labels:
      - "app=order-service"
      - "tier=backend"
      - "service=orders"

  # Notification service
  notification-service:
    name: notification-service
    namespace: microservices
    runtime: docker
    image: "mycompany/notification-service:v1.0.3"
    replicas: 1

    secrets:
      SMTP_HOST: "$SMTP_HOST"
      SMTP_USER: "$SMTP_USER"
      SMTP_PASSWORD: "$SMTP_PASSWORD"
      SLACK_WEBHOOK: "$SLACK_WEBHOOK"

    labels:
      - "app=notification-service"
      - "tier=backend"
      - "service=notifications"
```

## Applications with Private Registries

### Enterprise Application

```yaml title="enterprise-app.yaml"
deployments:
  internal-app:
    name: internal-app
    namespace: enterprise
    runtime: docker
    image: "registry.company.com/internal/myapp:v2.1.0"
    replicas: 5

    # Private registry authentication
    config:
      username: "registry-user"
      password: "$REGISTRY_PASSWORD"
      image_pull_policy: "Always"

    secrets:
      # Application configuration
      APP_ENV: "production"
      LOG_LEVEL: "info"

      # Database
      DB_HOST: "db.company.internal"
      DB_NAME: "production_db"
      DB_USER: "app_user"
      DB_PASSWORD: "$DB_PASSWORD"

      # External services
      LDAP_URL: "$LDAP_URL"
      LDAP_BIND_DN: "$LDAP_BIND_DN"
      LDAP_PASSWORD: "$LDAP_PASSWORD"

    labels:
      - "app=internal-app"
      - "environment=production"
      - "team=platform"
      - "compliance=required"
```

## Multiple Environments

### Environment-based Configuration

```yaml title="multi-env.yaml"
# Development
deployments:
  dev-api:
    name: api
    namespace: development
    runtime: docker
    image: "myapp:dev"
    replicas: 1

    secrets:
      NODE_ENV: "development"
      LOG_LEVEL: "debug"
      DATABASE_URL: "postgres://dev-db:5432/myapp_dev"
      REDIS_URL: "redis://dev-redis:6379"

    config:
      image_pull_policy: "Always"  # Always fetch latest

    labels:
      - "app=api"
      - "environment=development"

  # Test/Staging
  staging-api:
    name: api
    namespace: staging
    runtime: docker
    image: "myapp:staging"
    replicas: 2

    secrets:
      NODE_ENV: "staging"
      LOG_LEVEL: "info"
      DATABASE_URL: "$STAGING_DATABASE_URL"
      REDIS_URL: "$STAGING_REDIS_URL"

    config:
      image_pull_policy: "IfNotPresent"

    labels:
      - "app=api"
      - "environment=staging"

  # Production
  prod-api:
    name: api
    namespace: production
    runtime: docker
    image: "myapp:v1.5.2"  # Fixed version in production
    replicas: 5

    secrets:
      NODE_ENV: "production"
      LOG_LEVEL: "warn"
      DATABASE_URL: "$PRODUCTION_DATABASE_URL"
      REDIS_URL: "$PRODUCTION_REDIS_URL"

    config:
      image_pull_policy: "IfNotPresent"

    labels:
      - "app=api"
      - "environment=production"
      - "criticality=high"
```

## Deployment Types

Ring supports two types of deployments:

### Workers (default)
Services that run continuously with automatic restart and replica management.

```yaml
deployments:
  web-server:
    name: web-server
    runtime: docker
    kind: worker  # Optional (default)
    image: "nginx:latest"
    replicas: 3   # Scaling supported
```

### Jobs
Tasks that execute once and terminate. No automatic restart.

```yaml
deployments:
  migration:
    name: migration
    runtime: docker
    kind: job     # Required for jobs
    image: "myapp:latest"
    replicas: 1   # Always 1 for jobs
    command: ["npm", "run", "migrate"]
```

## Applications with Workers and Jobs

```yaml title="workers.yaml"
deployments:
  # Main API
  web-api:
    name: web-api
    namespace: workers
    runtime: docker
    image: "myapp:latest"
    replicas: 3

    secrets:
      ROLE: "web"
      PORT: "8000"
      REDIS_URL: "redis://redis.workers:6379"
      DATABASE_URL: "$DATABASE_URL"

    labels:
      - "app=myapp"
      - "component=web"

  # Workers for heavy tasks
  background-worker:
    name: background-worker
    namespace: workers
    runtime: docker
    image: "myapp:latest"
    replicas: 2

    secrets:
      ROLE: "worker"
      REDIS_URL: "redis://redis.workers:6379"
      DATABASE_URL: "$DATABASE_URL"
      WORKER_CONCURRENCY: "4"

    # Specific command for the worker
    command: ["python", "worker.py"]

    labels:
      - "app=myapp"
      - "component=worker"

  # Scheduler for periodic tasks
  scheduler:
    name: scheduler
    namespace: workers
    runtime: docker
    image: "myapp:latest"
    replicas: 1

    secrets:
      ROLE: "scheduler"
      REDIS_URL: "redis://redis.workers:6379"
      DATABASE_URL: "$DATABASE_URL"

    command: ["python", "scheduler.py"]

    labels:
      - "app=myapp"
      - "component=scheduler"
```

## Monitoring Tools

You can deploy your own monitoring tools with Ring:

```yaml title="monitoring.yaml"
deployments:
  # Prometheus to monitor your applications
  prometheus:
    name: prometheus
    namespace: monitoring
    runtime: docker
    image: "prom/prometheus:latest"
    replicas: 1
    volumes:
      - "./prometheus.yml:/etc/prometheus/prometheus.yml:ro"

  # Grafana for visualization
  grafana:
    name: grafana
    namespace: monitoring
    runtime: docker
    image: "grafana/grafana:latest"
    replicas: 1
    secrets:
      GF_SECURITY_ADMIN_PASSWORD: "$GRAFANA_PASSWORD"
```

!!! note "Ring Health Check"
    Ring exposes a simple health endpoint:
    ```bash
    curl http://localhost:3030/healthz
    # {"state":"UP"}
    ```


## Best Practices Illustrated

### Labels and Organization

```yaml
# ✅ Good example with organized labels
deployments:
  app:
    labels:
      # Application identification
      - "app=myapp"
      - "component=frontend"
      - "version=v1.2.3"

      # Environment and team
      - "environment=production"
      - "team=web"

      # Technical metadata
      - "monitoring=prometheus"
      - "backup=enabled"
      - "criticality=high"

      # Service discovery (e.g., Traefik)
      - "traefik.enable=true"
      - "traefik.http.routers.app.rule=Host(`app.example.com`)"
```

### Secret Management

```yaml
# ✅ Good example with secret separation
deployments:
  secure-app:
    secrets:
      # Application configuration (non-sensitive)
      NODE_ENV: "production"
      LOG_LEVEL: "info"
      PORT: "3000"

      # Secrets (from environment variables)
      DATABASE_PASSWORD: "$DATABASE_PASSWORD"
      JWT_SECRET: "$JWT_SECRET"
      API_KEY: "$EXTERNAL_API_KEY"
```

These examples cover most common use cases. Feel free to adapt them according to your specific needs!