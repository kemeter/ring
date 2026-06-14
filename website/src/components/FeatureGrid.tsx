import '@/styles/components/feature-grid.css';

interface GridFeature {
  number: string;
  title: string;
  description: string;
}

const FEATURES: GridFeature[] = [
  {
    number: '01',
    title: 'Pluggable runtimes',
    description: 'Run the same workload on Docker, Podman, containerd, Cloud Hypervisor, or Firecracker. One manifest, swap the runtime in a single line.',
  },
  {
    number: '02',
    title: 'Complete REST API',
    description: 'Every CLI operation is also an API call. Integrate Ring into your CI/CD pipelines and scripts.',
  },
  {
    number: '03',
    title: 'Encrypted secrets',
    description: 'Sensitive values encrypted at rest with AES-256-GCM, decrypted and injected at deployment time.',
  },
  {
    number: '04',
    title: 'Health checks & rolling updates',
    description: 'Zero-downtime rolling updates. If a new workload fails, the rollout stops automatically.',
  },
];

export default function FeatureGrid() {
  return (
    <section className="feature-grid-section">
      <div className="container">
        <div className="feature-grid-header">
          <span className="feature-grid-eyebrow">Features</span>
          <h2 className="feature-grid-title">Everything you need, nothing you don&apos;t.</h2>
          <p className="feature-grid-subtitle">
            A pragmatic alternative to Kubernetes for single-node deployments.
          </p>
        </div>
        <div className="feature-grid">
          {FEATURES.map((f) => (
            <article key={f.title} className="feature-grid-card">
              <div className="feature-grid-card-label">{f.number}</div>
              <h3 className="feature-grid-card-title">{f.title}</h3>
              <p className="feature-grid-card-description">{f.description}</p>
            </article>
          ))}
        </div>
      </div>
    </section>
  );
}
