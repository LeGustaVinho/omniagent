// @vitest-environment jsdom
import { invoke } from "@tauri-apps/api/core";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render as renderElement, screen, waitFor, within } from "@testing-library/react";
import type { ReactElement } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import IdeWorkspace from "@/ide-workspace";
import type { FileFormat } from "@/source-file-format";
import type { ItemEdit } from "@/checkout-item-operations";
import type { EditorExcerpt } from "@/editor-context";
import type { WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  Channel: class { onmessage = () => {}; },
}));
vi.mock("@/ide-code-editor", () => ({
  CodeEditor: ({ path, content, update, onExcerpt }: { path: string; content: string; update: (path: string, text: string) => void;
    onExcerpt?: (excerpt: EditorExcerpt, instruction: string) => void }) =>
    <><textarea aria-label={`Source editor for ${path}`} value={content} onChange={(event) => update(path, event.target.value)} />
      {onExcerpt && <button type="button" onClick={() => onExcerpt({ path, text: content, startLine: 2, endLine: 3, language: "rust" }, "Explain this code.")}>
        Explain selection…
      </button>}
    </>,
}));
vi.mock("@/git-diff-view", () => ({
  MonacoComparison: ({ file }: { file: { original: string; modified: string } }) =>
    <div aria-label="Compared local versions"><pre>{file.original}</pre><pre>{file.modified}</pre></div>,
}));
const terminalProps = vi.hoisted(() => vi.fn());
vi.mock("@/ide-terminal", () => ({
  ExampleTerminal: () => null,
  ManualTerminal: ({ onContextText: _onContextText, onOpenFile: _onOpenFile, ...props }: {
    checkoutId: string; active: boolean; clearRevision?: number; onContextText?: unknown; onOpenFile?: unknown;
  }) => {
    terminalProps(props);
    return null;
  },
}));

beforeEach(() => vi.stubGlobal("ResizeObserver", class {
  observe() {}
  unobserve() {}
  disconnect() {}
}));
afterEach(() => {
  cleanup();
  localStorage.clear();
  vi.unstubAllGlobals();
  vi.resetAllMocks();
});

const file = "src/review.rs";
const editorLabel = `Source editor for ${file}`;
const main = "checkout-main";
const alternate = "checkout-alternate";
const propsFor = (checkoutId: string): WorkspaceProps => ({
  checkout: { checkoutId, projectId: "project", repositoryRoot: "C:\\repo", checkoutRoot: `C:\\${checkoutId}` },
  simulation: null, openActivity: vi.fn(), openChanges: vi.fn(), openWorkspace: vi.fn(), startSimulation: vi.fn(),
});
const documentAt = (checkoutId: string, rev: string, text: string, path = file, format?: FileFormat) => ({
  status: "document",
  document: {
    revision: { id: rev, checkoutId, relativePath: path, sha256: rev, baseCommit: null },
    markdown: text,
    fileIdentity: rev,
    ...format,
  },
});

type Disk = ReturnType<typeof documentAt>;
type Draft = { revision: string; base: Disk["document"]["revision"]; markdown: string };
type Notice = (event: { checkoutId: string; relativePath: string }) => void;

