import { AlertCircle, CheckCircle2, CircleDot, Clock3 } from "lucide-react";
import type { ReactNode } from "react";
import { ResizablePanel } from "@/panel-resize";
import "@/tasks-git.css";

const statusTone: Record<string, string> = {
  "In Progress": "progress", "In Review": "progress", Running: "progress", Starting: "progress", Ready: "progress",
  Blocked: "warm", "Waiting for input": "warm", "Waiting for approval": "warm", "Needs review": "warm",
  Interrupted: "warm", Conflicted: "warm", Paused: "warm",
  Completed: "success", Verified: "success", Accepted: "success", Merged: "success", Succeeded: "success",
  Failed: "danger",
};

export function DeliveryStatus({ children }: { children: string }) {
  const tone = statusTone[children];
  const Icon = tone === "success" ? CheckCircle2 : tone === "danger" ? AlertCircle :
    tone === "warm" || children === "Queued" || children === "Waiting for jobs" ? Clock3 : CircleDot;
  return <span className="delivery-status" data-tone={tone}><Icon size={13} aria-hidden="true" />{children}</span>;
}

export function DeliveryDetailPanel({ label, resizeKey, className, children }: {
  label: string; resizeKey: string; className?: string; children: ReactNode;
}) {
  return <ResizablePanel as="aside" className={className ? `delivery-detail ${className}` : "delivery-detail"}
    aria-label={label} edge={["left", "top"]} resizeKey={resizeKey} resizeLabel={label}>{children}</ResizablePanel>;
}

export function InlineIssue({ children }: { children: ReactNode }) {
  return (
    <p role="alert" className="flex min-w-0 items-start gap-2 rounded-md border border-border bg-card p-2.5 text-sm leading-5 wrap-anywhere">
      <AlertCircle size={16} className="mt-0.5 shrink-0 text-[var(--warning,#ebbd70)]" aria-hidden="true" />
      <span>{children}</span>
    </p>
  );
}
