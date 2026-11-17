// @vitest-environment jsdom

import { EditorView } from "@codemirror/view";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import KnowledgeWorkspace from "@/knowledge-workspace";
import type { NoteRevision } from "@/repository-note-buffers";
import { exampleCheckout, type WorkspaceProps } from "@/workbench-contract";

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
      case "repository_note_planning": return [];
      case "rebuild_repository_note_index": return [
        { id: 1, path: release, title: "Release notes", tags: [], frontmatter: null },
        { id: 2, path: checklist, title: "Launch notes", tags: [], frontmatter: checklistProperties },
      ];
      case "search_repository_note_index": return [
        { path: checklist, title: "Launch notes", snippet: "Keep the saved checklist." },
      ];
      case "resolve_repository_note_links": return ((args as { queries: { target: string }[] }).queries).map(({ target }) =>
        target === "Launch checklist" ? { status: "resolved", location: { path: checklist, anchor: null, line: null } } : { status: "missing" });
      case "suggest_repository_note_links": return [];
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

it("adds only the selected unsaved Markdown to an existing conversation draft", async () => {
  mockRepository();
  const ipc = vi.mocked(invoke);
  const repository = ipc.getMockImplementation()!;
  const openWorkspace = vi.fn();
  const conversationId = "selected-conversation";
  ipc.mockImplementation(async (command, args) => {
    if (command === "list_conversations") {
      expect(args).toEqual({ checkoutId });
      return [{ id: conversationId, title: "Existing conversation" }];
    }
    if (command === "append_checkout_context_to_conversation") return;
    return repository(command, args);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} openWorkspace={openWorkspace} /></QueryClientProvider>);
  const user = userEvent.setup();
  const view = await editor(release);
  const quote = "Working text from the unsaved buffer.";
  const offset = view.state.doc.length + 1;
  act(() => view.dispatch({ changes: { from: offset - 1, insert: `\n${quote}` }, selection: { anchor: offset, head: offset + quote.length } }));
  const line = view.state.doc.lineAt(offset).number;
  vi.spyOn(view, "coordsAtPos").mockReturnValue(null);
  fireEvent.keyDown(view.contentDOM, { key: "F10", shiftKey: true });
  await user.click(screen.getByRole("menuitem", { name: "Add selection to conversation…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Add checkout context" }));
  expect(dialog.getByLabelText("Selected text").textContent).toBe(quote);
  await dialog.findByRole("option", { name: "Existing conversation" });
  await user.selectOptions(dialog.getByRole("combobox", { name: "Conversation" }), conversationId);
  expect(ipc).not.toHaveBeenCalledWith("append_checkout_context_to_conversation", expect.anything());
  await user.click(dialog.getByRole("button", { name: "Add to draft" }));
  await waitFor(() => expect(openWorkspace).toHaveBeenCalledWith("conversations", conversationId));
  expect(ipc).toHaveBeenCalledWith("append_checkout_context_to_conversation", {
    checkoutId, conversationId, context: `Source: ${release}:${line}\n\n\`\`\`markdown\n${quote}\n\`\`\``,
  });
  expect(ipc.mock.calls.some(([command]) => /send_conversation|submit_turn/.test(command))).toBe(false);
});

