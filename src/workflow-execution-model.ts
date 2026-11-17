import type { Job } from "@/bindings/Job";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { RunStatus } from "@/bindings/RunStatus";
import type { SessionMode } from "@/bindings/SessionMode";
import type { WorkflowDefinition, WorkflowGraphNode, WorkflowLayout, WorkflowWire } from "@/workflow-graph-model";
import type { WorkflowPendingDecision } from "@/workflow-stage-decisions";
import type { ExecutionComparisonStage, RecordedComparison } from "@/workflow-execution-comparison";

export type WorkflowExecutionFocus = { nodeId: string; artifactId?: string };

export function workflowRunTarget(runId: string, focus?: WorkflowExecutionFocus) {
  const target = `run:${runId}`;
  if (!focus) return target;
  const params = new URLSearchParams({ node: focus.nodeId });
  if (focus.artifactId) params.set("artifact", focus.artifactId);
  return `${target}?${params}`;
}

export type WorkflowExecutionState = "queued" | "running" | "waiting" | "skipped" | "failed" | "completed" | "interrupted" | "cancelled";
export type WorkflowVersion = { id: string; revision: string };
export type WorkflowRun = { id: string; projectId: string; version: WorkflowVersion; planRevision: string | null };
export type ExecutionPinnedNode = { instance: WorkflowGraphNode; revision: string; definition: WorkflowDefinition };
type Inputs = Record<string, string>;

export type ExecutionAttempt = { runId: string; ordinal: number; previousRunId: string | null; conversationId: string | null; status: RunStatus };
export type ExecutionJob = { job: Job; nodeId: string; unitId: string | null; inputs: Inputs | null; attempts: ExecutionAttempt[] };
export type ExecutionArtifact = { id: string; workflowRunId: string; nodeInstanceId: string; port: string; unitId: string | null;
  value: { kind: string; evidence: unknown } };
export type ExecutionOutput = { artifact: ExecutionArtifact; current: boolean; inputReceipts: { executionKey: string; revision: string; inputs: Inputs }[] };
export type ExecutionUnit = { planRevision: string; taskId: string; unit: { id: string; ordinal: number; purpose: string } };
export type ExecutionControl = {
  nodeId: string;
  unitId?: string | null;
  planRevision?: string | null;
  state: "completed" | "skipped" | "failed";
  selectedPorts: string[];
  reason?: string | null;
  sourceRunId?: string | null;
  dependencies?: { from: WorkflowWire["from"]; toPort: string; selected: boolean; artifactId?: string | null }[];
  forwardedOutputs?: Record<string, { input: string; artifactId: string }>;
};
export type ExecutionSubflow = {
  path: string;
  name: string;
  version: WorkflowVersion & { destination: "project" | "library"; kind: "subflow" };
  nodes: string[];
  outputs: Record<string, WorkflowWire["from"]>;
};
export type ExecutionRolePin = {
  nodeId: string;
  nodeRevision: string;
  version: WorkflowVersion & { destination: "project" | "library"; kind: "role" };
  requirementsOnly: boolean;
  role: {
    name: string;
    model: ModelSelection;
    effort: string | null;
    toolScope: SessionMode;
    reviewPolicy: "independent" | "cross_review" | "human_approval";
  } | null;
};
export type WorkflowExecutionSnapshot = {
  run: WorkflowRun;
  launch?: {
    checkoutId?: string;
    sourceCommit?: string;
    origin: { kind: "editor" } | { kind: "agile" | "rule"; workId: string } |
      { kind: "conversation"; conversationId: string; workId?: string | null } |
      { kind: "backlog"; workId: string; parentRunId: string; nodeId: string };
    checklistItem?: { itemId: string; revision: number; checked: boolean };
  } | null;
  layout?: WorkflowLayout;
  definition: { version: WorkflowVersion; name: string; nodes: ExecutionPinnedNode[]; wires: WorkflowWire[]; subflows?: ExecutionSubflow[] };
  rolePins?: ExecutionRolePin[];
  unitNodeIds: string[];
  units: ExecutionUnit[];
  jobs: ExecutionJob[];
  outputs: ExecutionOutput[];
  control: ExecutionControl[];
  pendingDecisions?: WorkflowPendingDecision[];
  comparisonStages?: ExecutionComparisonStage[];
  automaticAgentWaits?: UnitExecutionWait[];
  automaticPreparationWaits?: ExecutionWait[];
  testPhaseWaits?: UnitExecutionWait[];
  integratedChecksWaits?: ExecutionWait[];
  githubWaits?: UnitExecutionWait[];
  backlogExecutions?: BacklogExecution[];
};

