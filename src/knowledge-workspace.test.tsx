// @vitest-environment jsdom

import { EditorView } from "@codemirror/view";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import KnowledgeWorkspace from "@/knowledge-workspace";
import { getProperties, noteFrontmatter } from "@/knowledge-document";
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

it("edits the exact Reading property in the draft and searches same-value notes with the new YAML overlay", async () => {
  const source = "\uFEFF---\r\n'phase': 'ready'  # preserve me\r\nflag: one\r\n'flag': two\r\n---\r\n# Review release\r\n";
  const updated = source.replace("'ready'", '"done"');
  const other = "notes/Second.md";
  const fixture = mockRepository([documentAt(release, "phase-note", source), documentAt(other, "matching-note", "---\nphase: done\n---\n# Second\n")]);
  const normal = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "describe_note_properties") {
      const markdown = (args as { markdown: string }).markdown;
      if (markdown.includes("'phase':")) return [
        { name: "phase", line: 2, value: markdown.includes('"done"') ? '"done"' : "'ready'", editable: true, filterable: true },
        { name: "flag", line: 3, value: "one", editable: false, filterable: false },
        { name: "flag", line: 4, value: "two", editable: false, filterable: false },
      ];
    }
    if (command === "replace_note_property") {
      expect(args).toEqual({ markdown: source, key: "phase", value: '"done"' });
      return updated;
    }
    if (command === "search_repository_note_properties") {
      expect(args).toEqual({ checkoutId, key: "phase", value: '"done"', drafts: { [release]: noteFrontmatter(updated) } });
      return [{ path: other, title: "Second", line: 2, value: "done" }];
    }
    return normal(command, args);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} /></QueryClientProvider>);
  const user = userEvent.setup();
  await editor(release);
  await user.click(screen.getByRole("button", { name: "Reading" }));
  const reading = within(await screen.findByLabelText(`Reading view for ${release}`));
  await reading.findByText("phase", { selector: "dt" });
  fireEvent.contextMenu(reading.getByText("two", { selector: "dd" }));
  expect(await screen.findByRole("menuitem", { name: "Copy property value" })).toBeTruthy();
  expect(screen.queryByRole("menuitem", { name: "Edit property…" })).toBeNull();
  expect(screen.queryByRole("menuitem", { name: "Find notes with this property value" })).toBeNull();
  await user.keyboard("{Escape}");

  fireEvent.contextMenu(reading.getByText("phase", { selector: "dt" }));
  await user.click(await screen.findByRole("menuitem", { name: "Edit property…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Edit YAML property" }));
  const field = dialog.getByRole("textbox", { name: "phase" });
  expect(field).toHaveProperty("value", "'ready'");
  await user.clear(field);
  await user.type(field, '"done"');
  await user.click(dialog.getByRole("button", { name: "Apply" }));
  await waitFor(() => expect(fixture.drafts.get(release)?.markdown).toBe(updated));
  expect(fixture.writes).not.toHaveBeenCalled();

  fireEvent.contextMenu(await reading.findByText('"done"', { selector: "dd" }));
  await user.click(await screen.findByRole("menuitem", { name: "Find notes with this property value" }));
  const match = await screen.findByRole("option", { name: /Second.*Line 2.*phase: done/ });
  await user.click(match);
  expect(await screen.findByLabelText(`Reading view for ${other}`)).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Clear property filter" }));
  expect(screen.queryByRole("button", { name: "Clear property filter" })).toBeNull();
});

it.each(["Source", "Live Preview"])("offers the exact YAML property menu in %s", async (mode) => {
  mockRepository();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={checklist} /></QueryClientProvider>);
  const user = userEvent.setup();
  const view = await editor(checklist);
  await user.click(screen.getByRole("button", { name: "Note details" }));
  await user.click(screen.getByRole("button", { name: "Properties" }));
  await screen.findByRole("textbox", { name: "aliases" });
  await user.click(screen.getByRole("button", { name: mode }));
  vi.spyOn(view, "coordsAtPos").mockReturnValue(null);
  act(() => view.dispatch({ selection: { anchor: view.state.doc.line(2).from + 2 } }));
  fireEvent.keyDown(view.contentDOM, { key: "F10", shiftKey: true });
  expect(screen.queryByRole("menuitem", { name: "Format Markdown" })).toBeNull();
  await user.click(await screen.findByRole("menuitem", { name: "Edit property…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Edit YAML property" }));
  expect(dialog.getByRole("textbox", { name: "aliases" })).toHaveProperty("value", "[Launch checklist]");
});

