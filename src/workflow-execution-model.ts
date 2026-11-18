import type { Job } from "@/bindings/Job";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { RunStatus } from "@/bindings/RunStatus";
import type { SessionMode } from "@/bindings/SessionMode";
import { isSupportedWorkflowNode, wireKey, type WorkflowDefinition, type WorkflowGraphNode, type WorkflowLayout, type WorkflowPort, type WorkflowWire } from "@/workflow-graph-model";
import type { WorkflowPendingDecision } from "@/workflow-stage-decisions";

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
  inputs?: Record<string, WorkflowWire["to"][]>;
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
  conversationTurnActive?: boolean;
  pendingDecisions?: WorkflowPendingDecision[];
  timedWaits?: { nodeId: string; unitId: string | null; deadlineAt: number }[];
  forkBranches?: { forkId: string; branch: string; unitId: string | null; state: Exclude<WorkflowExecutionState, "waiting" | "interrupted">; settledOrder: number | null }[];
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

const executionHelperKinds = new Set(["loop_gate", "loop_step", "loop_result", "join_failures", "legacy_review_verdict"]);

export function isExecutionHelper({ instance }: ExecutionPinnedNode) {
  return executionHelperKinds.has(instance.configuration.kind);
}

function foldsSubflows(definition: WorkflowExecutionSnapshot["definition"]) {
  return definition.nodes.some((node) => isExecutionHelper(node) ||
    node.instance.kind === "end" && !node.instance.id.includes("::"));
}

function subflowForStage(definition: WorkflowExecutionSnapshot["definition"], nodeId: string) {
  if (!foldsSubflows(definition)) return undefined;
  return definition.subflows?.filter(({ nodes }) => nodes.includes(nodeId))
    .sort((a, b) => b.path.length - a.path.length)[0];
}

export function executionStageLocation(definition: WorkflowExecutionSnapshot["definition"], node: ExecutionPinnedNode) {
  const scope = subflowForStage(definition, node.instance.id);
  if (scope && isExecutionHelper(node)) {
    const parent = scope.path.lastIndexOf("::");
    return { nodeId: scope.path, scopePath: parent < 0 ? "" : scope.path.slice(0, parent) };
  }
  return { nodeId: node.instance.id, scopePath: scope?.path ?? "" };
}

