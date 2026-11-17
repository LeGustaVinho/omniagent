import cytoscape from "cytoscape";
import { CircleDot, Focus, Network, ZoomIn, ZoomOut } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { fileName, type ExampleFile } from "@/ide-knowledge-data";
import { exampleNoteGraph, indexNoteGraph, selectNoteGraph, type NoteGraphProjection } from "@/knowledge-graph-model";
import { ResizablePanel } from "@/panel-resize";

export type { NoteGraphProjection } from "@/knowledge-graph-model";

const elementId = (path: string, id?: number) => `note:${id ?? path}`;
const emptyExamples: readonly ExampleFile[] = [];

export default function NoteGraph({ files = emptyExamples, projection, selected, onOpen }: {
  files?: readonly ExampleFile[];
  projection?: NoteGraphProjection;
  selected?: string;
  onOpen: (path: string) => void;
}) {
  const container = useRef<HTMLDivElement>(null);
  const instance = useRef<cytoscape.Core>(undefined);
  const onOpenRef = useRef(onOpen);
  const [scope, setScope] = useState<"global" | "local">("global");
  const [depth, setDepth] = useState(1);
  const [filter, setFilter] = useState("");
  const [tag, setTag] = useState("");
  const indexed = useMemo(() => indexNoteGraph(projection ?? exampleNoteGraph(files)), [files, projection]);
  const hasActive = selected !== undefined && indexed.byPath.has(selected);
  const local = scope === "local" && hasActive ? selected : undefined;
  const selectedTag = indexed.tags.includes(tag) ? tag : "";
  const { notes, edges, distance } = useMemo(() => selectNoteGraph(indexed, {
    active: local, depth, search: filter, tag: selectedTag,
  }), [depth, filter, indexed, local, selectedTag]);

  useEffect(() => { onOpenRef.current = onOpen; }, [onOpen]);
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
        ...notes.map(({ id, path, title }) => ({ data: { id: elementId(path, id), name: title, path }, classes: path === selected ? "current" : "" })),
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
    const resize = new ResizeObserver(() => { if (mount.clientWidth && mount.clientHeight) graph.resize(); });
    resize.observe(mount);
    return () => { resize.disconnect(); graph.destroy(); instance.current = undefined; };
  }, [depth, distance, edges, indexed.byPath, local, notes, selected]);

  const zoomBy = (factor: number) => {
    const graph = instance.current;
    if (graph) graph.zoom({ level: graph.zoom() * factor, renderedPosition: { x: graph.width() / 2, y: graph.height() / 2 } });
  };

  return (
    <div className="note-graph">
      <div className="note-graph-tools">
        <Network size={16} aria-hidden="true" />
        <button type="button" className="fw-btn" aria-pressed={!local} onClick={() => setScope("global")}>Global</button>
        <button type="button" className="fw-btn" aria-pressed={Boolean(local)} onClick={() => setScope("local")} disabled={!hasActive}>Local</button>
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
      {notes.length ? <div className="note-graph-canvas" ref={container} role="img" aria-label="Graph of repository notes" /> : <p role="status" className="file-sidebar-note">{indexed.byPath.size ? "No notes match the current graph filter." : "No Markdown notes in this checkout."}</p>}
      <ResizablePanel className="note-graph-list-panel" edge="top" resizeKey="note-graph-list" resizeLabel="graph notes">
        <ul className="note-graph-node-list" aria-label="Graph notes">
          {notes.map(({ path, title }) => <li key={path}><button type="button" className="fw-btn" title={path}
            aria-label={projection ? `${title} (${path})` : undefined} aria-current={path === selected ? "true" : undefined}
            onClick={() => onOpen(path)}><CircleDot size={12} aria-hidden="true" />{projection ? title : fileName(path)}</button></li>)}
        </ul>
      </ResizablePanel>
    </div>
  );
}
