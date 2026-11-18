// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type cytoscape from "cytoscape";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { NoteGraphEdge } from "@/knowledge-graph-model";
import KnowledgeWorkspace from "@/knowledge-workspace";
import type { IndexedNote } from "@/repository-note-index";
import type { WorkspaceProps } from "@/workbench-contract";

const instances = vi.hoisted(() => [] as cytoscape.Core[]);
vi.mock("cytoscape", async (importOriginal) => {
  const { default: createGraph } = await importOriginal<{
    default: (options: cytoscape.CytoscapeOptions) => cytoscape.Core;
  }>();
  return {
    default: (options: cytoscape.CytoscapeOptions) => {
      const layout = Object.assign({}, options.layout ?? { name: "circle" as const }, {
        boundingBox: { x1: 0, y1: 0, w: 400, h: 400 },
      });
      const graph = createGraph({ ...options, headless: true, container: undefined, layout });
      instances.push(graph);
      return graph;
    },
  };
});

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
  instances.length = 0;
  for (const name of missingRangeMethods) Reflect.deleteProperty(Range.prototype, name);
  vi.unstubAllGlobals();
  vi.resetAllMocks();
});

const alpha = "checkout-alpha";
const beta = "checkout-beta";
const start = "notes/Start.md";
const guide = "planos/Guia.md";
const secondary = "planos/Ação.md";
const unrelated = "archive/Unlinked.md";
const other = "separado/Review.md";
const projectId = "same-project";

type Projection = { notes: IndexedNote[]; edges: NoteGraphEdge[] };
type IndexNotice = { checkoutId: string; kind: "paths"; paths: string[] };
const row = (id: number, path: string, title: string, tags: string[] = []): IndexedNote => ({ id, path, title, tags, frontmatter: null });
const edge = (sourcePath: string, targetPath: string): NoteGraphEdge => ({ sourcePath, targetPath });
const firstProjection = (): Projection => ({
  notes: [
    row(101, start, "Alpha start"),
    row(102, guide, "Guia ativo", ["release"]),
    row(103, secondary, "Ação seguinte", ["release"]),
    row(104, unrelated, "Unlinked"),
  ],
  edges: [edge(start, guide), edge(guide, secondary)],
});

function mockIndex() {
  const projections = new Map<string, Projection>([
    [alpha, firstProjection()],
    [beta, { notes: [row(201, start, "Beta start"), row(202, other, "Review only in Beta")], edges: [edge(start, other)] }],
  ]);
  const watchers = new Map<string, (notice: IndexNotice) => void>();
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const input = (args ?? {}) as {
      checkoutId: string; path: string; baseRevision: string; bufferRevision: string; query: string;
      channel: { onmessage: (notice: IndexNotice) => void };
    };
    const projection = projections.get(input.checkoutId);
    switch (command) {
      case "workbench_layout": return null;
      case "save_workbench_layout": return;
      case "watch_repository_note_index":
        if (!projection) throw new Error("Unknown index watch");
        watchers.set(input.checkoutId, input.channel.onmessage);
        return `index-watch-${input.checkoutId}`;
      case "unwatch_repository_note_index": return;
      case "rebuild_repository_note_index":
      case "reconcile_repository_note_index":
        if (!projection) throw new Error("Unknown indexed checkout");
        return projection.notes;
      case "repository_note_edges":
        if (!projection) throw new Error("Unknown edge checkout");
        return projection.edges;
      case "search_repository_note_index":
        return projection?.notes.filter((note) => note.title.toLowerCase().includes(input.query.toLowerCase()));
      case "repository_note_catalog":
      case "list_repository_markdown_drafts": return [];
      case "read_repository_markdown_draft": return null;
      case "watch_repository_note": return `note-watch-${input.baseRevision}`;
      case "unwatch_repository_note": return;
      case "analyze_note_markdown": return { bufferRevision: input.bufferRevision, nodes: [] };
      case "open_repository_document": {
        const note = projection?.notes.find(({ path }) => path.toLowerCase() === input.path.toLowerCase());
        if (!note) throw new Error(`Unknown note ${input.path} in ${input.checkoutId}`);
        return { status: "document", document: {
          markdown: `# ${note.title}\n`,
          fileIdentity: `file-${note.id}`,
          revision: { id: `revision-${note.id}`, checkoutId: input.checkoutId, relativePath: note.path, sha256: "fixture-hash", baseCommit: null },
        } };
      }
      default: throw new Error(`Unexpected IPC command ${command}`);
    }
  });
  return { projections, watchers };
}

