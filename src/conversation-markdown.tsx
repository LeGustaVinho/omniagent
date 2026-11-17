import Markdown, { type ExtraProps } from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ReactNode } from "react";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import { markdownExcerpt } from "@/editor-context";
import { markdownHighlight } from "@/markdown-highlight";

const rehypePlugins = [markdownHighlight];

type CodeActions = { addToDraft?: (markdown: string) => void; draftDisabled?: boolean };
type CodeNode = NonNullable<ExtraProps["node"]>;

function codeText(node: CodeNode): string {
  return node.children.map((child) => child.type === "text" ? child.value : child.type === "element" ? codeText(child) : "").join("");
}

function ConversationCodeBlock({ code, children, actions }: { code: CodeNode; children: ReactNode; actions: CodeActions }) {
  const { copy, feedback } = useContextActionFeedback();
  const text = codeText(code);
  const classes = code.properties.className;
  const language = Array.isArray(classes)
    ? classes.flatMap((value) => typeof value === "string" ? /^language-([a-z0-9][a-z0-9_.+#-]*)$/i.exec(value)?.[1] ?? [] : [])[0]
    : undefined;
  const markdown = () => markdownExcerpt({ text, language });
  const { addToDraft, draftDisabled } = actions;
  return <div className="conv-code-block">
    <ContextActions className="conv-code-context" label={language ? `${language} code block` : "code block"} groups={[[
      { label: "Copy code", run: () => copy(text) },
      { label: "Copy as fenced Markdown", run: () => copy(markdown()) },
    ], addToDraft ? [
      { label: "Add code block to draft", disabled: draftDisabled, run: () => addToDraft(markdown()) },
      { label: "Review code in draft", disabled: draftDisabled, run: () => addToDraft(`Review this code and explain any issues.\n\n${markdown()}`) },
    ] : []]}>
      <pre tabIndex={0} aria-label={language ? `${language} code` : "Code block"}>{children}</pre>
    </ContextActions>
    {feedback && <small role={feedback.failed ? "alert" : "status"} className="text-muted-foreground">{feedback.text}</small>}
  </div>;
}

export function ConversationMarkdown({ children, codeActions }: { children: string; codeActions?: CodeActions }) {
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
          pre: ({ children, node }) => {
            const code = node?.children.find((child): child is CodeNode => child.type === "element" && child.tagName === "code");
            return code && codeActions ? <ConversationCodeBlock code={code} actions={codeActions}>{children}</ConversationCodeBlock>
              : <pre>{children}</pre>;
          },
        }}
      >
        {children}
      </Markdown>
    </div>
  );
}
