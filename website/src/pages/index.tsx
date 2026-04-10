import Head from 'aplos/head';
import Hero from '@/components/Hero';
import FeatureSection from '@/components/FeatureSection';
import { Link } from 'aplos/navigation';

const DECLARATIVE_CODE = `deployments:
  web-app:
    name: web-app
    namespace: production
    runtime: docker
    image: "nginx:1.21"
    replicas: 3

    environment:
      NODE_ENV: "production"

    health_checks:
      - type: http
        url: "http://localhost:80/"
        interval: "10s"
        timeout: "5s"`;

const API_CODE = `$ curl -X POST http://localhost:3030/deployments \\
  -H "Authorization: Bearer $TOKEN" \\
  -H "Content-Type: application/json" \\
  -d '{
    "name": "web-app",
    "namespace": "production",
    "runtime": "docker",
    "image": "nginx:1.21",
    "replicas": 3
  }'`;

const SECRET_CODE = `deployments:
  secure-app:
    name: secure-app
    namespace: production
    image: "myapp:latest"

    environment:
      DATABASE_HOST: "db.production"
      DATABASE_PASSWORD:
        secretRef: "database-password"
      API_KEY:
        secretRef: "api-key"`;

const HEALTH_CODE = `deployments:
  web-app:
    name: web-app
    image: "nginx:1.21"
    replicas: 3

    health_checks:
      - type: http
        url: "http://localhost:80/healthz"
        interval: "10s"
        timeout: "5s"
        threshold: 3
        on_failure: restart`;

const NAMESPACE_CODE = `namespaces:
  production:
    name: production
  staging:
    name: staging

deployments:
  prod-app:
    name: my-app
    namespace: production
    image: "myapp:v1.2.3"
    replicas: 5

  staging-app:
    name: my-app
    namespace: staging
    image: "myapp:staging"
    replicas: 2`;

export default function HomePage() {
  return (
    <>
      <Head>
        <title>Ring - Lightweight Container Orchestrator</title>
      </Head>

      <Hero />

      <FeatureSection
        badge="Declarative"
        title="Define once, deploy anywhere"
        description="Describe your entire deployment in a simple YAML file. Ring handles container creation, networking, health checks, and scaling automatically."
        code={DECLARATIVE_CODE}
        language="yaml"
        filename="app.yaml"
      />

      <FeatureSection
        badge="API-First"
        title="Complete REST API"
        description="Every operation available through the CLI is also available via the REST API. Integrate Ring into your CI/CD pipelines, scripts, and automation workflows."
        code={API_CODE}
        language="bash"
        filename="Terminal"
        reversed
      />

      <FeatureSection
        badge="Security"
        title="Built-in secret management"
        description="Store sensitive values encrypted at rest with AES-256-GCM. Reference secrets in your deployments and they are decrypted and injected at deployment time."
        code={SECRET_CODE}
        language="yaml"
        filename="secure-app.yaml"
      />

      <FeatureSection
        badge="Reliability"
        title="Health checks & rolling updates"
        description="Configure health checks and Ring performs zero-downtime rolling updates automatically. If a new container fails, the rollout stops and old containers stay running."
        code={HEALTH_CODE}
        language="yaml"
        filename="health-check.yaml"
        reversed
      />

      <FeatureSection
        badge="Isolation"
        title="Namespace isolation"
        description="Organize deployments by environment with namespaces. Each namespace gets its own isolated Docker network. Deploy the same application across multiple environments."
        code={NAMESPACE_CODE}
        language="yaml"
        filename="multi-env.yaml"
      />

      <section className="section" style={{ textAlign: 'center' }}>
        <div className="container">
          <h2 style={{ marginBottom: '1rem' }}>Ready to deploy?</h2>
          <p style={{ marginBottom: '2rem', maxWidth: '500px', margin: '0 auto 2rem' }}>
            Get started in minutes. Deploy your first container with Ring.
          </p>
          <Link to="/documentation" className="btn-primary" style={{
            display: 'inline-flex',
            alignItems: 'center',
            gap: '0.5rem',
            padding: '0.75rem 1.5rem',
            background: 'var(--color-accent)',
            color: '#fff',
            fontWeight: 600,
            fontSize: '0.875rem',
            borderRadius: '8px',
            textDecoration: 'none',
          }}>
            Get started
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M5 12h14M12 5l7 7-7 7" />
            </svg>
          </Link>
        </div>
      </section>
    </>
  );
}
