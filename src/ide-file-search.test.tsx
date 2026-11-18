// @vitest-environment jsdom
import { EditorView } from "@codemirror/view";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { noteSearchTarget, ProjectFileSearch, readNoteSearchTarget, type RipgrepStatus } from "@/ide-file-search";
import IdeWorkspace from "@/ide-workspace";
import KnowledgeWorkspace from "@/knowledge-workspace";
import { SystemSettings } from "@/settings-system";
import type { WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), Channel: class { onmessage = () => undefined; } }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: () => Promise.resolve("0.1.0") }));
vi.mock("@/ide-code-editor", () => ({
  CodeEditor: ({ path, jump, onSearchText }: { path: string; jump?: { line: number; column?: number; token: number }; onSearchText: (text: string) => void }) => (
    <div aria-label={`Source editor for ${path}`} data-line={jump?.line} data-column={jump?.column} data-jump={jump?.token}>
      <button type="button" onClick={() => onSearchText("  selected.*literal  ")}>Search editor selection</button>
    </div>
  ),
}));
vi.mock("@/ide-terminal", () => ({ ExampleTerminal: () => null, ManualTerminal: () => null }));
afterEach(() => { cleanup(); vi.resetAllMocks(); vi.unstubAllGlobals(); });

const available: RipgrepStatus = { configuredExecutable: null, executable: "C:\\Tools\\rg.exe", version: "ripgrep 15.2.0", issue: null };
const missing: RipgrepStatus = { configuredExecutable: null, executable: null, version: null, issue: "ripgrep is not on PATH." };
const client = () => new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });

