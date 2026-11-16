// @vitest-environment jsdom

import { startCompletion } from "@codemirror/autocomplete";
import { EditorView } from "@codemirror/view";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import KnowledgeWorkspace from "@/knowledge-workspace";
import type { IndexedLinkResolution } from "@/repository-note-links";
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
const alias = "Launch Alias";
const sourceMarkdown = [
  "# Start",
  "",
  "[[Launch Alias#Cutover|Open heading]]",
  "[[Launch Alias#^proof|Open block]]",
  "",
  "[[La",
].join("\n");
const destinationMarkdown = [
  "---", "aliases:", "  - Launch Alias", "---", "# Café", "", "## Cutover", "", "Proof paragraph ^proof",
].join("\n");
const notes = [
  { path: source, title: "Start", markdown: sourceMarkdown, frontmatter: null, preview: "Release links." },
  { path: destination, title: "Café", markdown: destinationMarkdown, frontmatter: "aliases:\n  - Launch Alias", preview: "Proof paragraph" },
];
const props: WorkspaceProps = {
  checkout: { projectId: "project", checkoutId, repositoryRoot: "C:\\repo", checkoutRoot: "C:\\repo\\release" },
  target: source,
  simulation: null,
  startSimulation: vi.fn(), openActivity: vi.fn(), openChanges: vi.fn(), openWorkspace: vi.fn(),
};

type LinkQuery = { sourcePath: string; target: string; anchor: string | null };
const location = (path: string, anchor: string | null = null, line: number | null = null) => ({ path, anchor, line });

function resolution({ sourcePath, target, anchor }: LinkQuery): IndexedLinkResolution {
  if (sourcePath !== source) return { status: "missing" };
  if (target === alias) {
    if (anchor === "Cutover") return { status: "resolved", location: location(destination, anchor, 7) };
    if (anchor === "^proof") return { status: "resolved", location: location(destination, anchor, 9) };
    return { status: "resolved", location: location(destination) };
  }
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
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const input = (args ?? {}) as {
      checkoutId: string; path: string; baseRevision: string; bufferRevision: string; markdown: string;
      queries: LinkQuery[]; prefix: string; query: string;
    };
    if ("checkoutId" in input) expect(input.checkoutId).toBe(checkoutId);
    switch (command) {
      case "workbench_layout": return null;
      case "save_workbench_layout": return;
      case "watch_repository_note_index": return "index-watch";
      case "unwatch_repository_note_index": return;
      case "rebuild_repository_note_index":
        return notes.map(({ path, title, frontmatter, preview }, id) => ({ id: id + 1, path, title, frontmatter, preview, tags: [] }));
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
      default: throw new Error(`Unexpected IPC command ${command}`);
    }
  });
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
  await user.click(screen.getByRole("button", { name: "Reading" }));
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