type NoteChange = { checkoutId: string; relativePath: string };
type DocumentFixture = ReturnType<typeof documentAt>;
type SaveRace = {
  document: DocumentFixture;
  recovery: { message: string; editedPath: string; displacedPath: string };
};

function mockRepository(extra: DocumentFixture[] = []) {
  const onDisk = new Map([
    [release, documentAt(release, "release-1", releaseSource)],
    [checklist, documentAt(checklist, "checklist-1", checklistSource)],
  ]);
  for (const document of extra) onDisk.set(document.revision.relativePath, document);
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
        ...extra.map(({ revision }) => ({ path: revision.relativePath, frontmatter: null })),
      ];
      case "repository_note_planning": return [];
      case "rebuild_repository_note_index": return [
        { id: 1, path: release, title: "Release notes", tags: [], frontmatter: null },
        { id: 2, path: checklist, title: "Launch notes", tags: [], frontmatter: checklistProperties },
        ...extra.map(({ revision }, index) => ({ id: index + 3, path: revision.relativePath, title: revision.relativePath, tags: [], frontmatter: null })),
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
      case "describe_note_properties": return getProperties(input.markdown).map(({ key, value, line }) => ({ name: key, value, line,
        editable: true, filterable: true }));
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

it("opens a closed note in the chosen group and saves only that group's dirty notes after resolving a conflict", async () => {
  const appendix = "notes/Appendix.md";
  const service = mockRepository([documentAt(appendix, "appendix-1", "# Appendix\n")]);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} /></QueryClientProvider>);
  const edit = async (path: string, addition: string) => {
    const view = await editor(path);
    act(() => view.dispatch({ changes: { from: view.state.doc.length, insert: addition } }));
  };
  const groupOf = (name: string) => screen.getByRole("button", { name: `Close ${name}` }).closest<HTMLElement>(".dv-groupview")!;
  await edit(release, "Release draft\n");
  fireEvent.click(await screen.findByRole("button", { name: "notes" }));
  fireEvent.click(screen.getByRole("button", { name: "Checklist.md" }));
  await edit(checklist, "Checklist draft\n");
  fireEvent.contextMenu(screen.getByRole("button", { name: "Close Checklist.md" }).closest(".file-tab-label")!);
  fireEvent.keyDown(await screen.findByRole("menuitem", { name: "Move tab" }), { key: "ArrowRight" });
  fireEvent.click(await screen.findByRole("menuitem", { name: "New group to the right" }));
  expect(groupOf("Release.md")).not.toBe(groupOf("Checklist.md"));

  fireEvent.contextMenu(screen.getByRole("button", { name: "Appendix.md" }));
  fireEvent.keyDown(await screen.findByRole("menuitem", { name: "Open in editor group" }), { key: "ArrowRight" });
  fireEvent.click(await screen.findByRole("menuitem", { name: /Group \d+ · Release\.md/ }));
  await edit(appendix, "Appendix draft\n");
  expect(groupOf("Appendix.md")).toBe(groupOf("Release.md"));
  fireEvent.click(screen.getByRole("button", { name: "Checklist.md" }));
  await waitFor(() => expect(groupOf("Checklist.md").classList.contains("dv-active-group")).toBe(true));

  const saveGroup = async () => {
    fireEvent.contextMenu(within(groupOf("Release.md")).getByRole("button", { name: "Actions for Editor group" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Save all tabs in group" }));
  };
  await waitFor(() => {
    expect(service.drafts.size).toBe(3);
    expect(service.isWatched(release)).toBe(true);
  });
  act(() => service.externalChange(documentAt(release, "release-external", "# External revision\n")));
  await saveGroup();
  const conflict = await screen.findByRole("dialog", { name: "Note revision conflict" });
  expect(service.writes).not.toHaveBeenCalled();
  expect(service.drafts.get(appendix)?.markdown).toContain("Appendix draft");
  fireEvent.click(within(conflict).getByRole("button", { name: "Keep local edits" }));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Note revision conflict" })).toBeNull());

  await saveGroup();
  await waitFor(() => expect(service.writes.mock.calls.map(([args]) => args.path)).toEqual([release, appendix]));
  expect(service.drafts.get(checklist)?.markdown).toContain("Checklist draft");
  expect(groupOf("Checklist.md")).not.toBe(groupOf("Release.md"));
});

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


it("reviews an exact dirty selection, preserves it on collision, and opens the extracted note on retry", async () => {
  const fixture = mockRepository();
  const ipc = vi.mocked(invoke);
  const repository = ipc.getMockImplementation()!;
  const extracted = "notes/Release excerpt.md";
  const picked = "🙂 scope\n";
  const prefix = releaseSource + "\n" + picked;
  const markdown = prefix + picked + "\nNext paragraph.";
  const replacement = prefix + "[Release excerpt](Release%20excerpt.md)\n\nNext paragraph.";
  let published = false;
  ipc.mockImplementation(async (command, args) => {
    if (command === "list_checkout_folders") return ["notes"];
    if (command === "extract_repository_note_selection") {
      const draft = fixture.drafts.get(release);
      expect(draft?.markdown).toBe(markdown);
      expect(args).toEqual({
        checkoutId,
        request: {
          path: release, baseRevision: "release-1", fileIdentity: release, expectedDraftRevision: draft?.revision,
          from: prefix.length, to: prefix.length + picked.length, selectedText: picked,
          relativeDirectory: "notes", name: "Release excerpt.md", replaceWithLink: true,
        },
      });
      if (!published) {
        published = true;
        throw new Error("A note with that name already exists");
      }
      fixture.drafts.set(release, { base: draft!.base, revision: "extraction-draft", markdown: replacement });
      return { path: extracted, replacement: { markdown: replacement, draftRevision: "extraction-draft" } };
    }
    if (command === "open_repository_document" && (args as { path: string }).path === extracted) {
      return { status: "document", document: documentAt(extracted, "extracted-1", picked) };
    }
    if (command === "watch_repository_note" && (args as { baseRevision: string }).baseRevision === "extracted-1") {
      return "watch-extracted";
    }
    return repository(command, args);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} /></QueryClientProvider>);
  const user = userEvent.setup();
  const view = await editor(release);
  act(() => view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: markdown },
    selection: { anchor: prefix.length, head: prefix.length + picked.length } }));
  vi.spyOn(view, "coordsAtPos").mockReturnValue(null);
  fireEvent.keyDown(view.contentDOM, { key: "F10", shiftKey: true });
  await user.click(screen.getByRole("menuitem", { name: "Extract selection to a new note…" }));

  const dialog = within(await screen.findByRole("dialog", { name: "Extract selection to a new note" }));
  expect(dialog.getByLabelText("Selected Markdown").textContent).toBe(picked);
  expect((dialog.getByRole("combobox", { name: "Destination folder" }) as HTMLSelectElement).value).toBe("notes");
  const checkbox = dialog.getByRole("checkbox", { name: "Replace selection with a link in the source draft" }) as HTMLInputElement;
  expect(checkbox.checked).toBe(false);
  await user.type(dialog.getByRole("textbox", { name: "New note name" }), "Release excerpt");
  await user.click(checkbox);
  const extract = dialog.getByRole("button", { name: "Extract selection" });
  await user.click(extract);
  expect((await dialog.findByRole("alert")).textContent).toContain("A note with that name already exists");
  expect(view.state.doc.toString()).toBe(markdown);
  expect(fixture.drafts.get(release)?.markdown).toBe(markdown);

  await user.click(extract);
  const nextView = await editor(extracted);
  expect(nextView.state.doc.toString()).toBe(picked);
  expect(fixture.drafts.get(release)?.markdown).toBe(replacement);
  expect(fixture.writes).not.toHaveBeenCalled();
});

