import { useId, useMemo, useState, type ReactNode } from "react";
import { Background, Controls, ReactFlow, type Edge, type Node, type NodeChange, type ReactFlowProps } from "@xyflow/react";
import { CirclePlay, Layers3, PanelRight, Workflow } from "lucide-react";
import { Button } from "@/components/ui/button";
import "@xyflow/react/dist/style.css";
import "./pipelines-workspace.css";

type PipelineView<T extends string> = { id: T; label: string };

export function PipelineWorkspaceFrame({ actions, children }: { actions?: ReactNode; children: ReactNode }) {
  return <section className="pipelines-workspace" aria-label="Pipelines workspace">
    <header className="pipeline-workspace-heading">
      <div className="pipeline-title">
        <div className="pipeline-heading-icon"><Workflow size={18} aria-hidden="true" /></div>
        <h1>Pipelines</h1>
      </div>
      {actions && <div className="pipeline-actions">{actions}</div>}
    </header>
    {children}
  </section>;
}

export function PipelineTabs<T extends string>({ views, active, onSelect }: {
  views: readonly PipelineView<T>[]; active: T; onSelect: (id: T) => void;
}) {
  return <nav className="pipeline-tabs" aria-label="Pipelines views">
    {views.map(({ id, label }) => <button key={id} type="button"
      aria-current={active === id ? "page" : undefined} onClick={() => onSelect(id)}>{label}</button>)}
  </nav>;
}

export function PipelineSectionHeading({ title, description, icon }: { title: string; description?: string; icon: ReactNode }) {
  return <header className="pipeline-section-heading">
    <div><h2>{title}</h2>{description && <p>{description}</p>}</div>
    {icon}
  </header>;
}

export function PipelineRolesTemplatesHeading() {
  return <PipelineSectionHeading title="Roles & templates" description="Set reusable instructions, tools and review requirements."
    icon={<Layers3 size={20} aria-hidden />} />;
}

export function PipelineRunsEmpty({ onOpenBuilder }: { onOpenBuilder: () => void }) {
  return <div className="pipeline-grid flex-1">
    <div className="pipeline-empty">
      <div className="pipeline-empty-icon"><CirclePlay size={22} aria-hidden /></div>
      <h3>No runs yet</h3>
      <p>Launch a workflow from the builder to inspect its execution here.</p>
      <Button type="button" variant="outline" onClick={onOpenBuilder}>Open builder</Button>
    </div>
  </div>;
}

export function PipelineToolbar({ children }: { children: ReactNode }) {
  return <div className="pipeline-toolbar">{children}</div>;
}

export function PipelineGraphCanvas<N extends Node, E extends Edge>({ interactive = false, nodes, onNodesChange, ...props }:
  Omit<ReactFlowProps<N, E>, "children" | "nodes"> & { nodes: N[]; interactive?: boolean }) {
  // React Flow needs the dimensions back when a controlled node is rebuilt from workflow state.
  const [measurements, setMeasurements] = useState(() => new Map<string, NonNullable<Node["measured"]>>());
  const measuredNodes = useMemo(() => nodes.map((node) => {
    const measured = measurements.get(node.id);
    return measured ? { ...node, measured } : node;
  }), [nodes, measurements]);

  function changeNodes(changes: NodeChange<N>[]) {
    const dimensions = changes.filter((change) => change.type === "dimensions" || change.type === "remove");
    if (dimensions.length) setMeasurements((previous) => {
      const next = new Map(previous);
      for (const change of dimensions) {
        if (change.type === "remove") next.delete(change.id);
        else if (change.dimensions) next.set(change.id, change.dimensions);
      }
      return next;
    });
    onNodesChange?.(changes);
  }

  return <ReactFlow<N, E> {...props} nodes={measuredNodes} onNodesChange={changeNodes}>
    <Background gap={18} size={1} />
    <Controls position="bottom-left" showInteractive={interactive} />
  </ReactFlow>;
}

export function PipelineInspectorHeader({ detail, title, children }: { detail: string; title: string; children?: ReactNode }) {
  return <header>
    <small>{detail}</small>
    <h2>{title}</h2>
    {children}
  </header>;
}

export function PipelineCanvasFrame({ canvas, canvasLabel, inspector, inspectorLabel, inspectorClassName, palette, paletteLabel, className }: {
  canvas: ReactNode; canvasLabel: string; inspector: ReactNode; inspectorLabel: string;
  inspectorClassName?: string; palette?: ReactNode; paletteLabel?: string; className?: string;
}) {
  const inspectorId = useId();
  const [inspectorOpen, setInspectorOpen] = useState(false);
  return <div className={className ? `pipeline-editor ${className}` : "pipeline-editor"} data-palette={palette != null || undefined}>
    {palette != null && <aside className="pipeline-palette" aria-label={paletteLabel}>{palette}</aside>}
    <div className="pipeline-canvas" role="region" aria-label={canvasLabel}>{canvas}</div>
    <Button type="button" variant="ghost" className="pipeline-compact-inspector"
      aria-expanded={inspectorOpen} aria-controls={inspectorId} onClick={() => setInspectorOpen(!inspectorOpen)}>
      <PanelRight size={16} /> {inspectorOpen ? "Hide inspector" : "Show inspector"}
    </Button>
    <aside id={inspectorId} data-compact-open={inspectorOpen || undefined} className={inspectorClassName ? `pipeline-inspector ${inspectorClassName}` : "pipeline-inspector"} aria-label={inspectorLabel}>
      {inspector}
    </aside>
  </div>;
}
