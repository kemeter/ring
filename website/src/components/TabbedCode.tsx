import { useId, useState } from 'react';
import CodeBlock from './CodeBlock';

export interface CodePane {
  label: string;
  code: string;
  language: string;
}

// Shared tab UI (bar + panel + active state) used by both CodeTabs (explicit
// `:::code-tabs` directive) and YamlJsonTabs (auto YAML→JSON). A single pane
// degrades to a plain CodeBlock so callers don't special-case it.
export default function TabbedCode({ panes }: { panes: CodePane[] }) {
  const groupId = useId();
  const [active, setActive] = useState(0);

  if (panes.length === 0) return null;
  if (panes.length === 1) {
    return <CodeBlock code={panes[0].code} language={panes[0].language} />;
  }

  const current = panes[Math.min(active, panes.length - 1)];

  return (
    <div className="code-tabs">
      <div className="code-tabs-bar" role="tablist" aria-label="Code examples">
        {panes.map((pane, i) => (
          <button
            key={`${groupId}-${i}`}
            type="button"
            role="tab"
            id={`${groupId}-tab-${i}`}
            aria-selected={i === active}
            aria-controls={`${groupId}-panel-${i}`}
            className={`code-tabs-tab ${i === active ? 'is-active' : ''}`}
            onClick={() => setActive(i)}
          >
            {pane.label}
          </button>
        ))}
      </div>
      <div
        role="tabpanel"
        id={`${groupId}-panel-${active}`}
        aria-labelledby={`${groupId}-tab-${active}`}
      >
        <CodeBlock code={current.code} language={current.language} />
      </div>
    </div>
  );
}