it("refuses extraction if the source buffer changes while reviewing the selection", async () => {
  mockRepository();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} /></QueryClientProvider>);
  const user = userEvent.setup();
  const view = await editor(release);
  act(() => view.dispatch({ selection: { anchor: 0, head: view.state.doc.line(1).to } }));
  vi.spyOn(view, "coordsAtPos").mockReturnValue(null);
  fireEvent.keyDown(view.contentDOM, { key: "F10", shiftKey: true });
  await user.click(screen.getByRole("menuitem", { name: "Extract selection to a new note…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Extract selection to a new note" }));
  await user.type(dialog.getByRole("textbox", { name: "New note name" }), "Release excerpt");
  act(() => view.dispatch({ changes: { from: view.state.doc.length, insert: "Additional text." } }));
  await user.click(dialog.getByRole("button", { name: "Extract selection" }));
  expect((await dialog.findByRole("alert")).textContent).toContain("select the passage again");
  expect(vi.mocked(invoke)).not.toHaveBeenCalledWith("extract_repository_note_selection", expect.anything());
  expect(view.state.doc.toString()).toBe(releaseSource + "Additional text.");
});


it("previews a closed note and opens only that note when explicitly requested", async () => {
  mockRepository();
  const backend = vi.mocked(invoke).getMockImplementation()!;
  const body = checklistSource + "A".repeat(256 * 1024);
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "read_checkout_file_text") {
      expect(args).toEqual({ checkoutId, path: checklist });
      return { status: "text", text: body };
    }
    return backend(command, args);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} /></QueryClientProvider>);
  const user = userEvent.setup();
  const active = await editor(release);
  const tree = within(screen.getByRole("complementary", { name: "Knowledge navigation" }));
  const folder = tree.getByRole("button", { name: "notes" });
  if (folder.getAttribute("aria-expanded") !== "true") await user.click(folder);
  fireEvent.contextMenu(await tree.findByRole("button", { name: "Checklist.md" }));
  await user.click(await screen.findByRole("menuitem", { name: "Preview…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Preview file" }));
  expect((await dialog.findByLabelText("Text preview")).textContent).toBe(body.slice(0, 256 * 1024));
  expect(dialog.getByText("Showing the start of this file. Open it in the editor for the full text.")).toBeTruthy();
  expect(invoke).not.toHaveBeenCalledWith("open_repository_document", { checkoutId, path: checklist });
  await user.click(dialog.getByRole("button", { name: "Open in editor" }));
  const opened = await editor(checklist);
  expect(opened.state.doc.toString()).toBe(checklistSource);
  expect(active.state.doc.toString()).toBe(releaseSource);
});


