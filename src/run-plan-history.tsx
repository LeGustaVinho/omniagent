import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import type { WorkspaceProps } from "@/workbench-contract";

type PlanVersion = {
  revisionId: string;
  sourceRunId: string;
  eventSequence: string;
  text: string;
  current: boolean;
  approvalArtifactId: string | null;
  releaseId: string | null;
  releaseRevoked: boolean | null;
};
type PlanHistory = {
  request: { baseRevisionId: string; feedback: string } | null;
  versions: PlanVersion[];
};

export function RunPlanHistory({ projectId, runId, openActivity }: {
  projectId: string;
  runId: string;
  openActivity: WorkspaceProps["openActivity"];
}) {
  const [open, setOpen] = useState(false);
  const { copy, feedback } = useContextActionFeedback();
  const history = useQuery({
    queryKey: ["repository-delivery", projectId, "plan-history", runId],
    enabled: open,
    queryFn: () => invoke<PlanHistory | null>("repository_plan_history", { projectId, runId }),
  });
  return <details onToggle={(event) => setOpen(event.currentTarget.open)} className="min-w-0">
    <summary className="cursor-pointer">Plan revision history</summary>
    {open && <div className="grid gap-2 pt-2 text-sm">
      {history.isPending && <p role="status">Loading plan history…</p>}
      {history.error && <p role="alert">{String(history.error)}</p>}
      {history.data === null && <p>No saved workflow plan revisions for this attempt.</p>}
      {history.data?.request && <section aria-label="Plan revision request" className="grid gap-1">
        <h3 className="font-medium">Revision request</h3>
        <p className="break-all">Base version <code>{history.data.request.baseRevisionId}</code></p>
        <p className="whitespace-pre-wrap break-words">{history.data.request.feedback}</p>
      </section>}
      {history.data?.versions.map(({ revisionId, sourceRunId, eventSequence, text, current, approvalArtifactId, releaseId, releaseRevoked }) => {
        const label = `Proposal ${eventSequence}${current ? " · Selected plan" : ""}`;
        const openAttempt = () => openActivity(sourceRunId);
        return <ContextActions key={revisionId} label={label} className="run-evidence-menu rounded border border-border p-2" groups={[[
          ...(sourceRunId !== runId ? [{ label: "Open plan attempt", run: openAttempt }] : []),
        ], [
          { label: "Copy plan text", run: () => copy(text) },
          { label: "Copy plan revision ID", run: () => copy(revisionId) },
          { label: "Copy source run ID", run: () => copy(sourceRunId) },
          ...(approvalArtifactId ? [{ label: "Copy approval decision ID", run: () => copy(approvalArtifactId) }] : []),
          ...(releaseId ? [{ label: "Copy release ID", run: () => copy(releaseId) }] : []),
        ]]}>
          <details className="min-w-0 flex-1">
            <summary className="cursor-pointer">{label}</summary>
            <p className="break-all">Plan version <code>{revisionId}</code></p>
            {approvalArtifactId && <p className="break-all">Approval decision <code>{approvalArtifactId}</code></p>}
            {releaseId && <p className="break-all">
              {releaseRevoked ? "Revoked release" : "Approved release"} <code>{releaseId}</code>
            </p>}
            <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words">{text}</pre>
            {sourceRunId !== runId && <Button type="button" variant="outline" onClick={openAttempt}>Open plan attempt</Button>}
          </details>
        </ContextActions>;
      })}
      {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
    </div>}
  </details>;
}
