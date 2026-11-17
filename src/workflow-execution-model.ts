import type { Job } from "@/bindings/Job";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { RunStatus } from "@/bindings/RunStatus";
import type { SessionMode } from "@/bindings/SessionMode";
import type { WorkflowDefinition, WorkflowGraphNode, WorkflowLayout, WorkflowWire } from "@/workflow-graph-model";
import type { WorkflowPendingDecision } from "@/workflow-stage-decisions";
import type { ExecutionComparisonStage, RecordedComparison } from "@/workflow-execution-comparison";

export const workflowRunTarget = (runId: string) => `run:${runId}`;

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
    origin: { kind: "editor" } | { kind: "agile" | "rule"; workId: string };
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
};

export function controlForNode(snapshot: WorkflowExecutionSnapshot, nodeId: string, unit?: ExecutionUnit) {
  const unitScoped = snapshot.unitNodeIds.includes(nodeId);
  const scope = unitScoped ? unit : undefined;
  if (unitScoped && !scope) return undefined;
  const plan = scope?.planRevision ?? snapshot.run.planRevision;
  return snapshot.control.find((receipt) => receipt.nodeId === nodeId &&
    (receipt.unitId ?? null) === (scope?.unit.id ?? null) && (receipt.planRevision ?? null) === plan);
}

export function wiredExecutionInputs(snapshot: WorkflowExecutionSnapshot, node: ExecutionPinnedNode, unit?: ExecutionUnit) {
  const inputs: Inputs = {};
  for (const port of node.definition.inputs) {
    const wire = snapshot.definition.wires.find(({ to }) => to.node === node.instance.id && to.port === port.name);
    if (!wire) {
      if (node.definition.kind === "agent" && port.name === "comparison") continue;
      return null;
    }
    const sourceForUnit = snapshot.unitNodeIds.includes(wire.from.node);
    const selected = controlForNode(snapshot, wire.from.node, unit);
    if (selected?.state === "skipped") return null;
    if (port.value.kind === "control" && selected) {
      if (!selected.selectedPorts.includes(wire.from.port)) return null;
      continue;
    }
    const output = snapshot.outputs.findLast(({ artifact, current }) =>
      artifact.nodeInstanceId === wire.from.node && (sourceForUnit ? artifact.unitId === unit?.unit.id : current) &&
      (port.value.kind === "control" || artifact.port === wire.from.port));
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
  const eligible = (output: ExecutionOutput) => snapshot.unitNodeIds.includes(output.artifact.nodeInstanceId)
    ? Boolean(unit && output.artifact.unitId === unit.unit.id) : output.current;
  if (unitScoped && !unit) return { status: "waiting", reason: "Waiting for an approved logical unit." };
  const scoped = snapshot.jobs.filter((entry) => entry.nodeId === nodeId && entry.job.origin.type !== "agent" &&
    (unitScoped ? entry.unitId === unit?.unit.id : entry.job.pipeline?.planRevision == null || entry.job.pipeline.planRevision === snapshot.run.planRevision));
  const pending = (entry: ExecutionJob) => entry.job.queueState !== "cancelled" &&
    (entry.job.queuedRecovery || !entry.job.latestAttempt || ["starting", "running", "waiting_for_input"].includes(entry.job.latestAttempt.status));
  const job = (scoped.findLast(pending) ?? scoped.at(-1))?.job;
  const completed = snapshot.outputs.some((output) => output.artifact.nodeInstanceId === nodeId && eligible(output));
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
  if (completed || (job?.latestAttempt?.status === "succeeded" && node.definition.outputs.every(({ value }) => value.kind === "control"))) return { status: "completed" };
  const missing = snapshot.definition.wires.filter(({ from, to }) => {
    if (to.node !== nodeId) return false;
    const control = node.definition.inputs.find(({ name }) => name === to.port)?.value.kind === "control";
    const decision = controlForNode(snapshot, from.node, unit);
    if (decision?.state === "skipped") return true;
    if (decision && control) return !decision.selectedPorts.includes(from.port);
    if (decision?.state === "failed") return true;
    return !snapshot.outputs.some((output) => output.artifact.nodeInstanceId === from.node && eligible(output) &&
      (control || output.artifact.port === from.port));
  });
  if (missing.length > 0) return { status: "waiting", missing };
  if (job?.latestAttempt?.status === "succeeded") return { status: "waiting", reason: "Waiting for the recorded result of this attempt." };
  if (node.definition.executor === "record_implementation_release") return { status: "waiting", reason: "Waiting for an implementation approval decision." };
  return { status: "waiting", reason: "Ready for the next execution action." };
}
