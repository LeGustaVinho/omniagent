// @vitest-environment jsdom
import { EditorView } from "@codemirror/view";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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

it("inserts a Source-mode list, task and GFM table without rewriting neighboring Markdown and supports undo", () => {
  const source = "---\nkanban-plugin: board\n---\n\nScore:: 5\n- [/] Planned 📅 2026-10-05\n";
  const { container } = render(<MarkdownEditor {...props} mode="source" source={source} />);
  const view = EditorView.findFromDOM(container.querySelector<HTMLElement>(".cm-editor")!)!;
  act(() => view.dispatch({ selection: { anchor: source.length } }));
  fireEvent.click(screen.getByRole("button", { name: "Insert list item" }));
  fireEvent.click(screen.getByRole("button", { name: "Insert task" }));
  fireEvent.click(screen.getByRole("button", { name: "Insert table" }));
  expect(view.state.doc.toString()).toBe(source + "\n- \n- [ ] \n| Name | Status |\n| --- | --- |\n|  |  |\n");
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  expect(view.state.doc.toString().startsWith(source)).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Redo" }));
  expect(view.state.doc.toString()).toContain("| Name | Status |");
});
