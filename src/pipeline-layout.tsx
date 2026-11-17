import { useId, useState, type ReactNode } from "react";
import { PanelRight, Workflow } from "lucide-react";
import { Button } from "@/components/ui/button";
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

export function PipelineToolbar({ children }: { children: ReactNode }) {
  return <div className="pipeline-toolbar">{children}</div>;
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
