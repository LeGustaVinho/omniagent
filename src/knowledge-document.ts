import { markdownLanguage } from "@codemirror/lang-markdown";
import { Text } from "@codemirror/state";
import { fileName, normalizeRepositoryPath, type ExampleFile } from "@/ide-knowledge-data";

export interface NoteTarget {
  raw: string;
  target: string;
  alias?: string;
  anchor?: string;
  embed: boolean;
  sourcePath?: string;
  sourceLine?: number;
  sourceOffset?: number;
  syntax?: "wiki" | "markdown";
  editableInline?: boolean;
}

export interface NoteLocation {
  path: string;
  anchor?: string;
}

export type PreviewLinkResolver = (link: NoteTarget, path: string, files: readonly ExampleFile[]) => NoteLocation[];

export interface NoteHeading {
  label: string;
  anchor: string;
  line: number;
  level: number;
}

interface Frontmatter {
  start: string;
  body: string;
  end: string;
}

const frontmatterPattern = /^(\uFEFF?---[ \t]*\r?\n)([\s\S]*?)^(---[ \t]*(?:\r?\n|$))/m;
const wikiPattern = /(!?)\[\[([^\]\r\n]+)\]\]/g;

function frontmatter(content: string): Frontmatter | undefined {
  const result = frontmatterPattern.exec(content);
  if (result?.index !== 0 || !result[1] || result[2] === undefined || !result[3]) return;
  return { start: result[1], body: result[2], end: result[3] };
}

function lineParts(line: string) {
  const found = /^([\w-]+)(:[ \t]*)(.*)$/.exec(line);
  if (!found?.[1] || !found[2]) return;
  const body = found[3] ?? "";
  const comment = /^(?:[^"'#]|"[^"]*"|'[^']*')*?(\s+#[^\r\n]*)$/.exec(body)?.[1] ?? "";
  return { key: found[1], delimiter: found[2], value: comment ? body.slice(0, -comment.length) : body, comment };
}

export function getProperties(content: string) {
  return (frontmatter(content)?.body.split(/\r?\n/) ?? []).flatMap((line, index) => {
    const parts = lineParts(line);
    return parts ? [{ key: parts.key, value: parts.value.trim(), line: index + 2 }] : [];
  });
}

export function updateProperty(content: string, key: string, value: string) {
  const block = frontmatter(content);
  if (!block) return content;
  const valueLine = value.replace(/[\r\n]+/g, " ");
  let replaced = false;
  const body = block.body.split(/(\r?\n)/).map((line) => {
    const parsed = lineParts(line);
    if (replaced || parsed?.key !== key) return line;
    replaced = true;
    return parsed.key + parsed.delimiter + valueLine + parsed.comment;
  }).join("");
  const range = block.start.length + block.body.length + block.end.length;
  return block.start + body + block.end + content.slice(range);
}

export function withoutProperties(content: string) {
  const block = frontmatter(content);
  return block ? content.slice(block.start.length + block.body.length + block.end.length) : content;
}

function byMarkdownLine(content: string, map: (line: string, lineIndex: number) => string, inComment?: () => boolean) {
  let fence: string | undefined;
  return content.split(/(\r?\n)/).map((line, index) => {
    if (line === "\n" || line === "\r\n") return line;
    const lineIndex = index / 2;
    if (!fence && inComment?.()) return map(line, lineIndex);
    const boundary = /^ {0,3}(`{3,}|~{3,})/.exec(line)?.[1];
    if (boundary && (!fence || fence[0] === boundary[0] && boundary.length >= fence.length)) {
      fence = fence ? undefined : boundary;
      return line;
    }
    return fence ? line : map(line, lineIndex);
  }).join("");
}

function byText(content: string, map: (text: string, line: number) => string, stripComments = false) {
  let commenting = false;
  return byMarkdownLine(content, (line, lineIndex) => {
    let cursor = 0;
    let output = "";
    const fragment = (text: string) => commenting ? (stripComments ? "" : text) : map(text, lineIndex);
    for (const token of line.matchAll(/%%|(`+)(?!`)(.*?)\1(?!`)/g)) {
      const offset = token.index;
      output += fragment(line.slice(cursor, offset));
      if (token[0] === "%%") {
        commenting = !commenting;
        if (!stripComments) output += token[0];
      } else if (!commenting || !stripComments) {
        output += token[0];
      }
      cursor = offset + token[0].length;
    }
    return output + fragment(line.slice(cursor));
  }, () => commenting);
}

