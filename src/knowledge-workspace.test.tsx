// @vitest-environment jsdom

import { EditorView } from "@codemirror/view";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import KnowledgeWorkspace from "@/knowledge-workspace";
import type { NoteRevision } from "@/repository-note-buffers";
import type { WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  Channel: class { onmessage = () => {}; },
}));

const rangeLayout = {
  getClientRects: () => [],
  getBoundingClientRect: () => new DOMRect(),
};
const unsupportedRangeMethods = Object.keys(rangeLayout).filter((name) => !(name in Range.prototype)) as (keyof typeof rangeLayout)[];

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class {
    observe() {}
    unobserve() {}
    disconnect() {}
  });
  for (const name of unsupportedRangeMethods) {
    Object.defineProperty(Range.prototype, name, { configurable: true, value: rangeLayout[name] });
  }
});

afterEach(() => {
  cleanup();
  for (const name of unsupportedRangeMethods) Reflect.deleteProperty(Range.prototype, name);
  vi.unstubAllGlobals();
  vi.resetAllMocks();
});

const release = "notes/Release.md";
const checklist = "notes/Checklist.md";
const releaseSource = "# Release notes\n\n[[Launch checklist|Open launch notes]]\n";
const checklistProperties = "---\naliases: [Launch checklist]\n---\n";
const checklistSource = checklistProperties + "# Launch notes\n\nKeep the saved checklist.\n";
const checkoutId = "checkout";
const props: WorkspaceProps = {
  checkout: { projectId: "project", checkoutId, repositoryRoot: "C:\\repo", checkoutRoot: "C:\\repo" },
  simulation: null, openActivity: vi.fn(), openChanges: vi.fn(), openWorkspace: vi.fn(), startSimulation: vi.fn(),
};

const documentAt = (relativePath: string, id: string, markdown: string) => ({
  revision: { id, checkoutId, relativePath, sha256: id + "-sha256", baseCommit: null } satisfies NoteRevision,
  markdown,
  fileIdentity: relativePath,
});

function mockRepository() {
  const onDisk = new Map([
    [release, documentAt(release, "release-1", releaseSource)],
    [checklist, documentAt(checklist, "checklist-1", checklistSource)],
  ]);
  type Draft = { revision: string; base: NoteRevision; markdown: string };
  const drafts = new Map<string, Draft>();
  let draftNumber = 0;
  const writes = vi.fn();
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const input = (args ?? {}) as {
      path: string; baseRevision: string; expectedRevision: string;
      expectedDraftRevision: string | null; markdown: string; bufferRevision: string;
    };
    switch (command) {
      case "workbench_layout": return null;
      case "save_workbench_layout": return;
      case "repository_note_catalog": return [
        { path: release, frontmatter: null },
        { path: checklist, frontmatter: checklistProperties },
      ];
      case "list_repository_markdown_drafts": return Array.from(drafts.keys());
      case "open_repository_document": {
        const document = Array.from(onDisk.values()).find(({ revision }) => revision.relativePath.toLowerCase() === input.path.toLowerCase());
        if (!document) throw new Error(`Unknown fixture path ${input.path}`);
        return { status: "document", document };
      }
      case "read_repository_markdown_draft": return drafts.get(input.path) ?? null;
      case "watch_repository_note": return `watch-${input.baseRevision}`;
      case "unwatch_repository_note": return;
      case "analyze_note_markdown": return { bufferRevision: input.bufferRevision, nodes: [] };
      case "save_repository_markdown_draft": {
        const base = Array.from(onDisk.values()).find(({ revision }) => revision.id === input.baseRevision)?.revision;
        if (!base || input.expectedDraftRevision !== (drafts.get(base.relativePath)?.revision ?? null)) {
          throw new Error("Stale draft save");
        }
        const draft = { revision: `draft-${++draftNumber}`, base, markdown: input.markdown };
        drafts.set(base.relativePath, draft);
        return draft;
      }
      case "discard_repository_markdown_draft":
        return drafts.get(input.path)?.revision === input.expectedDraftRevision && drafts.delete(input.path);
      case "save_repository_document": {
        if (onDisk.get(input.path)?.revision.id !== input.expectedRevision) throw new Error("Stale note save");
        writes(args);
        const document = documentAt(input.path, `${input.expectedRevision}-saved`, input.markdown);
        onDisk.set(input.path, document);
        return { status: "saved", document };
      }
      default: throw new Error(`Unexpected IPC command ${command}`);
    }
  });
  return { drafts, writes };
}

async function editor(path: string) {
  const content = await screen.findByLabelText(`Markdown editor for ${path}`);
  const view = EditorView.findFromDOM(content.closest<HTMLElement>(".cm-editor")!);
  if (!view) throw new Error(`No CodeMirror view for ${path}`);
  return view;
}

function tab(name: string) {
  return within(screen.getByRole("button", { name: `Close ${name}` }).closest(".file-tab-label")!);
}

