import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import rehypeRaw from 'rehype-raw';
import { Link } from 'react-router-dom';
import Head from 'aplos/head';
import CodeBlock from './CodeBlock';
import DocSidebar from './DocSidebar';
import '@/styles/components/doc-content.css';
import '@/styles/components/sidebar.css';

interface MarkdownPageProps {
  content: string;
  title: string;
}

export default function MarkdownPage({ content, title }: MarkdownPageProps) {
  return (
    <>
      <Head>
        <title>{title}</title>
      </Head>
      <div className="container">
        <div className="doc-layout">
          <DocSidebar />
          <div className="doc-content-area">
            <article className="doc-content">
              <Markdown
                remarkPlugins={[remarkGfm]}
                rehypePlugins={[rehypeRaw]}
                components={{
                  code({ className, children, ...props }) {
                    const match = /language-(\w+)/.exec(className || '');
                    const code = String(children).replace(/\n$/, '');

                    if (match) {
                      return <CodeBlock code={code} language={match[1]} />;
                    }

                    return <code className={className} {...props}>{children}</code>;
                  },
                  a({ href, children, ...props }) {
                    if (href && (href.startsWith('http://') || href.startsWith('https://'))) {
                      return <a href={href} target="_blank" rel="noopener noreferrer" {...props}>{children}</a>;
                    }
                    if (href && href.startsWith('/')) {
                      return <Link to={href}>{children}</Link>;
                    }
                    return <a href={href} {...props}>{children}</a>;
                  },
                }}
              >
                {content}
              </Markdown>
            </article>
          </div>
        </div>
      </div>
    </>
  );
}
