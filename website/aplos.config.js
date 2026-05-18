const fs = require('node:fs');
const path = require('node:path');

function walkMdFiles(dir, prefix = '') {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const abs = path.join(dir, entry.name);
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      out.push(...walkMdFiles(abs, rel));
    } else if (entry.isFile() && entry.name.endsWith('.md') && entry.name.toLowerCase() !== 'readme.md') {
      out.push(rel.replace(/\.md$/, '').replace(/\/?index$/, ''));
    }
  }
  return out;
}

module.exports = {
  reactStrictMode: true,
  server: {
    port: 3001,
  },
  routes: [
    {
      source: '/documentation/[...path]',
      paths: () => {
        const docsDir = path.resolve(__dirname, '../documentation');
        if (!fs.existsSync(docsDir)) {
          return [];
        }
        return walkMdFiles(docsDir).map((slug) =>
          slug === '' ? '/documentation' : `/documentation/${slug}`
        );
      },
    },
  ],
  head: {
    defaultTitle: 'Ring - Lightweight Workload Orchestrator',
    titleTemplate: '%s | Ring',
    meta: [
      { name: 'description', content: 'A lightweight workload orchestrator. Deploy and manage workloads with declarative YAML, a REST API, and zero complexity.' },
      { name: 'viewport', content: 'width=device-width, initial-scale=1' },
      { property: 'og:title', content: 'Ring - Lightweight Workload Orchestrator' },
      { property: 'og:description', content: 'Deploy and manage workloads with declarative YAML, a REST API, and zero complexity.' },
      { property: 'og:type', content: 'website' },
    ],
    link: [
      { rel: 'icon', type: 'image/svg+xml', href: '/images/ring-logo.svg' },
    ],
  },
};
