import { useParams } from 'react-router-dom';
import MarkdownPage from '@/components/MarkdownPage';

const PATH_MAP: Record<string, { file: string; title: string }> = {
  '': { file: 'index.md', title: 'Overview' },
  'installation': { file: 'installation.md', title: 'Installation' },
  'getting-started': { file: 'getting-started/index.md', title: 'Getting Started' },
  'getting-started/first-deployment': { file: 'getting-started/first-deployment.md', title: 'First Deployment' },
  'getting-started/managing-deployments': { file: 'getting-started/managing-deployments.md', title: 'Managing Deployments' },
  'examples': { file: 'examples.md', title: 'Examples' },
  'reference': { file: 'reference.md', title: 'CLI Reference' },
  'api-reference': { file: 'api-reference.md', title: 'API Reference' },
  'faq': { file: 'faq.md', title: 'FAQ' },
};

export default function DocPage() {
  const params = useParams();
  const path = params['*'] || '';

  const page = PATH_MAP[path] || { file: path + '.md', title: 'Documentation' };

  return <MarkdownPage path={page.file} title={`${page.title} - Ring Docs`} />;
}
