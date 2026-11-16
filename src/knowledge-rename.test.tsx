// @vitest-environment jsdom

import { EditorView } from "@codemirror/view";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import KnowledgeWorkspace from "@/knowledge-workspace";
import type { NoteRevision } from "@/repository-note-buffers";
import type { WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  Channel: class { onmessage = () => {}; },
}));

const rangeLayout = { getClientRects: () => [], getBoundingClientRect: () => new DOMRect() };
const missingRangeMethods = Object.keys(rangeLayout).filter((name) => !(name in Range.prototype)) as (keyof typeof rangeLayout)[];

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class {
    observe() {}
    unobserve() {}
    disconnect() {}
  });
  for (const name of missingRangeMethods) {
    Object.defineProperty(Range.prototype, name, { configurable: true, value: rangeLayout[name] });
  }
});

afterEach(() => {
  cleanup();
  for (const name of missingRangeMethods) Reflect.deleteProperty(Range.prototype, name);
  vi.unstubAllGlobals();
  vi.resetAllMocks();
});

const checkoutId = "checkout";
const source = "notes/Release.md";
const destination = "docs/Lançamento.md";
const cleanLink = "planning/Runbook.md";
const racedLink = "planning/Concurrent.md";
const sourceRevision = "saved-source";
const movedRevision = "saved-after-move";
const previewId = "sealed-rename-preview";
const before = "[[notes/Release#Checklist|Release readiness]]";
const after = "[[docs/Lançamento#Checklist|Release readiness]]";
const props: WorkspaceProps = {
  checkout: { projectId: "project", checkoutId, repositoryRoot: "C:\\repo", checkoutRoot: "C:\\repo" },
  target: "notes/release.md",
  simulation: null,
  startSimulation: vi.fn(), openActivity: vi.fn(), openChanges: vi.fn(), openWorkspace: vi.fn(),
};

function setupRepository() {
  let moved = false;
  const original = "## Checklist\n\nThe prior saved note revision.\n";
  const movedMarkdown = "## Checklist\n\nThe repository returned the moved revision.\n";
  const recovery = {
    message: "A concurrent edit was preserved.",
    editedPath: "C:\\repo\\planning\\Concurrent.omniagent-edited.md",
    displacedPath: "C:\\repo\\planning\\Concurrent.omniagent-displaced.md",
  };
  const warnings = [{ path: "planning/Ambiguous.md", line: 11, message: "The original link was ambiguous." }];
  const documentAt = (path: string, id: string, markdown: string) => ({
    status: "document",
    document: {
      markdown, fileIdentity: "source-file",
      revision: { checkoutId, id, relativePath: path, sha256: `${id}-sha`, baseCommit: null } satisfies NoteRevision,
    },
  });
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const input = (args ?? {}) as { path: string; baseRevision: string; bufferRevision: string };
    switch (command) {
      case "workbench_layout": return null;
      case "save_workbench_layout": return;
      case "watch_repository_note_index": return "index-watch";
      case "unwatch_repository_note_index": return;
      case "rebuild_repository_note_index":
      case "reconcile_repository_note_index": return [
        { id: 1, path: moved ? destination : source, frontmatter: null, title: "Release", tags: [] },
        { id: 2, path: cleanLink, frontmatter: null, title: "Planning", tags: [] },
        { id: 3, path: racedLink, frontmatter: null, title: "Concurrent", tags: [] },
      ];
      case "repository_note_catalog":
      case "list_repository_markdown_drafts": return [];
      case "read_repository_markdown_draft": return null;
      case "watch_repository_note": return `watch-${input.baseRevision}`;
      case "unwatch_repository_note": return;
      case "analyze_note_markdown": return { bufferRevision: input.bufferRevision, nodes: [] };
      case "open_repository_document":
        if (input.path.toLowerCase() === source.toLowerCase() && !moved) return documentAt(source, sourceRevision, original);
        if (input.path === destination && moved) return documentAt(destination, movedRevision, movedMarkdown);
        return { status: "missing", relativePath: input.path };
      case "preview_markdown_rename": return {
        id: previewId, checkoutId, source, destination, sourceRevisionId: sourceRevision, warnings,
        files: [
          { path: cleanLink, revisionId: "stable-runbook", edits: [{ line: 4, byteStart: 24, byteEnd: 70, before, after }] },
          { path: racedLink, revisionId: "reviewed-concurrent", edits: [{ line: 9, byteStart: 86, byteEnd: 132, before, after }] },
        ],
      };
      case "apply_markdown_rename":
        moved = true;
        return {
          previewId, source, destination, partial: true, outcome: { status: "moved", revisionId: movedRevision },
          warnings,
          files: [
            { path: cleanLink, status: "rewritten", revisionId: "rewritten-runbook", count: 1 },
            { path: racedLink, status: "changed", message: "The note changed after preview.", recovery },
          ],
        };
      default: throw new Error(`Unexpected IPC command ${command}`);
    }
  });
  return { recovery, movedMarkdown };
}

