// @vitest-environment jsdom
import type cytoscape from "cytoscape";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import NoteGraph, { type NoteGraphProjection } from "@/knowledge-graph";

const instances = vi.hoisted(() => [] as cytoscape.Core[]);
vi.mock("cytoscape", async (importOriginal) => {
  const module = await importOriginal<{ default: (options: cytoscape.CytoscapeOptions) => cytoscape.Core }>();
  return {
    default: (options: cytoscape.CytoscapeOptions) => {
      const layout = Object.assign({}, options.layout ?? { name: "circle" as const }, {
        boundingBox: { x1: 0, y1: 0, w: 400, h: 400 },
      });
      const core = module.default({
        ...options,
        headless: true,
        container: undefined,
        layout,
      });
      instances.push(core);
      return core;
    },
  };
});

const active = "notes/Café.md";
const activeElement = "note:41";
const incoming = "planning/Guide.md";
const outgoing = "docs/Guide.md";
const secondHop = "docs/ação.md";
const unrelated = "archive/Unused.md";
const notes: NoteGraphProjection["notes"] = [
  { path: outgoing, title: "Guide", tags: ["Windows"] },
  { id: 41, path: active, title: "Café", tags: ["review"] },
  { path: incoming, title: "Guide", tags: ["planning"] },
  { path: secondHop, title: "Ação", tags: ["windows", "release"] },
  { path: unrelated, title: "Retired", tags: [] },
];
const projection: NoteGraphProjection = {
  notes,
  edges: [
    { sourcePath: incoming, targetPath: active },
    { sourcePath: active, targetPath: outgoing },
    { sourcePath: outgoing, targetPath: secondHop },
    { sourcePath: active, targetPath: outgoing },
    { sourcePath: active, targetPath: "outside/Missing.md" },
  ],
};

const observe = vi.fn();
const disconnect = vi.fn();

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class {
    observe = observe;
    disconnect = disconnect;
  });
});

afterEach(() => {
  cleanup();
  instances.length = 0;
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

function graph() {
  const core = instances.at(-1);
  if (!core) throw new Error("Cytoscape did not start");
  return core;
}

function graphPaths() {
  return within(screen.getByRole("list", { name: "Graph notes" }))
    .queryAllByRole("button").map((button) => button.getAttribute("title"));
}

it("uses indexed paths, titles, tags, and projected edges in the global graph", () => {
  const open = vi.fn();
  render(<NoteGraph projection={projection} selected={active} onOpen={open} />);
  expect(graphPaths()).toEqual(notes.map(({ path }) => path));
  expect(graph().edges().length).toBe(3);
  expect(graph().getElementById(activeElement).hasClass("current")).toBe(true);
  expect(graph().getElementById(`note:${active}`).empty()).toBe(true);
  expect(graph().getElementById(activeElement).connectedEdges().length).toBe(2);

  const text = screen.getByRole("searchbox", { name: "Filter graph" });
  fireEvent.change(text, { target: { value: "GUIDE" } });
  expect(graphPaths()).toEqual([outgoing, incoming]);
  fireEvent.click(screen.getByRole("button", { name: `Guide (${incoming})` }));
  expect(open).toHaveBeenLastCalledWith(incoming);

  fireEvent.change(text, { target: { value: "" } });
  fireEvent.change(screen.getByLabelText("Tag"), { target: { value: "Windows" } });
  expect(graphPaths()).toEqual([outgoing, secondHop]);
  expect(graph().edges().length).toBe(1);
  graph().getElementById(`note:${secondHop}`).emit("tap");
  expect(open).toHaveBeenLastCalledWith(secondHop);
  fireEvent.change(text, { target: { value: "AÇÃ" } });
  expect(graphPaths()).toEqual([secondHop]);
  expect(graph().edges().length).toBe(0);
});

it("centers Local on the active note, follows incoming and outgoing links, and applies the selected depth", () => {
  render(<NoteGraph projection={projection} selected={active} onOpen={vi.fn()} />);
  const global = graph();
  fireEvent.click(screen.getByRole("button", { name: "Local" }));
  const local = graph();
  expect(global.destroyed()).toBe(true);
  expect(graphPaths()).toEqual([outgoing, active, incoming]);
  const center = local.getElementById(activeElement).position();
  expect(center).toEqual({ x: 200, y: 200 });
  expect(local.getElementById(`note:${incoming}`).position()).not.toEqual(center);
  expect(local.getElementById(`note:${outgoing}`).position()).not.toEqual(center);
  expect(screen.getByRole("button", { name: `Café (${active})` }).getAttribute("aria-current")).toBe("true");

  fireEvent.change(screen.getByLabelText("Depth"), { target: { value: "2" } });
  expect(graphPaths()).toEqual([outgoing, active, incoming, secondHop]);
  fireEvent.change(screen.getByRole("searchbox"), { target: { value: "ação" } });
  expect(graphPaths()).toEqual([active, secondHop]);
  expect(graph().getElementById(activeElement).position()).toEqual(center);
});

it("keeps indexed file IDs distinct across checkout projections even when their paths match", () => {
  const open = vi.fn();
  const { rerender, unmount } = render(<NoteGraph projection={projection} selected={active} onOpen={open} />);
  const canvas = screen.getByRole("img", { name: "Graph of repository notes" });
  const first = graph();
  rerender(<NoteGraph selected={active} onOpen={open} projection={{
    notes: notes.map((note) => note.path === active ? { ...note, id: 85, title: "Café atualizado" } : note),
    edges: projection.edges,
  }} />);
  const second = graph();
  expect(first.destroyed()).toBe(true);
  expect(screen.getByRole("button", { name: `Café atualizado (${active})` })).toBeTruthy();
  expect(second.getElementById(activeElement).empty()).toBe(true);
  const current = second.getElementById("note:85");
  expect(current.connectedEdges().length).toBe(2);
  current.emit("tap");
  expect(open).toHaveBeenCalledExactlyOnceWith(active);
  unmount();
  expect(second.destroyed()).toBe(true);
  const canvasObservers = observe.mock.contexts.filter((_, index) => observe.mock.calls[index]?.[0] === canvas);
  expect(canvasObservers).toHaveLength(2);
  for (const observer of canvasObservers) expect(disconnect.mock.contexts).toContain(observer);
});

it("keeps the sample resolver working and represents an explicitly empty checkout", () => {
  const files = [
    { path: "demo/Start.md", content: "# Start\n\n[[./Next]]" },
    { path: "demo/Next.md", content: "# Next" },
  ];
  const open = vi.fn();
  const { rerender } = render(<NoteGraph files={files} selected={files[0]?.path} onOpen={open} />);
  expect(graphPaths()).toEqual(["demo/Start.md", "demo/Next.md"]);
  expect(graph().edges().length).toBe(1);
  fireEvent.click(screen.getByRole("button", { name: "Next.md" }));
  expect(open).toHaveBeenCalledExactlyOnceWith("demo/Next.md");
  const sample = graph();

  rerender(<NoteGraph files={files} projection={{ notes: [], edges: [] }} selected={files[0]?.path} onOpen={open} />);
  expect(sample.destroyed()).toBe(true);
  expect(graphPaths()).toEqual([]);
  expect(screen.getByRole("status").textContent).toBe("No Markdown notes in this checkout.");
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Local" }).disabled).toBe(true);
});