type ExecutionWait = { nodeId: string; reason: string | null };
type UnitExecutionWait = ExecutionWait & { unitId: string | null };
type BacklogExecution = ExecutionWait & {
  backlogArtifactId: string;
  state: "pending" | "completed" | "failed";
  tasks: { key: string; taskId: string; workflowRunId: string }[];
};

const recordedWaits = [
  ["automaticAgentWaits", "Waiting for agent dispatch"],
  ["automaticPreparationWaits", "Waiting for worktree preparation"],
  ["testPhaseWaits", "Waiting for the registered test check"],
  ["integratedChecksWaits", "Waiting for checks on the integrated revision"],
  ["githubWaits", "Waiting for confirmed GitHub evidence"],
] as const;

export function needsExecutionRefresh(snapshot: WorkflowExecutionSnapshot | null | undefined) {
  if (!snapshot) return false;
  return recordedWaits.some(([field]) => snapshot[field]?.length) ||
    snapshot.backlogExecutions?.some(({ state }) => state === "pending") ||
    snapshot.comparisonStages?.some(({ state }) => state === "queued" || state === "running") ||
    snapshot.jobs.some(({ job }) => job.queuedRecovery || job.downstreamWait ||
      ["starting", "running", "waiting_for_input"].includes(job.latestAttempt?.status ?? "") ||
      job.queueState === "queued" && !job.latestAttempt);
}

export function controlForNode(snapshot: WorkflowExecutionSnapshot, nodeId: string, unit?: ExecutionUnit) {
  const unitScoped = snapshot.unitNodeIds.includes(nodeId);
  const scope = unitScoped ? unit : undefined;
  if (unitScoped && !scope) return undefined;
  const plan = scope?.planRevision ?? snapshot.run.planRevision;
  return snapshot.control.find((receipt) => receipt.nodeId === nodeId &&
    (receipt.unitId ?? null) === (scope?.unit.id ?? null) && (receipt.planRevision ?? null) === plan);
}

function outputInScope(snapshot: WorkflowExecutionSnapshot, output: ExecutionOutput, unit?: ExecutionUnit) {
  return snapshot.unitNodeIds.includes(output.artifact.nodeInstanceId)
    ? Boolean(unit && output.artifact.unitId === unit.unit.id) : output.current;
}

export function executionOutputForWire(snapshot: WorkflowExecutionSnapshot, from: WorkflowWire["from"],
  unit?: ExecutionUnit, control = false) {
  const forwarded = controlForNode(snapshot, from.node, unit)?.forwardedOutputs?.[from.port];
  return snapshot.outputs.findLast((output) => outputInScope(snapshot, output, unit) &&
    (forwarded ? output.artifact.id === forwarded.artifactId
      : output.artifact.nodeInstanceId === from.node && (control || output.artifact.port === from.port)));
}

export function wiredExecutionInputs(snapshot: WorkflowExecutionSnapshot, node: ExecutionPinnedNode, unit?: ExecutionUnit) {
  const inputs: Inputs = {};
  for (const port of node.definition.inputs) {
    const wire = snapshot.definition.wires.find(({ to }) => to.node === node.instance.id && to.port === port.name);
    if (!wire) {
      if (node.definition.kind === "agent" && port.name === "comparison") continue;
      if (["implement", "review_candidate"].includes(node.definition.kind) && port.name === "brief") continue;
      if (node.definition.kind === "accept_candidate" && node.instance.configuration.kind === "none"
        && ["quality", "build"].includes(port.name)) continue;
      if (node.definition.kind === "verify_candidate" && port.name === "strategy") {
        const checks = node.instance.configuration.strategyChecks;
        if (node.instance.configuration.kind !== "staged_checks" || !Array.isArray(checks) || checks.length === 0) continue;
      }
      if (node.definition.kind === "materialize_backlog" && ["design", "strategy"].includes(port.name)) continue;
      return null;
    }
    const selected = controlForNode(snapshot, wire.from.node, unit);
    if (selected?.state === "skipped") return null;
    if (port.value.kind === "control" && selected) {
      if (!selected.selectedPorts.includes(wire.from.port)) return null;
      continue;
    }
    const output = executionOutputForWire(snapshot, wire.from, unit, port.value.kind === "control");
    if (!output) return null;
    if (port.value.kind === "data") inputs[port.name] = output.artifact.id;
  }
  return inputs;
}

