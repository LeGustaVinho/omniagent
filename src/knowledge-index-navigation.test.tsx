// @vitest-environment jsdom

import { startCompletion } from "@codemirror/autocomplete";
import { EditorView } from "@codemirror/view";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import KnowledgeWorkspace from "@/knowledge-workspace";
import type { IndexedLinkResolution, IndexedNoteRelationships } from "@/repository-note-links";
import type { NoteRevision } from "@/repository-note-buffers";
import type { WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  Channel: class { onmessage = () => {}; },
}));

const rangeLayout = { getClientRects: () => [], getBoundingClientRect: () => new DOMRect() };
const missingRangeMethods = Object.keys(rangeLayout).filter((name) => !(name in Range.prototype)) as (keyof typeof rangeLayout)[];
const scrollIntoView = vi.fn();
const originalScroll = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollIntoView");

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class {
    observe() {}
    unobserve() {}
    disconnect() {}
  });
  for (const name of missingRangeMethods) {
    Object.defineProperty(Range.prototype, name, { configurable: true, value: rangeLayout[name] });
  }
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scrollIntoView });
});

afterEach(() => {
  cleanup();
  for (const name of missingRangeMethods) Reflect.deleteProperty(Range.prototype, name);
  if (originalScroll) Object.defineProperty(HTMLElement.prototype, "scrollIntoView", originalScroll);
  else Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView");
  vi.unstubAllGlobals();
  vi.resetAllMocks();
});

const checkoutId = "release-worktree";
const source = "notes/Start.md";
const destination = "guides/Café.md";
const firstDuplicate = "handbook/Checklist.md";
const secondDuplicate = "release/Checklist.md";
const originalBacklink = "planning/Old.md";
const newBacklink = "planning/New.md";
const asset = "notes/assets/brief.png";
const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMB/6X+X1gAAAAASUVORK5CYII=";
const alias = "Launch Alias";
const sourceMarkdown = [
  "# Start",
  "",
  "[[Launch Alias#Cutover|Open heading]]",
  "[[Launch Alias#^proof|Open block]]",
  "[[Checklist|Choose duplicate]]",
  "[[Unknown|Missing note]]",
  "[[Launch Alias#Missing heading|Broken heading]]",
  "[[./assets/brief.png|Fixture image]]",
  "",
  "## Validation",
  "[[La",
].join("\n");
const destinationMarkdown = [
  "---", "aliases:", "  - Launch Alias", "---", "# Café", "", "## Cutover", "", "Proof paragraph ^proof",
].join("\n");
const notes = [
  { path: source, title: "Start", markdown: sourceMarkdown, frontmatter: null, preview: "Release links." },
  { path: destination, title: "Café", markdown: destinationMarkdown, frontmatter: "aliases:\n  - Launch Alias", preview: "Proof paragraph" },
  { path: firstDuplicate, title: "Handbook checklist", markdown: "# Handbook checklist", frontmatter: null, preview: "Handbook tasks." },
  { path: secondDuplicate, title: "Release checklist", markdown: "# Release checklist", frontmatter: null, preview: "Release tasks." },
  { path: originalBacklink, title: "Old plan", markdown: "# Old plan\n\n[[Start]]", frontmatter: null, preview: "Prior links." },
  { path: newBacklink, title: "New plan", markdown: "# New plan\n\n[[Start]]", frontmatter: null, preview: "Current links." },
];
const props: WorkspaceProps = {
  checkout: { projectId: "project", checkoutId, repositoryRoot: "C:\\repo", checkoutRoot: "C:\\repo\\release" },
  target: source,

  openActivity: vi.fn(), openChanges: vi.fn(), openWorkspace: vi.fn(),
};

type LinkQuery = { sourcePath: string; target: string; anchor: string | null };
const location = (path: string, anchor: string | null = null, line: number | null = null) => ({ path, anchor, line });

function resolution({ sourcePath, target, anchor }: LinkQuery): IndexedLinkResolution {
  if (sourcePath !== source) return { status: "missing" };
  if (target === alias) {
    if (anchor === "Cutover") return { status: "resolved", location: location(destination, anchor, 7) };
    if (anchor === "^proof") return { status: "resolved", location: location(destination, anchor, 9) };
    if (anchor) return { status: "missing_anchor", location: location(destination, anchor) };
    return { status: "resolved", location: location(destination) };
  }
  if (target === "Checklist") return { status: "ambiguous", candidates: [location(firstDuplicate), location(secondDuplicate)] };
  if (target === "./assets/brief.png") return { status: "attachment", paths: [asset] };
  return { status: "missing" };
}

