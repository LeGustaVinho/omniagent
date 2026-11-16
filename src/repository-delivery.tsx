import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { DeliveryAgent } from "@/repository-delivery-agent";
import {
  matchingJob, matchingReview, type DeliveryActions, type DeliveryInputs, type DeliveryRun,
  type DeliverySnapshot, type DeliveryTarget, type DeliveryUnit,
} from "@/repository-delivery-model";
import { RepositoryDeliveryUnit } from "@/repository-delivery-unit";
import { RepositoryMarkdownPicker } from "@/repository-markdown-picker";
import type { WorkspaceProps } from "@/workbench-contract";

type Proposal = { sequence: string; text: string };
type TaskOption = { id: string; title: string };
type TaskDeliveryStatus = {
  state: string;
  waitingForCommit: boolean;
  checklistResolved: boolean;
  pendingRepositoryChanges: boolean;
  activeWorkflowRunId: string | null;
  completion: { evidence: { kind: string; completionSha?: string } } | null;
};

function DeliveryRunView({ delivery, projectId, actions, openActivity }: {
  delivery: DeliverySnapshot;
  projectId: string;
  actions: DeliveryActions;
  openActivity: WorkspaceProps["openActivity"];
}) {
  const [chosenTask, setChosenTask] = useState("");
  const [title, setTitle] = useState("");
  const [proposal, setProposal] = useState("");
  const scope: DeliveryTarget = { projectId, runId: delivery.run.id };
  const context = delivery.artifacts.find(({ nodeInstanceId, port }) => nodeInstanceId === "context" && port === "context");
  const plan = delivery.artifacts.find(({ id }) => id === delivery.run.planRevision);
  const planner = context ? matchingJob(delivery, "plan", { context: context.id }) : undefined;
  const planAttempt = planner?.latestAttempt;
  const proposals = useQuery({
    queryKey: ["repository-delivery", projectId, scope.runId, "proposals", planAttempt?.runId],
    queryFn: () => invoke<Proposal[]>("repository_delivery_plan_proposals", { ...scope, attemptId: planAttempt?.runId }),
    enabled: planAttempt?.status === "succeeded",
  });
  const currentProposal = proposals.data?.find(({ sequence }) => sequence === proposal) ?? proposals.data?.at(-1);
  const reviewing = plan ? matchingJob(delivery, "review-plan", { plan: plan.id }) : undefined;
  const review = matchingReview(delivery, reviewing);
  const linked = delivery.artifacts.find(({ id }) => id === delivery.hostOutputs?.work);
  const approval = delivery.artifacts.find(({ value }) => value.kind === "approval" && value.evidence.planRevision === plan?.id);
  const release = delivery.artifacts.find(({ value }) => value.kind === "release" && value.evidence.decisionArtifactId === approval?.id);
  const worktree = delivery.artifacts.find(({ id }) => id === delivery.hostOutputs?.prepare);
  const taskId = linked?.value.kind === "task" ? linked.value.evidence.taskId : undefined;
  const tasks = useQuery({
    queryKey: ["repository-delivery", projectId, "tasks"],
    queryFn: () => invoke<TaskOption[]>("repository_delivery_tasks", { projectId }),
    enabled: Boolean(review && !linked),
  });
  const units = useQuery({
    queryKey: ["repository-delivery", projectId, scope.runId, "units", taskId],
    queryFn: () => invoke<DeliveryUnit[]>("repository_delivery_units", { ...scope, taskId }),
    enabled: Boolean(release && taskId),
  });
  const state = useQuery({
    queryKey: ["repository-delivery", projectId, scope.runId, "task", taskId],
    queryFn: () => invoke<TaskDeliveryStatus | null>("project_task_delivery_state", { projectId, taskId }),
    enabled: Boolean(taskId),
  });
  const host = (inputs: DeliveryInputs, action: object) => invoke("execute_repository_delivery_host", { ...scope, inputs, action });
  const awaitingUnit = units.data?.find(({ commitEvidenceId }) => !commitEvidenceId)?.unit.id;
  const last = units.data?.at(-1);
  const activeDelivery = state.data?.activeWorkflowRunId === scope.runId;
  const finalGit = delivery.artifacts.find(({ unitId, nodeInstanceId, port }) =>
    unitId === last?.unit.id && nodeInstanceId === "commit" && port === "git",
  );
  const result = delivery.artifacts.some(({ nodeInstanceId }) => nodeInstanceId === "result");
  const error = proposals.error ?? tasks.error ?? units.error ?? state.error;

  return <div className="pipeline-stack">
    {context?.value.kind === "markdown" && <p>
      <span className="font-medium">{context.value.evidence.relativePath}</span> · <code>{context.value.evidence.sha256.slice(0, 12)}</code>
    </p>}
    {context && <DeliveryAgent scope={scope} nodeId="plan" inputs={{ context: context.id }} job={planner}
      label="Planning" actions={actions} openActivity={openActivity} />}
    {currentProposal && <>
      {proposals.data && proposals.data.length > 1 && <label className="pipeline-stack">Planning proposal
        <select value={currentProposal.sequence} onChange={(event) => setProposal(event.currentTarget.value)}>
          {proposals.data.map(({ sequence }) => <option key={sequence} value={sequence}>Proposal {sequence}</option>)}
        </select>
      </label>}
      <details className="pipeline-evidence"><summary>Read proposed plan</summary>
        <pre className="max-h-80 overflow-auto whitespace-pre-wrap">{currentProposal.text}</pre>
      </details>
      {!(plan?.value.kind === "plan" && plan.value.evidence.run_id === planAttempt?.runId &&
        String(plan.value.evidence.event_sequence) === currentProposal.sequence) &&
        <Button variant="outline" disabled={actions.busy}
          onClick={() => actions.execute(() => invoke("publish_repository_delivery_evidence", {
            ...scope, nodeId: "plan", attemptId: planAttempt?.runId,
            planSequence: currentProposal.sequence, expectedPlanRevision: delivery.run.planRevision,
          }))}>Record plan revision</Button>}
    </>}
    {plan && <DeliveryAgent key={plan.id} scope={scope} nodeId="review-plan" inputs={{ plan: plan.id }} job={reviewing}
      label="Plan review" actions={actions} openActivity={openActivity} />}
    {reviewing?.latestAttempt?.status === "succeeded" && !review &&
      <Button variant="outline" disabled={actions.busy} onClick={() => actions.execute(() => invoke("publish_repository_delivery_evidence", {
        ...scope, nodeId: "review-plan", attemptId: reviewing.latestAttempt?.runId, planSequence: null, expectedPlanRevision: null,
      }))}>Record plan review</Button>}
    {plan && review && !linked && <div className="pipeline-form pipeline-stack">
      <h3 className="font-medium">Link work</h3>
      <label className="pipeline-stack">Existing Task or Sub-Task
        <select value={chosenTask} onChange={(event) => setChosenTask(event.currentTarget.value)}>
          <option value="">Select work</option>
          {tasks.data?.map((task) => <option key={task.id} value={task.id}>{task.title}</option>)}
        </select>
      </label>
      <Button variant="outline" disabled={!chosenTask || actions.busy}
        onClick={() => actions.execute(() => host({ plan: plan.id, review: review.id }, { action: "link_existing_task", taskId: chosenTask }))}>
        Link work
      </Button>
      <label className="pipeline-stack">New Task title <input value={title} onChange={(event) => setTitle(event.currentTarget.value)} /></label>
      <Button variant="outline" disabled={!title.trim() || actions.busy}
        onClick={() => actions.execute(() => host({ plan: plan.id, review: review.id }, { action: "create_task", title: title.trim() }))}>
        Create and link Task
      </Button>
    </div>}
    {plan && review && linked && !approval && <Button disabled={actions.busy}
      onClick={() => actions.execute(() => host({ plan: plan.id, task: linked.id }, { action: "approve_implementation" }))}>
      Approve implementation
    </Button>}
    {approval && linked && !release && <Button disabled={actions.busy}
      onClick={() => actions.execute(() => host({ decision: approval.id, task: linked.id }, { action: "prepare_worktree" }))}>
      Prepare managed worktree
    </Button>}
    {state.data && <div className="pipeline-stack">
      <p role="status">Task · {state.data.waitingForCommit ? "In review · Waiting for commit" : state.data.state.replaceAll("_", " ")}</p>
      {state.data.completion?.evidence.completionSha && <p>Completion SHA <code className="break-all">{state.data.completion.evidence.completionSha}</code></p>}
      {!state.data.checklistResolved && <p>Resolve the task checklist before completing delivery.</p>}
      {state.data.pendingRepositoryChanges && <p>Additional repository changes are assigned to this Task or Sub-Task.</p>}
      {state.data.pendingRepositoryChanges && last && taskId && <Button variant="outline" disabled={actions.busy}
        onClick={() => actions.execute(() => invoke("discard_pending_worktree_changes", { projectId, checkoutId: last.checkoutId, scopeTaskId: taskId }))}>
        Confirm restored worktree
      </Button>}
      {state.data.state === "completed" && taskId && <Button variant="outline" disabled={actions.busy}
        onClick={() => actions.execute(() => invoke("reopen_task_delivery", { projectId, taskId }))}>
        Reopen work
      </Button>}
    </div>}
    {plan && release && worktree && units.data?.map((unit) => <RepositoryDeliveryUnit
      key={unit.unit.id} scope={scope} delivery={delivery} unit={unit}
      implementationInputs={{ plan: plan.id, worktree: worktree.id, release: release.id }}
      actions={actions} openActivity={openActivity} active={activeDelivery && unit.unit.id === awaitingUnit} />)}
    {activeDelivery && linked && last && finalGit && !awaitingUnit && !result && <Button variant="outline" disabled={actions.busy}
      onClick={() => actions.execute(() => invoke("execute_repository_delivery_unit", {
        input: { ...scope, taskId, unitId: last.unit.id, nodeId: "result",
          inputs: { git: finalGit.id, task: linked.id }, request: { action: "result" } },
      }))}>Record delivery result</Button>}
    {error && <p role="alert">{String(error)}</p>}
  </div>;
}

