import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { JobAttempt } from "@/bindings/JobAttempt";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { Button } from "@/components/ui/button";
import { GoalVerificationPanel } from "@/goal-verification-panel";
import { DeliveryAgent } from "@/repository-delivery-agent";
import { WorkflowExecutionCandidate } from "@/workflow-execution-candidate";
import type { DeliveryInputs, DeliveryNode } from "@/repository-delivery-model";
import type { WorkspaceProps } from "@/workbench-contract";
import { readStageGoal, type StageGoalConfiguration } from "@/workflow-stage-goal";
import type { StagePlanConfiguration } from "@/workflow-stage-plan";
import { controlForNode, wiredExecutionInputs, type ExecutionPinnedNode, type ExecutionUnit, type WorkflowExecutionSnapshot }
  from "@/workflow-execution-model";

const agentStages = new Map<string, DeliveryNode>([
  ["plan", "plan"], ["review_plan", "review-plan"], ["implement", "implement"], ["review_candidate", "review-candidate"],
  ["review_diff_candidate", "review-diff"], ["agent", "agent"],
]);

export function WorkflowExecutionAgent({ snapshot, node, unit, openActivity }: {
  snapshot: WorkflowExecutionSnapshot; node: ExecutionPinnedNode; unit?: ExecutionUnit;
  openActivity: WorkspaceProps["openActivity"];
}) {
  const stage = agentStages.get(node.instance.kind);
  const unitScoped = snapshot.unitNodeIds.includes(node.instance.id);
  const scopedUnit = unitScoped ? unit : undefined;
  const inputs = stage && (!unitScoped || unit?.planRevision === snapshot.run.planRevision)
    ? wiredExecutionInputs(snapshot, node, scopedUnit) : null;
  if (!stage || !inputs || controlForNode(snapshot, node.instance.id, scopedUnit)?.state === "skipped") return null;
  return <AgentActions key={`${node.instance.id}:${scopedUnit?.unit.id ?? ""}`}
    snapshot={snapshot} node={node} unit={scopedUnit} stage={stage} inputs={inputs} openActivity={openActivity} />;
}

