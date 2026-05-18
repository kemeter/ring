import { useMemo } from 'react';
import yaml from 'js-yaml';
import CodeBlock from './CodeBlock';
import TabbedCode from './TabbedCode';

// Every ```yaml block in the docs renders as two tabs: the YAML source as
// written, and the same data serialized to JSON — generated client-side so
// the docs stay single-source (no duplicated JSON to keep in sync).
//
// If the YAML doesn't parse, or parses to a scalar (a fragment, not a
// document/object), there's no meaningful JSON twin: fall back to a plain
// YAML CodeBlock instead of showing a broken/empty JSON tab.
export default function YamlJsonTabs({ code }: { code: string }) {
  const json = useMemo(() => {
    try {
      const parsed = yaml.load(code);
      if (parsed === null || typeof parsed !== 'object') return null;
      return JSON.stringify(parsed, null, 2);
    } catch {
      return null;
    }
  }, [code]);

  if (json === null) {
    return <CodeBlock code={code} language="yaml" />;
  }

  return (
    <TabbedCode
      panes={[
        { label: 'YAML', code, language: 'yaml' },
        { label: 'JSON', code: json, language: 'json' },
      ]}
    />
  );
}
