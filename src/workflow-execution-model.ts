import type { Job } from "@/bindings/Job";
import type { RunStatus } from "@/bindings/RunStatus";
import type { WorkflowDefinition, WorkflowGraphNode, WorkflowWire } from "@/workflow-graph-model";

export const workflowRunTarget = (runId: string) => `run:${runId}`;

export type WorkflowExecutionState = "queued" | "running" | "waiting" | "failed" | "completed" | "interrupted" | "cancelled";
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
export type WorkflowExecutionSnapshot = {
  run: WorkflowRun;
  definition: { version: WorkflowVersion; name: string; nodes: ExecutionPinnedNode[]; wires: WorkflowWire[] };
  unitNodeIds: string[];
  units: ExecutionUnit[];
  jobs: ExecutionJob[];
  outputs: ExecutionOutput[];
};

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
  if (completed || (job?.latestAttempt?.status === "succeeded" && node.definition.outputs.every(({ value }) => value.kind === "control"))) return { status: "completed" };
  const missing = snapshot.definition.wires.filter(({ from, to }) => {
    if (to.node !== nodeId) return false;
    const control = node.definition.inputs.find(({ name }) => name === to.port)?.value.kind === "control";
    return !snapshot.outputs.some((output) => output.artifact.nodeInstanceId === from.node && eligible(output) &&
      (control || output.artifact.port === from.port));
  });
  if (missing.length > 0) return { status: "waiting", missing };
  if (job?.latestAttempt?.status === "succeeded") return { status: "waiting", reason: "Waiting for the recorded result of this attempt." };
  if (node.definition.executor === "record_implementation_release") return { status: "waiting", reason: "Waiting for an implementation approval decision." };
  return { status: "waiting", reason: "Ready for the next execution action." };
}