function fixture(extraSources: Record<string, string> = {}) {
  const disk = new Map([[main, documentAt(main, "m1", "fn original() {}")], [alternate, documentAt(alternate, "a1", "fn other_worktree() {}")]]);
  const drafts = new Map<string, Draft>();
  const folders = new Map<string, string[]>();
  const revisions = new Map([...disk.values()].map(({ document }) => [document.revision.id, document]));
  const layouts = new Map<string, string>();
  const notices = new Map<string, Notice>();
  let counter = 0;
  const store = (checkout: string, revision: string, text: string) => {
    const next = documentAt(checkout, revision, text);
    disk.set(checkout, next);
    revisions.set(revision, next.document);
    return next;
  };
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    const args = input as {
      checkoutId: string; path?: string; layout?: string; revisionId?: string; baseRevision?: string;
      expectedRevision?: string; expectedDraftRevision?: string | null; markdown?: string; channel?: { onmessage: Notice };
      relativeDirectory?: string; name?: string; directory?: boolean;
      contents?: string;
      position?: number;
      operation?: "copy" | "move" | "recycle"; edits?: ItemEdit[];
    };
    const id = args.checkoutId;
    switch (command) {
      case "workbench_layout": return layouts.get(id) ?? null;
      case "save_workbench_layout": layouts.set(id, args.layout!); return;
      case "list_repository_source_files": return [disk.get(id)?.document.revision.relativePath, "docs/Guide.md", ...Object.keys(extraSources)].filter(Boolean);
      case "list_checkout_folders": return folders.get(id) ?? [];
      case "checkout_document_history": return { entries: [
        { position: 2, revisionId: "opened-m1", recordedAt: 250, origin: "saved" },
        { position: 1, revisionId: "historical-m0", recordedAt: 200, origin: "before_save" },
      ], nextBefore: null };
      case "checkout_document_history_text": return { text: args.position === 1 ? "fn historical() {}" : disk.get(id)?.document.markdown,
        lineEndings: "none", utf8Bom: false };
      case "create_checkout_item": {
        const path = [args.relativeDirectory, args.name].filter(Boolean).join("/");
        if (!args.directory) {
          if (args.contents === undefined) throw new Error("Expected a saved copy in this fixture");
          return path;
        }
        folders.set(id, [...(folders.get(id) ?? []), path]);
        return path;
      }
      case "open_checkout_item_in_shell": return;
      case "mutate_checkout_items": {
        if (args.operation === "move") {
          for (const { source, destination } of args.edits ?? []) {
            const current = disk.get(id)?.document;
            if (current?.revision.relativePath === source && destination) {
              disk.set(id, documentAt(id, current.revision.id, current.markdown, destination));
            }
          }
        }
        return { completed: args.edits ?? [], error: null };
      }
      case "list_conversations": return [{ id: "review-chat", checkoutId: id, title: "Code review", contextId: "ctx" }];
      case "add_checkout_files_to_conversation": return { revision: "attached", files: [] };
      case "append_checkout_context_to_conversation": return;
      case "list_repository_source_drafts": return drafts.has(id) ? [file] : [];
      case "open_repository_source_file": {
        const extra = args.path && extraSources[args.path];
        if (extra !== undefined && args.path) return documentAt(id, `${id}:${args.path}`, extra, args.path);
        return disk.get(id) ?? { status: "missing", relativePath: file };
      }
      case "read_repository_source_draft": return args.path === file ? drafts.get(id) ?? null : null;
      case "read_repository_source_revision": return revisions.get(args.revisionId!) ?? null;
      case "watch_repository_source": notices.set(id, args.channel!.onmessage); return `watch-${id}`;
      case "unwatch_repository_source": return;
      case "save_repository_source_draft": {
        expect(args.expectedDraftRevision).toBe(drafts.get(id)?.revision ?? null);
        const draft = { revision: `draft-${++counter}`, base: revisions.get(args.baseRevision!)!.revision, markdown: args.markdown! };
        drafts.set(id, draft);
        return draft;
      }
      case "discard_repository_source_draft":
        return drafts.get(id)?.revision === args.expectedDraftRevision && drafts.delete(id);
      case "save_repository_source_file":
        expect(disk.get(id)?.document.revision.id).toBe(args.expectedRevision);
        return { ...store(id, `disk-${++counter}`, args.markdown!), status: "saved" };
      default: throw new Error(command);
    }
  });
  return {
    drafts, layouts, disk,
    changeDisk(checkout: string, revision: string, text: string) {
      store(checkout, revision, text);
      notices.get(checkout)?.({ checkoutId: checkout, relativePath: file });
    },
  };
}

const views = () => screen.getAllByRole<HTMLTextAreaElement>("textbox", { name: editorLabel });
const closeButtons = () => screen.getAllByRole("button", { name: "Close review.rs" });

function render(element: ReactElement) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return renderElement(element, { wrapper: ({ children }) => <QueryClientProvider client={client}>{children}</QueryClientProvider> });
}

it("previews an exact checkout file format and saves only after confirming the reviewed bytes", async () => {
  const { disk } = fixture();
  const source = "fn original() {}\n";
  disk.set(alternate, documentAt(alternate, "a1", source, file, { utf8Bom: false, encoding: "utf8", lineEndings: "lf" }));
  const native = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation((command, args) => {
    if (command === "preview_repository_source_format") return Promise.resolve({
      originalEncoding: "utf8", originalLineEndings: "lf", encoding: "utf16_le", lineEndings: "lf",
      originalBytes: 17, convertedBytes: 36, originalPrefix: "66 6E 20", convertedPrefix: "FF FE 66 00 6E 00 20 00", sha256: "reviewed",
    });
    if (command === "save_repository_source_format") {
      const converted = documentAt(alternate, "a2", source, file, { utf8Bom: false, encoding: "utf16_le", lineEndings: "lf" });
      disk.set(alternate, converted);
      return Promise.resolve({ ...converted, status: "saved" });
    }
    return native(command, args);
  });
  render(<IdeWorkspace {...propsFor(alternate)} target={file} />);
  await screen.findByRole("textbox", { name: editorLabel });
  const review = async () => {
    fireEvent.contextMenu(await screen.findByLabelText("Saved file format UTF-8, LF"));
    fireEvent.keyDown(await screen.findByRole("menuitem", { name: "Change encoding" }), { key: "ArrowRight" });
    fireEvent.click(await screen.findByRole("menuitem", { name: "UTF-16 LE with BOM" }));
    const dialog = await screen.findByRole("dialog", { name: "Review file format conversion" });
    await within(dialog).findByText("UTF-16 LE with BOM · LF · 36 bytes");
    expect(within(dialog).getByLabelText("Converted initial bytes").textContent).toBe("FF FE 66 00 6E 00 20 00");
    return dialog;
  };
  const args = { checkoutId: alternate, path: file, expectedRevision: "a1", change: { encoding: "utf16_le" } };
  const first = await review();
  expect(invoke).toHaveBeenCalledWith("preview_repository_source_format", args);
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "save_repository_source_format")).toHaveLength(0);
  fireEvent.click(within(first).getByRole("button", { name: "Cancel" }));
  const second = await review();
  fireEvent.click(within(second).getByRole("button", { name: "Convert and save" }));
  await screen.findByLabelText("Saved file format UTF-16 LE with BOM, LF");
  expect(invoke).toHaveBeenCalledWith("save_repository_source_format", { ...args, previewSha256: "reviewed" });
  expect(disk.get(main)?.document.revision.id).toBe("m1");
  expect((screen.getByRole("textbox", { name: editorLabel }) as HTMLTextAreaElement).value).toBe(source);
});

