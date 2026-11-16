// @vitest-environment jsdom
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import IdeWorkspace from "@/ide-workspace";
import type { WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  Channel: class { onmessage = () => {}; },
}));
vi.mock("@/ide-code-editor", () => ({
  CodeEditor: ({ path, content, update }: { path: string; content: string; update: (path: string, text: string) => void }) =>
    <textarea aria-label={`Source editor for ${path}`} value={content} onChange={(event) => update(path, event.target.value)} />,
}));
vi.mock("@/ide-terminal", () => ({ ExampleTerminal: () => null, ManualTerminal: () => null }));

beforeEach(() => vi.stubGlobal("ResizeObserver", class {
  observe() {}
  unobserve() {}
  disconnect() {}
}));
afterEach(() => {
  cleanup();
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
const documentAt = (checkoutId: string, rev: string, text: string) => ({
  status: "document",
  document: {
    revision: { id: rev, checkoutId, relativePath: file, sha256: rev, baseCommit: null },
    markdown: text,
    fileIdentity: rev,
  },
});

type Disk = ReturnType<typeof documentAt>;
type Draft = { revision: string; base: Disk["document"]["revision"]; markdown: string };
type Notice = (event: { checkoutId: string; relativePath: string }) => void;

function fixture() {
  const disk = new Map([[main, documentAt(main, "m1", "fn original() {}")], [alternate, documentAt(alternate, "a1", "fn other_worktree() {}")]]);
  const drafts = new Map<string, Draft>();
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
    };
    const id = args.checkoutId;
    switch (command) {
      case "workbench_layout": return layouts.get(id) ?? null;
      case "save_workbench_layout": layouts.set(id, args.layout!); return;
      case "list_repository_source_files": return [file, "docs/Guide.md"];
      case "list_repository_source_drafts": return drafts.has(id) ? [file] : [];
      case "open_repository_source_file": return disk.get(id);
      case "read_repository_source_draft": return drafts.get(id) ?? null;
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

it("opens the checkout explorer, shares edits across splits, and prompts only on the last dirty close", async () => {
  const service = fixture();
  const props = propsFor(main);
  render(<IdeWorkspace {...props} />);
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
