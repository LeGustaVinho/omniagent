import { createContext, useContext } from "react";
import { Orientation, type SerializedDockview } from "dockview-react";

export const editorPopoutPath = "/editor-popout.html";
export const FileEditorDocument = createContext<Document | undefined>(undefined);
export const useFileEditorDocument = () => useContext(FileEditorDocument);
const noteWindowPrefix = "omni-note-window:";

export function noteWindowTarget(path: string, request: number) {
  return noteWindowPrefix + encodeURIComponent(JSON.stringify([path, request]));
}

export function readNoteWindowTarget(target: string) {
  if (!target.startsWith(noteWindowPrefix)) return;
  try {
    const parts: unknown = JSON.parse(decodeURIComponent(target.slice(noteWindowPrefix.length)));
    if (Array.isArray(parts) && parts.length === 2 && typeof parts[0] === "string" && typeof parts[1] === "number") return parts[0];
  } catch {
    return undefined;
  }
}

type GridNode = SerializedDockview["grid"]["root"];

function leaves(node: GridNode): GridNode[] {
  return Array.isArray(node.data) ? node.data.flatMap(leaves) : [node];
}

export function dockviewLayoutInMain(layout: SerializedDockview): SerializedDockview {
  if (!layout.popoutGroups?.length) return layout;
  const { popoutGroups, ...main } = layout;
  const grid = structuredClone(main.grid);
  const mainGroups = new Map(leaves(grid.root).flatMap((node) =>
    Array.isArray(node.data) ? [] : [[node.data.id, node]] as const));
  const registered = new Set([...mainGroups.values()].flatMap(({ data }) => Array.isArray(data) ? [] : data.views));
  let activeGroup = main.activeGroup;
  for (const popup of popoutGroups) {
    const reference = popup.gridReferenceGroup ? mainGroups.get(popup.gridReferenceGroup) : undefined;
    const groups = popup.grid ? leaves(popup.grid.root).flatMap((node) => Array.isArray(node.data) ? [] : [node.data]) :
      popup.data ? [popup.data] : [];
    for (const [index, group] of groups.entries()) {
      const views = group.views.filter((id) => !registered.has(id));
      for (const id of views) registered.add(id);
      if (index === 0 && reference && !Array.isArray(reference.data)) {
        reference.visible = true;
        reference.data.views.push(...views);
        if (activeGroup === group.id) {
          activeGroup = reference.data.id;
          reference.data.activeView = group.activeView;
        }
        reference.data.activeView ??= views[0];
      } else if (views.length) {
        const size = (grid.orientation === Orientation.HORIZONTAL ? popup.position?.width : popup.position?.height) || 1;
        const node: GridNode = { type: "leaf", size, data: { ...group, views } };
        if (Array.isArray(grid.root.data)) grid.root.data.push(node);
        else grid.root = { type: "branch", data: [grid.root, node] };
      }
    }
  }
  return { ...main, grid, activeGroup };
}

export function mirrorEditorAppearance(popup: Window) {
  const source = document.documentElement;
  const destination = popup.document.documentElement;
  const names = new Set<string>();
  popup.document.body.classList.add("file-workspace", "file-editor-popup");
  const sync = () => {
    for (const name of names) {
      if (!source.hasAttribute(name)) destination.removeAttribute(name);
    }
    for (const { name, value } of source.attributes) {
      if (name === "class" || name === "style" || name.startsWith("data-")) {
        destination.setAttribute(name, value);
        names.add(name);
      }
    }
  };
  sync();
  const observer = new MutationObserver(sync);
  observer.observe(source, { attributes: true });
  const stopMonacoTheme = mirrorMonacoTheme(popup.document);
  return () => {
    observer.disconnect();
    stopMonacoTheme();
  };
}

function mirrorMonacoTheme(popup: Document) {
  const copy = popup.createElement("style");
  const update = () => {
    const source = document.head.querySelector<HTMLStyleElement>("style.monaco-colors");
    if (!source) return;

    if (!copy.isConnected) {
      // Dockview snapshots styles one rule at a time when it opens the window.
      const snapshot = new Set(Array.from(source.sheet?.cssRules ?? [], (rule) => rule.cssText));
      for (const style of popup.head.querySelectorAll("style")) {
        if (snapshot.has(style.textContent ?? "")) style.remove();
      }
      copy.media = source.media;
      popup.head.append(copy);
      observer.disconnect();
      observer.observe(source, { childList: true });
    }
    copy.textContent = source.textContent;
  };
  const observer = new MutationObserver(update);
  observer.observe(document.head, { childList: true });
  update();
  return () => {
    observer.disconnect();
    copy.remove();
  };
}
