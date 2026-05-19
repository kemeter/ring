import { useMemo } from 'react';
import yaml from 'js-yaml';
import CodeBlock from './CodeBlock';
import TabbedCode from './TabbedCode';

// Every ```yaml block in the docs renders as two tabs: the YAML source as
// written, and a JSON twin generated client-side so the docs stay
// single-source (no duplicated JSON to keep in sync).
//
// The YAML config file is NOT the API payload. A config file wraps
// deployments under a `deployments:` key, each one named:
//
//   deployments:
//     web-app:
//       name: web-app
//       image: nginx:latest
//
// but `POST /deployments` takes a single deployment object as its body —
// without the `deployments:` envelope and without the `web-app:` key. So
// when a block is a deployments config, the JSON tab shows the actual API
// request body (one block per deployment). Any other YAML (health_checks,
// environment fragments, CI files, …) keeps the literal JSON twin, or
// falls back to a plain YAML block when there's no meaningful JSON.

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === 'object' && !Array.isArray(v);
}

// Returns the API request bodies if `parsed` is a deployments config
// (`deployments:` mapping of named deployment objects), else null.
function deploymentBodies(parsed: unknown): string | null {
  if (!isPlainObject(parsed)) return null;

  const keys = Object.keys(parsed);
  if (keys.length !== 1 || keys[0] !== 'deployments') return null;

  const deployments = parsed.deployments;
  if (!isPlainObject(deployments)) return null;

  const entries = Object.values(deployments);
  if (entries.length === 0 || !entries.every(isPlainObject)) return null;

  // One request per deployment; the named key is dropped — it's the
  // config-file handle, not part of the wire payload.
  return entries
    .map(
      (body) =>
        `POST /deployments\n${JSON.stringify(body, null, 2)}`,
    )
    .join('\n\n');
}

export default function YamlJsonTabs({ code }: { code: string }) {
  const json = useMemo(() => {
    try {
      const parsed = yaml.load(code);
      if (parsed === null || typeof parsed !== 'object') return null;

      const apiBody = deploymentBodies(parsed);
      if (apiBody !== null) return apiBody;

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
