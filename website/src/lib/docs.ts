export interface DocEntry {
  slug: string;
  content: string;
  title: string;
  segments: string[];
}

function extractTitle(markdown: string): string {
  for (const rawLine of markdown.split('\n')) {
    const line = rawLine.trim();
    if (line.startsWith('# ')) {
      return line.slice(2).trim();
    }
  }
  return '';
}

export function extractDescription(markdown: string, maxLength = 160): string {
  const lines = markdown.split('\n');
  let inCode = false;
  let paragraph = '';
  for (const rawLine of lines) {
    const line = rawLine.trim();
    if (line.startsWith('```')) {
      inCode = !inCode;
      continue;
    }
    if (inCode) continue;
    if (!line) {
      if (paragraph) break;
      continue;
    }
    if (line.startsWith('#')) continue;
    if (line.startsWith('>') || line.startsWith('|') || line.startsWith('-') || line.startsWith('*')) continue;
    paragraph += (paragraph ? ' ' : '') + line;
  }
  const cleaned = paragraph
    .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1')
    .replace(/`([^`]+)`/g, '$1')
    .replace(/\*\*([^*]+)\*\*/g, '$1')
    .replace(/\*([^*]+)\*/g, '$1')
    .replace(/\s+/g, ' ')
    .trim();
  if (cleaned.length <= maxLength) return cleaned;
  return cleaned.slice(0, maxLength - 1).replace(/\s\S*$/, '') + '…';
}

function humanize(segment: string): string {
  return segment
    .split(/[-_]/)
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
    .join(' ');
}

const ctx = require.context('@docs', true, /\.md$/, 'sync');

// Folder display order in the sidebar. Folders not listed here appear after,
// in alphabetical order.
const FOLDER_ORDER = ['tutorials', 'how-to', 'runtimes', 'reference', 'concepts', 'help'];

// Per-folder page order. Pages not listed here fall back to alphabetical.
const PAGE_ORDER: Record<string, string[]> = {
  tutorials: ['install-and-run', 'first-deployment'],
  'how-to': [
    // Deploying workloads
    'deploy-with-secrets',
    'configure-health-checks',
    'perform-rolling-update',
    'run-a-job',
    // Networking & traffic
    'isolate-namespaces-network',
    'expose-http-traffic',
    'use-host-network',
    // Access & security
    'manage-users',
    'authenticate-scripts-with-tokens',
    'subscribe-to-events-with-webhooks',
    // Operations
    'use-the-dashboard',
    'run-as-service',
  ],
  runtimes: ['index', 'docker', 'podman', 'containerd', 'cloud-hypervisor', 'firecracker'],
  reference: ['manifest', 'cli', 'api', 'config-toml', 'environment-variables'],
  concepts: [
    'architecture',
    'reconciliation',
    'runtimes',
    'namespaces-and-networking',
    'secrets-encryption',
    'health-checks-design',
    'why-not-kubernetes',
  ],
  help: ['observe-and-debug', 'troubleshooting', 'faq'],
};

function pageOrderIndex(segments: string[]): number {
  if (segments.length < 2) return -1;
  const folder = segments[0];
  const page = segments[segments.length - 1];
  const order = PAGE_ORDER[folder];
  if (!order) return Number.MAX_SAFE_INTEGER;
  const idx = order.indexOf(page);
  return idx === -1 ? Number.MAX_SAFE_INTEGER : idx;
}

function folderOrderIndex(segments: string[]): number {
  if (segments.length === 0) return -1;
  const folder = segments[0];
  const idx = FOLDER_ORDER.indexOf(folder);
  return idx === -1 ? Number.MAX_SAFE_INTEGER : idx;
}

const docs: DocEntry[] = ctx.keys()
  .filter((key) => !/(^|\/)README\.md$/i.test(key))
  .map((key) => {
    const relPath = key.replace(/^\.\//, '').replace(/\.md$/, '');
    const slug = relPath.replace(/\/?index$/, '');
    const segments = slug.split('/').filter(Boolean);
    const content = ctx<string>(key);
    const title = extractTitle(content) || humanize(segments[segments.length - 1] || 'Overview');
    return { slug, content, title, segments };
  })
  .sort((a, b) => {
    // Root pages first
    if (a.segments.length <= 1 && b.segments.length > 1) return -1;
    if (a.segments.length > 1 && b.segments.length <= 1) return 1;

    // Then by folder order
    const folderDiff = folderOrderIndex(a.segments) - folderOrderIndex(b.segments);
    if (folderDiff !== 0) return folderDiff;

    // Then by page order within the folder
    const pageDiff = pageOrderIndex(a.segments) - pageOrderIndex(b.segments);
    if (pageDiff !== 0) return pageDiff;

    // Final tiebreaker: alphabetical
    return a.slug.localeCompare(b.slug);
  });

const bySlug = new Map(docs.map((doc) => [doc.slug, doc]));

export function getDoc(slug: string): DocEntry | undefined {
  return bySlug.get(slug);
}

export function getAllDocs(): DocEntry[] {
  return docs;
}

export function docToUrl(slug: string): string {
  return slug === '' ? '/documentation' : `/documentation/${slug}`;
}

export { humanize };