export function executionCanvasGraph(definition: WorkflowExecutionSnapshot["definition"], scope?: ExecutionSubflow) {
  const scoped = scope ? new Set(scope.nodes) : undefined;
  const original = new Map(definition.nodes.map((node) => [node.instance.id, node]));
  const prefix = scope ? scope.path + "::" : "";
  const subflows = new Map((foldsSubflows(definition) ? definition.subflows ?? [] : []).filter(({ path }) =>
    path.startsWith(prefix) && !path.slice(prefix.length).includes("::"))
    .map((subflow) => [subflow.path, subflow]));
  const owners = new Map<string, ExecutionSubflow>();
  const inputs = new Map<string, Map<string, WorkflowPort>>();
  const outputs = new Map<string, Map<string, WorkflowPort>>();
  const endpointKey = ({ node, port }: WorkflowWire["from"]) => JSON.stringify([node, port]);
  const inputNames = new Map<string, string>();
  const outputNames = new Map<string, string>();
  for (const subflow of subflows.values()) {
    for (const node of subflow.nodes) owners.set(node, subflow);
    const declaredInputs = new Map<string, WorkflowPort>();
    for (const [name, endpoints] of Object.entries(subflow.inputs ?? {})) {
      for (const endpoint of endpoints) {
        inputNames.set(endpointKey(endpoint), name);
        const port = original.get(endpoint.node)?.definition.inputs.find(({ name }) => name === endpoint.port);
        if (port) declaredInputs.set(name, { ...port, name });
      }
    }
    inputs.set(subflow.path, declaredInputs);
    const declaredOutputs = new Map<string, WorkflowPort>();
    for (const [name, endpoint] of Object.entries(subflow.outputs)) {
      if (!outputNames.has(endpointKey(endpoint))) outputNames.set(endpointKey(endpoint), name);
      const port = original.get(endpoint.node)?.definition.outputs.find(({ name }) => name === endpoint.port);
      if (port) declaredOutputs.set(name, { ...port, name });
    }
    outputs.set(subflow.path, declaredOutputs);
  }
  const wires = new Map<string, WorkflowWire>();
  for (const wire of definition.wires) {
    if (scoped && (!scoped.has(wire.from.node) || !scoped.has(wire.to.node))) continue;
    const source = owners.get(wire.from.node);
    const target = owners.get(wire.to.node);
    if (source && source === target) continue;
    let { from, to } = wire;
    if (source) {
      const name = outputNames.get(endpointKey(from)) ?? `${from.node.slice(source.path.length + 2)}.${from.port}`;
      const port = original.get(from.node)?.definition.outputs.find(({ name }) => name === from.port);
      if (port) outputs.get(source.path)?.set(name, { ...port, name });
      from = { node: source.path, port: name };
    }
    if (target) {
      const name = inputNames.get(endpointKey(to)) ?? `${to.node.slice(target.path.length + 2)}.${to.port}`;
      const port = original.get(to.node)?.definition.inputs.find(({ name }) => name === to.port);
      if (port) inputs.get(target.path)?.set(name, { ...port, name });
      to = { node: target.path, port: name };
    }
    const projected = { from, to };
    wires.set(wireKey(projected), projected);
  }
  const emitted = new Set<string>();
  const nodes = definition.nodes.flatMap((node): ExecutionPinnedNode[] => {
    const id = node.instance.id;
    if (scoped && !scoped.has(id)) return [];
    const subflow = owners.get(id);
    if (!subflow) return isExecutionHelper(node) ? [] : [node];
    if (emitted.has(subflow.path)) return [];
    emitted.add(subflow.path);
    const resultSource = subflow.outputs.results && original.get(subflow.outputs.results.node);
    const kind = resultSource?.instance.configuration.kind === "loop_result" ? "loop" : "subflow";
    return [{
      instance: { id: subflow.path, kind, configuration: { kind: "none" } },
      revision: subflow.version.revision,
      definition: {
        title: subflow.name, kind, configuration: "none", revision: 1,
        executor: "expand_subflow", effect: "none",
        inputs: [...inputs.get(subflow.path)!.values()],
        outputs: [...outputs.get(subflow.path)!.values()],
      },
    }];
  });
  const visible = new Set(nodes.map(({ instance }) => instance.id));
  return { name: scope?.name ?? definition.name, nodes,
    wires: [...wires.values()].filter(({ from, to }) => visible.has(from.node) && visible.has(to.node)), subflows };
}

const recordedWaits = [
  ["automaticAgentWaits", "Waiting for agent dispatch"],
  ["automaticPreparationWaits", "Waiting for worktree preparation"],
  ["testPhaseWaits", "Waiting for the registered test check"],
  ["integratedChecksWaits", "Waiting for checks on the integrated revision"],
  ["githubWaits", "Waiting for confirmed GitHub evidence"],
] as const;

export function needsExecutionRefresh(snapshot: WorkflowExecutionSnapshot | null | undefined) {
  if (!snapshot || snapshot.definition.nodes.some(({ instance }) => !isSupportedWorkflowNode(instance.kind))) return false;
  return snapshot.conversationTurnActive === true || Boolean(snapshot.timedWaits?.length) ||
    snapshot.forkBranches?.some(({ state }) => state === "queued" || state === "running") ||
    Boolean(snapshot.pendingDecisions?.length) || recordedWaits.some(([field]) => snapshot[field]?.length) ||
    snapshot.backlogExecutions?.some(({ state }) => state === "pending") ||
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
      if (node.definition.kind === "agent" && port.name === "review_subject" &&
        (node.instance.configuration.requirements as { reviewReuse?: boolean } | undefined)?.reviewReuse !== true) continue;
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
  const timed = snapshot.timedWaits?.find((entry) => entry.nodeId === nodeId && entry.unitId === (unitScoped ? unit?.unit.id : null));
  if (timed) return { status: "waiting", reason: "Resumes " + new Date(timed.deadlineAt).toLocaleString() + "." };
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
