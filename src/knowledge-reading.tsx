import { createContext, isValidElement, useContext, useEffect, useId, useRef, type ComponentPropsWithoutRef, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
import { ArrowUpRight, FileText } from "lucide-react";
import {
  anchorKey, getProperties, markdownNoteLink, parseReadingAddress, parseReadingFile, readingDocument, readingLocalAddress, resolveWiki, withoutProperties,
  type NoteTarget, type ReadingOrigin,
} from "@/knowledge-document";
import { isContextKey, useNoteContextMenu, type NoteContextActions, type NoteContextHit } from "@/knowledge-context";
import { fileName, type ExampleFile } from "@/ide-knowledge-data";
import { NoteDiagram, noteAttachments, type NoteAttachment } from "@/knowledge-media";
import { markdownHighlight } from "@/markdown-highlight";
import { noteAttachmentCandidates, RepositoryNoteMedia } from "@/repository-note-media";
import type { IndexedLinkResolution } from "@/repository-note-links";
import "katex/dist/katex.css";

const markdownPlugins = [remarkGfm, remarkMath];
const readingPlugins = [decorateTags, rehypeKatex, markdownHighlight];

type ReadingNode = {
  type: string;
  tagName?: string;
  value?: string;
  children?: ReadingNode[];
  properties?: Record<string, unknown>;
  position?: { start: { line: number }; end: { line: number } };
};

function markReadingOrigins(origins: readonly ReadingOrigin[]) {
  return function decorate(tree: ReadingNode) {
    function visit(node: ReadingNode) {
      if (node.type === "element" && node.position) {
        const from = origins[node.position.start.line - 1];
        const to = origins[node.position.end.line - 1];
        if (from && to?.path === from.path) {
          node.properties = { ...node.properties, "data-note-source": from.path,
            "data-note-line": String(from.line), "data-note-end-line": String(to.line) };
        }
      }
      for (const child of node.children ?? []) visit(child);
    }
    visit(tree);
  };
}

function originAttributes(node?: { properties: Record<string, unknown> }) {
  const read = (key: string) => {
    const value = node?.properties[key];
    return typeof value === "string" ? value : undefined;
  };
  return { "data-note-source": read("data-note-source"), "data-note-line": read("data-note-line"), "data-note-end-line": read("data-note-end-line") };
}

function markReadingLine(line: number) {
  return (tree: ReadingNode) => {
    const blocks = tree.children ?? [];
    const target = blocks.findLastIndex(({ type, position }) => type === "element" && position !== undefined && position.start.line <= line);
    if (target === -1) return;
    blocks.splice(target, 0, { type: "element", tagName: "span", properties: { dataNoteLineTarget: "" }, children: [] });
  };
}

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

const heading = (Tag: "h1" | "h2" | "h3" | "h4" | "h5" | "h6") => function MarkdownHeading({ children, id, node: _node, ...props }: ComponentPropsWithoutRef<"h1"> & { node?: unknown }) {
  const { anchorScope, contextActions } = useContext(ReadingContext);
  const label = textContent(children);
  return <Tag {...props} id={id && anchorScope + id} data-note-anchor={anchorKey(label)} data-note-reference={label}
    tabIndex={contextActions ? 0 : undefined}>{children}</Tag>;
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
  resolveLinks?: (link: NoteTarget, from: string) => IndexedLinkResolution | undefined;
  previewFor?: (path: string) => { title: string; preview?: string | null } | undefined;
  contextActions?: NoteContextActions;
};
const ReadingContext = createContext<ReadingProps & { anchorScope: string }>(null!);

const components: Components = {
  h1: heading("h1"), h2: heading("h2"), h3: heading("h3"), h4: heading("h4"), h5: heading("h5"), h6: heading("h6"),
  pre: ({ node, children, ref: _ref, ...props }) => {
    const code = node?.children.find((entry) => entry.type === "element" && entry.tagName === "code");
    if (code?.type === "element" && Array.isArray(code.properties.className) && code.properties.className.includes("language-mermaid")) {
      const source = code.children.map((entry) => entry.type === "text" ? entry.value : "").join("");
      return <div {...originAttributes(node)} data-note-mermaid={source}><NoteDiagram source={source} /></div>;
    }
    return <pre {...props}>{children}</pre>;
  },
  a: function ReadingLink({ href, children, id, className, node, "aria-describedby": describedBy, "aria-label": label, ...props }) {
    const { path, files, onLink, onAttachment, sampleAttachments = true, checkoutId, onRepositoryAttachment, anchorScope, resolveLinks, previewFor } = useContext(ReadingContext);
    const origin = originAttributes(node);
    const from = origin["data-note-source"] ?? path;
    const line = Number(origin["data-note-line"]) || undefined;
    if (href?.startsWith("omni-mark:")) return <mark>{children}</mark>;
    if (href?.startsWith("omni-wiki:")) {
      const parsed = parseReadingAddress(href);
      if (!parsed) return <span>{children}</span>;
      const link = { ...parsed, sourcePath: from, sourceLine: line };
      const attachment = sampleAttachments ? noteAttachments.find(({ name }) => link.target.split("/").at(-1)?.toLowerCase() === name) : undefined;
      if (attachment) return <button {...origin} type="button" className="note-inline-link" data-note-context-link={href}
        onClick={() => onAttachment(attachment)}><FileText size={13} />{children}</button>;
      const resolution = resolveLinks?.(link, path);
      const paths = resolveLinks
        ? resolution?.status === "attachment" ? resolution.paths : []
        : noteAttachmentCandidates(link.target, link.sourcePath ?? path, true);
      if (checkoutId && paths.length > 0) {
        const label = link.alias ?? link.target;
        if (link.embed) return <span {...origin} data-note-context-link={href}><RepositoryNoteMedia checkoutId={checkoutId} paths={paths} title={label} /></span>;
        if (onRepositoryAttachment) return <button {...origin} type="button" className="note-inline-link" data-note-context-link={href}
          onClick={() => onRepositoryAttachment(paths, label)}>{children}</button>;
      }
      const targets = resolveLinks ? [] : resolveWiki(link, link.sourcePath ?? path, files);
      const resolved = resolution?.status === "resolved" ? resolution.location.path : targets.length === 1 ? targets[0]?.path : undefined;
      const note = resolved ? files.find(({ path: filePath }) => filePath === resolved) : undefined;
      const indexed = resolved && previewFor?.(resolved);
      const excerpt = indexed ? indexed.preview : note && withoutProperties(note.content).split("\n").filter((line) => line.trim() && !line.startsWith("#")).slice(0, 2).join(" ");
      const problem = resolution?.status === "ambiguous" || targets.length > 1 ? "Ambiguous note reference"
        : resolution?.status === "missing_anchor" ? "The heading or block is unavailable in this note."
          : resolveLinks && !resolution ? "Resolving note reference…" : "Unresolved note reference";
      return (
        <span className="note-preview-anchor">
          <button {...origin} type="button" className="note-inline-link" data-invalid={!note} data-note-context-link={href}
            onClick={() => onLink(link)}>{children}<ArrowUpRight size={11} /></button>
          <span className="note-link-preview" role="tooltip"><strong>{indexed ? indexed.title : note ? fileName(note.path) : problem}</strong>{excerpt && <span>{excerpt}</span>}</span>
        </span>
      );
    }
    const asset = href && parseReadingFile(href);
    if (asset && checkoutId && onRepositoryAttachment) {
      const paths = noteAttachmentCandidates(asset[1], from, false);
      if (paths.length > 0) return <button {...origin} type="button" className="note-inline-link" data-note-context-file={href}
        onClick={() => onRepositoryAttachment(paths, textContent(children) || asset[1])}>{children}</button>;
    }
    if (!href?.startsWith("#")) return <span>{children}</span>;
    const noteAnchor = markdownNoteLink(from, href, line);
    if (noteAnchor) return <a {...origin} href={href} className={className} aria-label={label} onClick={(event) => {
      event.preventDefault();
      onLink(noteAnchor);
    }}>{children}</a>;
    const localTarget = href.startsWith("#user-content-fn") ? "#" + anchorScope + href.slice(1) : href;
    return <a {...props} href={localTarget} id={id && anchorScope + id} className={className} aria-label={label}
      aria-describedby={describedBy && anchorScope + describedBy}>{children}</a>;
  },
  li: function ReadingListItem({ id, className, children, node: _node, ...props }) {
    const { anchorScope, contextActions } = useContext(ReadingContext);
    const reference = readingBlockAnchor(children);
    return <li {...props} id={id && anchorScope + id} className={className} data-note-anchor={reference && anchorKey(reference)}
      data-note-reference={reference} tabIndex={contextActions && reference ? 0 : undefined}>{children}</li>;
  },
  img: function ReadingImage({ alt, src, node }) {
    const { checkoutId, path } = useContext(ReadingContext);
    const origin = originAttributes(node);
    const file = src && parseReadingFile(src);
    if (checkoutId && file) {
      return <span {...origin} data-note-context-file={src} aria-label={alt || file[1]}>
        <RepositoryNoteMedia checkoutId={checkoutId} paths={noteAttachmentCandidates(file[1], origin["data-note-source"] ?? path, false)} title={alt || file[1]} />
      </span>;
    }
    return <span className="file-help">Attachment preview unavailable · {alt}</span>;
  },
  p: function ReadingParagraph({ children, node: _node, ...props }) {
    const reference = readingBlockAnchor(children);
    const { contextActions } = useContext(ReadingContext);
    return <p {...props} data-note-anchor={reference && anchorKey(reference)} data-note-reference={reference}
      tabIndex={contextActions && reference ? 0 : undefined}>{children}</p>;
  },
};

function readingBlockAnchor(children: ReactNode) {
  return /(?:^|\s)(\^[\w-]+)\s*$/.exec(textContent(children))?.[1];
}

function readingNoteContext(root: HTMLElement, target: Element, fallbackPath: string): NoteContextHit {
  const origin = target.closest<HTMLElement>("[data-note-source]")?.dataset;
  const path = origin?.noteSource ?? fallbackPath;
  const line = origin?.noteLine && Number(origin.noteLine) || undefined;
  const nextBlock = target.closest("p, blockquote, pre, table")?.nextElementSibling;
  const marker = nextBlock instanceof HTMLElement && nextBlock.dataset.noteSource === path && nextBlock.dataset.noteReference;
  const standalone = marker && marker.startsWith("^") && nextBlock?.textContent?.trim() === marker ? marker : undefined;
  const anchor = target.closest<HTMLElement>("[data-note-reference]")?.dataset.noteReference ?? standalone;
  const address = target.closest<HTMLElement>("[data-note-context-link]")?.dataset.noteContextLink;
  const localAnchor = target.closest("a[href^='#']")?.getAttribute("href");
  const parsed = address ? parseReadingAddress(address) : localAnchor ? markdownNoteLink(path, localAnchor, line) : undefined;
  const reference = parsed && { ...parsed, sourcePath: path, sourceLine: line };
  const fileHost = target.closest<HTMLElement>("[data-note-context-file]");
  const file = parseReadingFile(fileHost?.dataset.noteContextFile ?? "");
  const paths = file ? noteAttachmentCandidates(file[1], path, false) : [];
  const attachment = file && paths.length ? { paths, label: fileHost?.textContent?.trim() || fileHost?.getAttribute("aria-label") || file[1] } : undefined;
  const tag = target.closest(".note-tag")?.textContent?.slice(1);
  const property = target.closest<HTMLElement>("[data-note-property]")?.dataset;
  const mermaid = target.closest<HTMLElement>("[data-note-mermaid]")?.dataset.noteMermaid;
  const hit: NoteContextHit = { path, line, anchor, link: reference, attachment, tag, mermaid,
    property: property?.noteProperty ? { name: property.noteProperty, value: property.notePropertyValue ?? "" } : undefined };
  const selection = root.ownerDocument.getSelection();
  if (!selection || selection.rangeCount !== 1 || selection.isCollapsed) return hit;
  const range = selection.getRangeAt(0);
  if (!root.contains(range.startContainer) || !root.contains(range.endContainer) || !range.intersectsNode(target)) return hit;
  const sourceOf = (node: Node) => (node instanceof Element ? node : node.parentElement)?.closest<HTMLElement>("[data-note-source]")?.dataset;
  const start = sourceOf(range.startContainer);
  const end = sourceOf(range.endContainer);
  const text = selection.toString();
  if (text.trim() && start?.noteSource && start.noteSource === end?.noteSource && start.noteLine && end.noteEndLine) {
    hit.excerpt = { path: start.noteSource, text, startLine: Number(start.noteLine), endLine: Number(end.noteEndLine), language: "markdown" };
  }
  return hit;
}

export function NoteReading(props: ReadingProps) {
  const { path, content, files, jump, resolveLinks, contextActions } = props;
  const anchorScope = useId() + "-";
  const root = useRef<HTMLDivElement>(null);
  const { open: openContextMenu, overlay } = useNoteContextMenu();
  const { markdown, renderedLine, origins } = readingDocument(content, path, files, resolveLinks ? (link, from) => {
    const target = resolveLinks(link, from);
    if (target?.status !== "resolved") return [];
    return [{ path: target.location.path, anchor: target.location.anchor ?? undefined }];
  } : undefined, jump?.line);
  const properties = getProperties(content);
  const open = (target: EventTarget | null, point: { x: number; y: number }) => {
    if (!contextActions || !root.current || !(target instanceof Element)) return false;
    const hit = readingNoteContext(root.current, target, path);
    const focused = root.current.ownerDocument.activeElement;
    return openContextMenu(hit.link?.alias || hit.link?.target || hit.attachment?.label || hit.anchor || hit.tag || "Note reading", contextActions(hit), point, () => {
      if (focused instanceof HTMLElement && focused.isConnected) focused.focus();
    });
  };
  useEffect(() => {
    if (!jump || !root.current) return;
    const key = jump.anchor && anchorKey(jump.anchor);
    const anchor = key && [...root.current.querySelectorAll("[data-note-anchor]")].find((entry) =>
      entry.getAttribute("data-note-anchor") === key);
    const target = anchor || root.current.querySelector("[data-note-line-target]")?.nextElementSibling;
    target?.scrollIntoView?.({ block: "start" });
  }, [jump, path, renderedLine]);

  return (
    <div className="note-reading" ref={root} aria-label={`Reading view for ${path}`} onContextMenu={(event) => {
      if (open(event.target, { x: event.clientX, y: event.clientY })) { event.preventDefault(); event.stopPropagation(); }
    }} onKeyDown={(event) => {
      if (!isContextKey(event.nativeEvent) || !(event.target instanceof Element)) return;
      const rect = event.target.getBoundingClientRect();
      if (open(event.target, { x: rect.left, y: rect.bottom })) { event.preventDefault(); event.stopPropagation(); }
    }}>
      <article>
        {properties.length > 0 && <dl aria-label="Properties" className="note-reading-properties">
          {properties.map(({ key, value, line }, index) => <div key={`${key}:${index}`} data-note-property={key} data-note-property-value={value}
            data-note-source={path} data-note-line={line} data-note-end-line={line} tabIndex={contextActions ? 0 : undefined}>
            <dt>{key}</dt><dd>{value}</dd>
          </div>)}
        </dl>}
        <ReadingContext.Provider value={{ ...props, anchorScope }}>
          <ReactMarkdown skipHtml urlTransform={(url) => trustedPreviewUrl(url, path)} remarkPlugins={markdownPlugins}
            rehypePlugins={renderedLine === undefined ? [[markReadingOrigins, origins], ...readingPlugins]
              : [[markReadingOrigins, origins], [markReadingLine, renderedLine], ...readingPlugins]}
            components={components}>
            {markdown}
          </ReactMarkdown>
        </ReadingContext.Provider>
      </article>
      {overlay}
    </div>
  );
}
