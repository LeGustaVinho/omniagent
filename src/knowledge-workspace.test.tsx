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

type NoteChange = { checkoutId: string; relativePath: string };
type DocumentFixture = ReturnType<typeof documentAt>;
type SaveRace = {
  document: DocumentFixture;
  recovery: { message: string; editedPath: string; displacedPath: string };
};

function mockRepository() {
  const onDisk = new Map([
    [release, documentAt(release, "release-1", releaseSource)],
    [checklist, documentAt(checklist, "checklist-1", checklistSource)],
  ]);
  type Draft = { revision: string; base: NoteRevision; markdown: string };
  const drafts = new Map<string, Draft>();
  const watchers = new Map<string, (event: NoteChange) => void>();
  const recovered = new Map<string, string>();
  let nextSaveRace: SaveRace | undefined;
  let draftNumber = 0;
  const writes = vi.fn();
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const input = (args ?? {}) as {
      path: string; baseRevision: string; expectedRevision: string;
      expectedDraftRevision: string | null; markdown: string; bufferRevision: string;
      channel: { onmessage: (event: NoteChange) => void };
    };
    switch (command) {
      case "workbench_layout": return null;
      case "save_workbench_layout": return;
      case "watch_repository_note_index": return "watch-index";
      case "unwatch_repository_note_index": return;
      case "repository_note_catalog": return [
        { path: release, frontmatter: null },
        { path: checklist, frontmatter: checklistProperties },
      ];
      case "rebuild_repository_note_index": return [
        { id: 1, path: release, title: "Release notes", tags: [], frontmatter: null },
        { id: 2, path: checklist, title: "Launch notes", tags: [], frontmatter: checklistProperties },
      ];
      case "search_repository_note_index": return [
        { path: checklist, title: "Launch notes", snippet: "Keep the saved checklist." },
      ];
      case "list_repository_markdown_drafts": return Array.from(drafts.keys());
      case "open_repository_document": {
        const document = Array.from(onDisk.values()).find(({ revision }) => revision.relativePath.toLowerCase() === input.path.toLowerCase());
        if (!document) throw new Error(`Unknown fixture path ${input.path}`);
        return { status: "document", document };
      }
      case "read_repository_markdown_draft": return drafts.get(input.path) ?? null;
      case "watch_repository_note": {
        const watching = Array.from(onDisk.values()).find(({ revision }) => revision.id === input.baseRevision);
        if (!watching) throw new Error("Missing watch base revision");
        watchers.set(watching.revision.relativePath, input.channel.onmessage);
        return `watch-${input.baseRevision}`;
      }
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
        if (nextSaveRace) {
          const { document, recovery } = nextSaveRace;
          nextSaveRace = undefined;
          expect(document.revision.relativePath).toBe(input.path);
          onDisk.set(input.path, document);
          recovered.set(recovery.editedPath, input.markdown);
          recovered.set(recovery.displacedPath, document.markdown);
          return { status: "conflict", recovery };
        }
        const document = documentAt(input.path, `${input.expectedRevision}-saved`, input.markdown);
        onDisk.set(input.path, document);
        return { status: "saved", document };
      }
      default: throw new Error(`Unexpected IPC command ${command}`);
    }
  });
  return {
    drafts, writes, recovered,
    isWatched: (path: string) => watchers.has(path),
    externalChange: (document: DocumentFixture) => {
      const relativePath = document.revision.relativePath;
      onDisk.set(relativePath, document);
      watchers.get(relativePath)?.({ checkoutId, relativePath });
    },
    raceNextSave: (race: SaveRace) => { nextSaveRace = race; },
  };
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