const views = () => screen.getAllByRole<HTMLTextAreaElement>("textbox", { name: editorLabel });
const closeButtons = () => screen.getAllByRole("button", { name: "Close review.rs" });

function render(element: ReactElement) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return renderElement(element, { wrapper: ({ children }) => <QueryClientProvider client={client}>{children}</QueryClientProvider> });
}

it("uses the clicked row for the Explorer, even with another source file active", async () => {
  fixture();
  render(<IdeWorkspace {...propsFor(alternate)} />);
  fireEvent.click(await screen.findByRole("button", { name: "src" }));
  fireEvent.click(screen.getByRole("button", { name: "docs" }));
  fireEvent.click(await screen.findByRole("button", { name: "review.rs" }));
  await screen.findByRole("textbox", { name: editorLabel });

  fireEvent.contextMenu(screen.getByRole("button", { name: "Guide.md" }));
  const menu = await screen.findByRole("menu", { name: "docs/Guide.md" });
  fireEvent.click(within(menu).getByRole("menuitem", { name: "Show in File Explorer" }));

  expect(invoke).toHaveBeenCalledWith("open_checkout_item_in_shell", {
    checkoutId: alternate, relativePath: "docs/Guide.md", action: "reveal",
  });
  expect(views()).toHaveLength(1);
  expect(screen.getByRole("button", { name: "review.rs" }).getAttribute("aria-current")).toBe("page");
});

it("compares local history in the clicked checkout and loads a revision as a protected unsaved edit", async () => {
  const service = fixture();
  render(<IdeWorkspace {...propsFor(alternate)} />);
  fireEvent.click(await screen.findByRole("button", { name: "src" }));
  const row = screen.getByRole("button", { name: "review.rs" });
  fireEvent.click(row);
  const editor = await screen.findByRole<HTMLTextAreaElement>("textbox", { name: editorLabel });
  fireEvent.change(editor, { target: { value: "fn existing_draft() {}" } });
  await waitFor(() => expect(service.drafts.get(alternate)?.markdown).toBe("fn existing_draft() {}"));

  fireEvent.contextMenu(row);
  fireEvent.click(await screen.findByRole("menuitem", { name: "Show local history" }));
  const history = await screen.findByRole("dialog", { name: "Local file history" });
  fireEvent.click(await within(history).findByRole("button", { name: /Before saving/ }));
  const comparison = await within(history).findByLabelText("Compared local versions");
  expect(comparison.textContent).toBe("fn historical() {}fn existing_draft() {}");
  expect(invoke).toHaveBeenCalledWith("checkout_document_history_text", { checkoutId: alternate, path: file, position: 1 });

  fireEvent.click(within(history).getByRole("button", { name: "Load text into editor" }));
  expect((await within(history).findByRole("alert")).textContent).toContain("Save or discard the current file draft");
  expect(service.drafts.get(alternate)?.markdown).toBe("fn existing_draft() {}");
  fireEvent.click(within(history).getByRole("button", { name: "Close dialog" }));
  fireEvent.change(editor, { target: { value: "fn other_worktree() {}" } });
  await waitFor(() => expect(service.drafts.has(alternate)).toBe(false));

  fireEvent.contextMenu(row);
  fireEvent.click(await screen.findByRole("menuitem", { name: "Show local history" }));
  const reopened = await screen.findByRole("dialog", { name: "Local file history" });
  fireEvent.click(await within(reopened).findByRole("button", { name: /Before saving/ }));
  const load = within(reopened).getByRole("button", { name: "Load text into editor" });
  await waitFor(() => expect((load as HTMLButtonElement).disabled).toBe(false));
  fireEvent.click(load);
  await waitFor(() => expect(service.drafts.get(alternate)?.markdown).toBe("fn historical() {}"));
  expect(editor.value).toBe("fn historical() {}");
  expect(service.disk.get(alternate)?.document.markdown).toBe("fn other_worktree() {}");
  expect(service.disk.get(main)?.document.markdown).toBe("fn original() {}");
});