export function RepositoryDeliveryWorkspace({ checkout, target, startSimulation, openActivity }: WorkspaceProps) {
  const client = useQueryClient();
  const { projectId, checkoutId } = checkout;
  const [selected, setSelected] = useState(target ?? "");
  useEffect(() => setSelected(target ?? ""), [target]);
  const runs = useQuery({
    queryKey: ["repository-delivery", projectId, "list"],
    queryFn: () => invoke<DeliveryRun[]>("list_repository_deliveries", { projectId }),
  });
  const runId = runs.data?.some(({ id }) => id === selected) ? selected : runs.data?.[0]?.id;
  const delivery = useQuery({
    queryKey: ["repository-delivery", projectId, runId],
    queryFn: () => invoke<DeliverySnapshot | null>("inspect_repository_delivery", { projectId, runId }),
    enabled: Boolean(runId),
    refetchInterval: ({ state }) => state.data?.jobs.some(({ job }) => job.latestAttempt
      ? ["starting", "running", "waiting_for_input"].includes(job.latestAttempt.status)
      : job.queueState === "queued") ? 900 : false,
  });
  const refresh = () => Promise.all([
    client.invalidateQueries({ queryKey: ["repository-delivery", projectId] }),
    client.invalidateQueries({ queryKey: ["delivery-selection", projectId] }),
    client.invalidateQueries({ queryKey: ["project-job-queue"] }),
  ]);
  const start = useMutation({
    mutationFn: (revisionId: string) => invoke<DeliveryRun>("start_repository_delivery", { projectId, checkoutId, revisionId }),
    onSuccess: (run) => { setSelected(run.id); return refresh(); },
  });
  const action = useMutation({
    mutationFn: (operation: () => Promise<unknown>) => operation(),
    onSettled: refresh,
  });
  const busy = start.isPending || action.isPending;
  const error = start.error ?? action.error ?? runs.error ?? delivery.error;
  return <div className="pipeline-view pipeline-view-content">
    <header className="pipeline-section-heading">
      <h2>Repository delivery</h2>
      <Button variant="outline" onClick={() => startSimulation("pipelines")}>Open pipeline preview</Button>
    </header>
    <RepositoryMarkdownPicker key={checkoutId} checkoutId={checkoutId} onDeliver={(id) => start.mutate(id)} delivering={busy} />
    {runs.data?.length ? <label className="pipeline-form pipeline-stack">Saved delivery
      <select value={runId} onChange={(event) => setSelected(event.currentTarget.value)}>
        {runs.data.map((run) => <option key={run.id} value={run.id}>{run.id}</option>)}
      </select>
    </label> : null}
    {delivery.data && <DeliveryRunView key={delivery.data.run.id} delivery={delivery.data} projectId={projectId}
      openActivity={openActivity} actions={{ busy, execute: (operation) => action.mutate(operation) }} />}
    {error && <p role="alert" className="pipeline-note">{String(error)}</p>}
  </div>;
}
