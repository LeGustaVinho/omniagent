import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import type { WorkspaceProps } from "@/workbench-contract";
import { ComparisonCandidateOutcome, comparisonOutcome, type CandidateOutcome } from "@/workflow-comparison-outcomes";
import type { ComparisonCandidate, ComparisonGroup } from "@/workflow-comparison-selection";

export type ExecutionComparisonStage = {
  nodeId: string;
  nodeRevision: string;
  unitId: string | null;
  planRevision: string | null;
  inputs: Record<string, string>;
  groupId: string | null;
  state: "queued" | "running" | "settled" | "failed";
  error: string | null;
};

export type RecordedComparison = {
  groupId: string;
  commit: string;
  state: "succeeded" | "partial" | "failed";
  candidates: (Pick<ComparisonCandidate, "selection" | "jobId" | "runId" | "dispatchId" | "state"> & {
    responseRevisionId: string | null;
  })[];
};

const terminalCandidate = new Set(["succeeded", "failed", "cancelled", "interrupted"]);
const recordedOutcome = { succeeded: "Comparison complete", partial: "Partial result", failed: "Comparison failed" };

export function WorkflowExecutionComparison({ projectId, stage, recorded, openActivity, openQueueJob }: {
  projectId: string;
  stage: ExecutionComparisonStage;
  recorded?: RecordedComparison;
} & Pick<WorkspaceProps, "openActivity" | "openQueueJob">) {
  const { groupId, state, error } = stage;
  const { copy, feedback } = useContextActionFeedback();
  const active = state === "queued" || state === "running";
  const groups = useQuery({
    queryKey: ["model-comparisons", projectId],
    queryFn: () => invoke<ComparisonGroup[]>("model_comparisons", { projectId }),
    enabled: Boolean(groupId),
    refetchInterval: ({ state }) => active || state.data?.some(({ id, candidates }) => id === groupId &&
      candidates.some(({ state }) => !terminalCandidate.has(state))) ? 1200 : false,
  });
  const group = groups.data?.find(({ id }) => id === groupId);
  const candidates = recorded ? recorded.candidates.map((saved) => {
    const live = group?.candidates.find(({ jobId, runId, dispatchId }) => jobId === saved.jobId && runId === saved.runId && dispatchId === saved.dispatchId);
    return { ...saved, modelReference: live?.modelReference, conversationId: live?.conversationId, hasResponse: saved.responseRevisionId !== null };
  }) : group?.candidates.map((candidate) => ({ ...candidate, hasResponse: true }));
  const commit = recorded?.commit ?? group?.commit;
  const outcomes = useQuery({
    queryKey: ["conversation", "workflow-comparison-outcomes", projectId, groupId, candidates?.map(({ runId, state }) => [runId, state])],
    queryFn: () => invoke<CandidateOutcome[]>("model_comparison_outcomes", { projectId, groupId }),
    enabled: Boolean(groupId && candidates?.some(({ runId, state }) => runId && terminalCandidate.has(state))),
  });
  const loadError = groups.error ?? outcomes.error;
  return <section className="workflow-record-card" aria-label="Workflow model comparison">
    {error && <p role="alert">{error}</p>}
    {loadError && <p role="alert">{String(loadError)}</p>}
    {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
    {!candidates && active && <p role="status">{state === "queued" ? "Comparison queued" : "Comparison in progress"}</p>}
    {!candidates && groupId && !active && groups.isPending && <p role="status">Loading recorded comparison…</p>}
    {!candidates && groupId && !active && !groups.isPending && !loadError && <p>The recorded comparison is unavailable.</p>}
    {candidates && <>
      <p role="status">{recorded ? recordedOutcome[recorded.state] : comparisonOutcome(candidates.map(({ state }) => state))}</p>
      {commit && <p>Pinned Git commit <code>{commit}</code></p>}
      <ol className="pipeline-stack" aria-label="Comparison candidates">
        {candidates.map(({ selection, modelReference, jobId, runId, conversationId, dispatchId, state, hasResponse }) => {
          const outcome = !outcomes.error && !outcomes.isFetching ? outcomes.data?.find((entry) => entry.jobId === jobId && entry.runId === runId) : undefined;
          const name = modelReference?.name ?? selection.modelId;
          return <li key={jobId} className="workflow-record-card">
            <ContextActions label={name} groups={[[
              ...(hasResponse && typeof outcome?.response === "string" ? [{ label: "Copy original response", run: () => copy(outcome.response!) }] : []),
              ...(runId ? [{ label: "Open execution evidence", run: () => openActivity(runId, conversationId ?? undefined) }]
                : openQueueJob ? [{ label: "Open queued job", run: () => openQueueJob(jobId) }] : []),
            ], [
              { label: "Copy job ID", run: () => copy(jobId) },
              ...(runId ? [{ label: "Copy run ID", run: () => copy(runId) }] : []),
              ...(dispatchId ? [{ label: "Copy dispatch ID", run: () => copy(dispatchId) }] : []),
            ]]}><strong>{name}</strong></ContextActions>
            <p>{selection.provider} · {state === "succeeded" && !hasResponse ? "Response unavailable" : state.replaceAll("_", " ")}</p>
            <ComparisonCandidateOutcome outcome={outcome && (hasResponse ? outcome : { ...outcome, response: null, reasoning: [] })}
              succeeded={hasResponse && state === "succeeded"} />
            {runId && <Button type="button" variant="outline" onClick={() => openActivity(runId, conversationId ?? undefined)}>Open execution</Button>}
            {!runId && openQueueJob && <Button type="button" variant="outline" onClick={() => openQueueJob(jobId)}>Open queued job</Button>}
          </li>;
        })}
      </ol>
    </>}
  </section>;
}
