// @vitest-environment jsdom
import { EditorView } from "@codemirror/view";
import { foldedRanges, syntaxTree } from "@codemirror/language";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { MarkdownEditor } from "@/knowledge-editor";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });
const props = { path: "docs/release.md", files: [], onLink: vi.fn(), onChange: vi.fn() };

it("applies only the canonical ranges returned for the exact CodeMirror buffer", async () => {
  const pending: { revision: string; resolve: (value: object) => void }[] = [];
  vi.mocked(invoke).mockImplementation((_command, args) => new Promise((resolve) => {
    pending.push({ revision: (args as { bufferRevision: string }).bufferRevision, resolve });
  }));
  const { rerender } = render(<MarkdownEditor {...props} mode="live" source={"# First\n\n- [ ] Deploy"} analyze />);
  await waitFor(() => expect(pending).toHaveLength(1));
  const current = "# 🚀 Atual\n\n- [x] Deploy";
  const offset = current.indexOf("[x]");
  rerender(<MarkdownEditor {...props} mode="live" source={current} analyze />);
  await waitFor(() => expect(pending).toHaveLength(2));
  await act(async () => pending[1]!.resolve({ bufferRevision: pending[1]!.revision, nodes: [{
    kind: "task_marker", checked: true, status: "x", range: { from: offset, to: offset + 3 },
    valueRange: { from: offset + 1, to: offset + 2 },
  }] }));
  expect(screen.getByRole<HTMLInputElement>("checkbox", { name: "Toggle note task" }).checked).toBe(true);
  await act(async () => pending[0]!.resolve({ bufferRevision: pending[0]!.revision, nodes: [] }));
  expect(screen.getByRole<HTMLInputElement>("checkbox", { name: "Toggle note task" }).checked).toBe(true);
});

it("reveals a formatted source range when the cursor enters and keeps the editor across modes", async () => {
  const source = "# Release 🚀\n\nBefore **revisão** after";
  const from = source.indexOf("**");
  const to = source.lastIndexOf("**") + 2;
  vi.mocked(invoke).mockImplementation(async (_command, args) => ({
    bufferRevision: (args as { bufferRevision: string }).bufferRevision,
    nodes: [{ kind: "strong", range: { from, to }, markers: [{ from, to: from + 2 }, { from: to - 2, to }] }],
  }));
  const { container, rerender } = render(<MarkdownEditor {...props} mode="live" source={source} analyze />);
  const view = EditorView.findFromDOM(container.querySelector<HTMLElement>(".cm-editor")!)!;
  await waitFor(() => expect(view.contentDOM.querySelector(".cm-note-strong")?.textContent).toBe("revisão"));
  select(view, from + 3);
  expect(view.contentDOM.textContent).toContain("**revisão**");
  select(view, to + 1);
  expect(view.contentDOM.textContent).not.toContain("**revisão**");

  rerender(<MarkdownEditor {...props} mode="source" source={source} analyze />);
  expect(EditorView.findFromDOM(container.querySelector<HTMLElement>(".cm-editor")!)).toBe(view);
  expect(view.contentDOM.textContent).toContain("**revisão**");
  rerender(<MarkdownEditor {...props} mode="live" source={source} analyze />);
  await waitFor(() => expect(view.contentDOM.querySelector(".cm-note-strong")?.textContent).toBe("revisão"));
  expect(view.state.doc.toString()).toBe(source);
  expect(props.onChange).not.toHaveBeenCalled();
});

it("opens context on the exact concealed link and quotes the live selection with its source range", async () => {
  const source = "# Launch\n\nSee [[Plan#Ready|Plan details]].\n\nReview only\nthese lines.\n";
  const from = source.indexOf("[[");
  const to = source.indexOf("]]", from) + 2;
  const selected = vi.fn();
  vi.mocked(invoke).mockImplementation(async (_command, args) => ({
    bufferRevision: (args as { bufferRevision: string }).bufferRevision,
    nodes: [{ kind: "wikilink", range: { from, to }, target: "Plan", anchor: "Ready", alias: "Plan details" }],
  }));
  const { container } = render(<MarkdownEditor {...props} mode="live" source={source} analyze
    contextActions={(hit) => [[{ label: "Use note context", run: () => selected(hit) }]]} />);
  const link = await waitFor(() => {
    const widget = container.querySelector(".cm-note-link");
    expect(widget?.textContent).toBe("Plan details");
    return widget!;
  });
  fireEvent.contextMenu(link);
  fireEvent.click(screen.getByRole("menuitem", { name: "Use note context" }));
  expect(selected).toHaveBeenCalledWith(expect.objectContaining({ link: expect.objectContaining({
    raw: "Plan#Ready|Plan details", target: "Plan", sourcePath: props.path, sourceLine: 3, syntax: "wiki",
  }) }));

  const view = EditorView.findFromDOM(container.querySelector<HTMLElement>(".cm-editor")!)!;
  vi.spyOn(view, "coordsAtPos").mockReturnValue(null);
  select(view, source.indexOf("Review only"), source.length - 1);
  fireEvent.keyDown(view.contentDOM, { key: "F10", shiftKey: true });
  fireEvent.click(screen.getByRole("menuitem", { name: "Use note context" }));
  expect(selected).toHaveBeenLastCalledWith(expect.objectContaining({ excerpt: {
    path: props.path, language: "markdown", text: "Review only\nthese lines.", startLine: 5, endLine: 6,
  } }));
});