function workspace(checkoutId: string, client: QueryClient) {
  const props: WorkspaceProps = {
    checkout: { projectId, checkoutId, repositoryRoot: "C:\\repo", checkoutRoot: `C:\\worktrees\\${checkoutId}` },
    target: "notes/start.md",
    openWorkspace: vi.fn(), openActivity: vi.fn(), openChanges: vi.fn(),
  };
  return <QueryClientProvider client={client}><KnowledgeWorkspace {...props} /></QueryClientProvider>;
}

function queryClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
}

const paths = () => within(screen.getByRole("list", { name: "Graph notes" }))
  .getAllByRole("button", { name: (name) => !name.startsWith("Actions for ") }).map((button) => button.getAttribute("title"));

function currentGraph() {
  const graph = instances.at(-1);
  if (!graph) throw new Error("The workspace did not open Cytoscape");
  return graph;
}

const nodeId = (graph: cytoscape.Core, path: string) => {
  const node = graph.nodes().filter((entry) => entry.data("path") === path)[0];
  if (!node) throw new Error(`Graph node unavailable for ${path}`);
  return node.id();
};
const edgePaths = (graph: cytoscape.Core) => graph.edges().map((item) => [item.source().data("path"), item.target().data("path")]);

it("opens the local graph centered on the clicked unopened note while keeping the editor's active note", async () => {
  mockIndex();
  render(workspace(alpha, queryClient()));
  const user = userEvent.setup();
  await screen.findByRole("button", { name: "Close Start.md" });
  const tree = within(screen.getByRole("complementary", { name: "Knowledge navigation" }));
  const folder = await tree.findByRole("button", { name: "planos" });
  if (folder.getAttribute("aria-expanded") !== "true") await user.click(folder);
  const note = await tree.findByRole("button", { name: "Ação.md" });
  fireEvent.contextMenu(note);
  await user.click(await screen.findByRole("menuitem", { name: "Open local graph" }));
  const graphNotes = within(await screen.findByRole("list", { name: "Graph notes" }));
  expect(screen.getByRole("button", { name: "Local" }).getAttribute("aria-pressed")).toBe("true");
  expect(graphNotes.getByRole("button", { name: `Ação seguinte (${secondary})` }).getAttribute("aria-current")).toBe("true");
  expect(paths()).toEqual([guide, secondary]);
  expect(invoke).not.toHaveBeenCalledWith("open_repository_document", { checkoutId: alpha, path: secondary });

  await user.click(screen.getByRole("button", { name: "Global" }));
  expect(paths()).toContain(start);
  fireEvent.contextMenu(note);
  await user.click(await screen.findByRole("menuitem", { name: "Open local graph" }));
  await waitFor(() => expect(paths()).toEqual([guide, secondary]));

  await user.click(screen.getByRole("button", { name: "Graph" }));
  const previousFolder = tree.getByRole("button", { name: "notes" });
  if (previousFolder.getAttribute("aria-expanded") !== "true") await user.click(previousFolder);
  expect(tree.getByRole("button", { name: "Start.md" }).getAttribute("aria-current")).toBe("page");
});