it("copies closed and dirty notes from their context menus without changing the selected note", async () => {
  mockRepository();
  const backend = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "read_checkout_file_text") {
      expect(args).toEqual({ checkoutId, path: checklist });
      return { status: "text", text: checklistSource.replaceAll("\n", "\r\n") };
    }
    return backend(command, args);
  });
  const user = userEvent.setup();
  const clipboard = vi.spyOn(navigator.clipboard, "writeText");
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} /></QueryClientProvider>);
  const view = await editor(release);
  const draft = releaseSource + "\nLive draft.";
  act(() => view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: draft } }));
  fireEvent.contextMenu(await screen.findByRole("button", { name: "Reveal file Release.md" }));
  await user.click(await screen.findByRole("menuitem", { name: "Copy text contents" }));
  expect(clipboard).toHaveBeenLastCalledWith(draft);
  expect(invoke).not.toHaveBeenCalledWith("read_checkout_file_text", expect.anything());

  const navigation = within(screen.getByRole("complementary", { name: "Knowledge navigation" }));
  const folder = navigation.getByRole("button", { name: "notes" });
  if (folder.getAttribute("aria-expanded") !== "true") await user.click(folder);
  fireEvent.contextMenu(await navigation.findByRole("button", { name: "Checklist.md" }));
  await user.click(await screen.findByRole("menuitem", { name: "Copy text contents" }));
  await waitFor(() => expect(clipboard).toHaveBeenLastCalledWith(checklistSource.replaceAll("\n", "\r\n")));
  expect(invoke).toHaveBeenCalledWith("read_checkout_file_text", { checkoutId, path: checklist });
  expect(screen.queryByLabelText("Markdown editor for " + checklist)).toBeNull();
  expect(view.state.doc.toString()).toBe(draft);
});


