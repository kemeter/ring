#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');

const SITE_URL = process.env.SITE_URL || 'https://kemeter.github.io/ring';
const distDir = path.resolve(__dirname, '..', 'public', 'dist');
const docsDir = path.resolve(__dirname, '..', '..', 'documentation');

function walkMdFiles(dir, prefix = '') {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const abs = path.join(dir, entry.name);
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      out.push(...walkMdFiles(abs, rel));
    } else if (
      entry.isFile() &&
      entry.name.endsWith('.md') &&
      entry.name.toLowerCase() !== 'readme.md'
    ) {
      out.push({
        slug: rel.replace(/\.md$/, '').replace(/\/?index$/, ''),
        lastmod: fs.statSync(abs).mtime.toISOString().slice(0, 10),
      });
    }
  }
  return out;
}

function buildUrls() {
  const today = new Date().toISOString().slice(0, 10);
  const urls = [{ loc: SITE_URL + '/', lastmod: today, priority: '1.0', changefreq: 'weekly' }];

  if (fs.existsSync(docsDir)) {
    urls.push({
      loc: `${SITE_URL}/documentation`,
      lastmod: today,
      priority: '0.9',
      changefreq: 'weekly',
    });
    for (const { slug, lastmod } of walkMdFiles(docsDir)) {
      if (!slug) continue;
      urls.push({
        loc: `${SITE_URL}/documentation/${slug}`,
        lastmod,
        priority: '0.7',
        changefreq: 'monthly',
      });
    }
  }
  return urls;
}

function writeSitemap(urls) {
  const body = urls
    .map(
      (u) =>
        `  <url>\n    <loc>${u.loc}</loc>\n    <lastmod>${u.lastmod}</lastmod>\n    <changefreq>${u.changefreq}</changefreq>\n    <priority>${u.priority}</priority>\n  </url>`,
    )
    .join('\n');
  const xml = `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${body}\n</urlset>\n`;
  fs.writeFileSync(path.join(distDir, 'sitemap.xml'), xml);
}

function writeRobots() {
  const robots = `User-agent: *\nAllow: /\n\nSitemap: ${SITE_URL}/sitemap.xml\n`;
  fs.writeFileSync(path.join(distDir, 'robots.txt'), robots);
}

function injectJsonLd(htmlPath, payload) {
  if (!fs.existsSync(htmlPath)) return;
  const html = fs.readFileSync(htmlPath, 'utf-8');
  if (html.includes('application/ld+json')) return;
  const script = `<script type="application/ld+json">${JSON.stringify(payload)}</script>`;
  const next = html.replace('</head>', `${script}</head>`);
  fs.writeFileSync(htmlPath, next);
}

function injectAllJsonLd() {
  const homeLd = {
    '@context': 'https://schema.org',
    '@type': 'SoftwareApplication',
    name: 'Ring',
    description:
      'Ring is a single-binary workload orchestrator: declarative YAML, REST API, SQLite state. A pragmatic alternative to Kubernetes for single-node deployments.',
    applicationCategory: 'DeveloperApplication',
    operatingSystem: 'Linux',
    url: SITE_URL,
    license: 'https://github.com/kemeter/ring/blob/main/LICENSE',
    codeRepository: 'https://github.com/kemeter/ring',
    programmingLanguage: 'Rust',
    offers: { '@type': 'Offer', price: '0', priceCurrency: 'USD' },
  };
  injectJsonLd(path.join(distDir, 'index.html'), homeLd);

  if (!fs.existsSync(docsDir)) return;
  for (const { slug } of walkMdFiles(docsDir)) {
    if (!slug) continue;
    const htmlPath = path.join(distDir, 'documentation', `${slug}.html`);
    const articleLd = {
      '@context': 'https://schema.org',
      '@type': 'TechArticle',
      headline: slug.split('/').pop().replace(/-/g, ' '),
      url: `${SITE_URL}/documentation/${slug}`,
      isPartOf: { '@type': 'WebSite', name: 'Ring Documentation', url: `${SITE_URL}/documentation` },
      publisher: { '@type': 'Organization', name: 'Kemeter', url: 'https://github.com/kemeter' },
    };
    injectJsonLd(htmlPath, articleLd);
  }
}

function main() {
  if (!fs.existsSync(distDir)) {
    console.error(`SEO: dist dir ${distDir} not found — run aplos build first.`);
    process.exit(1);
  }
  const urls = buildUrls();
  writeSitemap(urls);
  writeRobots();
  injectAllJsonLd();
  console.log(`SEO: wrote sitemap.xml (${urls.length} URLs), robots.txt, and JSON-LD to ${distDir}`);
}

main();
