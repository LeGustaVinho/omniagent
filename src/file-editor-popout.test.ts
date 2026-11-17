import { Orientation, type SerializedDockview } from "dockview-react";
import { expect, test } from "vitest";
import { dockviewLayoutInMain, noteWindowTarget, readNoteWindowTarget } from "@/file-editor-popout";

type GridNode = SerializedDockview["grid"]["root"];
const group = (id: string, views: string[], activeView?: string): GridNode => ({
  type: "leaf", size: 380, data: { id, views, activeView },
});
const branch = (...data: GridNode[]): GridNode => ({ type: "branch", size: 760, data });
const grid = (root: GridNode): SerializedDockview["grid"] => ({
  root, width: 760, height: 580, orientation: Orientation.HORIZONTAL,
});
const pinned = { id: "note", contentComponent: "file", title: "Meeting.md", params: { path: "notes/Meeting.md", view: "editor", pinned: true } };

test("saves detached tabs once in their reference group and preserves the pinned file and active tab", () => {
  const home = { ...group("main", ["other"], "other"), visible: false };
  const layout: SerializedDockview = {
    grid: grid(branch(home)),
    activeGroup: "detached",
    panels: { note: pinned },
    popoutGroups: [{ gridReferenceGroup: "main", position: null, data: { id: "detached", views: ["other", "note"], activeView: "note" } }],
  };
  const restored = dockviewLayoutInMain(layout);
  expect(restored).toEqual({
    grid: grid(branch({ ...group("main", ["other", "note"], "note"), visible: true })),
    activeGroup: "main",
    panels: { note: pinned },
  });
  expect(layout.grid.root.data).toEqual([home]);
  expect(dockviewLayoutInMain(restored)).toBe(restored);
});

test("keeps every detached group when its reference group was removed", () => {
  const layout: SerializedDockview = {
    grid: grid(branch(group("main", ["other"], "other"))),
    activeGroup: "note",
    panels: { note: pinned },
    popoutGroups: [{ gridReferenceGroup: "removed", position: null,
      grid: grid(branch(group("note", ["note"], "note"), branch(group("source", ["source"], "source")))) }],
  };
  const restored = dockviewLayoutInMain(layout);
  expect(restored.popoutGroups).toBeUndefined();
  expect(restored.activeGroup).toBe("note");
  expect(restored.grid.root.data).toEqual([
    group("main", ["other"], "other"),
    { ...group("note", ["note"], "note"), size: 1 },
    { ...group("source", ["source"], "source"), size: 1 },
  ]);
});

test("passes an exact note path with characters used by anchors and URLs", () => {
  const path = "notes/Release #2 (draft).md";
  expect(readNoteWindowTarget(noteWindowTarget(path, 1))).toBe(path);
  expect(noteWindowTarget(path, 1)).not.toBe(noteWindowTarget(path, 2));
  expect(readNoteWindowTarget(path)).toBeUndefined();
});
