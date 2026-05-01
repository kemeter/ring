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

function humanize(segment: string): string {
  return segment
    .split(/[-_]/)
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
    .join(' ');
}

const ctx = require.context('@docs', true, /\.md$/, 'sync');

// Folder display order in the sidebar. Folders not listed here appear after,
// in alphabetical order.
const FOLDER_ORDER = ['getting-started', 'runtimes', 'guides', 'reference', 'help'];

// Per-folder page order. Pages not listed here fall back to alphabetical.
const PAGE_ORDER: Record<string, string[]> = {
  'getting-started': ['installation', 'overview', 'first-deployment', 'managing-deployments'],
  runtimes: ['docker', 'cloud-hypervisor'],
  guides: ['examples'],
  reference: ['cli', 'api'],
  help: ['faq'],
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