it("reuses canonical tabs, shares edits with Reading, and offers Cancel, Save and Discard on the last editor", async () => {
  const repository = mockRepository();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const workspace = (target: string) => <QueryClientProvider client={client}>
    <KnowledgeWorkspace {...props} target={target} />
  </QueryClientProvider>;
  const { rerender } = render(workspace("notes/release.md"));
  const user = userEvent.setup();
  await screen.findByRole("button", { name: "Close Release.md" });
  const original = await editor(release);
  const draft = releaseSource + "\nDraft release paragraph.\n";
  act(() => original.dispatch({ changes: { from: releaseSource.length, insert: "\nDraft release paragraph.\n" } }));
  await waitFor(() => expect(tab("Release.md").getByLabelText("Unsaved changes")).toBeTruthy());
  expect(invoke).toHaveBeenCalledWith("open_repository_document", { checkoutId, path: "notes/release.md" });

  await user.click(screen.getByRole("button", { name: "Split Reading view" }));
  const reading = await screen.findByLabelText(`Reading view for ${release}`);
  expect(reading.closest(".dv-groupview")).toBeTruthy();
  expect(reading.closest(".dv-groupview")).not.toBe(original.dom.closest(".dv-groupview"));
  expect(within(reading).getByText("Draft release paragraph.")).toBeTruthy();
  const splitEdit = "\nLive update in the split.\n";
  const editable = await editor(release);
  act(() => editable.dispatch({ changes: { from: editable.state.doc.length, insert: splitEdit } }));
  expect(within(reading).getByText("Live update in the split.")).toBeTruthy();
  const edited = draft + splitEdit;
  await waitFor(() => expect(repository.drafts.get(release)?.markdown).toBe(edited));
  expect(repository.writes).not.toHaveBeenCalled();

  await user.click(screen.getByRole("button", { name: "Open launch notes" }));
  await screen.findByRole("button", { name: "Close Checklist.md" });
  const secondary = await editor(checklist);
  const discarded = checklistSource + "\nTransient checklist edit.\n";
  act(() => secondary.dispatch({ changes: { from: secondary.state.doc.length, insert: "\nTransient checklist edit.\n" } }));
  expect(tab("Checklist.md").getByLabelText("Unsaved changes")).toBeTruthy();
  await waitFor(() => expect(repository.drafts.get(checklist)?.markdown).toBe(discarded));

  const navigation = within(screen.getByRole("complementary", { name: "Knowledge navigation" }));
  const activeNote = (name: string) => navigation.getByRole("button", { name }).getAttribute("aria-current");
  await user.click(tab("Release.md").getByText("Release.md"));
  expect(activeNote("Release.md")).toBe("page");
  await user.click(tab("Checklist.md").getByText("Checklist.md"));
  expect(activeNote("Checklist.md")).toBe("page");
  expect((await editor(checklist)).state.doc.toString()).toBe(discarded);
  rerender(workspace("NOTES/RELEASE.md"));
  await waitFor(() => expect(activeNote("Release.md")).toBe("page"));
  expect(invoke).toHaveBeenCalledWith("open_repository_document", { checkoutId, path: "NOTES/RELEASE.md" });
  expect(screen.getAllByRole("button", { name: "Close Release.md" })).toHaveLength(1);
  expect((await editor(release)).state.doc.toString()).toBe(edited);

  await user.click(screen.getByRole("button", { name: "Close Release.md" }));
  let confirmation = await screen.findByRole("dialog", { name: "Save note before closing?" });
  expect(within(confirmation).getByText(`${release} has unsaved changes.`)).toBeTruthy();
  await user.click(within(confirmation).getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(tab("Release.md").getByLabelText("Unsaved changes")).toBeTruthy();
  expect(repository.writes).not.toHaveBeenCalled();

  await user.click(screen.getByRole("button", { name: "Close Release.md" }));
  confirmation = await screen.findByRole("dialog", { name: "Save note before closing?" });
  await user.click(within(confirmation).getByRole("button", { name: "Save and close" }));
  await waitFor(() => expect(screen.queryByRole("button", { name: "Close Release.md" })).toBeNull());
  expect(repository.writes).toHaveBeenCalledExactlyOnceWith({ checkoutId, path: release, expectedRevision: "release-1", markdown: edited });
  expect(repository.drafts.has(release)).toBe(false);

  await user.click(tab("Release.md · Reading").getByText("Release.md · Reading"));
  expect(within(await screen.findByLabelText(`Reading view for ${release}`)).getByText("Live update in the split.")).toBeTruthy();
  expect(screen.queryByText(/Unsaved.*release-1/)).toBeNull();

  await user.click(screen.getByRole("button", { name: "Close Checklist.md" }));
  confirmation = await screen.findByRole("dialog", { name: "Save note before closing?" });
  expect(within(confirmation).getByText(`${checklist} has unsaved changes.`)).toBeTruthy();
  await user.click(within(confirmation).getByRole("button", { name: "Discard changes" }));
  await waitFor(() => expect(screen.queryByRole("button", { name: "Close Checklist.md" })).toBeNull());
  expect(repository.drafts.has(checklist)).toBe(false);
  expect(repository.writes).toHaveBeenCalledTimes(1);

  await user.click(navigation.getByRole("button", { name: "Checklist.md" }));
  expect((await editor(checklist)).state.doc.toString()).toBe(checklistSource);
  expect(tab("Checklist.md").queryByLabelText("Unsaved changes")).toBeNull();
});
