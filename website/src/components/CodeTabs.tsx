import {
  Children,
  isValidElement,
  useMemo,
  type ReactElement,
  type ReactNode,
} from 'react';
import TabbedCode, { type CodePane } from './TabbedCode';
import '@/styles/components/code-tabs.css';

type Tab = CodePane;

// Label shown for each tab, derived from the code block's language/content —
// same idea as Symfony's configuration-block deriving "YAML"/"PHP" from the
// code-block language.
function deriveLabel(language: string, code: string): string {
  const lang = language.toLowerCase();
  if (lang === 'yaml' || lang === 'yml') return 'YAML';
  if (lang === 'http' || lang === 'json') return 'API';
  if (/^\s*curl[\s\\]/m.test(code) || /\bhttps?:\/\//.test(code)) return 'API';
  return 'CLI';
}

// Inside the directive, react-markdown maps each ``` fence to a <pre> whose
// child is MarkdownPage's `code` component (a function element), NOT a
// <CodeBlock> directly. So we can't match on `el.type === CodeBlock`. Instead
// we find each <pre>, read its inner code element's `className` (the
// `language-x` token) and text children — same data the `code` component uses.
function collectTabs(children: ReactNode, acc: Tab[]): void {
  Children.forEach(children, (child) => {
    if (!isValidElement(child)) return;
    const el = child as ReactElement<{
      className?: string;
      children?: ReactNode;
    }>;

    if (el.type === 'pre') {
      const inner = el.props.children;
      if (isValidElement(inner)) {
        const innerEl = inner as ReactElement<{
          className?: string;
          children?: ReactNode;
        }>;
        const className = innerEl.props.className || '';
        const match = /language-(\w+)/.exec(className);
        const code = String(innerEl.props.children ?? '').replace(/\n$/, '');
        if (code) {
          const language = match ? match[1] : 'bash';
          acc.push({ label: deriveLabel(language, code), code, language });
        }
      }
      return;
    }

    if (el.props && el.props.children) {
      collectTabs(el.props.children, acc);
    }
  });
}

export default function CodeTabs({ children }: { children?: ReactNode }) {
  const tabs = useMemo<Tab[]>(() => {
    const result: Tab[] = [];
    collectTabs(children, result);
    return result;
  }, [children]);

  return <TabbedCode panes={tabs} />;
}