it("starts Plan Mode delivery from the reviewed note revision and opens only its real linked Task", async () => {
  mockRepository();
  const ipc = vi.mocked(invoke);
  const repository = ipc.getMockImplementation()!;
  const openWorkspace = vi.fn();
  const saved = documentAt(release, "release-1", releaseSource);
  const source = { projectId: props.checkout.projectId, checkoutId, revisionId: saved.revision.id };
  let plan: { runId: string; task: { id: string; title: string } | null } | undefined;
  ipc.mockImplementation(async (command, args) => {
    if (command === "repository_note_planning") {
      expect(args).toEqual(source);
      return plan ? [plan] : [];
    }
    if (command === "read_repository_markdown") {
      expect(args).toEqual({ checkoutId, revisionId: saved.revision.id });
      return saved;
    }
    if (command === "start_repository_delivery") {
      expect(args).toEqual({ ...source, requestId: expect.any(String) });
      plan = { runId: "planning-run", task: null };
      return { id: plan.runId };
    }
    return repository(command, args);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} openWorkspace={openWorkspace} /></QueryClientProvider>);
  const user = userEvent.setup();
  await editor(release);
  expect(screen.queryByRole("button", { name: "Open linked task" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Plan from note" }));
  const review = within(await screen.findByRole("dialog", { name: "Plan from note" }));
  await user.click(await review.findByText("Read saved Markdown"));
  expect(review.getByText(/Open launch notes/).textContent).toBe(saved.markdown);
  expect(ipc.mock.calls.some(([command]) => command === "start_repository_delivery")).toBe(false);
  await user.click(review.getByRole("button", { name: "Start planning" }));
  await waitFor(() => expect(openWorkspace).toHaveBeenCalledWith("pipelines", "planning-run"));
  expect(ipc.mock.calls.some(([command]) => command === "submit_repository_delivery_agent")).toBe(false);
  expect(screen.queryByRole("button", { name: "Open linked task" })).toBeNull();

  plan = { runId: "planning-run", task: { id: "real-task", title: "Prepare release" } };
  await act(() => client.invalidateQueries({ queryKey: ["repository-note-planning"] }));
  await user.click(await screen.findByRole("button", { name: "Open linked task" }));
  expect(openWorkspace).toHaveBeenLastCalledWith("tasks", "real-task");
});

it.each(["Live Preview", "Reading"])("opens an unopened note from the full-text index in %s", async (mode) => {
  mockRepository();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} /></QueryClientProvider>);
  await editor(release);
  const user = userEvent.setup();
  if (mode === "Reading") {
    await user.click(screen.getByRole("button", { name: "Reading" }));
    await screen.findByLabelText(`Reading view for ${release}`);
    await user.click(screen.getByRole("button", { name: "Split Reading view" }));
    const editable = await editor(release);
    expect(editable.dom.closest("[hidden]")).toBeNull();
  }
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
  if (mode === "Reading") {
    const reading = await screen.findByLabelText(`Reading view for ${checklist}`);
    expect(within(reading).getByText(/Keep the saved checklist/)).toBeTruthy();
  } else {
    expect((await editor(checklist)).state.doc.toString()).toBe(checklistSource);
  }
  expect(screen.getByRole("button", { name: mode }).getAttribute("aria-pressed")).toBe("true");
  expect(screen.getAllByRole("button", { name: "Close Checklist.md" })).toHaveLength(1);
});

it("reviews and launches the exact saved note through the published workflow form", async () => {
  mockRepository();
  const ipc = vi.mocked(invoke);
  const repository = ipc.getMockImplementation()!;
  const openWorkspace = vi.fn();
  const saved = documentAt(release, "release-1-saved", releaseSource + "\nReview the pinned note.\n");
  const version = { id: "review-release", revision: "published-revision" };
  ipc.mockImplementation(async (command, args) => {
    if (command === "workflow_authoring_bases") return [{ version, name: "Review release", start: {
      fields: [{ name: "Reviewer", valueType: "text", required: true }],
      requiresDocument: false, requiresWork: false, requiresEvent: false,
    } }];
    if (command === "registered_checkouts") return [props.checkout, { ...props.checkout, checkoutId: "another-checkout" }];
    if (command === "project_work_hierarchy") return { work: [] };
    if (command === "read_repository_markdown") {
      expect(args).toEqual({ checkoutId, revisionId: saved.revision.id });
      return saved;
    }
    if (command === "launch_workflow_manual") return { id: "review-run" };
    return repository(command, args);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} openWorkspace={openWorkspace} /></QueryClientProvider>);
  const user = userEvent.setup();
  const view = await editor(release);
  const button = screen.getByRole<HTMLButtonElement>("button", { name: "Run workflow from note" });
  act(() => view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: saved.markdown } }));
  await waitFor(() => expect(button.disabled).toBe(true));
  await user.click(screen.getByRole("button", { name: "Save note" }));
  await waitFor(() => expect(button.disabled).toBe(false));
  await user.click(button);

  const review = within(await screen.findByRole("dialog", { name: "Run workflow from note" }));
  const checkout = await review.findByRole<HTMLSelectElement>("combobox", { name: "Checkout" });
  expect(checkout.disabled).toBe(true);
  expect(checkout.value).toBe(checkoutId);
  const start = await review.findByRole<HTMLButtonElement>("button", { name: "Start workflow" });
  expect(start.disabled).toBe(true);
  await user.click(await review.findByText("Read saved Markdown"));
  expect(review.getByText(/Review the pinned note/).textContent).toBe(saved.markdown);
  expect(ipc.mock.calls.map(([command]) => command)).not.toContain("list_repository_markdown_revisions");
  expect(ipc.mock.calls.map(([command]) => command)).not.toContain("launch_workflow_manual");

  await user.type(review.getByRole("textbox", { name: "Reviewer" }), "Release team");
  await user.click(start);
  await waitFor(() => expect(openWorkspace).toHaveBeenCalledWith("pipelines", "run:review-run"));
  expect(ipc).toHaveBeenCalledWith("launch_workflow_manual", { request: {
    projectId: props.checkout.projectId, workflow: version, checkoutId,
    documentRevisionId: saved.revision.id, workId: null, inputs: { Reviewer: "Release team" }, requestId: expect.any(String),
  } });
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("uses the same link and keyboard search controls with simulated notes", async () => {
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "analyze_note_markdown") return { bufferRevision: (args as { bufferRevision: string }).bufferRevision, nodes: [] };
    throw new Error(`Unexpected IPC command ${command}`);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const simulated = { scenario: "delivery", epoch: 1, delivery: "note", advanceDelivery: vi.fn() } as const;
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} checkout={exampleCheckout} simulation={simulated} /></QueryClientProvider>);
  const user = userEvent.setup();
  const workspace = screen.getByRole("region", { name: "Knowledge workspace" });
  await editor("docs/release.md");
  if (!screen.queryByRole("button", { name: "Links" })) await user.click(screen.getByRole("button", { name: "Note details" }));
  await user.click(screen.getByRole("button", { name: "Links" }));
  const links = within(screen.getByRole("region", { name: "Outgoing links" }));
  await user.click(links.getByRole("button", { name: "runner diagnostics" }));
  const runner = await editor("docs/runner.md");
  await waitFor(() => expect(runner.state.doc.lineAt(runner.state.selection.main.head).text).toBe("## Startup checks"));

  fireEvent.keyDown(workspace, { key: "p", ctrlKey: true });
  const search = screen.getByRole("combobox", { name: "Find notes and content" });
  expect(document.activeElement).toBe(search);
  await user.type(search, "Operations runbook");
  expect(screen.getByRole("option", { name: /docs\/ops\/runbook.md/ })).toBeTruthy();
  await user.keyboard("{Enter}");
  expect((await editor("docs/ops/runbook.md")).state.doc.toString()).toContain("# Operations runbook");
  expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).not.toContain("search_repository_note_index");
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
  if (!navigation.queryByRole("button", { name: "Release.md" })) await user.click(navigation.getByText("notes"));
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