export function comparisonForNode(snapshot: WorkflowExecutionSnapshot, node: ExecutionPinnedNode, unit?: ExecutionUnit) {
  const unitScoped = snapshot.unitNodeIds.includes(node.instance.id);
  if (unitScoped && !unit) return undefined;
  const scopedUnit = unitScoped ? unit : undefined;
  const stages = snapshot.comparisonStages?.filter(({ nodeId, nodeRevision }) =>
    nodeId === node.instance.id && nodeRevision === node.revision);
  if (!stages?.length) return undefined;
  const inputs = wiredExecutionInputs(snapshot, node, scopedUnit);
  if (!inputs) return undefined;
  const stage = stages.findLast((stage) => stage.unitId === (scopedUnit?.unit.id ?? null) &&
    stage.planRevision === (scopedUnit?.planRevision ?? null) && Object.keys(stage.inputs).length === Object.keys(inputs).length &&
    Object.entries(stage.inputs).every(([port, artifactId]) => inputs[port] === artifactId));
  if (!stage) return undefined;
  const recorded = snapshot.outputs.filter(({ artifact }) => artifact.nodeInstanceId === node.instance.id &&
    artifact.unitId === stage.unitId && artifact.value.kind === "comparison_results")
    .map(({ artifact }) => artifact.value.evidence as RecordedComparison).find(({ groupId }) => groupId === stage.groupId);
  return { stage, recorded };
}

export function presentation(snapshot: WorkflowExecutionSnapshot, node: ExecutionPinnedNode, unit?: ExecutionUnit): {
  status: WorkflowExecutionState;
  reason?: string;
  missing?: WorkflowWire[];
} {
  const nodeId = node.instance.id;
  const unitScoped = snapshot.unitNodeIds.includes(nodeId);
  if (unitScoped && !unit) return { status: "waiting", reason: "Waiting for an approved logical unit." };
  const scoped = snapshot.jobs.filter((entry) => entry.nodeId === nodeId && entry.job.origin.type !== "agent" &&
    (unitScoped ? entry.unitId === unit?.unit.id : entry.job.pipeline?.planRevision == null || entry.job.pipeline.planRevision === snapshot.run.planRevision));
  const pending = (entry: ExecutionJob) => entry.job.queueState !== "cancelled" &&
    (entry.job.queuedRecovery || !entry.job.latestAttempt || ["starting", "running", "waiting_for_input"].includes(entry.job.latestAttempt.status));
  const job = (scoped.findLast(pending) ?? scoped.at(-1))?.job;
  const completed = snapshot.outputs.some((output) => output.artifact.nodeInstanceId === nodeId && outputInScope(snapshot, output, unit));
  if (job) {
    if (job.queueState !== "cancelled") {
      if (job.downstreamWait) return { status: "waiting", reason: "Waiting for recorded downstream work." };
      if (job.queueState === "paused") return { status: "waiting", reason: "The job is paused." };
      if (!job.latestAttempt || job.queuedRecovery) return { status: "queued" };
    }
    const started = job.latestAttempt?.status;
    if (started === "starting" || started === "running") return { status: "running" };
    if (started === "waiting_for_input") return { status: "waiting", reason: "This attempt is waiting for input." };
    if (started === "failed" || started === "interrupted" || started === "cancelled") return { status: started };
    if (job.queueState === "cancelled" && !started) return { status: "cancelled" };
  }
  const control = controlForNode(snapshot, nodeId, unit);
  if (control) return { status: control.state };
  const backlog = snapshot.backlogExecutions?.findLast((entry) => entry.nodeId === nodeId);
  if (backlog?.state === "failed") return { status: "failed", reason: backlog.reason ?? "A backlog delivery failed." };
  if (backlog?.state === "pending") return { status: "waiting", reason: backlog.reason ?? "Waiting for eligible tasks and verified merges." };
  if (completed || (job?.latestAttempt?.status === "succeeded" && node.definition.outputs.every(({ value }) => value.kind === "control"))) return { status: "completed" };
  for (const [field, fallback] of recordedWaits) {
    const wait = snapshot[field]?.find((entry) => entry.nodeId === nodeId &&
      (!("unitId" in entry) || entry.unitId === null || entry.unitId === unit?.unit.id));
    if (wait) return { status: "waiting", reason: wait.reason ?? fallback };
  }
  const missing = snapshot.definition.wires.filter(({ from, to }) => {
    if (to.node !== nodeId) return false;
    const control = node.definition.inputs.find(({ name }) => name === to.port)?.value.kind === "control";
    const decision = controlForNode(snapshot, from.node, unit);
    if (decision?.state === "skipped") return true;
    if (decision && control) return !decision.selectedPorts.includes(from.port);
    if (decision?.state === "failed") return true;
    return !executionOutputForWire(snapshot, from, unit, control);
  });
  if (missing.length > 0) return { status: "waiting", missing };
  if (job?.latestAttempt?.status === "succeeded") return { status: "waiting", reason: "Waiting for the recorded result of this attempt." };
  if (node.definition.executor === "record_implementation_release") return { status: "waiting", reason: "Waiting for an implementation approval decision." };
  return { status: "waiting", reason: "Ready for the next execution action." };
}
