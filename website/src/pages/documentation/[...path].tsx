import { useLocation } from 'react-router-dom';
import MarkdownPage from '@/components/MarkdownPage';
import { getDoc } from '@/lib/docs';

const NOT_FOUND = {
  content: '# Page not found\n\nThis documentation page does not exist.',
  title: 'Page not found',
};

export default function DocPage() {
  const { pathname } = useLocation();
  const slug = pathname.replace(/^\/documentation\/?/, '').replace(/\/$/, '');
  const page = getDoc(slug) ?? NOT_FOUND;

  return <MarkdownPage content={page.content} title={`${page.title} — Ring Docs`} />;
}
