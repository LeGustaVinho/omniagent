import cytoscape from "cytoscape";
import { CircleDot, Focus, Network, ZoomIn, ZoomOut } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { fileName, type ExampleFile } from "@/ide-knowledge-data";
import { resolveWiki, wikiLinks } from "@/knowledge-document";

function graphEdges(notes: readonly ExampleFile[]) {
  const seen = new Set<string>();
  return notes.flatMap(({ path, content }) => wikiLinks(content).flatMap((link) => {
    const resolved = resolveWiki(link, path, notes);
    const target = resolved.length === 1 ? resolved[0]?.path : undefined;
    if (!target || target === path) return [];
    const id = `${path}→${target}`;
    if (seen.has(id)) return [];
    seen.add(id);
    return [{ id, source: path, target }];
  }));
}

export default function NoteGraph({ files, selected, onOpen }: {
  files: readonly ExampleFile[];
  selected?: string;
  onOpen: (path: string) => void;
}) {
  const container = useRef<HTMLDivElement>(null);
  const instance = useRef<cytoscape.Core>(undefined);
  const onOpenRef = useRef(onOpen);
  const [scope, setScope] = useState<"global" | "local">("global");
  const [depth, setDepth] = useState(1);
  const [filter, setFilter] = useState("");
  const edges = useMemo(() => graphEdges(files), [files]);
  const eligible = useMemo(() => {
    if (scope === "global" || !selected) return new Set(files.map(({ path }) => path));
    const reachable = new Set([selected]);
    for (let level = 0; level < depth; level++) {
      const touched = edges.filter(({ source, target }) => reachable.has(source) || reachable.has(target));
      for (const { source, target } of touched) {
        reachable.add(source);
        reachable.add(target);
      }
    }
    return reachable;
  }, [depth, edges, files, scope, selected]);
  const nodes = useMemo(() => [...eligible].filter((path) => path.toLowerCase().includes(filter.trim().toLowerCase())), [eligible, filter]);
  const nodeSet = useMemo(() => new Set(nodes), [nodes]);
  const visibleEdges = useMemo(() => edges.filter(({ source, target }) => nodeSet.has(source) && nodeSet.has(target)), [edges, nodeSet]);

  useEffect(() => { onOpenRef.current = onOpen; }, [onOpen]);
  useEffect(() => {
    const mount = container.current;
    if (!mount || !nodes.length) return;
    const style = getComputedStyle(document.documentElement);
    const foreground = style.getPropertyValue("--foreground").trim() || "#e7e7ec";
    const muted = style.getPropertyValue("--muted-foreground").trim() || "#a9a9b0";
    const accent = style.getPropertyValue("--primary").trim() || "#a7a3ff";
    const graph = cytoscape({
      container: mount,
      elements: [
        ...nodes.map((path) => ({ data: { id: path, name: fileName(path).replace(/\.md$/i, "") }, classes: path === selected ? "current" : "" })),
        ...visibleEdges.map((edge) => ({ data: edge })),
      ],
      style: [
        { selector: "node", style: { "background-color": muted, color: foreground, label: "data(name)", "font-family": "Geist, Segoe UI", "font-size": 13, width: 20, height: 20, "text-margin-y": 8, "text-valign": "bottom" } },
        { selector: ".current", style: { "background-color": accent, width: 26, height: 26 } },
        { selector: "edge", style: { width: 1.2, "line-color": muted, "target-arrow-shape": "triangle", "target-arrow-color": muted, "curve-style": "bezier", opacity: 0.65 } },
      ],
      layout: { name: "circle", padding: 52 },
      minZoom: 0.35,
      maxZoom: 2.5,
      wheelSensitivity: 1,
    });
    instance.current = graph;
    graph.on("tap", "node", (event) => onOpenRef.current(event.target.id()));
    const resize = new ResizeObserver(() => { if (mount.clientWidth && mount.clientHeight) graph.resize(); });
    resize.observe(mount);
    return () => { resize.disconnect(); graph.destroy(); instance.current = undefined; };
  }, [nodes, selected, visibleEdges]);

  const zoomBy = (factor: number) => {
    const graph = instance.current;
    if (graph) graph.zoom({ level: graph.zoom() * factor, renderedPosition: { x: graph.width() / 2, y: graph.height() / 2 } });
  };

  return (
    <div className="note-graph">
      <div className="note-graph-tools">
        <Network size={16} />
        <button type="button" className="fw-btn" aria-pressed={scope === "global"} onClick={() => setScope("global")}>Global</button>
        <button type="button" className="fw-btn" aria-pressed={scope === "local"} onClick={() => setScope("local")} disabled={!selected}>Local</button>
        {scope === "local" && <label>Depth <select value={depth} onChange={(event) => setDepth(Number(event.target.value))}>
          <option value={1}>1</option><option value={2}>2</option><option value={3}>3</option>
        </select></label>}
        <label className="file-field"><input type="search" placeholder="Filter graph" aria-label="Filter graph" value={filter} onChange={(event) => setFilter(event.target.value)} /></label>
        <div className="file-toolbar">
          <button type="button" className="fw-icon" aria-label="Zoom in" onClick={() => zoomBy(1.2)}><ZoomIn size={15} /></button>
          <button type="button" className="fw-icon" aria-label="Zoom out" onClick={() => zoomBy(0.83)}><ZoomOut size={15} /></button>
          <button type="button" className="fw-icon" aria-label="Fit graph" onClick={() => instance.current?.fit(undefined, 52)}><Focus size={15} /></button>
        </div>
      </div>
      {nodes.length ? <div className="note-graph-canvas" ref={container} aria-label="Graph of repository notes" /> : <p className="file-sidebar-note">No notes match the current graph filter.</p>}
      <ul className="note-graph-node-list" aria-label="Graph notes">
        {nodes.map((path) => <li key={path}><button type="button" className="fw-btn" title={path} onClick={() => onOpen(path)}><CircleDot size={12} />{fileName(path)}</button></li>)}
      </ul>
    </div>
  );
}
