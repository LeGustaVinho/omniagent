import Markdown, { type ExtraProps } from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ReactNode } from "react";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import { markdownExcerpt } from "@/editor-context";
import { markdownHighlight } from "@/markdown-highlight";

const rehypePlugins = [markdownHighlight];

export type ConversationCode = { text: string; language?: string };
type CodeActions = {
  addToDraft?: (markdown: string) => void;
  draftDisabled?: boolean;
  saveFile?: (block: ConversationCode) => void;
  reviewTerminal?: (block: ConversationCode) => void;
};
type CodeNode = NonNullable<ExtraProps["node"]>;
type ParsedNode = {
  type: string; value?: string; children?: ParsedNode[];
  position?: { start: { line: number; offset?: number }; end: { line: number } };
};

function captureCodeLiterals(source: string, literals: Map<number, string>) {
  return (tree: ParsedNode) => {
    const endings = Array.from(source.matchAll(/\r\n|\r|\n/g), ([ending]) => ending);
    function read(node: ParsedNode) {
      const position = node.position;
      const offset = position?.start.offset;
      if (node.type === "code" && typeof node.value === "string" && position && offset !== undefined) {
        const opening = source.slice(offset, offset + 3);
        const fenced = opening === "```" || opening === "~~~";
        const endLine = position.start.line + Number(fenced) + node.value.split(/\r\n|\r|\n/).length - 1;
        // Markdown's code node excludes the last line ending and its HTML adds LF. Use the source ending.
        const ending = endLine <= position.end.line && (node.value || endLine < position.end.line)
          ? endings[endLine - 1] ?? "" : "";
        literals.set(offset, node.value + ending);
      }
      node.children?.forEach(read);
    }
    read(tree);
  };
}

function codeText(node: CodeNode): string {
  return node.children.map((child) => child.type === "text" ? child.value : child.type === "element" ? codeText(child) : "").join("");
}

function ConversationCodeBlock({ code, children, actions, literal }: {
  code: CodeNode; children: ReactNode; actions: CodeActions; literal?: string;
}) {
  const { copy, feedback } = useContextActionFeedback();
  const text = literal ?? codeText(code);
  const classes = code.properties.className;
  const language = Array.isArray(classes)
    ? classes.flatMap((value) => typeof value === "string" ? /^language-([a-z0-9][a-z0-9_.+#-]*)$/i.exec(value)?.[1] ?? [] : [])[0]
    : undefined;
  const markdown = () => markdownExcerpt({ text, language });
  const { addToDraft, draftDisabled, saveFile, reviewTerminal } = actions;
  return <div className="conv-code-block">
    <ContextActions className="conv-code-context" label={language ? `${language} code block` : "code block"} groups={[[
      { label: "Copy code", run: () => copy(text) },
      { label: "Copy as fenced Markdown", run: () => copy(markdown()) },
    ], addToDraft ? [
      { label: "Add code block to draft", disabled: draftDisabled, run: () => addToDraft(markdown()) },
      { label: "Review code in draft", disabled: draftDisabled, run: () => addToDraft(`Review this code and explain any issues.\n\n${markdown()}`) },
    ] : [], literal === undefined ? [] : [
      ...(saveFile ? [{ label: "Save code block as file…", run: () => saveFile({ text, language }) }] : []),
      ...(reviewTerminal && text.trim() ? [{ label: "Insert in terminal for review", run: () => reviewTerminal({ text, language }) }] : []),
    ]]}>
      <pre tabIndex={0} aria-label={language ? `${language} code` : "Code block"}>{children}</pre>
    </ContextActions>
    {feedback && <small role={feedback.failed ? "alert" : "status"} className="text-muted-foreground">{feedback.text}</small>}
  </div>;
}

export function ConversationMarkdown({ children, codeActions }: { children: string; codeActions?: CodeActions }) {
  const literals = new Map<number, string>();
  return (
    <div className="conv-markdown">
      <Markdown
        skipHtml
        remarkPlugins={codeActions ? [remarkGfm, [captureCodeLiterals, children, literals]] : [remarkGfm]}
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
            return code && codeActions ? <ConversationCodeBlock code={code} actions={codeActions}
              literal={literals.get(code.position?.start.offset ?? -1)}>{children}</ConversationCodeBlock>
              : <pre>{children}</pre>;
          },
        }}
      >
        {children}
      </Markdown>
    </div>
  );
}
