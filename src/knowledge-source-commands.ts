import { isolateHistory } from "@codemirror/commands";
import { ensureSyntaxTree, type syntaxTree } from "@codemirror/language";
import { EditorSelection, Transaction, type ChangeSpec, type EditorState, type Line } from "@codemirror/state";
import type { Command, EditorView } from "@codemirror/view";

type Tree = ReturnType<typeof syntaxTree>;
type Node = ReturnType<Tree["resolveInner"]>;
type Span = { from: number; to: number };

const literalBlocks = ["FencedCode", "CodeBlock", "HTMLBlock", "CommentBlock"];
const linePrefix = (line: Line) => /^[\t ]*(?:>[\t ]*)*/.exec(line.text)![0];

function findBlock(node: Node, names: readonly string[]) {
  for (let current: Node | null = node; current; current = current.parent) {
    if (names.includes(current.name)) return current;
  }
  return null;
}

function frontmatterEnd(state: EditorState) {
  const { doc } = state;
  if (!/^\uFEFF?---[\t ]*$/.test(doc.line(1).text)) return 0;
  for (let number = 2; number <= doc.lines; number++) {
    if (/^(?:---|\.\.\.)[\t ]*$/.test(doc.line(number).text)) return number;
  }
  return 0;
}

function lastSelectedLine(state: EditorState) {
  const { from, to } = state.selection.main;
  return state.doc.lineAt(from === to ? to : to - 1);
}

function applyEdit(view: EditorView, edits: ChangeSpec, selection?: number | Span) {
  const changes = view.state.changes(edits);
  if (changes.empty) return false;
  view.dispatch({
    changes,
    selection: selection === undefined ? view.state.selection.map(changes, 1)
      : typeof selection === "number" ? EditorSelection.cursor(selection) : EditorSelection.range(selection.from, selection.to),
    annotations: [Transaction.userEvent.of("input.markdown"), isolateHistory.of("full")],
  });
  return true;
}

function editableBlock(state: EditorState, offset: number) {
  const line = state.doc.lineAt(offset);
  if (line.number <= frontmatterEnd(state)) return;
  const tree = ensureSyntaxTree(state, line.to, 35);
  const node = tree?.resolveInner(offset, offset === line.to ? -1 : 1);
  if (!node || findBlock(node, literalBlocks)) return;
  return { line, node };
}

export function canFormatMarkdown(state: EditorState, { from, to }: Span) {
  return Boolean(editableBlock(state, from) && editableBlock(state, from === to ? to : to - 1));
}

export function canFormatInlineMarkdown(state: EditorState, { from, to }: Span) {
  if (!canFormatMarkdown(state, { from, to }) || state.doc.lineAt(from).number !== state.doc.lineAt(to).number) return false;
  const tree = ensureSyntaxTree(state, to, 35);
  const node = tree?.resolveInner(from, 1);
  return Boolean(node && !findBlock(node, ["URL", "LinkTitle", "LinkLabel", "InlineCode"]));
}