it("favorites distinct headings and a block, follows the edited source, and removes the exact favorite", async () => {
  const fixture = mockRepository();
  const content = "# Overview\n\n## Scope\n\nFirst scope.\n\n## Scope\n\nSecond scope.\n\nA decision to preserve. ^decision\n";
  fixture.externalChange(documentAt(release, "favorite-source", content));
  const scroll = vi.fn(function (this: HTMLElement) { return this; });
  const descriptor = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollIntoView");
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scroll });
  const storageKey = "omniagent:checkout-favorites:" + checkoutId;
  const previous = localStorage.getItem(storageKey);
  localStorage.removeItem(storageKey);
  try {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
    render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} /></QueryClientProvider>);
    const user = userEvent.setup();
    const view = await editor(release);
    await user.click(screen.getByRole("button", { name: "Reading" }));
    const reader = within(await screen.findByLabelText("Reading view for " + release));
    for (const heading of await reader.findAllByRole("heading", { name: "Scope" })) {
      fireEvent.contextMenu(heading);
      await user.click(await screen.findByRole("menuitem", { name: "Add heading to favorites" }));
    }
    fireEvent.contextMenu(reader.getByText(/A decision to preserve/));
    await user.click(await screen.findByRole("menuitem", { name: "Add block to favorites" }));
    const favorites = within(screen.getByRole("region", { name: "Favorites" }));
    const first = favorites.getByTitle(release + "#Scope");
    const second = favorites.getByTitle(release + "#Scope · Occurrence 2");
    const block = favorites.getByTitle(release + "#^decision");

    await user.click(screen.getByRole("button", { name: "Live Preview" }));
    act(() => view.dispatch({ changes: { from: 0, insert: "New introduction.\n\n" } }));
    await user.click(screen.getByRole("button", { name: "Reading" }));
    const updatedReader = within(await screen.findByLabelText("Reading view for " + release));
    await user.click(second);
    await waitFor(() => expect(scroll.mock.results.at(-1)?.value).toBe(updatedReader.getAllByRole("heading", { name: "Scope" })[1]));
    scroll.mockClear();
    await user.click(block);
    await waitFor(() => expect(scroll.mock.results.at(-1)?.value).toBe(updatedReader.getByText(/A decision to preserve/)));
    scroll.mockClear();
    await user.click(first);
    await waitFor(() => expect(scroll.mock.results.at(-1)?.value).toBe(updatedReader.getAllByRole("heading", { name: "Scope" })[0]));

    fireEvent.contextMenu(second);
    await user.click(await screen.findByRole("menuitem", { name: "Remove from favorites" }));
    expect(favorites.queryByTitle(release + "#Scope · Occurrence 2")).toBeNull();
    expect(favorites.getByTitle(release + "#Scope")).toBeTruthy();
    expect(favorites.getByTitle(release + "#^decision")).toBeTruthy();
  } finally {
    if (previous === null) localStorage.removeItem(storageKey);
    else localStorage.setItem(storageKey, previous);
    if (descriptor) Object.defineProperty(HTMLElement.prototype, "scrollIntoView", descriptor);
    else Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView");
  }
});


it("opens tag rename from the indexed tag menu, previews occurrences, and keeps blocked drafts intact", async () => {
  const fixture = mockRepository();
  const ipc = vi.mocked(invoke);
  const repository = ipc.getMockImplementation()!;
  const doc = documentAt(release, "release-1", releaseSource);
  const draftMarkdown = releaseSource + "\n#delivery/QA\n";
  let previewCount = 0;
  ipc.mockImplementation(async (command, args) => {
    if (command === "rebuild_repository_note_index") return [
      { id: 1, path: release, title: "Release notes", tags: ["release"], frontmatter: null },
      { id: 2, path: checklist, title: "Launch notes", tags: [], frontmatter: checklistProperties },
    ];
    if (command === "preview_markdown_tag_rename") {
      expect(args).toEqual({ checkoutId, source: "release", destination: "delivery", includeNested: true });
      return { id: "tag-preview", source: "release", destination: "delivery",
        files: [{ path: release, revisionId: doc.revision.id, occurrences: [
          { line: 12, before: "#release/QA", after: "#delivery/QA", property: true },
        ] }],
        blocked: ++previewCount === 1 ? [checklist] : [], warnings: [],
      };
    }
    if (command === "stage_markdown_tag_rename") {
      expect(args).toEqual({ checkoutId, previewId: "tag-preview" });
      const draft = { base: doc.revision, revision: "tag-draft", markdown: draftMarkdown };
      fixture.drafts.set(release, draft);
      return [draft];
    }
    return repository(command, args);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} /></QueryClientProvider>);
  const user = userEvent.setup();
  const view = await editor(release);
  const details = screen.getByRole("button", { name: "Note details" });
  if (details.getAttribute("aria-expanded") === "false") await user.click(details);
  await user.click(screen.getByRole("button", { name: "Properties" }));
  fireEvent.contextMenu(await screen.findByRole("button", { name: "#release" }));
  await user.click(screen.getByRole("menuitem", { name: "Rename tag in checkout…" }));

  const input = within(await screen.findByRole("dialog", { name: "Rename tag" }));
  await user.type(input.getByRole("textbox", { name: "New tag name" }), "delivery");
  await user.click(input.getByRole("checkbox", { name: "Include nested tags" }));
  await user.click(input.getByRole("button", { name: "Review occurrences" }));
  const reviewed = within(await screen.findByRole("dialog", { name: "Review tag rename" }));
  expect(reviewed.getByText("#release/QA")).toBeTruthy();
  expect(reviewed.getByText("#delivery/QA")).toBeTruthy();
  expect(reviewed.getByText("Line 12 · Property")).toBeTruthy();
  expect(reviewed.getByRole("list", { name: "Affected drafts" }).textContent).toBe(checklist);
  expect((reviewed.getByRole("button", { name: "Stage drafts" }) as HTMLButtonElement).disabled).toBe(true);
  expect(ipc).not.toHaveBeenCalledWith("stage_markdown_tag_rename", expect.anything());
  await user.click(reviewed.getByRole("button", { name: "Back" }));
  await user.click(input.getByRole("button", { name: "Review occurrences" }));
  await user.click(reviewed.getByRole("button", { name: "Stage drafts" }));
  const result = within(await screen.findByRole("dialog", { name: "Tag rename result" }));
  expect(result.getByRole("list", { name: "Pending tag drafts" }).textContent).toBe(release);
  expect(fixture.writes).not.toHaveBeenCalled();
  await waitFor(() => expect(view.state.doc.toString()).toBe(draftMarkdown));
});


