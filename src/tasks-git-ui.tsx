import { AlertCircle, CircleOff, FlaskConical } from "lucide-react";
import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { ResizablePanel } from "@/panel-resize";
import type { WorkspaceProps } from "@/workbench-contract";
import "@/tasks-git.css";

const statusTone: Record<string, string> = {
  "In Progress": "progress", "In Review": "progress", Running: "progress", Ready: "progress",
  Blocked: "warm", "Waiting for input": "warm", Interrupted: "warm", Conflicted: "warm", Paused: "warm",
  Completed: "success", Verified: "success", Accepted: "success", Merged: "success", Succeeded: "success",
  Failed: "danger",
};

export function DeliveryStatus({ children }: { children: string }) {
  return <span className="delivery-status" data-tone={statusTone[children]}>{children}</span>;
}

export function DeliveryDetailPanel({ label, resizeKey, className, children }: {
  label: string; resizeKey: string; className?: string; children: ReactNode;
}) {
  return <ResizablePanel as="aside" className={className ? `delivery-detail ${className}` : "delivery-detail"}
    aria-label={label} edge={["left", "top"]} resizeKey={resizeKey} resizeLabel={label}>{children}</ResizablePanel>;
}

export function IntegrationNotice({ title, children, onSimulate }: {
  title: string;
  children: ReactNode;
  onSimulate: WorkspaceProps["startSimulation"];
}) {
  return (
    <div className="delivery-empty">
      <span className="work-empty-icon"><CircleOff size={20} aria-hidden="true" /></span>
      <h3 className="font-semibold text-foreground">{title}</h3>
      <p className="text-sm leading-6">{children}</p>
      <Button className="mt-1 px-2.5 py-1.5 text-xs" variant="outline" type="button" onClick={() => onSimulate()}>
        <FlaskConical size={15} aria-hidden="true" /> Simulate
      </Button>
    </div>
  );
}

export function InlineIssue({ children }: { children: ReactNode }) {
  return (
    <p role="alert" className="flex min-w-0 items-start gap-2 rounded-md border border-border bg-card p-2.5 text-sm leading-5 wrap-anywhere">
      <AlertCircle size={16} className="mt-0.5 shrink-0 text-[var(--warning,#ebbd70)]" aria-hidden="true" />
      <span>{children}</span>
    </p>
  );
}
