import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { Button } from "@/components/ui/button";
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
      {history.data?.versions.map((version) => <details key={version.revisionId} className="rounded border border-border p-2">
        <summary className="cursor-pointer">
          Proposal {version.eventSequence}{version.current ? " · Selected plan" : ""}
        </summary>
        <p className="break-all">Plan version <code>{version.revisionId}</code></p>
        {version.approvalArtifactId && <p className="break-all">Approval decision <code>{version.approvalArtifactId}</code></p>}
        {version.releaseId && <p className="break-all">
          {version.releaseRevoked ? "Revoked release" : "Approved release"} <code>{version.releaseId}</code>
        </p>}
        <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words">{version.text}</pre>
        {version.sourceRunId !== runId && <Button type="button" variant="outline"
          onClick={() => openActivity(version.sourceRunId)}>Open plan attempt</Button>}
      </details>)}
    </div>}
  </details>;
}
