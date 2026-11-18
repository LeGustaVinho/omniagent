import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { JobAttempt } from "@/bindings/JobAttempt";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { GoalVerificationPanel } from "@/goal-verification-panel";
import { DeliveryAgent } from "@/repository-delivery-agent";
import type { DeliveryInputs } from "@/repository-delivery-model";
import type { WorkspaceProps } from "@/workbench-contract";
import { readStageGoal, type StageGoalConfiguration } from "@/workflow-stage-goal";
import type { StagePlanConfiguration } from "@/workflow-stage-plan";
import { controlForNode, wiredExecutionInputs, type ExecutionPinnedNode, type ExecutionUnit, type WorkflowExecutionSnapshot }
  from "@/workflow-execution-model";

export function WorkflowExecutionAgent({ snapshot, node, unit, openActivity }: {
  snapshot: WorkflowExecutionSnapshot; node: ExecutionPinnedNode; unit?: ExecutionUnit;
  openActivity: WorkspaceProps["openActivity"];
}) {
  if (node.instance.kind !== "agent") return null;
  const unitScoped = snapshot.unitNodeIds.includes(node.instance.id);
  const scopedUnit = unitScoped ? unit : undefined;
  const inputs = (!unitScoped || unit?.planRevision === snapshot.run.planRevision)
    ? wiredExecutionInputs(snapshot, node, scopedUnit) : null;
  if (!inputs || controlForNode(snapshot, node.instance.id, scopedUnit)?.state === "skipped") return null;
  return <AgentActions key={`${node.instance.id}:${scopedUnit?.unit.id ?? ""}:${inputs.plan ?? ""}`}
    snapshot={snapshot} node={node} unit={scopedUnit} inputs={inputs} openActivity={openActivity} />;
}

function AgentActions({ snapshot, node, unit, inputs, openActivity }: {
  snapshot: WorkflowExecutionSnapshot; node: ExecutionPinnedNode; unit?: ExecutionUnit;
  inputs: DeliveryInputs; openActivity: WorkspaceProps["openActivity"];
}) {
  const client = useQueryClient();
  const { projectId, id: runId } = snapshot.run;
  const scope = { projectId, runId };
  const nodeId = node.instance.id;
  const unitId = unit?.unit.id ?? null;
  const wired = Object.entries(inputs);
  const matchesInputs = (recorded: DeliveryInputs | null) =>
    Object.keys(recorded ?? {}).length === wired.length &&
    wired.every(([port, id]) => recorded?.[port] === id);
  const job = snapshot.jobs.findLast((entry) => entry.nodeId === nodeId && entry.unitId === unitId && entry.job.origin.type !== "agent" &&
    matchesInputs(entry.inputs))?.job;
  const terminal = job?.latestAttempt?.status === "succeeded" ? job.latestAttempt : null;
  const published = snapshot.outputs.some(({ artifact, current, inputReceipts }) => artifact.nodeInstanceId === nodeId &&
    (unitId ? artifact.unitId === unitId : current) &&
    inputReceipts.some((receipt) => matchesInputs(receipt.inputs)));
  const role = snapshot.rolePins?.find((pin) => pin.nodeId === nodeId && pin.nodeRevision === node.revision && !pin.requirementsOnly)?.role;
  const modelOverride = node.instance.configuration.model as ModelSelection | null | undefined;
  const requirements = node.instance.configuration.requirements as { goal?: unknown; plan?: StagePlanConfiguration } | undefined;
  const goal = readStageGoal(requirements?.goal);
  const plan = requirements?.plan;
  const mutate = useMutation({
    mutationFn: (operation: () => Promise<unknown>) => operation(),
    onSuccess: () => client.invalidateQueries({ queryKey: ["repository-delivery", projectId] }),
  });
  return <>
    {plan && <p>Plan Mode · {plan.questionRoute === "native" ? "Verified CLI questions" : "App-managed questions"}</p>}
    {!published && <DeliveryAgent scope={scope} nodeId={nodeId} inputs={inputs} unitId={unitId}
      job={job} label={node.definition.title} actions={{ execute: (operation) => mutate.mutate(operation), busy: mutate.isPending }}
      openActivity={openActivity} pinnedModel={modelOverride ?? role?.model} pinnedEffort={role?.effort} />}
    {goal && terminal && <AgentGoalVerification attempt={terminal} projectId={projectId} goal={goal} />}
    {mutate.error && <p role="alert">{String(mutate.error)}</p>}
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
