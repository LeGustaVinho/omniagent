import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import { Background, Controls, ReactFlow, type Edge, type Node, type NodeChange, type ReactFlowProps } from "@xyflow/react";
import { ArrowRight, CirclePlay, FileCode2, FileDiff, Layers3, PackageCheck, PanelRight, Puzzle, Workflow, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ControlTooltip } from "@/components/ui/control-tooltip";
import { ContextActions, type MenuGroups } from "@/components/ui/context-actions";
import { ResizablePanel } from "@/panel-resize";
import { useWorkspaceSize, WorkbenchPanel } from "@/workbench-panel";
import "@xyflow/react/dist/style.css";
import "./pipelines-workspace.css";
import "./pipeline-maintenance.css";

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

export function PipelineTemplateList({ entries, onOpen, kind = "template" }: {
  entries: { id: string; name: string; detail: string; menu?: MenuGroups }[];
  onOpen: (id: string) => void;
  kind?: "template" | "run";
}) {
  const Icon = kind === "run" ? CirclePlay : FileCode2;
  return entries.length ? <ul className="pipeline-select-list">
    {entries.map(({ id, name, detail, menu }) => <li key={id}>
      <ContextActions label={`${name} ${detail}`} groups={menu ?? []}>
        <button className="pipeline-select-item" type="button" onClick={() => onOpen(id)}>
          <Icon size={15} aria-hidden />
          <span>{name}<small>{detail}</small></span>
          <ArrowRight size={14} className="pipeline-end" aria-hidden />
        </button>
      </ContextActions>
    </li>)}
  </ul> : <p className="text-muted-foreground">{kind === "run" ? "No saved runs." : "No saved templates."}</p>;
}

export function PipelineMaintenanceLayout({ proposals, releases, inventory }: {
  proposals: ReactNode; releases: ReactNode; inventory: ReactNode;
}) {
  const sections = [
    { id: "proposals", name: "Proposed changes", icon: FileDiff,
      detail: "Review workflow requests to change project settings or template labels.", body: proposals },
    { id: "package", name: "Candidate package", icon: PackageCheck,
      detail: "Compare the running build with packages accepted by this project.", body: releases },
    { id: "plugins", name: "CLI plugins", icon: Puzzle,
      detail: "Check or manage plugins in the selected CLI’s user profile.", body: inventory },
  ];
  const sectionId = useId();
  return <section className="pipeline-view-content pipeline-maintenance" aria-label="Workflow maintenance">
    <header className="pipeline-maintenance-heading"><h2>Maintenance</h2></header>
    <div className="pipeline-maintenance-grid">
      {sections.map(({ id, name, icon: Icon, detail, body }) => <section key={id} className="pipeline-maintenance-card"
        data-area={id} aria-labelledby={`${sectionId}-${id}`}>
        <header className="pipeline-maintenance-card-heading">
          <span className="pipeline-maintenance-icon"><Icon size={17} aria-hidden /></span>
          <div><h3 id={`${sectionId}-${id}`}>{name}</h3><p>{detail}</p></div>
        </header>
        <div className="pipeline-maintenance-card-body">{body}</div>
      </section>)}
    </div>
  </section>;
}