function openSource(source: string, mode: "source" | "live" = "source", contextual = false) {
  const onChange = vi.fn();
  function Controlled() {
    const [content, setContent] = useState(source);
    return <MarkdownEditor {...props} mode={mode} source={content} contextActions={contextual ? () => [] : undefined}
      onChange={(updated) => { onChange(updated); setContent(updated); }} />;
  }
  const { container } = render(<Controlled />);
  return { view: EditorView.findFromDOM(container.querySelector<HTMLElement>(".cm-editor")!)!, onChange };
}

const command = (name: string) => fireEvent.click(screen.getByRole("button", { name }));
const select = (view: EditorView, anchor: number, head = anchor) => act(() => view.dispatch({ selection: { anchor, head } }));

const expandFormat = () => fireEvent.keyDown(screen.getByRole("menuitem", { name: "Format Markdown" }), { key: "ArrowRight" });

it.each(["source", "live"] as const)("formats the selected text and inserts the Markdown link destination in %s mode", (mode) => {
  const source = "# Release\n\nRead café 🚀 and check the plan.";
  const { view } = openSource(source, mode, true);
  vi.spyOn(view, "coordsAtPos").mockReturnValue(null);
  const start = source.indexOf("café");
  const end = start + "café 🚀".length;
  select(view, start, end);
  fireEvent.keyDown(view.contentDOM, { key: "F10", shiftKey: true });
  expandFormat();
  fireEvent.click(screen.getByRole("menuitem", { name: "Bold" }));
  expect(view.state.doc.toString()).toBe(source.replace("café 🚀", "**café 🚀**"));
  command("Undo");
  expect(view.state.doc.toString()).toBe(source);

  select(view, start, end);
  fireEvent.keyDown(view.contentDOM, { key: "F10", shiftKey: true });
  expandFormat();
  fireEvent.click(screen.getByRole("menuitem", { name: "Insert Markdown link" }));
  const next = source.replace("café 🚀", "[café 🚀]()");
  expect(view.state.doc.toString()).toBe(next);
  expect(view.state.selection.main.head).toBe(next.indexOf(")"));
});

it("changes the clicked Setext heading while the editor cursor is elsewhere", () => {
  const source = "## Release ##\n\nRelease\n-------\n\nKeep the details intact.";
  const { view } = openSource(source, "source", true);
  select(view, 0, "## Release ##".length);
  vi.spyOn(view, "posAtCoords").mockReturnValue(source.lastIndexOf("Release") + 1);
  fireEvent.contextMenu(view.contentDOM);
  expandFormat();
  fireEvent.keyDown(screen.getByRole("menuitem", { name: "Heading" }), { key: "ArrowRight" });
  fireEvent.click(screen.getByRole("menuitem", { name: "Heading 4" }));
  expect(view.state.doc.toString()).toBe("## Release ##\n\n#### Release\n\nKeep the details intact.");
  command("Undo");
  expect(view.state.doc.toString()).toBe(source);
});

it.each(["source", "live"] as const)("folds the exact duplicate heading and keeps the full Setext title in %s mode", (mode) => {
  const source = "# Milestone\n\nFirst body\n\nMilestone\n=========\n\nSecond body\n\n# Next\n\nKeep the last section.";
  const { view, onChange } = openSource(source, mode, true);
  const second = source.indexOf("\nMilestone\n") + 1;
  vi.spyOn(view, "posAtCoords").mockReturnValue(second + 1);
  select(view, source.indexOf("First body"));
  fireEvent.contextMenu(view.contentDOM);
  fireEvent.click(screen.getByRole("menuitem", { name: "Collapse section" }));
  const expectedRange = { from: source.indexOf("\n\nSecond body"), to: source.indexOf("\n\n# Next") };
  const ranges: { from: number; to: number }[] = [];
  foldedRanges(view.state).between(0, source.length, (from, to) => { ranges.push({ from, to }); });
  expect(ranges).toEqual([expectedRange]);
  expect(view.state.doc.toString()).toBe(source);
  expect(onChange).not.toHaveBeenCalled();

  fireEvent.contextMenu(view.contentDOM);
  fireEvent.click(screen.getByRole("menuitem", { name: "Expand section" }));
  expect(foldedRanges(view.state).size).toBe(0);
  expect(view.state.doc.toString()).toBe(source);
});