function wikiTokens(markdown: string) {
  return [...markdown.matchAll(/\[\[([^#|\]]+)(?:#([^|\]]+))?(?:\|([^\]]+))?\]\]/g)].map((match) => ({
    kind: "wikilink",
    target: match[1]!,
    anchor: match[2] ?? null,
    alias: match[3] ?? null,
    range: { from: match.index, to: match.index + match[0].length },
  }));
}

function setupRepository() {
  let indexChannel: { onmessage: (notice: { checkoutId: string; kind: "paths"; paths: string[] }) => void };
  let refreshed = false;
  const indexRows = () => notes.filter(({ path }) => path !== (refreshed ? originalBacklink : newBacklink))
    .map(({ path, title, frontmatter, preview }, id) => ({ id: id + 1, path, title, frontmatter, preview, tags: [] }));
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const input = (args ?? {}) as {
      checkoutId: string; path: string; baseRevision: string; bufferRevision: string; markdown: string;
      queries: LinkQuery[]; prefix: string; query: string; channel: typeof indexChannel;
    };
    if ("checkoutId" in input) expect(input.checkoutId).toBe(checkoutId);
    switch (command) {
      case "workbench_layout": return null;
      case "save_workbench_layout": return;
      case "watch_repository_note_index":
        indexChannel = input.channel;
        return "index-watch";
      case "unwatch_repository_note_index": return;
      case "rebuild_repository_note_index":
      case "reconcile_repository_note_index": return indexRows();
      case "repository_note_catalog":
      case "list_repository_markdown_drafts": return [];
      case "read_repository_markdown_draft": return null;
      case "watch_repository_note": return `watch-${input.baseRevision}`;
      case "unwatch_repository_note": return;
      case "analyze_note_markdown": return { bufferRevision: input.bufferRevision, nodes: wikiTokens(input.markdown) };
      case "open_repository_document": {
        const note = notes.find(({ path }) => path === input.path);
        if (!note) return { status: "missing", relativePath: input.path };
        return { status: "document", document: {
          markdown: note.markdown,
          fileIdentity: note.path,
          revision: { checkoutId, id: note.path, relativePath: note.path, sha256: "saved-content", baseCommit: null } satisfies NoteRevision,
        } };
      }
      case "resolve_repository_note_links": return input.queries.map(resolution);
      case "suggest_repository_note_links":
        return input.prefix === "La" ? [{ path: destination, target: alias, label: alias }] : [];
      case "search_repository_note_index":
        if (input.query === "Café") return [{ ...location(destination), title: "Café", snippet: null }];
        return input.query === "proof" ? [
          { ...location(source, null, 4), title: "Start", snippet: "Link to proof" },
          { ...location(destination, "^proof", 9), title: "Café", snippet: "Proof paragraph" },
        ] : [];
      case "repository_indexed_note": return input.path === source
        ? { headings: [{ label: "Start", level: 1, line: 1 }, { label: "Validation", level: 2, line: 10 }] }
        : { headings: [{ label: "Café", level: 1, line: 5 }, { label: "Cutover", level: 2, line: 7 }] };
      case "repository_note_relationships": {
        if (input.path !== source) return { outgoing: [], backlinks: [] };
        return {
          outgoing: wikiTokens(sourceMarkdown).map(({ target, anchor, alias: label }, index) => ({
            link: { target, anchor, alias: label, line: index + 3, syntax: "wiki", embed: false },
            resolution: resolution({ sourcePath: source, target, anchor }),
          })),
          backlinks: refreshed ? [{ path: newBacklink, title: "New plan", line: 3, label: "Updated backlink" }]
            : [{ path: originalBacklink, title: "Old plan", line: 3, label: "Earlier backlink" }],
        } satisfies IndexedNoteRelationships;
      }
      case "read_repository_attachment":
        return input.path === asset ? { relativePath: asset, mimeType: "image/png", contentBase64: png } : null;
      default: throw new Error(`Unexpected IPC command ${command}`);
    }
  });
  return {
    updateBacklinks() {
      refreshed = true;
      indexChannel.onmessage({ checkoutId, kind: "paths", paths: [originalBacklink, newBacklink] });
    },
  };
}

function renderWorkspace() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><KnowledgeWorkspace {...props} /></QueryClientProvider>);
  return userEvent.setup();
}

async function editorFor(path: string) {
  const element = await screen.findByLabelText(`Markdown editor for ${path}`);
  const view = EditorView.findFromDOM(element.closest<HTMLElement>(".cm-editor")!);
  if (!view) throw new Error(`CodeMirror did not load ${path}`);
  return view;
}

function selectedLine(view: EditorView) {
  return view.state.doc.lineAt(view.state.selection.main.head).number;
}

