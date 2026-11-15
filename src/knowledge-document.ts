import { fileName, normalizeRepositoryPath, type ExampleFile } from "@/ide-knowledge-data";

export interface NoteTarget {
  raw: string;
  target: string;
  alias?: string;
  anchor?: string;
  embed: boolean;
}

export interface NoteLocation {
  path: string;
  anchor?: string;
}

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
  return (frontmatter(content)?.body.split(/\r?\n/) ?? []).flatMap((line) => {
    const parts = lineParts(line);
    return parts ? [{ key: parts.key, value: parts.value.trim() }] : [];
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

function byMarkdownLine(content: string, map: (line: string) => string) {
  let fence: string | undefined;
  return content.split(/(\r?\n)/).map((line) => {
    if (line === "\n" || line === "\r\n") return line;
    const boundary = /^ {0,3}(`{3,}|~{3,})/.exec(line)?.[1];
    if (boundary && (!fence || fence[0] === boundary[0] && boundary.length >= fence.length)) {
      fence = fence ? undefined : boundary;
      return line;
    }
    return fence ? line : map(line);
  }).join("");
}

function byText(content: string, map: (text: string) => string, stripComments = false) {
  let commenting = false;
  return byMarkdownLine(content, (line) => {
    let cursor = 0;
    let output = "";
    const fragment = (text: string) => commenting ? (stripComments ? "" : text) : map(text);
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
  });
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

function noteAliases(source: string) {
  const aliases = getProperties(source).find(({ key }) => key === "aliases")?.value ?? "";
  return aliases.replace(/^\[|\]$/g, "").split(",").map((alias) => unquote(alias.trim().toLowerCase())).filter(Boolean);
}

export function resolveWiki(link: NoteTarget, from: string, files: readonly ExampleFile[]): NoteLocation[] {
  const raw = link.target.replaceAll("\\", "/");
  const find = (path: string) => {
    const normalized = normalizeRepositoryPath(path);
    if (!normalized) return [];
    const candidates = [normalized.toLowerCase(), (normalized + ".md").toLowerCase()];
    return files.filter((file) => candidates.includes(file.path.toLowerCase())).map(({ path: canonical }) => ({ path: canonical, anchor: link.anchor }));
  };
  if (!raw) return link.anchor ? find(from) : [];
  if (raw.startsWith("/")) return find(raw.slice(1));
  if (raw.includes("/")) {
    const exact = find(raw);
    if (exact.length) return exact;
    return find(`${from.slice(0, from.lastIndexOf("/") + 1)}${raw}`);
  }
  const lower = raw.toLowerCase().replace(/\.md$/, "");
  return files.flatMap((file) => {
    const name = fileName(file.path).toLowerCase().replace(/\.md$/, "");
    const title = unquote(getProperties(file.content).find(({ key }) => key === "title")?.value ?? "").toLowerCase();
    return name === lower || title === lower || noteAliases(file.content).includes(lower) ? [{ path: file.path, anchor: link.anchor }] : [];
  });
}

export function headings(content: string): NoteHeading[] {
  const body = withoutProperties(content);
  const skippedLines = content.slice(0, content.length - body.length).split(/\r?\n/).length - 1;
  const lines = byText(body, (line) => line, true).split(/\r?\n/);
  const result: NoteHeading[] = [];
  let fence: string | undefined;
  for (const [index, line] of lines.entries()) {
    const boundary = /^ {0,3}(`{3,}|~{3,})/.exec(line)?.[1];
    if (boundary && (!fence || fence[0] === boundary[0] && boundary.length >= fence.length)) {
      fence = fence ? undefined : boundary;
      continue;
    }
    if (fence) continue;
    const heading = /^(#{1,6})\s+(.+?)\s*#*\s*$/.exec(line);
    if (heading?.[1] && heading[2]) result.push({ label: heading[2], anchor: heading[2], line: skippedLines + index + 1, level: heading[1].length });
  }
  return result;
}

export function anchorKey(label: string) {
  return label.normalize("NFKC").toLowerCase().trim().replace(/[*`~]/g, "").replace(/[\s-]+/g, "-");
}

export function anchorLine(source: string, target: string): number | undefined {
  if (target.startsWith("^")) {
    const line = source.split(/\r?\n/).findIndex((entry) => entry.endsWith(" " + target) || entry.trim() === target);
    return line >= 0 ? line + 1 : undefined;
  }
  return headings(source).find(({ anchor }) => anchorKey(anchor) === anchorKey(target))?.line;
}

function excerpt(source: string, target?: string) {
  if (!target) return withoutProperties(source);
  const line = anchorLine(source, target);
  if (!line) return;
  const lines = source.split(/\r?\n/);
  if (target.startsWith("^")) return lines[line - 1]?.replace(/\s+\^[\w-]+$/, "");
  const entries = headings(source);
  const level = entries.find((heading) => heading.line === line)?.level ?? 6;
  const next = entries.find((heading) => heading.line > line && heading.level <= level)?.line;
  return lines.slice(line - 1, next && next - 1).join("\n");
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

export function repairWikiAddress(content: string, raw: string, to: string, anchor?: string) {
  const text = withoutProperties(content);
  const updated = byText(text, (line) => line.replace(wikiPattern, (original: string, embed: string, entry: string) => {
    if (entry !== raw) return original;
    const { alias } = parseWiki(entry);
    return `${embed}[[${to}${anchor ? "#" + anchor : ""}${alias ? "|" + alias : ""}]]`;
  }));
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

export function prepareReading(source: string, path: string, files: readonly ExampleFile[], lineage: readonly string[] = [path]): string {
  const prepared = byMarkdownLine(withoutProperties(source), (line) => {
    const wholeEmbed = /^\s*!\[\[([^\]]+)\]\]\s*$/.exec(line)?.[1];
    if (!wholeEmbed) return line;
    const matches = resolveWiki(parseWiki(wholeEmbed, true), path, files);
    const target = matches.length === 1 ? matches[0] : undefined;
    const note = files.find((file) => file.path === target?.path);
    if (!target || !note) return line;
    const content = lineage.includes(target.path) || lineage.length >= 4
      ? "This transclusion points to a note already in the current embed chain."
      : excerpt(note.content, target.anchor);
    if (!content) return line;
    const embedded = content === "This transclusion points to a note already in the current embed chain."
      ? content : prepareReading(content, target.path, files, [...lineage, target.path]);
    return `> **Embedded · ${fileName(target.path)}**\n>\n` + embedded.split("\n").map((row) => `> ${row}`).join("\n");
  });
  return byText(prepared, (text) => text.replace(wikiPattern, (_entry: string, embed: string, raw: string) => {
    const { target, alias, anchor } = parseWiki(raw);
    const label = alias ?? (target || anchor || raw);
    return `[${escapedLabel(label)}](omni-wiki:${encodeURIComponent(embed + raw)})`;
  }).replace(/==([^=]+)==/g, (_match, value: string) => `[${escapedLabel(value)}](omni-mark:${encodeURIComponent(value)})`)
    .replace(/^> \[!([A-Za-z]+)\]\s*(.*)$/, (_match, kind: string, label: string) => `> **${kind}${label ? " · " + label : ""}**`), true);
}