function AgentActions({ snapshot, node, unit, stage, inputs, openActivity }: {
  snapshot: WorkflowExecutionSnapshot; node: ExecutionPinnedNode; unit?: ExecutionUnit;
  stage: DeliveryNode; inputs: DeliveryInputs; openActivity: WorkspaceProps["openActivity"];
}) {
  const [sequence, setSequence] = useState("");
  const client = useQueryClient();
  const { projectId, id: runId, planRevision } = snapshot.run;
  const scope = { projectId, runId };
  const nodeId = node.instance.id;
  const unitId = unit?.unit.id ?? null;
  const wired = Object.entries(inputs);
  const matchesInputs = (recorded: DeliveryInputs | null) =>
    (stage !== "agent" || Object.keys(recorded ?? {}).length === wired.length) &&
    wired.every(([port, id]) => recorded?.[port] === id);
  const job = snapshot.jobs.findLast((entry) => entry.nodeId === nodeId && entry.unitId === unitId && entry.job.origin.type !== "agent" &&
    matchesInputs(entry.inputs))?.job;
  const terminal = job?.latestAttempt?.status === "succeeded" ? job.latestAttempt : null;
  const published = snapshot.outputs.some(({ artifact, current, inputReceipts }) => artifact.nodeInstanceId === nodeId &&
    (unitId ? artifact.unitId === unitId : current) &&
    (stage !== "agent" || inputReceipts.some((receipt) => matchesInputs(receipt.inputs))));
  const role = snapshot.rolePins?.find((pin) => pin.nodeId === nodeId && pin.nodeRevision === node.revision && !pin.requirementsOnly)?.role;
  const modelOverride = node.instance.configuration.model as ModelSelection | null | undefined;
  const requirements = node.instance.configuration.requirements as { goal?: unknown; plan?: StagePlanConfiguration } | undefined;
  const goal = readStageGoal(requirements?.goal);
  const genericPlan = stage === "agent" ? requirements?.plan : undefined;
  const proposals = useQuery({
    queryKey: ["workflow-plan-proposals", projectId, runId, terminal?.runId],
    queryFn: () => invoke<{ sequence: string; text: string }[]>("repository_delivery_plan_proposals", {
      ...scope, attemptId: terminal?.runId,
    }),
    enabled: stage === "plan" && terminal !== null && !published,
  });
  const mutate = useMutation({
    mutationFn: (operation: () => Promise<unknown>) => operation(),
    onSuccess: () => client.invalidateQueries({ queryKey: ["repository-delivery", projectId] }),
  });
  const publish = terminal && stage !== "implement" && stage !== "agent" && !published;
  return <>
    {genericPlan && <p>Plan Mode · {genericPlan.questionRoute === "native" ? "Verified CLI questions" : "App-managed questions"}</p>}
    {!published && <DeliveryAgent scope={scope} nodeId={nodeId} stage={stage} inputs={inputs} unitId={unitId} taskId={unit?.taskId}
      job={job} label={node.definition.title} actions={{ execute: (operation) => mutate.mutate(operation), busy: mutate.isPending }}
      openActivity={openActivity} pinnedModel={modelOverride ?? role?.model} pinnedEffort={role?.effort} goalEnabled={Boolean(goal)} />}
    {stage === "agent" && goal && terminal && <AgentGoalVerification attempt={terminal} projectId={projectId} goal={goal} />}
    {stage === "implement" && unit && terminal && !published && <WorkflowExecutionCandidate
      key={terminal.runId} snapshot={snapshot} node={node} unit={unit} attemptId={terminal.runId} />}
    {publish && stage === "plan" && <label className="workflow-execution-field">Recorded plan proposal
      <select value={sequence} onChange={(event) => setSequence(event.currentTarget.value)}>
        <option value="">Select a proposal</option>
        {proposals.data?.map((proposal) => <option key={proposal.sequence} value={proposal.sequence}>{proposal.text}</option>)}
      </select>
    </label>}
    {publish && <Button type="button" variant="outline" disabled={mutate.isPending || stage === "plan" && !sequence}
      onClick={() => mutate.mutate(() => invoke("publish_repository_delivery_evidence", {
        ...scope, nodeId, attemptId: terminal.runId, planSequence: stage === "plan" ? sequence : null, expectedPlanRevision: planRevision,
      }))}>{stage === "plan" ? "Publish selected plan" : "Publish review evidence"}</Button>}
    {(mutate.error || proposals.error) && <p role="alert">{String(mutate.error ?? proposals.error)}</p>}
  </>;
}

function AgentGoalVerification({ attempt, projectId, goal }: {
  attempt: JobAttempt; projectId: string; goal: StageGoalConfiguration;
}) {
  const client = useQueryClient();
  const { runId, conversationId } = attempt;
  const checkpoint = useQuery({
    queryKey: ["run-orchestration", projectId, runId, conversationId ?? undefined],
    queryFn: () => invoke<{ goal: { controlState: string; checkpoint: { runId: string; nextAction: string } | null } | null } | null>(
      "run_orchestration", { projectId, runId, conversationId },
    ),
    refetchInterval: ({ state }) => {
      const current = state.data?.goal;
      const action = current?.checkpoint?.nextAction;
      return current && current.controlState !== "cancelled" && action !== "review" && action !== "limit_reached" ? 1200 : false;
    },
  });
  if (checkpoint.error) return <p role="alert">{String(checkpoint.error)}</p>;
  const result = checkpoint.data?.goal?.checkpoint;
  if (result?.nextAction !== "review") return null;
  return <GoalVerificationPanel key={result.runId} projectId={projectId} runId={result.runId}
    requiredEvidence={goal.criteria.map(({ evidence }) => evidence)}
    onVerificationSaved={() => void client.invalidateQueries({ queryKey: ["repository-delivery", projectId] })} />;
}
