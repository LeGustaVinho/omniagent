import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { FailureCause } from "@/bindings/FailureCause";
import type { FailurePhase } from "@/bindings/FailurePhase";
import type { JobFailure } from "@/bindings/JobFailure";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
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
  const { copy, feedback } = useContextActionFeedback();
  const query = useQuery({
    queryKey: ["job-failure", projectId, runId],
    queryFn: () => invoke<JobFailure | null>("job_failure", { projectId, runId }),
    staleTime: Infinity,
  });
  if (query.isError) return <p role="alert" className="text-sm">{String(query.error)}</p>;
  const failure = query.data;
  if (!failure) return null;
  const { phase, cause, process } = failure.observation;
  const processLabel = process && `${process.cancelled ? "Cancelled" : "Exited"} (code ${process.code})`;
  const dispatch = !openLogs && failure.dispatchRejection && queueBlockerReason({ type: "dispatch_rejected", rejection: failure.dispatchRejection });
  const facts = [
    { label: "Phase", value: phases[phase] },
    { label: "Worktree", value: failure.checkoutRoot, className: "break-all font-mono text-xs" },
    ...(cause ? [{ label: "Cause", value: causes[cause] }] : []),
    ...(dispatch ? [{ label: "Dispatch", value: dispatch, className: "break-words" }] : []),
    ...(processLabel ? [{ label: "Process", value: processLabel }] : []),
    ...(failure.providerOutcome ? [{ label: "Provider outcome", value: failure.providerOutcome }] : []),
  ];
  return (
    <section aria-label="Failure record" className="grid gap-2 rounded border border-border p-3 text-sm">
      <ContextActions label="Failure record" groups={[[
        ...(openLogs && failure.logsAvailable ? [{ label: "Open recorded activity", run: openLogs }] : []),
        { label: "Copy failure summary", run: () => copy([`Run ${runId}`, ...facts.map(({ label, value }) => `${label}\t${value}`)].join("\n")) },
        { label: "Copy worktree path", run: () => copy(failure.checkoutRoot) },
        ...(process ? [{ label: "Copy exit code", run: () => copy(String(process.code)) }] : []),
      ]]}><h3 className="font-semibold">Failure record</h3></ContextActions>
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1">
        {facts.map(({ label, value, className }) => <div key={label} className="contents">
          <dt>{label}</dt><dd className={className}>{value}</dd>
        </div>)}
      </dl>
      {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
      {openLogs && failure.logsAvailable && <Button type="button" variant="outline" className="justify-self-start" onClick={openLogs}>Open recorded activity</Button>}
      {!failure.logsAvailable && <p className="text-muted-foreground">Recorded activity is unavailable.</p>}
      {failure.savedDiffAvailable ? <WorktreeEvidence projectId={projectId} runId={runId} ownerTaskId={null} /> :
        <p className="text-muted-foreground">Saved diff is unavailable.</p>}
    </section>
  );
}
