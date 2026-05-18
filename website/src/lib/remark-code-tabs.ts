import { visit } from 'unist-util-visit';

// Turns the `:::code-tabs` container directive into a `<code-tabs>` element
// that react-markdown can map via the `components` prop. Unlike a raw HTML
// block, a directive's children ARE parsed as markdown, so the code fences
// inside stay real <pre><code> nodes — this is the equivalent of Symfony's
// `.. configuration-block::` directive, in markdown.
export default function remarkCodeTabs() {
  return (tree: unknown) => {
    visit(tree as never, (node: any) => {
      if (node.type === 'containerDirective' && node.name === 'code-tabs') {
        const data = node.data || (node.data = {});
        data.hName = 'code-tabs';
        data.hProperties = {};
      }
    });
  };
}
