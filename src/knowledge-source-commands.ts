import { isolateHistory } from "@codemirror/commands";
import { ensureSyntaxTree, type syntaxTree } from "@codemirror/language";
import { EditorSelection, Transaction, type ChangeSpec, type EditorState, type Line } from "@codemirror/state";
import type { Command, EditorView } from "@codemirror/view";

type Tree = ReturnType<typeof syntaxTree>;
type Node = ReturnType<Tree["resolveInner"]>;

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

function applyEdit(view: EditorView, edits: ChangeSpec, cursor?: number) {
  const changes = view.state.changes(edits);
  if (changes.empty) return false;
  view.dispatch({
    changes,
    selection: cursor === undefined ? view.state.selection.map(changes, 1) : EditorSelection.cursor(cursor),
    annotations: [Transaction.userEvent.of("input.markdown"), isolateHistory.of("full")],
  });
  return true;
}

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
