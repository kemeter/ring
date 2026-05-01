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

const docs: DocEntry[] = ctx.keys()
  .filter((key) => !/(^|\/)README\.md$/i.test(key))
  .map((key) => {
    const relPath = key.replace(/^\.\//, '').replace(/\.md$/, '');
    const slug = relPath.replace(/\/?index$/, '');
    const segments = slug.split('/').filter(Boolean);
    const content = ctx<string>(key);
    const title = extractTitle(content) || humanize(segments[segments.length - 1] || 'Overview');
    return { slug, content, title, segments };
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