it("renames the underlying file and keeps split editor tabs on the renamed path", async () => {
  const service = fixture();
  render(<IdeWorkspace {...propsFor(main)} />);
  fireEvent.click(await screen.findByRole("button", { name: "src" }));
  const row = screen.getByRole("button", { name: "review.rs" });
  fireEvent.click(row);
  await screen.findByRole("textbox", { name: editorLabel });
  fireEvent.click(screen.getByRole("button", { name: "Split editor" }));
  expect(views()).toHaveLength(2);
  const tab = closeButtons()[0]!.closest(".file-tab-label")!;
  fireEvent.contextMenu(tab);
  fireEvent.click(await screen.findByRole("menuitem", { name: "Pin tab" }));
  expect(await screen.findByLabelText("Pinned")).toBeTruthy();

  fireEvent.contextMenu(row);
  fireEvent.click(await screen.findByRole("menuitem", { name: "Rename…" }));
  const dialog = await screen.findByRole("dialog", { name: "Rename" });
  fireEvent.change(within(dialog).getByRole("textbox", { name: "New name" }), { target: { value: "renamed.rs" } });
  fireEvent.click(within(dialog).getByRole("button", { name: "Rename" }));

  const editors = await screen.findAllByRole<HTMLTextAreaElement>("textbox", { name: "Source editor for src/renamed.rs" });
  expect(editors.map(({ value }) => value)).toEqual(["fn original() {}", "fn original() {}"]);
  expect(screen.queryByRole("textbox", { name: editorLabel })).toBeNull();
  expect(invoke).toHaveBeenCalledWith("mutate_checkout_items", {
    checkoutId: main, operation: "move", edits: [{ source: file, directory: false, destination: "src/renamed.rs" }],
  });
  await waitFor(() => {
    expect(service.layouts.get(main)).toContain('"path":"src/renamed.rs"');
    expect(service.layouts.get(main)).toContain('"pinned":true');
  });
});

it("closes tabs to the right of an inactive tab, prompts on drafts and preserves pins", async () => {
  const service = fixture({ "src/archive.rs": "fn archive() {}", "src/keep.rs": "fn keep() {}", "src/recent.rs": "fn recent() {}" });
  render(<IdeWorkspace {...propsFor(main)} />);
  fireEvent.click(await screen.findByRole("button", { name: "src" }));
  const open = async (name: string) => {
    fireEvent.click(screen.getByRole("button", { name }));
    return screen.findByRole<HTMLTextAreaElement>("textbox", { name: `Source editor for src/${name}` });
  };
  const close = (name: string) => screen.getByRole("button", { name: `Close ${name}` });
  const contextMenu = async (name: string) => {
    fireEvent.contextMenu(close(name).closest(".file-tab-label")!);
    return within(await screen.findByRole("menu", { name }));
  };
  await open("archive.rs");
  fireEvent.change(await open("review.rs"), { target: { value: "fn saved_from_batch() {}" } });
  await waitFor(() => expect(service.drafts.get(main)?.markdown).toBe("fn saved_from_batch() {}"));
  await open("keep.rs");
  fireEvent.click((await contextMenu("keep.rs")).getByRole("menuitem", { name: "Pin tab" }));
  await open("recent.rs");

  fireEvent.click((await contextMenu("archive.rs")).getByRole("menuitem", { name: "Close tabs to the right" }));
  const initialPrompt = within(await screen.findByRole("dialog", { name: "Save changes?" }));
  expect(initialPrompt.getByText("src/review.rs has unsaved changes.")).toBeTruthy();
  fireEvent.click(initialPrompt.getByRole("button", { name: "Cancel" }));
  expect(close("recent.rs")).toBeTruthy();
  expect(service.disk.get(main)?.document.markdown).toBe("fn original() {}");

  fireEvent.click((await contextMenu("archive.rs")).getByRole("menuitem", { name: "Close tabs to the right" }));
  const saveClose = within(await screen.findByRole("dialog", { name: "Save changes?" })).getByRole("button", { name: "Save and close" });
  fireEvent.click(saveClose);
  fireEvent.click(saveClose);
  await waitFor(() => expect(screen.queryByRole("button", { name: "Close recent.rs" })).toBeNull());
  expect(screen.queryByRole("button", { name: "Close review.rs" })).toBeNull();
  expect(close("archive.rs")).toBeTruthy();
  expect(close("keep.rs").closest(".file-tab-label")?.querySelector('[aria-label="Pinned"]')).toBeTruthy();
  expect(service.disk.get(main)?.document.markdown).toBe("fn saved_from_batch() {}");
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "save_repository_source_file")).toHaveLength(1);

  fireEvent.click((await contextMenu("archive.rs")).getByRole("menuitem", { name: "Reopen closed tab" }));
  expect((await screen.findByRole<HTMLTextAreaElement>("textbox", { name: "Source editor for src/recent.rs" })).value).toBe("fn recent() {}");
});