async function followEditorLink(view: EditorView, text: string) {
  const offset = view.state.doc.toString().indexOf(text);
  expect(offset).toBeGreaterThan(-1);
  const hitTest = vi.spyOn(view, "posAtCoords").mockReturnValue(offset);
  try {
    fireEvent.mouseDown(view.contentDOM, { ctrlKey: true });
  } finally {
    hitTest.mockRestore();
  }
}

function selectTab(user: ReturnType<typeof userEvent.setup>, name: string) {
  const label = screen.getByRole("button", { name: `Close ${name}` }).closest<HTMLElement>(".file-tab-label")!;
  return user.click(within(label).getByText(name));
}

it("opens the same canonical note and exact heading or block from Source, Live Preview, and Reading", async () => {
  setupRepository();
  const user = renderWorkspace();
  await editorFor(source);
  await user.click(screen.getByRole("button", { name: "Source" }));
  const sourceEditor = await editorFor(source);
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("analyze_note_markdown", expect.objectContaining({ markdown: sourceMarkdown })));
  await followEditorLink(sourceEditor, "Open heading");

  const targetEditor = await editorFor(destination);
  expect(targetEditor.dom.closest(".note-editor")?.getAttribute("data-mode")).toBe("source");
  await waitFor(() => expect(selectedLine(targetEditor)).toBe(7));
  expect(invoke).toHaveBeenCalledWith("resolve_repository_note_links", { checkoutId, queries: [
    { sourcePath: source, target: alias, anchor: "Cutover" },
  ] });
  expect(invoke).toHaveBeenCalledWith("open_repository_document", { checkoutId, path: destination });

  await selectTab(user, "Start.md");
  await user.click(screen.getByRole("button", { name: "Live Preview" }));
  const live = await editorFor(source);
  await within(live.dom).findByText("Open block");
  await followEditorLink(live, "Open block");
  await waitFor(async () => expect(selectedLine(await editorFor(destination))).toBe(9));
  expect(invoke).toHaveBeenCalledWith("resolve_repository_note_links", { checkoutId, queries: [
    { sourcePath: source, target: alias, anchor: "^proof" },
  ] });

  await user.click(screen.getByRole("button", { name: "Reading" }));
  await selectTab(user, "Start.md");
  expect(screen.getByRole("button", { name: "Reading" }).getAttribute("aria-pressed")).toBe("true");
  const reading = within(await screen.findByLabelText(`Reading view for ${source}`));
  const heading = reading.getByRole("button", { name: "Open heading" });
  const preview = within(heading.parentElement!);
  expect(await preview.findByText("Café")).toBeTruthy();
  expect(await preview.findByText(/Proof paragraph/)).toBeTruthy();
  await user.click(heading);

  const headingDestination = await within(await screen.findByLabelText(`Reading view for ${destination}`))
    .findByRole("heading", { level: 2, name: "Cutover" });
  await waitFor(() => expect(scrollIntoView.mock.contexts).toContain(headingDestination));
  expect(screen.getAllByRole("button", { name: "Close Café.md" })).toHaveLength(1);
});

it("uses scoped wiki suggestions and opens title and content results by keyboard in a single tab", async () => {
  setupRepository();
  const user = renderWorkspace();
  const editor = await editorFor(source);
  act(() => {
    editor.dispatch({ selection: { anchor: editor.state.doc.length } });
    startCompletion(editor);
  });
  expect(await screen.findByRole("option", { name: /Launch Alias/ })).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("suggest_repository_note_links", { checkoutId, sourcePath: source, prefix: "La", limit: 40 });

  fireEvent.keyDown(editor.contentDOM, { key: "Escape" });
  fireEvent.keyDown(editor.contentDOM, { key: "p", ctrlKey: true });
  const search = await screen.findByRole("combobox", { name: "Find notes and content" });
  expect(document.activeElement).toBe(search);
  await user.type(search, "Café");
  expect(await screen.findByRole("option", { name: /Café.*guides/ })).toBeTruthy();
  await user.keyboard("{Enter}");
  await editorFor(destination);

  await selectTab(user, "Start.md");
  fireEvent.keyDown((await editorFor(source)).contentDOM, { key: "p", ctrlKey: true });
  expect(document.activeElement).toBe(search);
  await user.clear(search);
  await user.type(search, "proof");
  const indexedHit = await screen.findByRole("option", { name: /Café.*Proof paragraph/ });
  expect(indexedHit.textContent).toContain(`${destination}#^proof · Line 9`);
  expect(invoke).toHaveBeenCalledWith("search_repository_note_index", { checkoutId, query: "proof", limit: 100 });
  await user.keyboard("{ArrowDown}{Enter}");
  const targetEditor = await editorFor(destination);
  await waitFor(() => expect(selectedLine(targetEditor)).toBe(9));
  expect(screen.getAllByRole("button", { name: "Close Café.md" })).toHaveLength(1);
  expect(screen.queryByRole("listbox", { name: "Note search results" })).toBeNull();
});

