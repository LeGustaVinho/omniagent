import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import type { ComparisonSelection } from "@/workflow-comparison-selection";
import { controlForNode, wiredExecutionInputs, type WorkflowExecutionSnapshot, type WorkflowRun } from "@/workflow-execution-model";

export type ComparisonHandoff = {
  runId: string;
  nodeId: string;
  planRevision: string;
  sourceCheckoutId: string;
  selection: ComparisonSelection;
};

export function comparisonReviewStages(snapshot: WorkflowExecutionSnapshot, sourceCheckoutId: string) {
  const { planRevision } = snapshot.run;
  if (!planRevision) return [];
  const plan = snapshot.outputs.find(({ artifact, current }) =>
    current && artifact.id === planRevision && artifact.value.kind === "plan")?.artifact.value.evidence;
  const origin = plan && typeof plan === "object" && "run_id" in plan ? plan.run_id : undefined;
  if (typeof origin !== "string" || !snapshot.jobs.some(({ attempts, job }) =>
    job.checkoutId === sourceCheckoutId && attempts.some(({ runId }) => runId === origin))) return [];

  return snapshot.definition.nodes.filter((node) => {
    const { id, kind } = node.instance;
    if (kind !== "review_plan" || snapshot.unitNodeIds.includes(id) ||
      controlForNode(snapshot, id)?.state === "skipped" || wiredExecutionInputs(snapshot, node)?.plan !== planRevision) return false;
    if (snapshot.outputs.some(({ artifact, current }) => current && artifact.nodeInstanceId === id)) return false;
    const attempt = snapshot.jobs.findLast((entry) => entry.nodeId === id && entry.unitId === null &&
      entry.inputs?.plan === planRevision && entry.job.origin.type !== "agent")?.job;
    return !attempt || (!attempt.latestAttempt && attempt.queueState !== "queued") ||
      ["failed", "cancelled", "interrupted"].includes(attempt.latestAttempt?.status ?? "");
  });
}

export function WorkflowComparisonDestination({ projectId, sourceCheckoutId, selection, onUse }: {
  projectId: string;
  sourceCheckoutId: string;
  selection: ComparisonSelection;
  onUse: (handoff: ComparisonHandoff) => void;
}) {
  const [runId, setRunId] = useState("");
  const [nodeId, setNodeId] = useState("");
  const runs = useQuery({
    queryKey: ["repository-delivery", projectId, "list"],
    queryFn: () => invoke<WorkflowRun[]>("list_repository_deliveries", { projectId }),
    refetchOnMount: "always",
  });
  const execution = useQuery({
    queryKey: ["repository-delivery", projectId, "execution", runId],
    queryFn: () => invoke<WorkflowExecutionSnapshot | null>("inspect_workflow_execution", { projectId, runId }),
    enabled: Boolean(runId),
    refetchOnMount: "always",
  });
  const eligible = execution.data ? comparisonReviewStages(execution.data, sourceCheckoutId) : [];
  const revision = execution.data?.run.planRevision;
  const canUse = Boolean(revision && eligible.some(({ instance }) => instance.id === nodeId)) &&
    !execution.isFetching && !execution.error && selection.runIds.length > 0;
  const planned = runs.data?.filter(({ planRevision }) => planRevision !== null);
  const error = runs.error ?? execution.error;
  return <section className="pipeline-box pipeline-form pipeline-stack" aria-label="Comparison destination">
    <h3>Use responses in a plan review</h3>
    <p className="pipeline-meta">
      Choose a saved execution with a plan in the comparison's checkout. Review the selected responses before queuing.
    </p>
    {runs.isPending && <p role="status">Loading workflow runs…</p>}
    {error && <p role="alert">{String(error)}</p>}
    {planned?.length === 0 && <p>No workflow runs have a saved plan yet.</p>}
    {!!planned?.length && <label>Workflow run
      <select value={runId} onChange={(event) => { setRunId(event.currentTarget.value); setNodeId(""); }}>
        <option value="">Choose a run</option>
        {planned.map(({ id, version }) => <option key={id} value={id}>{id} · {version.revision.slice(0, 12)}</option>)}
      </select>
    </label>}
    {runId && execution.isFetching && <p role="status">Checking review stages…</p>}
    {runId && execution.isSuccess && !execution.isFetching && !eligible.length &&
      <p className="pipeline-note">This run has no eligible plan-review stage in the comparison's checkout.</p>}
    {!!eligible.length && <label>Plan-review stage
      <select value={nodeId} onChange={(event) => setNodeId(event.currentTarget.value)}>
        <option value="">Choose a stage</option>
        {eligible.map(({ instance, definition }) => <option key={instance.id} value={instance.id}>{definition.title} · {instance.id}</option>)}
      </select>
    </label>}
    <Button type="button" variant="outline" disabled={!canUse}
      onClick={() => { if (canUse && revision) onUse({ runId, nodeId, planRevision: revision, sourceCheckoutId, selection }); }}>
      Open plan review
    </Button>
  </section>;
}