it("moves the clicked tab to a new group and merges groups without changing the draft or pin", async () => {
  const service = fixture({ "src/other.rs": "fn other() {}" });
  render(<IdeWorkspace {...propsFor(main)} />);
  fireEvent.click(await screen.findByRole("button", { name: "src" }));
  fireEvent.click(screen.getByRole("button", { name: "review.rs" }));
  fireEvent.change(await screen.findByRole("textbox", { name: editorLabel }), { target: { value: "fn draft_before_move() {}" } });
  await waitFor(() => expect(service.drafts.get(main)?.markdown).toBe("fn draft_before_move() {}"));
  fireEvent.click(screen.getByRole("button", { name: "other.rs" }));
  await screen.findByRole("textbox", { name: "Source editor for src/other.rs" });

  const selected = () => screen.getByRole("button", { name: "Close review.rs" }).closest(".file-tab-label")!;
  fireEvent.contextMenu(selected());
  fireEvent.click(await screen.findByRole("menuitem", { name: "Pin tab" }));
  fireEvent.contextMenu(selected());
  fireEvent.keyDown(await screen.findByRole("menuitem", { name: "Move tab" }), { key: "ArrowRight" });
  fireEvent.click(await screen.findByRole("menuitem", { name: "New group to the right" }));

  const movedGroup = selected().closest(".dv-groupview")!;
  expect(movedGroup).not.toBe(screen.getByRole("button", { name: "Close other.rs" }).closest(".dv-groupview"));
  expect(views()).toHaveLength(1);
  expect(views()[0]?.value).toBe("fn draft_before_move() {}");
  expect(within(movedGroup as HTMLElement).getByLabelText("Pinned")).toBeTruthy();
  expect(screen.queryByRole("dialog", { name: "Save changes?" })).toBeNull();

  fireEvent.contextMenu(within(movedGroup as HTMLElement).getByRole("button", { name: "Actions for Editor group" }));
  fireEvent.click(await screen.findByRole("menuitem", { name: "Merge all groups here" }));
  expect(selected().closest(".dv-groupview")).toBe(screen.getByRole("button", { name: "Close other.rs" }).closest(".dv-groupview"));
  expect(await screen.findByLabelText("Pinned")).toBeTruthy();
  expect(service.drafts.get(main)?.markdown).toBe("fn draft_before_move() {}");
});

it("reveals the breadcrumb folder and reopens the last tab from the empty group", async () => {
  fixture();
  render(<IdeWorkspace {...propsFor(alternate)} />);
  fireEvent.click(await screen.findByRole("button", { name: "src" }));
  fireEvent.click(screen.getByRole("button", { name: "review.rs" }));
  await screen.findByRole("textbox", { name: editorLabel });
  const path = within(screen.getByRole("navigation", { name: "File breadcrumbs" }));
  fireEvent.contextMenu(path.getByRole("button", { name: "Reveal folder src" }));
  fireEvent.click(await screen.findByRole("menuitem", { name: "Show in File Explorer" }));
  expect(invoke).toHaveBeenCalledWith("open_checkout_item_in_shell", { checkoutId: alternate, relativePath: "src", action: "reveal" });

  fireEvent.click(closeButtons()[0]!);
  fireEvent.contextMenu(await screen.findByText("Open a source file"));
  fireEvent.click(await screen.findByRole("menuitem", { name: "Reopen closed tab" }));
  expect((await screen.findByRole<HTMLTextAreaElement>("textbox", { name: editorLabel })).value).toBe("fn other_worktree() {}");
});

it("uses the selected rows for bulk recycling after confirmation", async () => {
  fixture();
  render(<IdeWorkspace {...propsFor(alternate)} />);
  fireEvent.click(await screen.findByRole("button", { name: "src" }));
  fireEvent.click(screen.getByRole("button", { name: "docs" }));
  const source = screen.getByRole("button", { name: "review.rs" });
  const note = screen.getByRole("button", { name: "Guide.md" });
  fireEvent.click(source);
  await screen.findByRole("textbox", { name: editorLabel });
  fireEvent.click(note, { ctrlKey: true });
  expect(source.getAttribute("aria-pressed")).toBe("true");
  expect(note.getAttribute("aria-pressed")).toBe("true");
  fireEvent.contextMenu(note);
  const menu = await screen.findByRole("menu", { name: "2 selected items" });
  fireEvent.click(within(menu).getByRole("menuitem", { name: "Move to Recycle Bin…" }));
  const dialog = await screen.findByRole("dialog", { name: "Move to Recycle Bin" });
  expect(invoke).not.toHaveBeenCalledWith("mutate_checkout_items", expect.anything());
  fireEvent.click(within(dialog).getByRole("button", { name: "Move to Recycle Bin" }));
  await screen.findByText("Recycled 2 items");
  expect(invoke).toHaveBeenCalledWith("mutate_checkout_items", {
    checkoutId: alternate, operation: "recycle", edits: [
      { source: "docs/Guide.md", directory: false, destination: null },
      { source: file, directory: false, destination: null },
    ],
  });
});