it("opens an unopened note from the checkout-wide full-text index", async () => {
  mockRepository();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} /></QueryClientProvider>);
  await editor(release);
  const user = userEvent.setup();
  if (!screen.queryByRole("button", { name: "Search notes" })) {
    await user.click(screen.getByRole("button", { name: "Toggle Knowledge navigation" }));
  }
  await user.click(screen.getByRole("button", { name: "Search notes" }));
  const search = screen.getByRole("combobox", { name: "Find notes and content" });
  await user.click(search);
  await user.paste("saved checklist");
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("search_repository_note_index", {
    checkoutId, query: "saved checklist", limit: 100,
  }));
  expect(await screen.findByRole("option", { name: /Launch notes.*Keep the saved checklist/i })).toBeTruthy();
  expect(invoke).not.toHaveBeenCalledWith("open_repository_document", { checkoutId, path: checklist });
  await user.keyboard("{Enter}");
  expect((await editor(checklist)).state.doc.toString()).toBe(checklistSource);
  expect(screen.getAllByRole("button", { name: "Close Checklist.md" })).toHaveLength(1);
});

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

it("keeps a dirty note recoverable across a watched external edit and a save-time race", async () => {
  const repository = mockRepository();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} /></QueryClientProvider>);
  const user = userEvent.setup();
  const view = await editor(release);
  await waitFor(() => expect(repository.isWatched(release)).toBe(true));
  const local = "# Local release draft\n\nThese unsaved edits must survive.\n";
  act(() => view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: local } }));
  await waitFor(() => expect(repository.drafts.get(release)?.markdown).toBe(local));

  await user.click(screen.getByRole("button", { name: "Split Reading view" }));
  const reading = await screen.findByLabelText(`Reading view for ${release}`);
  expect(within(reading).getByRole("heading", { name: "Local release draft" })).toBeTruthy();

  const external = "# External change from the watcher\n\nKeep the saved revision.\n";
  act(() => repository.externalChange(documentAt(release, "release-2", external)));
  const reviews = await screen.findAllByRole("button", { name: "Review external change" });
  expect(view.state.doc.toString()).toBe(local);
  expect(within(reading).getByRole("heading", { name: "Local release draft" })).toBeTruthy();
  await user.click(reviews[0]!);
  let conflict = await screen.findByRole("dialog", { name: "Note revision conflict" });
  expect(conflict.querySelectorAll("pre")[0]?.textContent).toBe(local);
  expect(conflict.querySelectorAll("pre")[1]?.textContent).toBe(external);
  expect(repository.writes).not.toHaveBeenCalled();

  await user.click(within(conflict).getByRole("button", { name: "Keep local edits" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(repository.drafts.get(release)).toMatchObject({ base: { id: "release-2" }, markdown: local });
  expect(within(reading).getByRole("heading", { name: "Local release draft" })).toBeTruthy();

  const raced = "# External change during save\n\nThe final check must preserve these bytes.\n";
  const recovery = {
    message: "The document changed while saving. Both versions were retained.",
    editedPath: "C:\\repo\\notes\\Release.edited.md",
    displacedPath: "C:\\repo\\notes\\Release.displaced.md",
  };
  repository.raceNextSave({ document: documentAt(release, "release-3", raced), recovery });
  await user.click(screen.getByRole("button", { name: "Save note" }));
  conflict = await screen.findByRole("dialog", { name: "Note revision conflict" });
  expect(repository.writes).toHaveBeenCalledExactlyOnceWith({ checkoutId, path: release, expectedRevision: "release-2", markdown: local });
  expect(conflict.querySelectorAll("pre")[0]?.textContent).toBe(local);
  expect(conflict.querySelectorAll("pre")[1]?.textContent).toBe(raced);
  expect(repository.drafts.get(release)?.markdown).toBe(local);
  const displayedRecovery = conflict.textContent;

  await user.click(within(conflict).getByRole("button", { name: "Load external version" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(view.state.doc.toString()).toBe(raced);
  expect(within(reading).getByRole("heading", { name: "External change during save" })).toBeTruthy();
  expect(within(reading).queryByRole("heading", { name: "Local release draft" })).toBeNull();
  expect(tab("Release.md").queryByLabelText("Unsaved changes")).toBeNull();
  expect(repository.drafts.has(release)).toBe(false);
  expect(repository.recovered.get(recovery.editedPath)).toBe(local);
  expect(repository.recovered.get(recovery.displacedPath)).toBe(raced);
  expect(displayedRecovery).toContain(recovery.editedPath);
  expect(displayedRecovery).toContain(recovery.displacedPath);
});
