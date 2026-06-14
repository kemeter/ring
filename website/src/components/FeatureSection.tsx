import CodeBlock from './CodeBlock';
import TabbedCode, { CodePane } from './TabbedCode';
import '@/styles/components/feature-section.css';

interface FeatureSectionProps {
  badge?: string;
  title: string;
  description: string;
  code?: string;
  language?: string;
  filename?: string;
  panes?: CodePane[];
  reversed?: boolean;
}

export default function FeatureSection({
  badge,
  title,
  description,
  code,
  language = 'yaml',
  filename,
  panes,
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
          {panes && panes.length > 0 ? (
            <TabbedCode panes={panes} />
          ) : (
            <CodeBlock code={code ?? ''} language={language} filename={filename} />
          )}
        </div>
      </div>
    </section>
  );
}