export function PipelinePluginItem({ name, detail, action }: { name: string; detail: string; action?: ReactNode }) {
  return <div className="pipeline-plugin">
    <div><strong>{name}</strong><p className="pipeline-meta">{detail}</p></div>
    {action}
  </div>;
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

export function PipelineCanvasFrame({ canvas, canvasLabel, inspector, inspectorLabel, inspectorClassName, palette, paletteLabel, className, inspectorRequest, onClosePalette }: {
  canvas: ReactNode; canvasLabel: string; inspector: ReactNode; inspectorLabel: string;
  inspectorClassName?: string; palette?: ReactNode; paletteLabel?: string; className?: string;
  inspectorRequest?: number;
  onClosePalette?: () => void;
}) {
  const inspectorId = useId();
  const workspace = useRef<HTMLDivElement>(null);
  const toggle = useRef<HTMLButtonElement>(null);
  const inspectorOrigin = useRef<HTMLElement | null>(null);
  const { width } = useWorkspaceSize(workspace);
  const drawer = width < 792;
  const paletteDrawer = width < 1032;
  const [inspectorPreference, setInspectorPreference] = useState(false);
  const [dismissedRequest, setDismissedRequest] = useState(0);
  const inspectorOpen = inspectorPreference || Boolean(inspectorRequest && inspectorRequest !== dismissedRequest);
  const inspectorBody = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!inspectorRequest || inspectorRequest === dismissedRequest) return;
    const focused = document.activeElement;
    if (focused instanceof HTMLElement && !focused.closest(".pipeline-inspector")) inspectorOrigin.current = focused;
    const frame = requestAnimationFrame(() => inspectorBody.current?.focus());
    return () => cancelAnimationFrame(frame);
  }, [dismissedRequest, inspectorRequest]);
  function closeInspector() {
    setDismissedRequest(inspectorRequest ?? 0);
    setInspectorPreference(false);
    requestAnimationFrame(() => (inspectorOrigin.current ?? toggle.current)?.focus({ preventScroll: true }));
  }
  return <div ref={workspace} className={className ? `pipeline-editor ${className}` : "pipeline-editor"} data-palette={Boolean(palette) && !paletteDrawer}>
    {palette != null && <WorkbenchPanel as="aside" open drawer={paletteDrawer} label={paletteLabel ?? "Node catalog"}
      drawerClass="pipeline-palette-drawer" className="pipeline-palette" aria-label={paletteLabel}
      onClose={() => onClosePalette?.()} edge={["right", "bottom"]} resizeKey="pipeline-palette" resizeLabel={paletteLabel ?? "node palette"}>
      {paletteDrawer && <div className="work-panel-header"><span>{paletteLabel ?? "Node catalog"}</span>
        <Button type="button" variant="ghost" className="ui-icon-button" aria-label="Close node catalog" onClick={onClosePalette}><X size={16} aria-hidden /></Button>
      </div>}
      <div className="pipeline-palette-scroll">{palette}</div>
    </WorkbenchPanel>}
    <ResizablePanel className="pipeline-canvas" role="region" aria-label={canvasLabel} edge="bottom"
      resizeKey={canvasLabel} resizeLabel={canvasLabel}>{canvas}</ResizablePanel>
    {drawer && <Button type="button" ref={toggle} variant="ghost" className="pipeline-compact-inspector"
      aria-expanded={inspectorOpen} aria-controls={inspectorId} onClick={() => {
        inspectorOrigin.current = toggle.current;
        if (inspectorOpen) closeInspector();
        else setInspectorPreference(true);
      }}>
      <PanelRight size={16} /> {inspectorOpen ? "Hide inspector" : "Show inspector"}
    </Button>}
    <WorkbenchPanel as="aside" id={inspectorId} drawer={drawer} open={!drawer || (inspectorOpen && !(paletteDrawer && palette))}
      label={inspectorLabel} drawerClass="pipeline-inspector-drawer" onClose={closeInspector}
      className={inspectorClassName ? `pipeline-inspector ${inspectorClassName}` : "pipeline-inspector"} aria-label={inspectorLabel}
      edge="left" resizeKey={inspectorLabel} resizeLabel={inspectorLabel}>
      {drawer && <div className="work-panel-header"><span>{inspectorLabel}</span>
        <ControlTooltip label="Close inspector"><Button type="button" variant="ghost" className="ui-icon-button" aria-label="Close inspector"
          onClick={closeInspector}><X size={16} aria-hidden /></Button></ControlTooltip>
      </div>}
      <div className="pipeline-inspector-scroll" tabIndex={-1} ref={inspectorBody}>{inspector}</div>
    </WorkbenchPanel>
  </div>;
}
