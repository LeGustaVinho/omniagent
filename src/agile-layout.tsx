import { LayoutGrid, List, Search, X } from "lucide-react";
import { Dialog } from "radix-ui";
import type { ComponentProps, CSSProperties, ReactNode, Ref } from "react";
import { DeliveryStatus } from "@/tasks-git-ui";
import "./agile-layout.css";

type BoardView = "kanban" | "table";
const workDetailViews = ["Overview", "Workflow", "Work history", "Context", "Changes"] as const;
export type WorkDetailView = (typeof workDetailViews)[number];

export function AgileWorkspaceFrame({ title, view, onViewChange, action, filters, children }: {
  title: string;
  view: BoardView;
  onViewChange: (view: BoardView) => void;
  action: ReactNode;
  filters?: ReactNode;
  children: ReactNode;
}) {
  return <section className="delivery-view agile-workspace" aria-label="Tasks workspace">
    <header className="delivery-toolbar agile-workspace-heading">
      <h2>{title}</h2>
      <nav className="agile-view-switch" aria-label="Agile view">
        <button type="button" className="delivery-tab" aria-pressed={view === "kanban"} onClick={() => onViewChange("kanban")}>
          <LayoutGrid size={14} aria-hidden="true" /> Kanban
        </button>
        <button type="button" className="delivery-tab" aria-pressed={view === "table"} onClick={() => onViewChange("table")}>
          <List size={14} aria-hidden="true" /> Table
        </button>
      </nav>
      <div className="agile-workspace-action">{action}</div>
    </header>
    {filters && <div className="delivery-toolbar delivery-filters agile-filters" role="group" aria-label="Work filters">{filters}</div>}
    {children}
  </section>;
}

export function AgileTableFrame({ columns, children }: { columns: readonly string[]; children: ReactNode }) {
  return <div className="overflow-auto"><table className="delivery-table agile-table" aria-label="Agile table">
    <thead><tr>{columns.map((column) => <th key={column} scope="col">{column}</th>)}</tr></thead>
    <tbody>{children}</tbody>
  </table></div>;
}

export function AgileSearch({ label, value, onChange }: { label: string; value: string; onChange: (value: string) => void }) {
  return <label className="agile-search">
    <Search size={15} aria-hidden="true" />
    <input className="delivery-filter delivery-search" placeholder={label} aria-label={label}
      value={value} onChange={(event) => onChange(event.currentTarget.value)} />
  </label>;
}

export function AgileKanbanColumn({ label, count, over, nodeRef, children }: {
  label: string; count: number; over: boolean; nodeRef: Ref<HTMLElement>; children: ReactNode;
}) {
  return <section ref={nodeRef} className="delivery-column" aria-label={label} data-over={over}>
    <header className="agile-column-heading">
      <DeliveryStatus>{label}</DeliveryStatus>
      <span className="agile-column-count">{count}</span>
    </header>
    {children}
  </section>;
}

export function AgileWorkCard({ title, identity, selected, dragging, nodeRef, style, onOpen, handle, children }: {
  title: string; identity: string; selected: boolean; dragging: boolean; nodeRef: Ref<HTMLElement>;
  style?: CSSProperties; onOpen: () => void; handle?: ReactNode; children: ReactNode;
}) {
  return <article ref={nodeRef} style={style} className="delivery-card agile-work-card" data-selected={selected} data-dragging={dragging}>
    <div className="agile-work-card-meta"><small>{identity}</small>{handle}</div>
    <button type="button" className="agile-work-card-title" aria-pressed={selected} onClick={onOpen}>{title}</button>
    <div className="agile-work-card-content">{children}</div>
  </article>;
}

export function AgileDetailFrame({ label, title, identity, state, note, closeLabel, close, navigation, children }: {
  label: string; title: string; identity: string; state?: string; note?: string;
  closeLabel: string; close: () => void; navigation?: ReactNode; children: ReactNode;
}) {
  return <aside className="delivery-detail" aria-label={label}>
    <section className="agile-detail-frame" aria-label={title}>
      <header className="agile-detail-heading">
        <div className="min-w-0">
          <p className="agile-detail-identity">{identity}</p>
          <h3>{title}</h3>
          {state && <p className="agile-detail-status" role="status">
            <DeliveryStatus>{state}</DeliveryStatus>{note && <span> · {note}</span>}
          </p>}
        </div>
        <button className="agile-icon-button" onClick={close} type="button" aria-label={closeLabel}>
          <X size={16} aria-hidden="true" />
        </button>
      </header>
      {navigation}
      <div className="delivery-detail-content agile-detail-content">{children}</div>
    </section>
  </aside>;
}

export function AgileDetailTabs({ view, onViewChange }: { view: WorkDetailView; onViewChange: (view: WorkDetailView) => void }) {
  return <nav className="agile-detail-tabs" aria-label="Task detail views">
    {workDetailViews.map((item) => <button type="button" key={item} className="delivery-tab"
      aria-pressed={view === item} onClick={() => onViewChange(item)}>{item}</button>)}
  </nav>;
}

export function AgileWorkDialog({ title, description, closeLabel, busy, onCloseAutoFocus, children }: {
  title: string; description: string; closeLabel: string; busy?: boolean;
  onCloseAutoFocus?: ComponentProps<typeof Dialog.Content>["onCloseAutoFocus"]; children: ReactNode;
}) {
  return <Dialog.Portal>
    <Dialog.Overlay className="agile-dialog-overlay" />
    <Dialog.Content className="agile-dialog" onCloseAutoFocus={onCloseAutoFocus}>
      <header className="agile-dialog-heading">
        <Dialog.Title>{title}</Dialog.Title>
        <Dialog.Close aria-label={closeLabel} className="agile-icon-button" disabled={busy}><X size={17} aria-hidden="true" /></Dialog.Close>
      </header>
      <Dialog.Description className="sr-only">{description}</Dialog.Description>
      {children}
    </Dialog.Content>
  </Dialog.Portal>;
}