it("submits literal search to the selected checkout, marks partial results, and opens a specific line", async () => {
  const path = "nested/src/ação file.ts";
  const markdown = "docs/plan#blue.md";
  const checkoutId = "selected-worktree";
  const openWorkspace = vi.fn();
  const props: WorkspaceProps = {
    checkout: { projectId: "project", checkoutId, repositoryRoot: "C:\\repo", checkoutRoot: "C:\\linked" },
    openWorkspace, openActivity: vi.fn(), openChanges: vi.fn(),
  };
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "workbench_layout": case "read_repository_source_draft": return null;
      case "list_repository_source_files": return [path, markdown];
      case "list_repository_source_drafts": return [];
      case "save_workbench_layout": case "unwatch_repository_source": return undefined;
      case "watch_repository_source": return "watch";
      case "ripgrep_status": return available;
      case "search_repository_files": {
        const source = { path, line: 42, columns: [9, 16], occurrences: 2, excerpt: "  value a.*b   a.*b ", lineText: "  value a.*b   a.*b " };
        const scoped = Boolean(args && "relativeFile" in args);
        return { status: "results", truncated: !scoped, matches: scoped ? [source] : [source, { path: markdown, line: 4, excerpt: "Markdown result" }] };
      }
      case "open_repository_source_file":
        expect(args).toEqual({ checkoutId, path });
        return { status: "document", document: { revision: {
          id: "source", checkoutId, relativePath: path, sha256: "sha", baseCommit: null,
        }, markdown: "source", fileIdentity: "file" } };
      default: throw new Error(command);
    }
  });
  render(<QueryClientProvider client={client()}><IdeWorkspace {...props} /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Search" }));
  const form = within(screen.getByRole("form", { name: "Project file search" }));
  const input = form.getByRole("searchbox", { name: "Search file contents" });
  await user.type(input, "  a.*b  ");
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "search_repository_files")).toBe(false);
  await user.click(form.getByRole("button", { name: "Search" }));
  expect(invoke).toHaveBeenCalledWith("search_repository_files", { checkoutId, query: "  a.*b  " });
  expect(await form.findByText(/Search stopped early/)).toBeTruthy();
  const result = form.getByRole("button", { name: /nested\/src\/ação file.ts · 42/ });
  await user.click(result);
  const editor = await screen.findByLabelText(`Source editor for ${path}`);
  expect(editor.getAttribute("data-line")).toBe("42");
  expect(editor.getAttribute("data-column")).toBe("9");
  await user.click(result);
  await waitFor(() => expect(editor.getAttribute("data-jump")).toBe("2"));
  expect(screen.getAllByRole("button", { name: "Close ação file.ts" })).toHaveLength(1);
  await user.click(form.getByRole("button", { name: /plan#blue.md · 4/ }));
  const [workspace, target] = openWorkspace.mock.calls[0] ?? [];
  expect(workspace).toBe("knowledge");
  expect(readNoteSearchTarget(target)).toEqual({ path: markdown, line: 4 });
  fireEvent.contextMenu(form.getByRole("button", { name: /plan#blue.md · 4/ }));
  await user.click(screen.getByRole("menuitem", { name: "Open to the side" }));
  expect(readNoteSearchTarget(openWorkspace.mock.lastCall?.[1])).toEqual({ path: markdown, line: 4, side: true });

  const group = form.getByRole("button", { name: `${path}, 2 matches` });
  fireEvent.contextMenu(group);
  await user.click(screen.getByRole("menuitem", { name: "Copy visible occurrence references" }));
  expect(await navigator.clipboard.readText()).toBe(`${path}:42:9\n${path}:42:16`);
  fireEvent.contextMenu(result);
  await user.click(screen.getByRole("menuitem", { name: "Copy matching line" }));
  expect(await navigator.clipboard.readText()).toBe("  value a.*b   a.*b ");
  fireEvent.contextMenu(group);
  await user.click(screen.getByRole("menuitem", { name: "Open first match to the side" }));
  await waitFor(() => expect(screen.getAllByRole("button", { name: "Close ação file.ts" })).toHaveLength(2));
  expect(screen.getAllByLabelText(`Source editor for ${path}`).every((view) => view.getAttribute("data-column") === "9")).toBe(true);

  fireEvent.contextMenu(group);
  await user.click(screen.getByRole("menuitem", { name: "Search in this file" }));
  expect(await form.findByText(`Search in ${path}`)).toBeTruthy();
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("search_repository_files", { checkoutId, query: "  a.*b  ", relativeFile: path }));
  expect(form.queryByRole("button", { name: /plan#blue.md · 4/ })).toBeNull();
  const priorSearchCount = vi.mocked(invoke).mock.calls.filter(([command]) => command === "search_repository_files").length;
  await user.click(within(editor).getByRole("button", { name: "Search editor selection" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("search_repository_files", { checkoutId, query: "  selected.*literal  " }));
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "search_repository_files").slice(priorSearchCount))
    .toEqual([["search_repository_files", { checkoutId, query: "  selected.*literal  " }]]);
  expect((input as HTMLInputElement).value).toBe("  selected.*literal  ");
  expect(form.queryByText(`Search in ${path}`)).toBeNull();
  expect(await form.findByRole("button", { name: /plan#blue.md · 4/ })).toBeTruthy();
});

it("scopes content search to a closed Explorer file, an inactive tab and a file breadcrumb", async () => {
  const checkoutId = "linked-worktree";
  const first = "src/ação file.ts";
  const second = "src/active.ts";
  const props: WorkspaceProps = {
    checkout: { projectId: "project", checkoutId, repositoryRoot: "C:\\repo", checkoutRoot: "C:\\linked" },
    openWorkspace: vi.fn(), openActivity: vi.fn(), openChanges: vi.fn(),
  };
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "workbench_layout": case "read_repository_source_draft": return null;
      case "list_repository_source_files": return [first, second];
      case "list_repository_source_drafts": case "list_checkout_folders": return [];
      case "save_workbench_layout": case "unwatch_repository_source": return undefined;
      case "watch_repository_source": return "watch";
      case "ripgrep_status": return available;
      case "search_repository_files": return { status: "results", truncated: false, matches: [] };
      case "open_repository_source_file": {
        const { path } = args as { path: string };
        return { status: "document", document: { revision: {
          id: path, checkoutId, relativePath: path, sha256: "sha", baseCommit: null,
        }, markdown: "source", fileIdentity: path } };
      }
      default: throw new Error(command);
    }
  });
  render(<QueryClientProvider client={client()}><IdeWorkspace {...props} target={second} /></QueryClientProvider>);
  const user = userEvent.setup();
  await screen.findByLabelText(`Source editor for ${second}`);
  await user.click(await screen.findByRole("button", { name: "src" }));

  async function searchFileFrom(anchor: HTMLElement, path: string) {
    fireEvent.contextMenu(anchor);
    await user.click(await screen.findByRole("menuitem", { name: "Search in this file" }));
    const form = within(screen.getByRole("form", { name: "Project file search" }));
    expect(form.getByText(`Search in ${path}`)).toBeTruthy();
    const input = form.getByRole<HTMLInputElement>("searchbox", { name: "Search file contents" });
    await waitFor(() => expect(document.activeElement).toBe(input));
    return input;
  }

  const explorerSearch = await searchFileFrom(screen.getByRole("button", { name: "ação file.ts" }), first);
  await user.type(explorerSearch, "closed.*literal{Enter}");
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("search_repository_files", { checkoutId, query: "closed.*literal", relativeFile: first }));
  expect(screen.queryByRole("button", { name: "Close ação file.ts" })).toBeNull();

  await user.click(screen.getByRole("button", { name: "Files" }));
  await user.click(screen.getByRole("button", { name: "ação file.ts" }));
  await screen.findByLabelText(`Source editor for ${first}`);
  const activeTab = screen.getByRole("button", { name: "Close active.ts" }).closest<HTMLElement>(".file-tab-label")!;
  await user.click(within(activeTab).getByText("active.ts"));
  await screen.findByLabelText(`Source editor for ${second}`);
  const inactiveTab = screen.getByRole("button", { name: "Close ação file.ts" }).closest<HTMLElement>(".file-tab-label")!;
  const tabSearch = await searchFileFrom(inactiveTab, first);
  await user.type(tabSearch, "tab needle{Enter}");
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("search_repository_files", { checkoutId, query: "tab needle", relativeFile: first }));
  expect(screen.getByLabelText(`Source editor for ${second}`)).toBeTruthy();

  const breadcrumb = screen.getByRole("button", { name: "Reveal file active.ts" });
  const breadcrumbSearch = await searchFileFrom(breadcrumb, second);
  expect(breadcrumbSearch.value).toBe("tab needle");
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("search_repository_files", { checkoutId, query: "tab needle", relativeFile: second }));
});

it("shows a missing executable and immediately shares a validated System Settings path with search", async () => {
  const override = "C:\\Program Files\\ripgrep\\rg.exe";
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "runtime_status": return "Desktop runtime is ready.";
      case "desktop_cli_prerequisites": return { git: { executable: null, version: null, issue: null }, providers: [] };
      case "ripgrep_status": return missing;
      case "set_ripgrep_executable":
        expect(args).toEqual({ executable: override });
        return { ...available, configuredExecutable: override, executable: override };
      case "search_repository_files": return { status: "results", matches: [], truncated: false };
      default: throw new Error(command);
    }
  });
  render(<QueryClientProvider client={client()}>
    <ProjectFileSearch checkoutId="checkout" onOpen={vi.fn()} />
    <SystemSettings />
  </QueryClientProvider>);
  const search = within(screen.getByRole("form", { name: "Project file search" }));
  expect((await search.findByRole("alert")).textContent).toContain("System Settings");
  const user = userEvent.setup();
  await user.type(search.getByRole("searchbox"), "literal");
  const searchButton = search.getByRole<HTMLButtonElement>("button", { name: "Search" });
  expect(searchButton.disabled).toBe(true);
  const settings = within(screen.getByRole("form", { name: "ripgrep settings" }));
  fireEvent.change(settings.getByLabelText("ripgrep executable"), { target: { value: `"${override}"` } });
  await user.click(settings.getByRole("button", { name: "Save ripgrep path" }));
  expect(await settings.findByText(override)).toBeTruthy();
  await waitFor(() => expect(searchButton.disabled).toBe(false));
  expect(search.queryByRole("alert")).toBeNull();
  await user.click(searchButton);
  expect(await search.findByText("No matching lines.")).toBeTruthy();
});