it("reviews pinned link edits, applies the sealed partial rename, and reloads the canonical note tab", async () => {
  const { recovery, movedMarkdown } = setupRepository();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const workspace = (target = props.target) =>
    <QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={target} /></QueryClientProvider>;
  const { rerender } = render(workspace());
  const user = userEvent.setup();
  await screen.findByRole("button", { name: "Close Release.md" });
  await user.click(screen.getByRole("button", { name: "Split Reading view" }));
  await screen.findByLabelText(`Reading view for ${source}`);
  const rename = screen.getByRole("button", { name: "Rename note" });
  await user.click(rename);
  const inputDialog = await screen.findByRole("dialog", { name: "Rename note" });
  const newPath = within(inputDialog).getByRole("textbox", { name: "New path" });
  await user.clear(newPath);
  await user.type(newPath, destination);
  await user.click(within(inputDialog).getByRole("button", { name: "Preview link rewrites" }));

  const review = within(await screen.findByRole("dialog", { name: "Review link rewrites" }));
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "preview_markdown_rename")).toEqual([
    ["preview_markdown_rename", { checkoutId, source, destination, expectedSourceRevision: sourceRevision }],
  ]);
  expect(review.getByText(`${source} → ${destination}`)).toBeTruthy();
  for (const path of [cleanLink, racedLink]) {
    const edits = within(review.getByText(path).closest("details")!);
    expect(edits.getByText(before)).toBeTruthy();
    expect(edits.getByText(after)).toBeTruthy();
  }
  expect(review.getByText(/planning\/Ambiguous\.md · Line 11/)).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "apply_markdown_rename")).toBe(false);

  const invocationCount = vi.mocked(invoke).mock.calls.length;
  await user.click(review.getByRole("button", { name: "Apply reviewed changes" }));
  const result = within(await screen.findByRole("dialog", { name: "Rename result" }));
  const applied = vi.mocked(invoke).mock.calls.filter(([command]) => command === "apply_markdown_rename");
  expect(applied).toEqual([["apply_markdown_rename", { checkoutId, previewId }]]);
  expect(result.getByText("Note moved. Some link rewrites require attention.")).toBeTruthy();
  expect(result.getByText(cleanLink)).toBeTruthy();
  expect(result.getByText("1 link rewrite applied.")).toBeTruthy();
  const conflict = within(result.getByText(racedLink).closest("li")!);
  expect(conflict.getByText(recovery.editedPath)).toBeTruthy();
  expect(conflict.getByText(recovery.displacedPath)).toBeTruthy();
  expect(conflict.getByText(recovery.message)).toBeTruthy();

  await user.click(result.getByRole("button", { name: "Close" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(await screen.findAllByRole("button", { name: "Close Lançamento.md" })).toHaveLength(1);
  expect(screen.queryByRole("button", { name: "Close Release.md" })).toBeNull();
  expect(screen.queryByLabelText(`Reading view for ${source}`)).toBeNull();
  const editor = await screen.findByLabelText(`Markdown editor for ${destination}`);
  expect(EditorView.findFromDOM(editor.closest<HTMLElement>(".cm-editor")!)?.state.doc.toString()).toBe(movedMarkdown);
  expect(invoke).toHaveBeenCalledWith("open_repository_document", { checkoutId, path: destination });
  expect(invoke).toHaveBeenCalledWith("unwatch_repository_note", { watchId: `watch-${sourceRevision}` });
  expect(invoke).toHaveBeenCalledWith("watch_repository_note", expect.objectContaining({ checkoutId, baseRevision: movedRevision }));
  const navigation = within(screen.getByRole("complementary", { name: "Knowledge navigation" }));
  expect(await navigation.findByRole("button", { name: "Lançamento.md" })).toBeTruthy();
  expect(navigation.queryByRole("button", { name: "Release.md" })).toBeNull();
  const afterApply = vi.mocked(invoke).mock.calls.slice(invocationCount);
  expect(afterApply.some(([command, args]) =>
    (command === "rebuild_repository_note_index" || command === "reconcile_repository_note_index")
      && (args as { checkoutId: string }).checkoutId === checkoutId)).toBe(true);
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "save_repository_document")).toBe(false);

  const beforeOldRoute = vi.mocked(invoke).mock.calls.length;
  rerender(workspace(source));
  await screen.findByRole("heading", { name: "Note unavailable" });
  expect(vi.mocked(invoke).mock.calls.slice(beforeOldRoute)).toContainEqual([
    "open_repository_document", { checkoutId, path: source },
  ]);
  expect(screen.queryByLabelText(`Markdown editor for ${source}`)).toBeNull();
  expect(screen.getAllByRole("button", { name: "Close Lançamento.md" })).toHaveLength(1);
});