it("makes and toggles list tasks through separate undo steps while preserving neighboring plugin syntax", () => {
  const before = "---\nkanban-plugin: board # comment\n---\n\nScore:: 5\n\n";
  const selected = "Build café\nShip release";
  const after = "\n\n- [/] Planned 📅 2026-10-05\n\n```dataview\nTABLE foo FROM #release\n```\n";
  const source = before + selected + after;
  const listed = before + "- Build café\n- Ship release" + after;
  const unchecked = before + "- [ ] Build café\n- [ ] Ship release" + after;
  const checked = before + "- [x] Build café\n- [x] Ship release" + after;
  const { view, onChange } = openSource(source);
  select(view, before.length, before.length + selected.length + 1);
  command("Insert list item");
  expect(view.state.doc.toString()).toBe(listed);
  command("Toggle task checkbox");
  expect(view.state.doc.toString()).toBe(unchecked);
  command("Toggle task checkbox");
  expect(view.state.doc.toString()).toBe(checked);
  expect(onChange.mock.calls.map(([text]) => text)).toEqual([listed, unchecked, checked]);

  for (const expected of [unchecked, listed, source]) {
    command("Undo");
    expect(view.state.doc.toString()).toBe(expected);
  }
  for (const expected of [listed, unchecked, checked]) {
    command("Redo");
    expect(view.state.doc.toString()).toBe(expected);
  }

  select(view, checked.indexOf("[/]"));
  command("Toggle task checkbox");
  select(view, checked.indexOf("TABLE foo"));
  command("Insert list item");
  command("Toggle task checkbox");
  command("Insert table");
  select(view, checked.indexOf("kanban-plugin"));
  command("Toggle task checkbox");
  expect(view.state.doc.toString()).toBe(checked);
});

it("keeps the original quote, ordered marker and task metadata while supporting list continuation by Enter", () => {
  const source = "> 3) [X] Release 🚀 📅 2026-10-05 ^ready";
  const { view } = openSource(source);
  select(view, source.indexOf("Release"));
  command("Toggle task checkbox");
  const unchecked = source.replace("[X]", "[ ]");
  expect(view.state.doc.toString()).toBe(unchecked);
  select(view, unchecked.length);
  fireEvent.keyDown(view.contentDOM, { key: "Enter" });
  expect(view.state.doc.toString()).toBe(unchecked + "\n> 4) ");
  command("Toggle task checkbox");
  expect(view.state.doc.toString()).toBe(unchecked + "\n> 4) [ ] ");
});

it("adds a row to an aligned GFM table and inserts a valid new table without splitting surrounding text", () => {
  const table = "| Label | | Notes |\n| :--- | ---: | :---: |\n| café | 1 | pipe \\| literal |";
  const source = "Title:: value\n\n" + table + "\n\nRead me carefully\nKeep this exact ^footer";
  const { view } = openSource(source);
  select(view, source.indexOf("Label"));
  command("Insert table");
  const extended = source.replace("| :--- | ---: | :---: |", "| :--- | ---: | :---: |\n|  |  |  |");
  expect(view.state.doc.toString()).toBe(extended);
  expect(view.state.doc.lineAt(view.state.selection.main.head).text).toBe("|  |  |  |");

  select(view, extended.indexOf("carefully"));
  command("Insert table");
  expect(view.state.doc.toString()).toBe(extended.replace("Read me carefully\n", "Read me carefully\n\n| Name | Status |\n| --- | --- |\n|  |  |\n\n"));
  expect(syntaxTree(view.state).topNode.getChildren("Table")).toHaveLength(2);
  command("Undo");
  expect(view.state.doc.toString()).toBe(extended);
  command("Undo");
  expect(view.state.doc.toString()).toBe(source);
});

it("finds and selects the next matching source text with the CodeMirror search panel", () => {
  const source = "# Café 🚀\n\nScore:: café\n";
  const { view, onChange } = openSource(source);
  command("Find in note");
  const field = screen.getByRole("textbox", { name: "Find" });
  fireEvent.change(field, { target: { value: "café" } });
  command("next");
  expect(view.state.selection.main.from).toBe(source.indexOf("Café"));
  command("next");
  const { from, to } = view.state.selection.main;
  expect(from).toBe(source.indexOf("café"));
  expect(view.state.sliceDoc(from, to)).toBe("café");
  expect(onChange).not.toHaveBeenCalled();
});
