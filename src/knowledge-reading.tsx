import { createContext, isValidElement, useContext, useEffect, useId, useRef, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
import { ArrowUpRight, FileText } from "lucide-react";
import {
  anchorKey, getProperties, parseReadingAddress, parseReadingFile, prepareReading, readingLocalAddress, resolveWiki, withoutProperties,
  type NoteTarget,
} from "@/knowledge-document";
import { fileName, type ExampleFile } from "@/ide-knowledge-data";
import { NoteDiagram, noteAttachments, type NoteAttachment } from "@/knowledge-media";
import { noteAttachmentCandidates, RepositoryNoteMedia } from "@/repository-note-media";
import "katex/dist/katex.css";

const markdownPlugins = [remarkGfm, remarkMath];
const readingPlugins = [decorateTags, rehypeKatex];

type ReadingNode = {
  type: string;
  tagName?: string;
  value?: string;
  children?: ReadingNode[];
  properties?: Record<string, unknown>;
};

const tagExcludedElements = new Set(["a", "code", "pre", "script", "style"]);

function decorateTags() {
  function apply(node: ReadingNode) {
    if (!node.children || (node.tagName && tagExcludedElements.has(node.tagName))) return;
    node.children = node.children.flatMap((child) => {
      if (child.type !== "text" || !child.value) {
        apply(child);
        return [child];
      }
      const text = child.value;
      const parts: ReadingNode[] = [];
      let end = 0;
      for (const match of text.matchAll(/(?<![\p{L}\p{N}_/#\\])#[\p{L}\p{N}_-]+(?:\/[\p{L}\p{N}_-]+)*/gu)) {
        if (!/[\p{L}_]/u.test(match[0])) continue;
        const start = match.index;
        if (start > end) parts.push({ type: "text", value: text.slice(end, start) });
        parts.push({ type: "element", tagName: "span", properties: { className: ["note-tag"] }, children: [{ type: "text", value: match[0] }] });
        end = start + match[0].length;
      }
      if (!parts.length) return [child];
      if (end < text.length) parts.push({ type: "text", value: text.slice(end) });
      return parts;
    });
  }
  return apply;
}

function textContent(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textContent).join("");
  if (isValidElement<{ children?: ReactNode }>(node)) return textContent(node.props.children);
  return "";
}

const heading = (Tag: "h1" | "h2" | "h3" | "h4" | "h5" | "h6") => function MarkdownHeading({ children, id, className }: {
  children?: ReactNode;
  id?: string;
  className?: string;
}) {
  const { anchorScope } = useContext(ReadingContext);
  return <Tag id={id && anchorScope + id} className={className} data-note-anchor={anchorKey(textContent(children))}>{children}</Tag>;
};

function trustedPreviewUrl(url: string, path: string) {
  if (/^omni-(?:wiki|mark|file):/.test(url)) return url;
  if (url.startsWith("#")) return url;
  return readingLocalAddress(path, url);
}

type ReadingProps = {
  path: string;
  content: string;
  files: readonly ExampleFile[];
  jump?: { anchor?: string; line?: number; token: number };
  onLink: (link: NoteTarget) => void;
  onAttachment: (attachment: NoteAttachment) => void;
  sampleAttachments?: boolean;
  checkoutId?: string;
  onRepositoryAttachment?: (paths: string[], label: string) => void;
};
const ReadingContext = createContext<ReadingProps & { anchorScope: string }>(null!);

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
  a: function ReadingLink({ href, children, id, className, "aria-describedby": describedBy, "aria-label": label }) {
    const { path, files, onLink, onAttachment, sampleAttachments = true, checkoutId, onRepositoryAttachment, anchorScope } = useContext(ReadingContext);
    if (href?.startsWith("omni-mark:")) return <mark>{children}</mark>;
    if (href?.startsWith("omni-wiki:")) {
      const link = parseReadingAddress(href);
      if (!link) return <span>{children}</span>;
      const attachment = sampleAttachments ? noteAttachments.find(({ name }) => link.target.split("/").at(-1)?.toLowerCase() === name) : undefined;
      if (attachment) return <button type="button" className="note-inline-link" onClick={() => onAttachment(attachment)}><FileText size={13} />{children}</button>;
      const paths = noteAttachmentCandidates(link.target, link.sourcePath ?? path, true);
      if (checkoutId && paths.length > 0) {
        const label = link.alias ?? link.target;
        if (link.embed) return <RepositoryNoteMedia checkoutId={checkoutId} paths={paths} title={label} />;
        if (onRepositoryAttachment) return <button type="button" className="note-inline-link"
          onClick={() => onRepositoryAttachment(paths, label)}>{children}</button>;
      }
      const targets = resolveWiki(link, link.sourcePath ?? path, files);
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
    const asset = href && parseReadingFile(href);
    if (asset && checkoutId && onRepositoryAttachment) {
      const paths = noteAttachmentCandidates(asset[1], asset[0], false);
      if (paths.length > 0) return <button type="button" className="note-inline-link"
        onClick={() => onRepositoryAttachment(paths, textContent(children) || asset[1])}>{children}</button>;
    }
    if (!href?.startsWith("#")) return <span>{children}</span>;
    const localTarget = href.startsWith("#user-content-fn") ? "#" + anchorScope + href.slice(1) : href;
    return <a href={localTarget} id={id && anchorScope + id} className={className} aria-label={label}
      aria-describedby={describedBy && anchorScope + describedBy}>{children}</a>;
  },
  li: function ReadingListItem({ id, className, children }) {
    const { anchorScope } = useContext(ReadingContext);
    return <li id={id && anchorScope + id} className={className}>{children}</li>;
  },
  img: function ReadingImage({ alt, src }) {
    const { checkoutId } = useContext(ReadingContext);
    const file = src && parseReadingFile(src);
    if (checkoutId && file) {
      return <RepositoryNoteMedia checkoutId={checkoutId} paths={noteAttachmentCandidates(file[1], file[0], false)} title={alt || file[1]} />;
    }
    return <span className="file-help">Attachment preview unavailable · {alt}</span>;
  },
  p: ({ children }) => {
    const body = textContent(children);
    const block = /\s+(\^[\w-]+)\s*$/.exec(body)?.[1];
    return <p data-note-anchor={block && anchorKey(block)}>{children}</p>;
  },
};

export function NoteReading(props: ReadingProps) {
  const { path, content, files, jump } = props;
  const anchorScope = useId() + "-";
  const root = useRef<HTMLDivElement>(null);
  const rendered = prepareReading(content, path, files);
  const properties = getProperties(content);
  useEffect(() => {
    if (!jump?.anchor || !root.current) return;
    const key = anchorKey(jump.anchor);
    const target = [...root.current.querySelectorAll("[data-note-anchor]")].find((entry) =>
      entry.getAttribute("data-note-anchor") === key);
    target?.scrollIntoView({ block: "start" });
  }, [jump, path]);

  return (
    <div className="note-reading" ref={root} aria-label={`Reading view for ${path}`}>
      <article>
        {properties.length > 0 && <dl aria-label="Properties" className="note-reading-properties">
          {properties.map(({ key, value }, index) => <div key={`${key}:${index}`}>
            <dt>{key}</dt><dd>{value}</dd>
          </div>)}
        </dl>}
        <ReadingContext.Provider value={{ ...props, anchorScope }}>
          <ReactMarkdown skipHtml urlTransform={(url) => trustedPreviewUrl(url, path)} remarkPlugins={markdownPlugins}
            rehypePlugins={readingPlugins} components={components}>
            {rendered}
          </ReactMarkdown>
        </ReadingContext.Provider>
      </article>
    </div>
  );
}
