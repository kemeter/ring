import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import remarkDirective from 'remark-directive';
import rehypeRaw from 'rehype-raw';
import { Link } from 'react-router-dom';
import Head from 'aplos/head';
import CodeBlock from './CodeBlock';
import CodeTabs from './CodeTabs';
import YamlJsonTabs from './YamlJsonTabs';
import remarkCodeTabs from '@/lib/remark-code-tabs';
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
                remarkPlugins={[remarkGfm, remarkDirective, remarkCodeTabs]}
                rehypePlugins={[rehypeRaw]}
                components={{
                  'code-tabs': ({ children }: { children?: React.ReactNode }) => (
                    <CodeTabs>{children}</CodeTabs>
                  ),
                  code({ className, children, node, ...props }) {
                    // `node` is the hast node injected by react-markdown's
                    // passNode — strip it so it isn't spread onto the DOM
                    // (would render as node="[object Object]").
                    void node;
                    const match = /language-(\w+)/.exec(className || '');
                    const code = String(children).replace(/\n$/, '');

                    if (match) {
                      // Every YAML block gets an auto-generated JSON twin
                      // tab — single-source docs, no duplicated JSON.
                      if (match[1] === 'yaml' || match[1] === 'yml') {
                        return <YamlJsonTabs code={code} />;
                      }
                      return <CodeBlock code={code} language={match[1]} />;
                    }

                    // A fenced block with no language still routes here.
                    // Multi-line content means it's a block, not inline
                    // code, so give it the proper CodeBlock chrome.
                    if (code.includes('\n')) {
                      return <CodeBlock code={code} language="text" />;
                    }

                    return <code className={className} {...props}>{children}</code>;
                  },
                  a({ href, children, node, ...props }) {
                    // Strip the hast `node` prop so it isn't spread onto the
                    // <a> DOM element (would render as node="[object Object]").
                    void node;
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