it("uses a checkout's indexed edges to focus a canonical tab and keeps identities separate across worktrees", async () => {
  const { watchers } = mockIndex();
  const client = queryClient();
  const { rerender } = render(workspace(alpha, client));
  const user = userEvent.setup();
  await screen.findByRole("button", { name: "Close Start.md" });
  const fileTree = within(screen.getByRole("complementary", { name: "Knowledge navigation" }));
  await user.click(fileTree.getByRole("button", { name: "Search notes" }));
  const search = fileTree.getByRole("combobox", { name: "Find notes and content" });
  await user.click(search);
  await user.paste("Guia ativo");
  await fileTree.findByRole("option", { name: /Guia ativo/ });
  await user.keyboard("{Enter}");
  await screen.findByRole("button", { name: "Close Guia.md" });
  await user.click(fileTree.getByRole("button", { name: "Show notes" }));
  await user.click(screen.getByRole("button", { name: "Graph" }));
  await screen.findByRole("list", { name: "Graph notes" });
  await waitFor(() => expect(edgePaths(currentGraph())).toContainEqual([start, guide]));

  const before = currentGraph();
  const alphaStart = nodeId(before, start);
  expect(paths()).toEqual([start, guide, secondary, unrelated]);
  expect(invoke).toHaveBeenCalledWith("rebuild_repository_note_index", { checkoutId: alpha });
  expect(invoke).toHaveBeenCalledWith("repository_note_edges", { checkoutId: alpha });
  const reference = before.edges().filter((item) => item.target().data("path") === guide)[0];
  if (!reference) throw new Error("Missing edge targeting the canonical guide");
  await act(async () => { reference.target().emit("tap"); });
  await waitFor(() => expect(screen.queryByRole("list", { name: "Graph notes" })).toBeNull());
  expect(screen.getAllByRole("button", { name: "Close Guia.md" })).toHaveLength(1);
  const folder = fileTree.getByRole("button", { name: "planos" });
  if (folder.getAttribute("aria-expanded") !== "true") await user.click(folder);
  expect(fileTree.getByRole("button", { name: "Guia.md" }).getAttribute("aria-current")).toBe("page");
  expect(invoke).toHaveBeenCalledWith("open_repository_document", { checkoutId: alpha, path: guide });

  await user.click(screen.getByRole("button", { name: "Graph" }));
  await screen.findByRole("list", { name: "Graph notes" });
  await waitFor(() => expect(currentGraph().destroyed()).toBe(false));
  const alphaGraphAtSwitch = currentGraph();
  const oldWatch = watchers.get(alpha);
  if (!oldWatch) throw new Error("Missing watch channel for the first checkout");
  rerender(workspace(beta, client));
  await screen.findByRole("button", { name: "Close Start.md" });
  expect(screen.queryByRole("button", { name: "Close Guia.md" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Graph" }));
  await screen.findByRole("button", { name: `Beta start (${start})` });
  await waitFor(() => expect(edgePaths(currentGraph())).toEqual([[start, other]]));

  expect(paths()).toEqual([start, other]);
  expect(nodeId(currentGraph(), start)).not.toBe(alphaStart);
  expect(alphaGraphAtSwitch.destroyed()).toBe(true);
  expect(invoke).toHaveBeenCalledWith("unwatch_repository_note_index", { watchId: `index-watch-${alpha}` });
  expect(invoke).toHaveBeenCalledWith("rebuild_repository_note_index", { checkoutId: beta });
  expect(invoke).toHaveBeenCalledWith("repository_note_edges", { checkoutId: beta });
  await act(async () => { oldWatch({ checkoutId: alpha, kind: "paths", paths: [guide] }); });
  expect(invoke).not.toHaveBeenCalledWith("reconcile_repository_note_index", { checkoutId: alpha, paths: [guide] });
  expect(paths()).toEqual([start, other]);
});

it("preserves local depth and tag filters as watch events reconcile notes and edges", async () => {
  const { projections, watchers } = mockIndex();
  const client = queryClient();
  render(workspace(alpha, client));
  const user = userEvent.setup();
  await screen.findByRole("button", { name: "Close Start.md" });
  await user.click(screen.getByRole("button", { name: "Graph" }));
  await screen.findByRole("list", { name: "Graph notes" });
  await waitFor(() => expect(edgePaths(currentGraph())).toEqual([[start, guide], [guide, secondary]]));
  await user.click(screen.getByRole("button", { name: "Local" }));
  expect(paths()).toEqual([start, guide]);
  await user.selectOptions(screen.getByLabelText("Depth"), "2");
  expect(paths()).toEqual([start, guide, secondary]);
  const filter = screen.getByRole("searchbox", { name: "Filter graph" });
  await user.type(filter, "AÇÃO");
  expect(paths()).toEqual([start, secondary]);
  await user.clear(filter);
  await user.selectOptions(screen.getByLabelText("Tag"), "release");
  expect(paths()).toEqual([start, guide, secondary]);
  const previousId = nodeId(currentGraph(), secondary);

  const renamed = "planos/Próxima ação.md";
  const projection = firstProjection();
  projections.set(alpha, {
    notes: projection.notes.map((note) => note.path === secondary
      ? { ...note, path: renamed, title: "Próxima ação" } : note),
    edges: [edge(start, guide), edge(guide, renamed)],
  });
  const notice = { checkoutId: alpha, kind: "paths", paths: [secondary, renamed] } as const;
  const notify = watchers.get(alpha);
  if (!notify) throw new Error("Missing watch channel for the checkout");
  await act(async () => { notify({ ...notice, paths: [...notice.paths] }); });
  expect(invoke).toHaveBeenCalledWith("reconcile_repository_note_index", { checkoutId: alpha, paths: notice.paths });
  await screen.findByRole("button", { name: `Próxima ação (${renamed})` });
  await waitFor(() => expect(edgePaths(currentGraph())).toEqual([[start, guide], [guide, renamed]]));
  expect(paths()).toEqual([start, guide, renamed]);
  expect(nodeId(currentGraph(), renamed)).toBe(previousId);
  expect(screen.getByRole("button", { name: "Local" }).getAttribute("aria-pressed")).toBe("true");
  expect(screen.getByLabelText<HTMLSelectElement>("Depth").value).toBe("2");
  expect(screen.getByLabelText<HTMLSelectElement>("Tag").value).toBe("release");
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "repository_note_edges").length).toBeGreaterThanOrEqual(2);
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "save_repository_document")).toBe(false);
});