it("ignores delayed hits when switching checkout", async () => {
  let complete!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementation(async command => {
    if (command === "ripgrep_status") return available;
    if (command === "search_repository_files") return new Promise(resolve => { complete = resolve; });
    throw new Error(command);
  });
  const queryClient = client();
  const open = vi.fn();
  const surface = (checkoutId: string) => <QueryClientProvider client={queryClient}>
    <ProjectFileSearch checkoutId={checkoutId} onOpen={open} />
  </QueryClientProvider>;
  const { rerender } = render(surface("first"));
  const user = userEvent.setup();
  await user.type(screen.getByRole("searchbox"), "find");
  await user.click(screen.getByRole("button", { name: "Search" }));
  await screen.findByText("Searching this checkout…");
  rerender(surface("second"));
  await act(async () => { complete({ status: "results", matches: [{ path: "old.txt", line: 5, excerpt: "old result" }], truncated: false }); });
  expect(screen.queryByText("old result")).toBeNull();
  expect(open).not.toHaveBeenCalled();
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "search_repository_files"))
    .toEqual([["search_repository_files", { checkoutId: "first", query: "find" }]]);
});

it("jumps to a Markdown match while preserving the canonical tab and repeated opens", async () => {
  const path = "docs/plan#blue.md";
  const content = "# A real note\n\nThird line\nFourth line";
  const checkoutId = "selected-worktree";
  const checkout = { projectId: "project", checkoutId, repositoryRoot: "C:\\repo", checkoutRoot: "C:\\worktree" };
  const props: WorkspaceProps = { checkout, openActivity: vi.fn(), openChanges: vi.fn(), openWorkspace: vi.fn() };
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  const unsupported = ["getClientRects", "getBoundingClientRect"].filter(name => !(name in Range.prototype));
  for (const name of unsupported) Object.defineProperty(Range.prototype, name, {
    configurable: true, value: name === "getClientRects" ? () => [] : () => new DOMRect(),
  });
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "workbench_layout": case "read_repository_markdown_draft": case "repository_indexed_note": return null;
      case "save_workbench_layout": case "unwatch_repository_note": case "unwatch_repository_note_index": return undefined;
      case "repository_note_catalog": return [{ path, frontmatter: null }];
      case "list_repository_markdown_drafts": case "repository_note_edges": case "resolve_repository_note_links": return [];
      case "watch_repository_note": case "watch_repository_note_index": return command;
      case "rebuild_repository_note_index": return [{ id: 1, path, title: "A real note", frontmatter: null, tags: [] }];
      case "open_repository_document": {
        expect(args).toEqual({ checkoutId, path });
        return { status: "document", document: {
          revision: { id: "saved", checkoutId, relativePath: path, sha256: "stable-sha", baseCommit: null }, markdown: content, fileIdentity: "file",
        } };
      }
      case "analyze_note_markdown": return { bufferRevision: (args as { bufferRevision: string }).bufferRevision, nodes: [] };
      default: throw new Error(command);
    }
  });
  try {
    const queryClient = client();
    const workspace = (line: number, sequence: number, side = false) => <QueryClientProvider client={queryClient}>
      <KnowledgeWorkspace {...props} target={noteSearchTarget(path, line, sequence, side)} />
    </QueryClientProvider>;
    const { rerender, unmount } = render(workspace(4, 1));
    const surface = await screen.findByLabelText(`Markdown editor for ${path}`);
    const editor = EditorView.findFromDOM(surface.closest<HTMLElement>(".cm-editor")!)!;
    const jump = (line: number) => waitFor(() => expect(editor.state.selection.main.head).toBe(editor.state.doc.line(line).from));
    await jump(4);
    rerender(workspace(3, 2));
    await jump(3);
    act(() => editor.dispatch({ selection: { anchor: 0 } }));
    rerender(workspace(3, 3));
    await jump(3);
    expect(screen.getAllByRole("button", { name: "Close plan#blue.md" })).toHaveLength(1);
    expect(readNoteSearchTarget(path)).toEqual({ path });
    expect(readNoteSearchTarget(noteSearchTarget(path, 0, 4))).toBeUndefined();
    rerender(workspace(4, 4, true));
    expect(await screen.findByLabelText(`Reading view for ${path}`)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Close plan#blue.md · Reading" })).toBeTruthy();
    unmount();
  } finally {
    cleanup();
    for (const name of unsupported) Reflect.deleteProperty(Range.prototype, name);
  }
});