it("previews a note merge and reports the exact remaining drafts when a reviewed save conflicts", async () => {
  const fixture = mockRepository();
  const ipc = vi.mocked(invoke);
  const repository = ipc.getMockImplementation()!;
  const refPath = "notes/Reference.md";
  const refText = "[Keep my label](Release.md)\n";
  const ref = documentAt(refPath, "reference-1", refText);
  fixture.externalChange(ref);
  const merged = checklistSource + "\n" + releaseSource;
  const sourceRedirect = "Merged into [Checklist](Checklist.md).\n";
  const definitions = [
    { ...documentAt(checklist, "checklist-1", checklistSource), after: merged },
    { ...ref, after: "[Keep my label](Checklist.md)\n" },
    { ...documentAt(release, "release-1", releaseSource), after: sourceRedirect },
  ];
  ipc.mockImplementation(async (command, args) => {
    if (command === "list_checkout_folders") return ["notes"];
    if (command === "preview_markdown_merge") {
      expect(args).toEqual({ checkoutId, source: release, destination: checklist });
      return { id: "reviewed-merge", source: release, destination: checklist,
        mergedMarkdown: merged, sourceRedirect, warnings: [],
        files: definitions.map(({ revision }) => ({ path: revision.relativePath, revisionId: revision.id })),
        references: [{ path: refPath, edits: [{ line: 1, before: "Release.md", after: "Checklist.md" }] }],
      };
    }
    if (command === "stage_markdown_merge") {
      expect(args).toEqual({ checkoutId, previewId: "reviewed-merge" });
      return definitions.map(({ revision, after }, index) => {
        const draft = { base: revision, revision: "merge-draft-" + index, markdown: after };
        fixture.drafts.set(revision.relativePath, draft);
        return draft;
      });
    }
    if (command === "save_repository_document" && (args as { path: string }).path === refPath) {
      fixture.raceNextSave({
        document: documentAt(refPath, "reference-external", "Concurrent edit to the reference."),
        recovery: { message: "The reviewed reference changed", editedPath: "C:\\recover\\reference", displacedPath: "C:\\recover\\external" },
      });
    }
    return repository(command, args);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} /></QueryClientProvider>);
  const user = userEvent.setup();
  await editor(release);
  fireEvent.contextMenu(await screen.findByRole("button", { name: "Reveal file Release.md" }));
  await user.click(screen.getByRole("menuitem", { name: "Merge into another note…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Merge notes" }));
  await user.selectOptions(dialog.getByRole("combobox", { name: "Destination note" }), checklist);
  await user.click(dialog.getByRole("button", { name: "Review merge" }));
  const review = within(await screen.findByRole("dialog", { name: "Review note merge" }));
  const rewrites = within(review.getByRole("list", { name: "Reference rewrites" }));
  expect(rewrites.getByText("Release.md").textContent).toBe("Release.md");
  expect(rewrites.getByText("Checklist.md").textContent).toBe("Checklist.md");
  expect(fixture.writes).not.toHaveBeenCalled();
  expect(fixture.drafts.size).toBe(0);
  await user.click(review.getByRole("button", { name: "Merge and save" }));
  const report = within(await screen.findByRole("dialog", { name: "Merge result" }));
  expect(report.getByRole("list", { name: "Saved notes" }).textContent).toContain(checklist);
  const pending = within(report.getByRole("list", { name: "Pending merge drafts" }));
  expect(pending.getByRole("button", { name: refPath })).toBeTruthy();
  expect(pending.getByRole("button", { name: release })).toBeTruthy();
  expect(fixture.writes.mock.calls.map(([input]) => input.path)).toEqual([checklist, refPath]);
  expect(fixture.drafts.get(release)?.markdown).toBe(sourceRedirect);
  expect(fixture.drafts.get(refPath)?.markdown).toBe(definitions[1]!.after);
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

it("opens existing planning and a real linked Task for the clicked note's saved revision without launching work", async () => {
  mockRepository();
  const ipc = vi.mocked(invoke);
  const repository = ipc.getMockImplementation()!;
  ipc.mockImplementation(async (command, args) => {
    if (command === "repository_note_planning" && (args as { revisionId: string }).revisionId === "checklist-1") {
      expect(args).toEqual({ projectId: props.checkout.projectId, checkoutId, revisionId: "checklist-1" });
      return [{ runId: "earlier-planning-run", task: null },
        { runId: "delivery-run", task: { id: "linked-task", title: "Verify checklist" } }];
    }
    return repository(command, args);
  });
  const openWorkspace = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} openWorkspace={openWorkspace} /></QueryClientProvider>);
  const user = userEvent.setup();
  await editor(release);
  const tree = within(screen.getByRole("complementary", { name: "Knowledge navigation" }));
  const folder = tree.getByRole("button", { name: "notes" });
  if (folder.getAttribute("aria-expanded") !== "true") await user.click(folder);
  const note = await tree.findByRole("button", { name: "Checklist.md" });
  async function planningDialog() {
    fireEvent.contextMenu(note);
    await user.click(await screen.findByRole("menuitem", { name: "Open linked planning / delivery…" }));
    return within(await screen.findByRole("dialog", { name: "Linked planning and delivery" }));
  }
  const dialog = await planningDialog();
  expect(dialog.getByText(/notes\/Checklist\.md · checklist-1/)).toBeTruthy();
  expect(screen.queryByLabelText("Markdown editor for " + checklist)).toBeNull();
  expect(openWorkspace).not.toHaveBeenCalled();
  await user.click(await dialog.findByRole("button", { name: "Open planning · earlier-" }));
  expect(openWorkspace).toHaveBeenLastCalledWith("pipelines", "earlier-planning-run");

  const reopened = await planningDialog();
  await user.click(await reopened.findByRole("button", { name: "Open Task Verify checklist" }));
  expect(openWorkspace).toHaveBeenLastCalledWith("tasks", "linked-task");
  expect(ipc).not.toHaveBeenCalledWith("start_repository_delivery", expect.anything());
  expect(ipc).not.toHaveBeenCalledWith("launch_workflow_manual", expect.anything());
});

it("shows the saved revision's empty planning links while the tab has a draft", async () => {
  mockRepository();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} target={release} /></QueryClientProvider>);
  const user = userEvent.setup();
  const view = await editor(release);
  act(() => view.dispatch({ changes: { from: view.state.doc.length, insert: "Unpublished decision." } }));
  fireEvent.contextMenu(tab("Release.md").getByText("Release.md"));
  const plan = await screen.findByRole("menuitem", { name: "Plan from note…" });
  expect(plan.getAttribute("aria-disabled")).toBe("true");
  await user.click(screen.getByRole("menuitem", { name: "Open linked planning / delivery…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Linked planning and delivery" }));
  expect(await dialog.findByText("No planning runs are linked to this saved revision.")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("repository_note_planning", { projectId: props.checkout.projectId, checkoutId, revisionId: "release-1" });
  expect(view.state.doc.toString()).toBe(releaseSource + "Unpublished decision.");
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
