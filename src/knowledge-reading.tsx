import { isValidElement, useEffect, useRef, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
import { ArrowUpRight, FileText } from "lucide-react";
import { anchorKey, parseWiki, prepareReading, resolveWiki, withoutProperties, type NoteTarget } from "@/knowledge-document";
import { fileName, type ExampleFile } from "@/ide-knowledge-data";
import { NoteDiagram, noteAttachments, type NoteAttachment } from "@/knowledge-media";
import "katex/dist/katex.css";

const markdownPlugins = [remarkGfm, remarkMath];
const readingPlugins = [rehypeKatex];

function textContent(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textContent).join("");
  if (isValidElement<{ children?: ReactNode }>(node)) return textContent(node.props.children);
  return "";
}

const heading = (Tag: "h1" | "h2" | "h3" | "h4" | "h5" | "h6") => function MarkdownHeading({ children }: { children?: ReactNode }) {
  return <Tag data-note-anchor={anchorKey(textContent(children))}>{children}</Tag>;
};

function trustedPreviewUrl(url: string) {
  if (/^omni-(?:wiki|mark):/.test(url)) return url;
  return url.startsWith("#") ? url : "";
}

export function NoteReading({ path, content, files, jump, onLink, onAttachment }: {
  path: string;
  content: string;
  files: readonly ExampleFile[];
  jump?: { anchor?: string; line?: number; token: number };
  onLink: (link: NoteTarget) => void;
  onAttachment: (attachment: NoteAttachment) => void;
}) {
  const root = useRef<HTMLDivElement>(null);
  const rendered = prepareReading(content, path, files);
  useEffect(() => {
    if (!jump?.anchor || !root.current) return;
    const key = anchorKey(jump.anchor);
    const target = [...root.current.querySelectorAll("[data-note-anchor]")].find((entry) =>
      entry.getAttribute("data-note-anchor") === key);
    target?.scrollIntoView({ block: "start" });
  }, [jump, path]);

  const components: Components = {
    h1: heading("h1"), h2: heading("h2"), h3: heading("h3"), h4: heading("h4"), h5: heading("h5"), h6: heading("h6"),
    pre: ({ node, children }) => {
      const code = node?.children.find((entry) => entry.type === "element" && entry.tagName === "code");
      if (code?.type === "element" && Array.isArray(code.properties.className) && code.properties.className.includes("language-mermaid")) {
        const source = code.children.map((entry) => entry.type === "text" ? entry.value : "").join("");
        return <NoteDiagram source={source} />;
      }
      return <pre>{children}</pre>;
    },
    a: ({ href, children }) => {
      if (href?.startsWith("omni-mark:")) return <mark>{children}</mark>;
      if (href?.startsWith("omni-wiki:")) {
        let value: string;
        try {
          value = decodeURIComponent(href.slice("omni-wiki:".length));
        } catch {
          return <span>{children}</span>;
        }
        const link = parseWiki(value.replace(/^!/, ""), value.startsWith("!"));
        const attachment = noteAttachments.find(({ name }) => link.target.split("/").at(-1)?.toLowerCase() === name);
        if (attachment) return <button type="button" className="note-inline-link" onClick={() => onAttachment(attachment)}><FileText size={13} />{children}</button>;
        const targets = resolveWiki(link, path, files);
        const note = targets.length === 1 ? files.find(({ path: filePath }) => filePath === targets[0]?.path) : undefined;
        const excerpt = note && withoutProperties(note.content).split("\n").filter((line) => line.trim() && !line.startsWith("#")).slice(0, 2).join(" ");
        const problem = targets.length > 1 ? "Ambiguous note reference" : "Unresolved note reference";
        return (
          <span className="note-preview-anchor">
            <button type="button" className="note-inline-link" data-invalid={!note} onClick={() => onLink(link)}>{children}<ArrowUpRight size={11} /></button>
            <span className="note-link-preview" role="tooltip"><strong>{note ? fileName(note.path) : problem}</strong>{excerpt && <span>{excerpt}</span>}</span>
          </span>
        );
      }
      return href?.startsWith("#") ? <a href={href}>{children}</a> : <span>{children}</span>;
    },
    img: ({ alt }) => <span className="file-help">Attachment preview unavailable · {alt}</span>,
    p: ({ children }) => {
      const body = textContent(children);
      const block = /\s+(\^[\w-]+)\s*$/.exec(body)?.[1];
      return <p data-note-anchor={block && anchorKey(block)}>{children}</p>;
    },
  };

  return (
    <div className="note-reading" ref={root} aria-label={`Reading view for ${path}`}>
      <article>
        <ReactMarkdown skipHtml urlTransform={trustedPreviewUrl} remarkPlugins={markdownPlugins} rehypePlugins={readingPlugins} components={components}>
          {rendered}
        </ReactMarkdown>
      </article>
    </div>
  );
}