export function parseWiki(raw: string, embed = false): NoteTarget {
  const [address = "", ...alias] = raw.split("|");
  const [target = "", ...anchor] = address.split("#");
  return { raw, target: target.trim(), anchor: anchor.join("#").trim() || undefined, alias: alias.join("|").trim() || undefined, embed };
}

export function wikiLinks(source: string): NoteTarget[] {
  const outgoing: NoteTarget[] = [];
  byText(withoutProperties(source), (line) => {
    for (const match of line.matchAll(wikiPattern)) {
      if (match[2]) outgoing.push(parseWiki(match[2], match[1] === "!"));
    }
    return line;
  });
  return outgoing;
}

function unquote(value: string) {
  return value.replace(/^["']|["']$/g, "");
}

function yamlScalar(value: string) {
  const trimmed = value.trim();
  if (trimmed.startsWith('"') && trimmed.endsWith('"')) {
    try { return JSON.parse(trimmed) as string; } catch { return unquote(trimmed); }
  }
  return trimmed.startsWith("'") && trimmed.endsWith("'")
    ? trimmed.slice(1, -1).replaceAll("''", "'")
    : trimmed;
}

function noteAliases(source: string) {
  const lines = frontmatter(source)?.body.split(/\r?\n/) ?? [];
  for (const [index, line] of lines.entries()) {
    const property = lineParts(line);
    if (property?.key !== "aliases" && property?.key !== "alias") continue;
    const value = property.value.trim();
    if (value.startsWith("[") && value.endsWith("]")) {
      const entries = value.slice(1, -1).match(/"(?:\\.|[^"\\])*"|'(?:''|[^'])*'|[^,]+/g) ?? [];
      return entries.map(yamlScalar).filter(Boolean);
    }
    if (value) return [yamlScalar(value)];
    const aliases: string[] = [];
    for (const continuation of lines.slice(index + 1)) {
      const entry = /^\s*-\s+(.*?)\s*$/.exec(continuation)?.[1];
      if (entry) aliases.push(yamlScalar(lineParts("item: " + entry)?.value ?? entry));
      else if (continuation.trim() && !continuation.trimStart().startsWith("#")) break;
    }
    return aliases;
  }
  return [];
}

export function resolveWiki(link: NoteTarget, from: string, files: readonly ExampleFile[]): NoteLocation[] {
  const raw = link.target.replaceAll("\\", "/");
  const find = (path: string) => {
    const normalized = normalizeRepositoryPath(path);
    if (!normalized) return [];
    const key = normalized.toLowerCase();
    const candidates = /\.(?:md|markdown)$/i.test(key) ? [key] : [key, key + ".md", key + ".markdown"];
    return files.filter((file) => candidates.includes(file.path.toLowerCase())).map(({ path: canonical }) => ({ path: canonical, anchor: link.anchor }));
  };
  if (!raw) return link.anchor ? find(from) : [];
  if (raw.startsWith("/")) return find(raw.slice(1));
  const parent = from.slice(0, from.lastIndexOf("/") + 1);
  if (raw.startsWith("./") || raw.startsWith("../")) return find(parent + raw);
  if (raw.includes("/")) {
    const exact = find(raw);
    if (exact.length) return exact;
    return find(parent + raw);
  }
  const lower = raw.toLowerCase().replace(/\.(?:md|markdown)$/, "");
  return files.flatMap((file) => {
    const name = fileName(file.path).toLowerCase().replace(/\.(?:md|markdown)$/, "");
    const title = unquote(getProperties(file.content).find(({ key }) => key === "title")?.value ?? "").toLowerCase();
    return name === lower || title === lower || noteAliases(file.content).some((alias) => alias.toLowerCase() === lower)
      ? [{ path: file.path, anchor: link.anchor }] : [];
  });
}

function readableDocument(source: string) {
  const body = withoutProperties(source);
  const text = byText(body, (line) => line, true);
  const firstLine = source.slice(0, source.length - body.length).split("\n").length;
  const tree = markdownLanguage.parser.parse(text);
  const lines = Text.of(text.split("\n"));
  const lineAt = (offset: number) => firstLine + lines.lineAt(offset).number - 1;
  return { text, tree, firstLine, lineAt };
}

function documentHeadings({ text, tree, lineAt }: ReturnType<typeof readableDocument>): NoteHeading[] {
  const result: NoteHeading[] = [];
  tree.iterate({ enter({ name, node, from, to }) {
    const heading = /^(ATX|Setext)Heading([1-6])$/.exec(name);
    if (!heading) return;
    const markers = node.getChildren("HeaderMark");
    const start = heading[1] === "ATX" ? markers[0]?.to ?? from : from;
    const end = heading[1] === "Setext" || markers.length > 1 ? markers.at(-1)?.from ?? to : to;
    const label = text.slice(start, end).replace(/\s+/g, " ").trim();
    if (label) result.push({ label, anchor: label, line: lineAt(from), level: Number(heading[2]) });
    return false;
  } });
  return result;
}

export function headings(content: string): NoteHeading[] {
  return documentHeadings(readableDocument(content));
}

export function anchorKey(label: string) {
  return label.normalize("NFKC").toLowerCase().trim().replace(/[*`~]/g, "").replace(/[\s-]+/g, "-");
}

export function anchorLine(source: string, target: string): number | undefined {
  if (target.startsWith("^")) return findBlock(source, target)?.line;
  return headings(source).find(({ anchor }) => anchorKey(anchor) === anchorKey(target))?.line;
}

function findBlock(source: string, target: string) {
  if (!/^\^[\w-]+$/.test(target)) return;
  const { text, tree, lineAt } = readableDocument(source);
  const markerPattern = new RegExp(`(^|[ \\t])\\${target}[ \\t]*$`, "m");
  let result: { line: number; content: string } | undefined;
  tree.iterate({ enter({ name, node, from, to }) {
    if (result) return false;
    if (name !== "Paragraph") return;
    const paragraph = text.slice(from, to);
    const marker = markerPattern.exec(paragraph);
    if (!marker) return false;
    const offset = from + marker.index + marker[1]!.length;
    const syntax = tree.resolveInner(offset + 1);
    if (["InlineCode", "Link", "Image"].includes(syntax.name)) return false;
    if (paragraph.trim() === target) {
      const block = node.prevSibling;
      if (block) result = { line: lineAt(block.from), content: text.slice(block.from, block.to) };
    } else {
      const start = node.parent?.name === "ListItem" ? node.parent.from : from;
      const lineEnd = offset + target.length;
      const content = (text.slice(start, from + marker.index).trimEnd() + text.slice(lineEnd, to)).trimEnd();
      if (content) result = { line: lineAt(start), content };
    }
    return false;
  } });
  return result;
}

function excerpt(source: string, target?: string) {
  if (!target) return withoutProperties(source);
  if (target.startsWith("^")) return findBlock(source, target)?.content;
  const document = readableDocument(source);
  const entries = documentHeadings(document);
  const heading = entries.find(({ anchor }) => anchorKey(anchor) === anchorKey(target));
  if (!heading) return;
  const { text, firstLine } = document;
  const lines = text.split(/\r?\n/);
  const next = entries.find(({ line, level }) => line > heading.line && level <= heading.level)?.line;
  return lines.slice(heading.line - firstLine, next === undefined ? undefined : next - firstLine).join("\n");
}

export function noteAnchorExcerpt(source: string, anchor: string) {
  const firstLine = anchorLine(source, anchor);
  const text = excerpt(source, anchor)?.trimEnd();
  if (!firstLine || !text) return;
  return { text, startLine: firstLine, endLine: firstLine + text.split(/\r?\n/).length - 1, language: "markdown" };
}

function replaceWikiAddress(content: string, replace: (link: NoteTarget) => string | undefined) {
  const text = withoutProperties(content);
  const rewritten = byText(text, (line) => line.replace(wikiPattern, (original: string, embed: string, raw: string) => {
    const link = parseWiki(raw, embed === "!");
    const address = replace(link);
    return address ? `${embed}[[${address}${link.anchor ? "#" + link.anchor : ""}${link.alias ? "|" + link.alias : ""}]]` : original;
  }));
  return content.slice(0, content.length - text.length) + rewritten;
}

export function repairWikiAddress(content: string, raw: string, to: string, anchor?: string, sourceLine?: number) {
  const text = withoutProperties(content);
  const firstLine = content.slice(0, content.length - text.length).split("\n").length;
  let matches = 0;
  const updated = byText(text, (line, index) => line.replace(wikiPattern, (original: string, embed: string, entry: string) => {
    if (entry !== raw || sourceLine !== undefined && index + firstLine !== sourceLine) return original;
    matches += 1;
    const { alias } = parseWiki(entry);
    return `${embed}[[${to}${anchor ? "#" + anchor : ""}${alias ? "|" + alias : ""}]]`;
  }));
  if (sourceLine !== undefined && matches !== 1) throw new Error("The source line no longer identifies one matching wikilink. Edit the source to disambiguate it.");
  return content.slice(0, content.length - text.length) + updated;
}

export function rewriteNoteReferences(content: string, originalPath: string, path: string, renamedPath: string, files: readonly ExampleFile[]) {
  return replaceWikiAddress(content, (link) => {
    if (!link.target) return;
    const targets = resolveWiki(link, originalPath, files);
    if (targets.length !== 1 || targets[0]?.path !== path) return;
    return renamedPath;
  });
}

function escapedLabel(label: string) {
  return label.replace(/[\\\[\]]/g, "\\$&");
}

export function readingWikiAddress(path: string, raw: string, line?: number, markdown?: { target: string; anchor?: string }) {
  return "omni-wiki:" + encodeURIComponent(JSON.stringify([path, raw, line ?? null, markdown ?? null]));
}

export function parseReadingAddress(href: string): NoteTarget | undefined {
  try {
    const input: unknown = JSON.parse(decodeURIComponent(href.slice("omni-wiki:".length)));
    if (!href.startsWith("omni-wiki:") || !Array.isArray(input) || typeof input[0] !== "string" || typeof input[1] !== "string") return;
    const [sourcePath, raw, line, markdown] = input;
    const sourceLine = typeof line === "number" && Number.isSafeInteger(line) && line > 0 ? line : undefined;
    if (markdown && typeof markdown === "object" && typeof markdown.target === "string") {
      const anchor = typeof markdown.anchor === "string" ? markdown.anchor : undefined;
      return { raw, target: markdown.target, anchor, embed: false, sourcePath, sourceLine, syntax: "markdown", editableInline: sourceLine !== undefined };
    }
    return { ...parseWiki(raw.replace(/^!/, ""), raw.startsWith("!")), sourcePath, sourceLine, syntax: "wiki" };
  } catch {
    return undefined;
  }
}

export function readingLocalAddress(path: string, destination: string, line?: number) {
  if (!destination || destination.startsWith("#") || /^[\\/]{2}/.test(destination) || /^[a-z][\w+.-]*:/i.test(destination)) return "";
  try {
    const [target, hash] = destination.split("#", 2).map(decodeURIComponent);
    if (!target || target.includes(":") || /^[\\/]{2}/.test(target)) return "";
    if (/\.(?:md|markdown)$/i.test(target)) {
      const relative = /^(?:\/|\.\.?\/)/.test(target) ? target : "./" + target;
      return readingWikiAddress(path, destination, line, { target: relative, anchor: hash || undefined });
    }
    return "omni-file:" + encodeURIComponent(JSON.stringify([path, target]));
  } catch {
    return "";
  }
}

export function markdownNoteLink(path: string, address: string, sourceLine?: number): NoteTarget | undefined {
  if (address.startsWith("#")) {
    if (/^#(?:user-content-)?fn(?:ref)?-/i.test(address)) return;
    try {
      const anchor = decodeURIComponent(address.slice(1));
      if (anchor) return { raw: address, target: "", anchor, embed: false, sourcePath: path, sourceLine, syntax: "markdown" };
    } catch { return; }
  }
  return parseReadingAddress(readingLocalAddress(path, address, sourceLine));
}

export function parseReadingFile(href: string): [string, string] | undefined {
  if (!href.startsWith("omni-file:")) return;
  try {
    const value: unknown = JSON.parse(decodeURIComponent(href.slice("omni-file:".length)));
    if (Array.isArray(value) && value.length === 2 && typeof value[0] === "string" && typeof value[1] === "string") {
      return [value[0], value[1]];
    }
  } catch {
    return undefined;
  }
}

const markdownDestination = /(\]\()(<[^>]+>|[^()\s]+)(?=[\s)])/g;

export function notePreviewLinks(source: string, path: string): NoteTarget[] {
  const links = wikiLinks(source);
  byText(withoutProperties(source), (text) => {
    for (const match of text.matchAll(markdownDestination)) {
      if (!match[2]) continue;
      const address = readingLocalAddress(path, match[2].replace(/^<|>$/g, ""));
      const link = parseReadingAddress(address);
      if (link) links.push(link);
    }
    return text;
  });
  return links;
}

export function prepareReading(source: string, path: string, files: readonly ExampleFile[], lineage: readonly string[] = [path], resolve: PreviewLinkResolver = resolveWiki): string {
  return readingDocument(source, path, files, resolve, undefined, lineage).markdown;
}

export type ReadingOrigin = { path: string; line: number };

export function readingDocument(source: string, path: string, files: readonly ExampleFile[], resolve: PreviewLinkResolver = resolveWiki,
  sourceLine?: number, lineage: readonly string[] = [path], originOffset = 0) {
  const body = withoutProperties(source);
  const content = byText(body, (text) => text, true);
  const firstLine = source.slice(0, source.length - body.length).split("\n").length + originOffset;
  let renderedLine = sourceLine === undefined ? undefined : Math.max(1, sourceLine - firstLine + 1);
  const expandedOrigins = new Map<number, ReadingOrigin[]>();
  const markdown = byMarkdownLine(content, (line, lineIndex) => {
    const origin = { path, line: firstLine + lineIndex };
    const wholeEmbed = /^\s*!\[\[([^\]]+)\]\]\s*$/.exec(line)?.[1];
    if (wholeEmbed) {
      const matches = resolve(parseWiki(wholeEmbed, true), path, files);
      const target = matches.length === 1 ? matches[0] : undefined;
      const note = files.find((file) => file.path === target?.path);
      if (target && note) {
        const anchor = target.anchor?.startsWith("^") ? target.anchor : target.anchor && anchorKey(target.anchor);
        const scope = anchor ? `${target.path}\0${anchor}` : target.path;
        if (lineage.includes(scope)) return "> This transclusion points to a note already in the current embed chain.";
        if (lineage.length >= 4) return "> This transclusion reached the maximum embed depth.";
        const excerptSource = excerpt(note.content, target.anchor);
        if (excerptSource !== undefined) {
          const startLine = target.anchor ? anchorLine(note.content, target.anchor) ?? 1 : readableDocument(note.content).firstLine;
          const embedded = readingDocument(excerptSource, target.path, files, resolve, undefined, [...lineage, scope], startLine - 1);
          const rows = embedded.markdown.split("\n");
          expandedOrigins.set(lineIndex, [origin, origin, ...embedded.origins]);
          if (sourceLine !== undefined && renderedLine !== undefined && firstLine + lineIndex < sourceLine) renderedLine += rows.length + 1;
          return `> **Embedded · ${fileName(target.path)}**\n>\n` + rows.map((row) => `> ${row}`).join("\n");
        }
      }
    }
    return byText(line, (text) => text.replace(markdownDestination,
      (original, prefix: string, destination: string) => {
        const url = readingLocalAddress(path, destination.replace(/^<|>$/g, ""), origin.line);
        return url ? prefix + url : original;
      })
    .replace(wikiPattern, (_entry: string, embed: string, raw: string) => {
      const { target, alias, anchor } = parseWiki(raw);
      const label = alias ?? (target || anchor || raw);
      return `[${escapedLabel(label)}](${readingWikiAddress(path, embed + raw, origin.line)})`;
    }).replace(/==([^=]+)==/g, (_match, value: string) => `[${escapedLabel(value)}](omni-mark:${encodeURIComponent(value)})`)
      .replace(/^> \[!([A-Za-z]+)\]\s*(.*)$/, (_match, kind: string, label: string) => `> **${kind}${label ? " · " + label : ""}**`));
  });
  const origins = content.split("\n").flatMap((_, index) => expandedOrigins.get(index) ?? [{ path, line: firstLine + index }]);
  return { markdown, renderedLine, origins };
}
