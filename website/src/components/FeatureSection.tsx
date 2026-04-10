import CodeBlock from './CodeBlock';
import '@/styles/components/feature-section.css';

interface FeatureSectionProps {
  badge?: string;
  title: string;
  description: string;
  code: string;
  language?: string;
  filename?: string;
  reversed?: boolean;
}

export default function FeatureSection({
  badge,
  title,
  description,
  code,
  language = 'yaml',
  filename,
  reversed = false,
}: FeatureSectionProps) {
  return (
    <section className={`feature-section ${reversed ? 'reversed' : ''}`}>
      <div className="feature-inner">
        <div className="feature-text">
          {badge && <span className="feature-badge">{badge}</span>}
          <h2>{title}</h2>
          <p>{description}</p>
        </div>
        <div className="feature-code">
          <CodeBlock code={code} language={language} filename={filename} />
        </div>
      </div>
    </section>
  );
}
