import cytoscape from "cytoscape";
import { CircleDot, Focus, Network, ZoomIn, ZoomOut } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { ContextActions, type MenuGroups } from "@/components/ui/context-actions";
import { fileName } from "@/ide-knowledge-data";
import { isContextKey, useNoteContextMenu } from "@/knowledge-context";
import { indexNoteGraph, selectNoteGraph, type NoteGraphProjection } from "@/knowledge-graph-model";
import { ResizablePanel } from "@/panel-resize";

export type { NoteGraphProjection } from "@/knowledge-graph-model";

const elementId = (path: string, id?: number) => `note:${id ?? path}`;
export default function NoteGraph({ projection, selected, initialCenter, onOpen, onNodeActions }: {
  projection: NoteGraphProjection;
  selected?: string;
  initialCenter?: string;
  onOpen: (path: string) => void;
  onNodeActions?: (path: string) => MenuGroups;
}) {
  const container = useRef<HTMLDivElement>(null);
  const instance = useRef<cytoscape.Core>(undefined);
  const onOpenRef = useRef(onOpen);
  const onNodeActionsRef = useRef(onNodeActions);
  const [scope, setScope] = useState<"global" | "local">(initialCenter ? "local" : "global");
  const [center, setCenter] = useState(initialCenter);
  const [depth, setDepth] = useState(1);
  const [filter, setFilter] = useState("");
  const [tag, setTag] = useState("");
  const { open: openContextMenu, overlay } = useNoteContextMenu();
  const indexed = useMemo(() => indexNoteGraph(projection), [projection]);
  const hasActive = selected !== undefined && indexed.byPath.has(selected);
  const anchor = scope === "local" ? center ?? selected : undefined;
  const local = anchor && indexed.byPath.has(anchor) ? anchor : undefined;
  const unavailable = Boolean(anchor && !local);
  const highlightedPath = local ?? selected;
  const selectedTag = indexed.tags.includes(tag) ? tag : "";
  const { notes, edges, distance } = useMemo(() => unavailable ? { notes: [], edges: [], distance: new Map<string, number>() } : selectNoteGraph(indexed, {
    active: local, depth, search: filter, tag: selectedTag,
  }), [depth, filter, indexed, local, selectedTag, unavailable]);

  useEffect(() => { onOpenRef.current = onOpen; }, [onOpen]);
  useEffect(() => { onNodeActionsRef.current = onNodeActions; }, [onNodeActions]);
  function focusNote(path: string) {
    setFilter("");
    setTag("");
    setCenter(path);
    setScope("local");
  }
  function nodeGroups(path: string): MenuGroups {
    return [[{ label: "Open note", run: () => onOpenRef.current(path) },
      { label: "Center local graph here", run: () => focusNote(path) }], ...(onNodeActionsRef.current?.(path) ?? [])];
  }
  function backgroundGroups(): MenuGroups {
    return [[{ label: "Fit graph", run: () => instance.current?.fit(undefined, 52) },
      { label: "Show global graph", run: () => setScope("global") },
      { label: "Clear graph filters", run: () => { setFilter(""); setTag(""); } }]];
  }
  useEffect(() => {
    const mount = container.current;
    if (!mount || !notes.length) return;
    const style = getComputedStyle(document.documentElement);
    const foreground = style.getPropertyValue("--foreground").trim() || "#e7e7ec";
    const muted = style.getPropertyValue("--muted-foreground").trim() || "#a9a9b0";
    const accent = style.getPropertyValue("--primary").trim() || "#a7a3ff";
    const graph = cytoscape({
      container: mount,
      elements: [
        ...notes.map(({ id, path, title }) => ({ data: { id: elementId(path, id), name: title, path }, classes: path === highlightedPath ? "current" : "" })),
        ...edges.map(({ sourcePath, targetPath }) => ({ data: {
          id: `edge:${JSON.stringify([sourcePath, targetPath])}`,
          source: elementId(sourcePath, indexed.byPath.get(sourcePath)?.id),
          target: elementId(targetPath, indexed.byPath.get(targetPath)?.id),
        } })),
      ],
      style: [
        { selector: "node", style: { "background-color": muted, color: foreground, label: "data(name)", "font-family": "Geist, Segoe UI", "font-size": 13, width: 20, height: 20, "text-margin-y": 8, "text-valign": "bottom" } },
        { selector: ".current", style: { "background-color": accent, width: 26, height: 26 } },
        { selector: "edge", style: { width: 1.2, "line-color": muted, "target-arrow-shape": "triangle", "target-arrow-color": muted, "curve-style": "bezier", opacity: 0.65 } },
      ],
      layout: local ? {
        name: "concentric", padding: 52, levelWidth: () => 1,
        concentric: (node) => -(distance.get(node.data("path")) ?? depth),
      } : { name: "circle", padding: 52 },
      minZoom: 0.35,
      maxZoom: 2.5,
    });
    instance.current = graph;
    graph.on("tap", "node", (event) => onOpenRef.current(event.target.data("path")));
    graph.on("cxttap", (event) => {
      if (!onNodeActionsRef.current) return;
      const bounds = mount.getBoundingClientRect();
      const point = { x: bounds.left + (event.renderedPosition?.x ?? 0), y: bounds.top + (event.renderedPosition?.y ?? 0) };
      const target = event.target;
      const focus = () => { if (mount.isConnected) mount.focus(); };
      if (target === graph) openContextMenu("Note graph", backgroundGroups(), point, focus);
      else if (target.isNode()) {
        const path = String(target.data("path"));
        openContextMenu(path, nodeGroups(path), point, focus);
      } else if (target.isEdge()) {
        const from = String(target.source().data("path"));
        const to = String(target.target().data("path"));
        openContextMenu(`${from} → ${to}`, [[
          { label: "Open source note", run: () => onOpenRef.current(from) },
          { label: "Open destination note", run: () => onOpenRef.current(to) },
          { label: "Center graph on source note", run: () => focusNote(from) },
          { label: "Center graph on destination note", run: () => focusNote(to) },
        ]], point, focus);
      }
    });
    const resize = new ResizeObserver(() => { if (mount.clientWidth && mount.clientHeight) graph.resize(); });
    resize.observe(mount);
    return () => { resize.disconnect(); graph.destroy(); instance.current = undefined; };
  }, [depth, distance, edges, highlightedPath, indexed.byPath, local, notes, openContextMenu]);

  const zoomBy = (factor: number) => {
    const graph = instance.current;
    if (graph) graph.zoom({ level: graph.zoom() * factor, renderedPosition: { x: graph.width() / 2, y: graph.height() / 2 } });
  };

  return (
    <div className="note-graph">
      <div className="note-graph-tools">
        <Network size={16} aria-hidden="true" />
        <button type="button" className="fw-btn" aria-pressed={scope === "global"} onClick={() => setScope("global")}>Global</button>
        <button type="button" className="fw-btn" aria-pressed={scope === "local"} onClick={() => { if (hasActive) setCenter(undefined); setScope("local"); }}
          disabled={!hasActive && !center}>Local</button>
        {local && <label>Depth <select value={depth} onChange={(event) => setDepth(Number(event.target.value))}>
          <option value={1}>1</option><option value={2}>2</option><option value={3}>3</option>
        </select></label>}
        <label className="file-field"><input type="search" placeholder="Filter graph" aria-label="Filter graph" value={filter} onChange={(event) => setFilter(event.target.value)} /></label>
        {indexed.tags.length > 0 && <label>Tag <select value={selectedTag} onChange={(event) => setTag(event.target.value)}>
          <option value="">All tags</option>{indexed.tags.map((value) => <option key={value} value={value}>{value}</option>)}
        </select></label>}
        <div className="file-toolbar">
          <button type="button" className="fw-icon" aria-label="Zoom in" disabled={!notes.length} onClick={() => zoomBy(1.2)}><ZoomIn size={15} /></button>
          <button type="button" className="fw-icon" aria-label="Zoom out" disabled={!notes.length} onClick={() => zoomBy(0.83)}><ZoomOut size={15} /></button>
          <button type="button" className="fw-icon" aria-label="Fit graph" disabled={!notes.length} onClick={() => instance.current?.fit(undefined, 52)}><Focus size={15} /></button>
        </div>
      </div>
      {notes.length ? <div className="note-graph-canvas" ref={container} role="img" aria-label="Graph of repository notes"
        tabIndex={onNodeActions ? 0 : undefined} onContextMenu={onNodeActions ? (event) => event.preventDefault() : undefined}
        onKeyDown={onNodeActions ? (event) => {
          if (!isContextKey(event.nativeEvent)) return;
          const canvas = event.currentTarget;
          const { left, top } = canvas.getBoundingClientRect();
          event.preventDefault();
          event.stopPropagation();
          openContextMenu("Note graph", backgroundGroups(), { x: left, y: top }, () => { if (canvas.isConnected) canvas.focus(); });
        } : undefined} /> : <p role="status" className="file-sidebar-note">{unavailable ? `The note ${anchor} is unavailable in the graph.`
          : indexed.byPath.size ? "No notes match the current graph filter." : "No Markdown notes in this checkout."}</p>}
      <ResizablePanel className="note-graph-list-panel" edge="top" resizeKey="note-graph-list" resizeLabel="graph notes">
        <ul className="note-graph-node-list" aria-label="Graph notes">
          {notes.map(({ path, title }) => <li key={path}><ContextActions label={path} groups={onNodeActions ? nodeGroups(path) : []}>
            <button type="button" className="fw-btn" title={path}
            aria-label={projection ? `${title} (${path})` : undefined} aria-current={path === highlightedPath ? "true" : undefined}
            onClick={() => onOpen(path)}><CircleDot size={12} aria-hidden="true" />{projection ? title : fileName(path)}</button>
          </ContextActions></li>)}
        </ul>
      </ResizablePanel>
      {overlay}
    </div>
  );
}
