import { useState, useEffect } from 'react';
import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import rehypeRaw from 'rehype-raw';
import Head from 'aplos/head';
import CodeBlock from './CodeBlock';
import DocSidebar from './DocSidebar';
import '@/styles/components/doc-content.css';
import '@/styles/components/sidebar.css';

interface MarkdownPageProps {
  path: string;
  title: string;
}

export default function MarkdownPage({ path, title }: MarkdownPageProps) {
  const [content, setContent] = useState('');
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    fetch(`/docs/${path}`)
      .then((res) => {
        if (!res.ok) throw new Error('Not found');
        return res.text();
      })
      .then((text) => {
        setContent(text);
        setLoading(false);
      })
      .catch(() => {
        setContent('# Page not found\n\nThis documentation page could not be loaded.');
        setLoading(false);
      });
  }, [path]);

  return (
    <>
      <Head>
        <title>{title}</title>
      </Head>
      <div className="doc-layout">
        <DocSidebar />
        <div className="doc-content-area">
          {loading ? (
            <article className="doc-content">
              <div style={{ color: 'var(--color-text-muted)' }}>Loading...</div>
            </article>
          ) : (
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
                }}
              >
                {content}
              </Markdown>
            </article>
          )}
        </div>
      </div>
    </>
  );
}
