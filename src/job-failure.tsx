import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { FailureCause } from "@/bindings/FailureCause";
import type { FailurePhase } from "@/bindings/FailurePhase";
import type { JobFailure } from "@/bindings/JobFailure";
import { Button } from "@/components/ui/button";
import { queueBlockerReason } from "@/queue-model";
import { WorktreeEvidence } from "@/worktree-evidence";

const phases: Record<FailurePhase, string> = {
  unknown: "Unavailable",
  preparation: "Preparation",
  launch: "Process launch",
  startup: "Provider startup",
  dispatch: "Prompt dispatch",
  execution: "Execution",
  process_stop: "Process shutdown",
  evidence_capture: "Worktree evidence",
  result_validation: "Result validation",
  restart: "Recovered after restart",
};

const causes: Record<FailureCause, string> = {
  worktree_unavailable: "The worktree identity could not be verified.",
  launch_rejected: "The runner returned a launch error.",
  startup_timeout: "The provider did not become ready before the startup deadline.",
  goal_time_limit: "The goal reached its time limit.",
  goal_authority_revoked: "Authorization changed while the goal was running.",
  provider_error: "The provider reported an error.",
  transport_closed: "The provider transport closed before the attempt settled.",
  process_exit: "The provider exited without completing the attempt.",
  stop_unconfirmed: "The runtime could not confirm process shutdown.",
  evidence_unavailable: "The runtime could not capture worktree evidence.",
  invalid_result: "The structured result failed validation.",
  shutdown: "OmniAgent interrupted the attempt during exit.",
  runtime_restart: "OmniAgent restarted before the runtime had settled the attempt.",
};

export function JobFailurePanel({ projectId, runId, openLogs }: {
  projectId: string;
  runId: string;
  openLogs?: () => void;
}) {
  const query = useQuery({
    queryKey: ["job-failure", projectId, runId],
    queryFn: () => invoke<JobFailure | null>("job_failure", { projectId, runId }),
    staleTime: Infinity,
  });
  if (query.isError) return <p role="alert" className="text-sm">{String(query.error)}</p>;
  const failure = query.data;
  if (!failure) return null;
  const { phase, cause, process } = failure.observation;
  return (
    <section aria-label="Failure record" className="grid gap-2 rounded border border-border p-3 text-sm">
      <h3 className="font-semibold">Failure record</h3>
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1">
        <dt>Phase</dt><dd>{phases[phase]}</dd>
        <dt>Worktree</dt><dd className="break-all font-mono text-xs">{failure.checkoutRoot}</dd>
        {cause && <><dt>Cause</dt><dd>{causes[cause]}</dd></>}
        {!openLogs && failure.dispatchRejection && <><dt>Dispatch</dt>
          <dd className="break-words">{queueBlockerReason({ type: "dispatch_rejected", rejection: failure.dispatchRejection })}</dd>
        </>}
        {process && <><dt>Process</dt><dd>{process.cancelled ? "Cancelled" : "Exited"} (code {process.code})</dd></>}
        {failure.providerOutcome && <><dt>Provider outcome</dt><dd>{failure.providerOutcome}</dd></>}
      </dl>
      {openLogs && failure.logsAvailable && <Button type="button" variant="outline" className="justify-self-start" onClick={openLogs}>Open recorded activity</Button>}
      {!failure.logsAvailable && <p className="text-muted-foreground">Recorded activity is unavailable.</p>}
      {failure.savedDiffAvailable ? <WorktreeEvidence projectId={projectId} runId={runId} ownerTaskId={null} /> :
        <p className="text-muted-foreground">Saved diff is unavailable.</p>}
    </section>
  );
}