export function surroundInlineMarkdown(marker: "**" | "*" | "~~" | "`"): Command {
  return (view) => {
    const { state } = view;
    const { from, to } = state.selection.main;
    if (!canFormatInlineMarkdown(state, { from, to })) return false;
    const text = state.sliceDoc(from, to);
    if (marker === "`" && text.includes("`")) {
      const fence = "`".repeat(Math.max(...Array.from(text.matchAll(/`+/g), ([run]) => run.length)) + 1);
      return applyEdit(view, { from, to, insert: `${fence} ${text} ${fence}` },
        { from: from + fence.length + 1, to: to + fence.length + 1 });
    }
    const start = from + (text.length - text.trimStart().length);
    const end = from + text.trimEnd().length;
    if (end < start) return false;
    const body = state.sliceDoc(start, end);
    return applyEdit(view, { from: start, to: end, insert: marker + body + marker },
      { from: start + marker.length, to: end + marker.length });
  };
}

export const insertMarkdownLink: Command = (view) => {
  const { state } = view;
  const { from, to } = state.selection.main;
  if (!canFormatInlineMarkdown(state, { from, to })) return false;
  const tree = ensureSyntaxTree(state, to, 35);
  if (tree && findBlock(tree.resolveInner(from, 1), ["Link", "Image"])) return false;
  const selected = state.sliceDoc(from, to);
  const label = selected.replace(/(?<!\\)[\[\]]/g, "\\$&") || "link";
  return applyEdit(view, { from, to, insert: `[${label}]()` },
    selected ? from + label.length + 3 : { from: from + 1, to: from + label.length + 1 });
};

function headingEditAt(state: EditorState, offset: number) {
  const block = editableBlock(state, offset);
  if (!block || findBlock(block.node, ["Table"])) return;
  const { line, node } = block;
  const syntax = ensureSyntaxTree(state, line.to, 35);
  const lineStart = line.from + linePrefix(line).length;
  for (let current: Node | null = syntax?.resolveInner(lineStart, 1) ?? node; current; current = current.parent) {
    const heading = /^(ATX|Setext)Heading([1-6])$/.exec(current.name);
    if (!heading) continue;
    const markers = current.getChildren("HeaderMark");
    const first = markers[0];
    if (!first) return;
    const level = Number(heading[2]);
    if (heading[1] === "Setext") {
      const caption = state.doc.lineAt(current.from);
      const underline = state.doc.lineAt(first.from);
      if (caption.number + 1 !== underline.number) return;
      return { kind: "setext" as const, level, start: current.from, underline: { from: caption.to, to: underline.to } };
    }
    const padding = /^[\t ]*/.exec(state.sliceDoc(first.to, line.to))![0];
    const end = first.to + padding.length;
    const closing = markers[1];
    return { kind: "atx" as const, level, open: { from: first.from, to: first.to }, paddingEnd: end,
      close: closing && { from: end + state.sliceDoc(end, closing.from).trimEnd().length, to: line.to } };
  }
  const paragraph = syntax?.resolveInner(lineStart, 1);
  if (!line.text.trim() || paragraph && findBlock(paragraph, ["Paragraph"])) return { kind: "paragraph" as const, level: 0, start: lineStart };
}

export function markdownHeadingLevel(state: EditorState, offset: number) {
  return headingEditAt(state, offset)?.level;
}

export function setMarkdownHeading(level: number): Command {
  return (view) => {
    const heading = headingEditAt(view.state, view.state.selection.main.from);
    if (!heading || level === heading.level) return false;
    const marker = "#".repeat(level);
    if (heading.kind === "paragraph") return applyEdit(view, { from: heading.start, insert: `${marker} ` });
    if (heading.kind === "setext") return applyEdit(view, [
      ...(level ? [{ from: heading.start, insert: `${marker} ` }] : []), heading.underline,
    ]);
    if (level) return applyEdit(view, { ...heading.open, insert: marker });
    return applyEdit(view, [{ from: heading.open.from, to: heading.paddingEnd }, ...(heading.close ? [heading.close] : [])]);
  };
}

export const insertCodeBlock: Command = (view) => {
  const { state } = view;
  const { from, to } = state.selection.main;
  if (!canFormatMarkdown(state, { from, to })) return false;
  const value = state.sliceDoc(from, to);
  const width = Array.from(value.matchAll(/`+/g), ([run]) => run.length).reduce((maximum, length) => Math.max(maximum, length + 1), 3);
  const fence = `\n${"`".repeat(width)}\n`;
  return applyEdit(view, { from, to, insert: fence + value + fence }, from + fence.length + value.length);
};

function markLines(view: EditorView, task: boolean) {
  const { state } = view;
  const first = state.doc.lineAt(state.selection.main.from);
  const last = lastSelectedLine(state);
  const tree = ensureSyntaxTree(state, last.to, 35);
  if (!tree) return false;
  const propertiesEnd = frontmatterEnd(state);
  const changes: ChangeSpec[] = [];
  for (let number = Math.max(first.number, propertiesEnd + 1); number <= last.number; number++) {
    const line = state.doc.line(number);
    if (first.number !== last.number && !line.text.trim()) continue;
    const start = line.from + linePrefix(line).length;
    const node = tree.resolveInner(start, 1);
    if (findBlock(node, literalBlocks) || findBlock(node, ["Table"])) continue;
    if (node.name !== "ListMark" || node.from !== start) {
      changes.push({ from: start, insert: task ? "- [ ] " : "- " });
      continue;
    }
    if (!task) continue;

    const content = state.sliceDoc(node.to, line.to);
    const padding = /^[\t ]*/.exec(content)![0];
    const marker = /^\[([^\]])\](?=[\t ]|$)/u.exec(content.slice(padding.length));
    const at = node.to + padding.length;
    if (!marker) {
      changes.push({ from: at, insert: padding ? "[ ] " : " [ ] " });
    } else if (marker[1] === " " || marker[1] === "x" || marker[1] === "X") {
      changes.push({ from: at + 1, to: at + 2, insert: marker[1] === " " ? "x" : " " });
    }
  }
  return applyEdit(view, changes);
}

export const insertListItem: Command = (view) => markLines(view, false);
export const toggleTaskCheckbox: Command = (view) => markLines(view, true);

export const insertTable: Command = (view) => {
  const { state } = view;
  const { doc } = state;
  const line = lastSelectedLine(state);
  const tree = ensureSyntaxTree(state, line.to, 35);
  if (!tree || line.number <= frontmatterEnd(state)) return false;
  const node = tree.resolveInner(line.from + linePrefix(line).length, 1);
  if (findBlock(node, literalBlocks)) return false;
  const table = findBlock(node, ["Table"]);
  const separator = table?.getChild("TableDelimiter");
  if (separator) {
    const columns = state.sliceDoc(separator.from, separator.to).split("|").filter((cell) => cell.trim()).length;
    const row = doc.lineAt(Math.max(line.from, separator.from));
    const prefix = "\n" + linePrefix(row);
    return applyEdit(view, { from: row.to, insert: `${prefix}|${"  |".repeat(columns)}` }, row.to + prefix.length + 2);
  }

  const before = line.length ? "\n\n" : line.number > 1 && doc.line(line.number - 1).text.trim() ? "\n" : "";
  const after = line.number < doc.lines && doc.line(line.number + 1).text.trim() ? "\n" : "";
  const header = "| Name | Status |\n| --- | --- |\n";
  return applyEdit(view, { from: line.to, insert: `${before}${header}|  |  |${after}` }, line.to + before.length + header.length + 2);
};
