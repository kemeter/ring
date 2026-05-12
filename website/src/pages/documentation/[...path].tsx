import { useLocation } from 'react-router-dom';
import MarkdownPage from '@/components/MarkdownPage';
import { getDoc, extractDescription } from '@/lib/docs';

const SITE_URL = 'https://kemeter.github.io/ring';

const NOT_FOUND = {
  content: '# Page not found\n\nThis documentation page does not exist.',
  title: 'Page not found',
};

export const meta = (url: string, params: { path?: string }) => {
  const slug = (params.path || '').replace(/\/$/, '');
  const doc = getDoc(slug);
  if (!doc) {
    return {
      title: 'Page not found — Ring Docs',
      description: 'This documentation page does not exist.',
    };
  }
  const description = extractDescription(doc.content);
  const canonical = `${SITE_URL}${url}`;
  return {
    title: `${doc.title} — Ring Docs`,
    description,
    canonical,
    og: {
      title: `${doc.title} — Ring Docs`,
      description,
      type: 'article',
      url: canonical,
    },
    twitter: {
      card: 'summary',
      title: `${doc.title} — Ring Docs`,
      description,
    },
  };
};

export default function DocPage() {
  const { pathname } = useLocation();
  const slug = pathname.replace(/^\/documentation\/?/, '').replace(/\/$/, '');
  const page = getDoc(slug) ?? NOT_FOUND;

  return <MarkdownPage content={page.content} title={`${page.title} — Ring Docs`} />;
}