it("opens a folder terminal from the keyboard menu and keeps the root session mounted", async () => {
  fixture();
  render(<IdeWorkspace {...propsFor(main)} />);
  const folder = await screen.findByRole("button", { name: "src" });
  folder.focus();
  fireEvent.keyDown(folder, { key: "F10", shiftKey: true });
  const menu = await screen.findByRole("menu", { name: "Actions for src" });
  fireEvent.click(within(menu).getByRole("menuitem", { name: "Open terminal here" }));

  await waitFor(() => expect(terminalProps).toHaveBeenCalledWith({
    checkoutId: main, active: true, relativeDirectory: "src", autoStart: true, clearRevision: 0,
  }));
  expect(terminalProps).toHaveBeenCalledWith({ checkoutId: main, active: false, clearRevision: 0 });
  expect((screen.getByRole("combobox", { name: "Terminal directory" }) as HTMLSelectElement).value).toBe("src");
});

it("creates an empty folder under the clicked parent and refreshes the tree", async () => {
  fixture();
  render(<IdeWorkspace {...propsFor(main)} />);
  fireEvent.contextMenu(await screen.findByRole("button", { name: "src" }));
  fireEvent.click(await screen.findByRole("menuitem", { name: "New folder…" }));
  const dialog = await screen.findByRole("dialog", { name: "New folder" });
  fireEvent.change(within(dialog).getByRole("textbox", { name: "Name" }), { target: { value: "empty" } });
  fireEvent.click(within(dialog).getByRole("button", { name: "Create" }));

  expect(await screen.findByRole("button", { name: "empty" })).toHaveProperty("title", "src/empty");
  expect(invoke).toHaveBeenCalledWith("create_checkout_item", {
    checkoutId: main, relativeDirectory: "src", name: "empty", directory: true,
  });
});

it("adds the clicked file to the chosen conversation draft and opens the draft", async () => {
  fixture();
  const props = propsFor(alternate);
  render(<IdeWorkspace {...props} />);
  fireEvent.click(await screen.findByRole("button", { name: "docs" }));
  fireEvent.contextMenu(await screen.findByRole("button", { name: "Guide.md" }));
  fireEvent.click(await screen.findByRole("menuitem", { name: "Add to conversation…" }));
  const dialog = await screen.findByRole("dialog", { name: "Add checkout context" });
  await within(dialog).findByRole("option", { name: "Code review" });
  fireEvent.change(within(dialog).getByRole("combobox", { name: "Conversation" }), { target: { value: "review-chat" } });
  fireEvent.click(within(dialog).getByRole("button", { name: "Add to draft" }));

  await waitFor(() => expect(props.openWorkspace).toHaveBeenCalledWith("conversations", "review-chat"));
  expect(invoke).toHaveBeenCalledWith("add_checkout_files_to_conversation", {
    checkoutId: alternate, conversationId: "review-chat", paths: ["docs/Guide.md"],
  });
});

