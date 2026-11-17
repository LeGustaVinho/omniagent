import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { markdownHighlight } from "@/markdown-highlight";

const rehypePlugins = [markdownHighlight];

export function ConversationMarkdown({ children }: { children: string }) {
  return (
    <div className="conv-markdown">
      <Markdown
        skipHtml
        remarkPlugins={[remarkGfm]}
        rehypePlugins={rehypePlugins}
        components={{
          a: ({ children, href }) => {
            const label = Array.isArray(children) && children.length === 1 ? children[0] : children;
            return <span>{children}{href && href !== label ? <span className="conv-link-destination"> ({href})</span> : null}</span>;
          },
          img: ({ alt, src }) => <span className="conv-link-destination">{alt || "Image"}{src ? ` (${src})` : ""}</span>,
          table: ({ children }) => <div className="conv-table-scroll"><table>{children}</table></div>,
        }}
      >
        {children}
      </Markdown>
    </div>
  );
}