it("shows link problems and previews, scopes attachment reads, and refreshes backlinks after an index event", async () => {
  const { updateBacklinks } = setupRepository();
  const user = renderWorkspace();
  const sourceEditor = await editorFor(source);
  await user.click(screen.getByRole("button", { name: "Note details" }));
  const inspector = within(screen.getByRole("complementary", { name: "Note details" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("repository_indexed_note", { checkoutId, path: source }));
  await user.click(await inspector.findByRole("button", { name: "Validation" }));
  await waitFor(() => expect(selectedLine(sourceEditor)).toBe(10));
  await user.click(inspector.getByRole("button", { name: "Links" }));
  const outgoing = within(inspector.getByRole("region", { name: "Outgoing links" }));
  expect(await outgoing.findByRole("button", { name: /Missing note.*Missing/ })).toBeTruthy();
  const chooseDuplicate = await outgoing.findByRole("button", { name: /Choose duplicate.*Ambiguous/ });
  expect(await inspector.findByRole("button", { name: /Earlier backlink/ })).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("repository_note_relationships", { checkoutId, path: source });

  async function checkAmbiguousDialog() {
    const dialog = within(await screen.findByRole("dialog", { name: "Choose a note" }));
    const target = dialog.getByRole("combobox", { name: "Target note" });
    expect(dialog.getAllByRole("option").map((item) => (item as HTMLOptionElement).value)).toEqual(["", firstDuplicate, secondDuplicate]);
    expect((dialog.getByRole("button", { name: "Open note" }) as HTMLButtonElement).disabled).toBe(true);
    return { dialog, target };
  }

  await user.click(chooseDuplicate);
  const fromInspector = await checkAmbiguousDialog();
  await user.click(fromInspector.dialog.getByRole("button", { name: "Close dialog" }));

  await act(async () => { updateBacklinks(); });
  const updated = await inspector.findByRole("button", { name: /Updated backlink/ });
  expect(inspector.queryByRole("button", { name: /Earlier backlink/ })).toBeNull();
  expect(invoke).toHaveBeenCalledWith("reconcile_repository_note_index", { checkoutId, paths: [originalBacklink, newBacklink] });
  await user.click(updated);
  const backlinkEditor = await editorFor(newBacklink);
  await waitFor(() => expect(selectedLine(backlinkEditor)).toBe(3));

  await selectTab(user, "Start.md");
  await user.click(screen.getByRole("button", { name: "Source" }));
  await followEditorLink(await editorFor(source), "Choose duplicate");
  const fromSource = await checkAmbiguousDialog();
  await user.click(fromSource.dialog.getByRole("button", { name: "Close dialog" }));
  await user.click(screen.getByRole("button", { name: "Reading" }));
  const reading = within(await screen.findByLabelText(`Reading view for ${source}`));
  await user.click(reading.getByRole("button", { name: "Missing note" }));
  const missing = within(await screen.findByRole("dialog", { name: "Repair note link" }));
  expect((missing.getByRole("button", { name: "Open note" }) as HTMLButtonElement).disabled).toBe(true);
  await user.click(missing.getByRole("button", { name: "Close dialog" }));

  await user.click(reading.getByRole("button", { name: "Broken heading" }));
  const missingAnchor = within(await screen.findByRole("dialog", { name: "Repair note link" }));
  expect(missingAnchor.getByRole("alert").textContent).toContain("The heading or block is unavailable");
  await user.click(missingAnchor.getByRole("button", { name: "Close dialog" }));

  await user.click(reading.getByRole("button", { name: "Fixture image" }));
  const attachment = within(await screen.findByRole("dialog", { name: "Fixture image" }));
  expect((await attachment.findByRole("img", { name: "Fixture image" })).getAttribute("src")).toBe(`data:image/png;base64,${png}`);
  expect(invoke).toHaveBeenCalledWith("read_repository_attachment", { checkoutId, path: asset });
  expect(screen.queryByRole("button", { name: "Close brief.png" })).toBeNull();
  await user.click(attachment.getByRole("button", { name: "Close dialog" }));

  await user.click(reading.getByRole("button", { name: "Choose duplicate" }));
  const { dialog: ambiguous, target } = await checkAmbiguousDialog();
  await user.selectOptions(target, secondDuplicate);
  await user.click(ambiguous.getByRole("button", { name: "Open note" }));
  await editorFor(secondDuplicate);
  expect(invoke).toHaveBeenCalledWith("open_repository_document", { checkoutId, path: secondDuplicate });
  expect(invoke).not.toHaveBeenCalledWith("open_repository_document", { checkoutId, path: firstDuplicate });
});