it("saves the open editor buffer as a new file without saving the original", async () => {
  const service = fixture();
  render(<IdeWorkspace {...propsFor(alternate)} target={file} />);
  const editor = await screen.findByRole("textbox", { name: editorLabel });
  fireEvent.change(editor, { target: { value: "fn edited_copy() {}" } });
  const tab = await screen.findByText("review.rs", { selector: ".file-tab-label span" });
  fireEvent.contextMenu(tab);
  fireEvent.click(await screen.findByRole("menuitem", { name: "Save a copy…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Save a copy" }));
  expect(dialog.getByRole<HTMLInputElement>("textbox", { name: "Name" }).value).toBe("review copy.rs");
  fireEvent.click(dialog.getByRole("button", { name: "Save copy" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("create_checkout_item", {
    checkoutId: alternate, relativeDirectory: "src", name: "review copy.rs", directory: false, contents: "fn edited_copy() {}",
  }));
  expect(service.disk.get(alternate)?.document.markdown).toBe("fn other_worktree() {}");
  expect(invoke).not.toHaveBeenCalledWith("save_repository_source_file", expect.anything());
});

it("previews live editor text and appends its range and prompt only to the chosen draft", async () => {
  fixture();
  const props = propsFor(alternate);
  render(<IdeWorkspace {...props} target={file} />);
  const editor = await screen.findByRole("textbox", { name: editorLabel });
  fireEvent.change(editor, { target: { value: "let value = \"```\";\n  check(value);\n" } });
  fireEvent.click(screen.getByRole("button", { name: "Explain selection…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Add checkout context" }));
  expect(dialog.getByLabelText("Selected text").textContent).toBe("let value = \"```\";\n  check(value);\n");
  await dialog.findByRole("option", { name: "Code review" });
  fireEvent.change(dialog.getByRole("combobox", { name: "Conversation" }), { target: { value: "review-chat" } });
  fireEvent.click(dialog.getByRole("button", { name: "Add to draft" }));
  await waitFor(() => expect(props.openWorkspace).toHaveBeenCalledWith("conversations", "review-chat"));
  expect(invoke).toHaveBeenCalledWith("append_checkout_context_to_conversation", {
    checkoutId: alternate, conversationId: "review-chat",
    context: `Explain this code.\n\nSource: ${file}:2-3\n\n` + "````rust\nlet value = \"```\";\n  check(value);\n````",
  });
  expect(invoke).not.toHaveBeenCalledWith("add_checkout_files_to_conversation", expect.anything());
  expect(invoke).not.toHaveBeenCalledWith("create_conversation", expect.anything());
});

it("exposes the shared Clear terminal control in the active worktree", async () => {
  fixture();
  render(<IdeWorkspace {...propsFor(main)} />);
  await waitFor(() => expect(terminalProps).toHaveBeenLastCalledWith({ checkoutId: main, active: true, clearRevision: 0 }));
  const clear = screen.getByRole<HTMLButtonElement>("button", { name: "Clear terminal" });
  fireEvent.click(clear);
  expect(terminalProps).toHaveBeenLastCalledWith({ checkoutId: main, active: true, clearRevision: 1 });
  fireEvent.click(screen.getByRole("button", { name: "Problems" }));
  expect(clear.disabled).toBe(true);
  expect(terminalProps).toHaveBeenLastCalledWith({ checkoutId: main, active: false, clearRevision: 1 });
});

it("opens the checkout explorer, shares edits across splits, and prompts only on the last dirty close", async () => {
  const service = fixture();
  const props = propsFor(main);
  render(<IdeWorkspace {...props} />);
  const folder = await screen.findByRole("button", { name: "src" });
  expect(folder.getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryByRole("button", { name: "review.rs" })).toBeNull();
  fireEvent.click(folder);
  fireEvent.click(screen.getByRole("button", { name: "docs" }));
  const tree = await screen.findByRole("button", { name: "review.rs" });
  fireEvent.click(screen.getByRole("button", { name: "Guide.md" }));
  expect(props.openWorkspace).toHaveBeenCalledWith("knowledge", "docs/Guide.md");
  fireEvent.click(tree);
  const editor = await screen.findByRole("textbox", { name: editorLabel });
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("watch_repository_source", expect.objectContaining({ checkoutId: main, baseRevision: "m1" })));
  fireEvent.change(editor, { target: { value: "fn edited() {}" } });
  await waitFor(() => expect(service.drafts.get(main)?.markdown).toBe("fn edited() {}"));
  fireEvent.click(screen.getByRole("button", { name: "Split editor" }));
  await waitFor(() => expect(views().map(({ value }) => value)).toEqual(["fn edited() {}", "fn edited() {}"]));
  fireEvent.click(closeButtons()[1]!);
  await waitFor(() => expect(views()).toHaveLength(1));
  expect(screen.queryByRole("dialog")).toBeNull();
  fireEvent.click(closeButtons()[0]!);
  const dialog = screen.getByRole("dialog", { name: "Save changes?" });
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  expect(views()[0]?.value).toBe("fn edited() {}");
  fireEvent.click(closeButtons()[0]!);
  fireEvent.click(screen.getByRole("button", { name: "Save and close" }));
  await waitFor(() => expect(screen.queryByRole("textbox", { name: editorLabel })).toBeNull());
  expect(invoke).toHaveBeenCalledWith("save_repository_source_file", {
    checkoutId: main, path: file, expectedRevision: "m1", markdown: "fn edited() {}",
  });
  expect(service.drafts.has(main)).toBe(false);
});

it("restores source splits and drafts, shows an external conflict, and keeps the other checkout separate", async () => {
  const service = fixture();
  const props = propsFor(main);
  const first = render(<IdeWorkspace {...props} target="SRC/REVIEW.RS" />);
  const editor = await screen.findByRole("textbox", { name: editorLabel });
  fireEvent.click(await screen.findByRole("button", { name: "src" }));
  const docs = screen.getByRole("button", { name: "docs" });
  fireEvent.click(docs);
  fireEvent.click(docs);
  expect(invoke).toHaveBeenCalledWith("open_repository_source_file", { checkoutId: main, path: "SRC/REVIEW.RS" });
  act(() => service.changeDisk(main, "m2", "fn clean_refresh() {}"));
  await waitFor(() => expect(views()[0]?.value).toBe("fn clean_refresh() {}"));
  fireEvent.click(screen.getByRole("button", { name: "Split editor" }));
  await waitFor(() => expect(views()).toHaveLength(2));
  await waitFor(() => expect(service.layouts.get(main)).toContain(`split:${file}`));
  fireEvent.change(editor, { target: { value: "fn unsaved() {}" } });
  await waitFor(() => expect(service.drafts.get(main)?.markdown).toBe("fn unsaved() {}"));
  first.unmount();
  service.changeDisk(main, "m3", "fn outside() {}");

  const second = render(<IdeWorkspace {...props} />);
  await waitFor(() => expect(views().map(({ value }) => value)).toEqual(["fn unsaved() {}", "fn unsaved() {}"]));
  expect(screen.getByRole("button", { name: "review.rs" }).getAttribute("aria-current")).toBe("page");
  expect(screen.queryByRole("button", { name: "Guide.md" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Review external change" }));
  const conflict = screen.getByRole("dialog", { name: "File changed externally" });
  expect(within(conflict).getByText("fn unsaved() {}")).toBeTruthy();
  expect(within(conflict).getByText("fn outside() {}")).toBeTruthy();
  fireEvent.click(within(conflict).getByRole("button", { name: "Keep local edits" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  fireEvent.click(screen.getByRole("button", { name: "Save file" }));
  await waitFor(() => expect(service.disk.get(main)?.document.markdown).toBe("fn unsaved() {}"));
  expect(invoke).toHaveBeenCalledWith("save_repository_source_file", {
    checkoutId: main, path: file, expectedRevision: "m3", markdown: "fn unsaved() {}",
  });
  await waitFor(() => expect(service.drafts.has(main)).toBe(false));
  second.rerender(<IdeWorkspace {...propsFor(alternate)} target={file} />);
  await waitFor(() => expect(views().map(({ value }) => value)).toEqual(["fn other_worktree() {}"]));
  expect(screen.queryByRole("button", { name: "review.rs" })).toBeNull();
});

it("shows the saved checkout path and the failure in a restored file tab", async () => {
  const service = fixture();
  const props = propsFor(main);
  const first = render(<IdeWorkspace {...props} target={file} />);
  await screen.findByRole("textbox", { name: editorLabel });
  await waitFor(() => expect(service.layouts.get(main)).toContain(file));
  first.unmount();
  service.disk.delete(main);

  render(<IdeWorkspace {...props} />);
  const heading = await screen.findByRole("heading", { name: "File unavailable" });
  const unavailable = within(heading.closest("section")!);
  expect(unavailable.getByText(file)).toBeTruthy();
  expect(unavailable.getByText("The file is missing from this checkout.")).toBeTruthy();
});

it("reuses the canonical tab in each checkout and discards the last dirty view without saving", async () => {
  const service = fixture();
  const workspace = (checkoutId: string) => <IdeWorkspace {...propsFor(checkoutId)} target="SRC/REVIEW.RS" />;
  const { rerender } = render(workspace(main));

  for (const checkoutId of [main, alternate]) {
    rerender(workspace(checkoutId));
    const original = service.disk.get(checkoutId)!;
    const editor = await screen.findByRole<HTMLTextAreaElement>("textbox", { name: editorLabel });
    expect(editor.value).toBe(original.document.markdown);
    expect(invoke).toHaveBeenCalledWith("open_repository_source_file", { checkoutId, path: "SRC/REVIEW.RS" });
    fireEvent.click(await screen.findByRole("button", { name: "src" }));
    const treeRow = await screen.findByRole("button", { name: "review.rs" });
    await act(async () => { fireEvent.click(treeRow); });
    expect(views()).toHaveLength(1);
    expect(views()[0]).toBe(editor);
    expect(closeButtons()).toHaveLength(1);

    const edit = `// discarded in ${checkoutId}`;
    fireEvent.change(editor, { target: { value: edit } });
    await waitFor(() => expect(service.drafts.get(checkoutId)?.markdown).toBe(edit));
    const draft = service.drafts.get(checkoutId)!;
    fireEvent.click(closeButtons()[0]!);
    const dialog = screen.getByRole("dialog", { name: "Save changes?" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Discard changes" }));
    await waitFor(() => expect(screen.queryByRole("textbox", { name: editorLabel })).toBeNull());
    expect(invoke).toHaveBeenCalledWith("discard_repository_source_draft", {
      checkoutId, path: file, expectedDraftRevision: draft.revision,
    });
    expect(service.drafts.has(checkoutId)).toBe(false);
    expect(service.disk.get(checkoutId)).toBe(original);

    fireEvent.click(screen.getByRole("button", { name: "review.rs" }));
    const reopened = await screen.findByRole<HTMLTextAreaElement>("textbox", { name: editorLabel });
    expect(reopened.value).toBe(original.document.markdown);
    expect(closeButtons()).toHaveLength(1);
  }
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "save_repository_source_file")).toBe(false);
});
