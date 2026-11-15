import type { Edge, Node } from "@xyflow/react";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import catalog from "../src-tauri/model-catalog/v1.json";

export const sampleModels: ModelOption[] = catalog.models.map((model) => ({
  selection: { provider: model.provider as Provider, profileId: "simulated-read-only", modelId: model.id },
  name: model.name,
  selectable: true,
  unavailableReason: null,
  rating: {
    intelligenceTenths: model.intelligenceTenths,
    apiReferenceCents: model.apiReferenceCents,
    priceScore: model.priceScore,
  },
  catalogVersion: catalog.version,
}));

export function getSampleModel(modelId: string) {
  return sampleModels.find((option) => option.selection.modelId === modelId);
}

export const stageKinds = {
  plan: "Planning",
  condition: "Condition",
  review: "Review",
  join: "Join",
  task: "Create or link work",
  approval: "Approve implementation",
  implementation: "Implementation",
  verify: "Verify candidate",
  accept: "Accept diff",
  commit: "Commit",
  merge: "Merge",
} as const;

export type StageKind = keyof typeof stageKinds;
export type StageMode = "read-only" | "plan" | "write";
export type ReleasePredicate = "plan-present" | "reviews-passed";
type OutputType = "plan" | "review" | "task" | "release" | "patch" | "checks" | "decision" | "commit" | "integration";

export interface Role {
  id: string;
  title: string;
  model: ModelSelection | null;
  effort: string;
  instructions: string;
  tools: string;
  completion: string;
  review: string;
  requirements: string[];
}

export type Stage = {
  title: string;
  kind: StageKind;
  roleId: string;
  mode: StageMode;
  model: ModelSelection | null;
  timeoutMinutes: number;
  retryLimit: number;
  output: OutputType;
  requiredCapability: string;
  subagents: number;
  release: "human" | "rule";
  predicate: ReleasePredicate;
  goal: string;
  evidence: string;
  goalMinutes: number;
  goalContinuation: "managed" | "native";
};

export type StageNode = Node<Stage, "stage">;
export type StageEdge = Edge<{ branch?: "migration" }>;

export interface WorkflowRevision {
  label: string;
  revision: number;
  nodes: StageNode[];
  edges: StageEdge[];
  roles: Role[];
  comparisonRuns?: string[];
  mergeRequired?: boolean;
}

export function createStage(kind: StageKind, id: string, x: number, y: number): StageNode {
  const stageRoles: Partial<Record<StageKind, string>> = { plan: "planner", review: "reviewer", implementation: "implementer", verify: "reviewer" };
  const outputs: Record<StageKind, OutputType> = {
    plan: "plan", condition: "decision", review: "review", join: "review", task: "task",
    approval: "release", implementation: "patch", verify: "checks", accept: "decision", commit: "commit", merge: "integration",
  };
  return {
    id, type: "stage", position: { x, y }, ariaLabel: stageKinds[kind],
    data: {
      title: stageKinds[kind], kind, roleId: stageRoles[kind] ?? "", mode: ["implementation", "commit", "merge"].includes(kind) ? "write" : kind === "plan" ? "plan" : "read-only",
      model: null, timeoutMinutes: 10, retryLimit: kind === "verify" ? 1 : 0, output: outputs[kind], requiredCapability: "",
      subagents: 0, release: "human", predicate: "reviews-passed", goal: "", evidence: "", goalMinutes: 15, goalContinuation: "managed",
    },
  };
}

export function createDeliveryWorkflow(): WorkflowRevision {
  const nodes: StageNode[] = [
    createStage("plan", "plan", 24, 160),
    createStage("condition", "condition", 224, 160),
    { ...createStage("review", "review", 424, 42), data: { ...createStage("review", "review", 424, 42).data, title: "Cross-review" } },
    { ...createStage("review", "migration", 424, 276), data: { ...createStage("review", "migration", 424, 276).data, title: "Migration review" } },
    createStage("join", "join", 624, 160),
    createStage("task", "task", 824, 160),
    createStage("approval", "approval", 824, 360),
    createStage("implementation", "implementation", 624, 360),
    createStage("verify", "verify", 424, 360),
    createStage("accept", "accept", 224, 360),
    createStage("commit", "commit", 24, 360),
    createStage("merge", "merge", 24, 540),
  ];
  const pairs: [string, string][] = [
    ["plan", "condition"], ["condition", "review"], ["condition", "migration"], ["review", "join"],
    ["migration", "join"], ["join", "task"], ["task", "approval"], ["approval", "implementation"],
    ["implementation", "verify"], ["verify", "accept"], ["accept", "commit"], ["commit", "merge"],
  ];
  const roles = [
    { id: "planner", title: "Planner", model: getSampleModel("gpt-6.1-sol")?.selection ?? null, effort: "high", instructions: "Read the pinned release note and propose a scoped plan.", tools: "Repository read", completion: "Return a versioned plan and acceptance criteria.", review: "Cross-review", requirements: ["Interactive questions"] },
    { id: "reviewer", title: "Reviewer", model: getSampleModel("claude-sonnet-5-5")?.selection ?? null, effort: "medium", instructions: "Check the candidate against the release criteria.", tools: "Repository read", completion: "Return structured findings and verification results.", review: "Independent", requirements: [] },
    { id: "implementer", title: "Implementer", model: getSampleModel("gpt-6.1-terra")?.selection ?? null, effort: "high", instructions: "Implement the approved plan in the assigned worktree.", tools: "Approved worktree write", completion: "Return a patch and verification evidence.", review: "Human approval", requirements: [] },
  ] satisfies Role[];
  return {
    label: "Task delivery",
    revision: 3,
    nodes,
    edges: pairs.map(([source, target]) => ({
      id: source + "-" + target, source, target,
      label: source === "condition" ? (target === "migration" ? "If migration" : "Always") : undefined,
      data: target === "migration" ? { branch: "migration" } : undefined,
    })),
    roles,
  };
}

function upstreamStages(workflow: Pick<WorkflowRevision, "nodes" | "edges">, target: string): StageNode[] {
  const upstream = new Set<string>();
  function visit(id: string) {
    for (const edge of workflow.edges) {
      if (edge.target === id && !upstream.has(edge.source)) {
        upstream.add(edge.source);
        visit(edge.source);
      }
    }
  }
  visit(target);
  return workflow.nodes.filter((stage) => upstream.has(stage.id));
}

export function snapshotWorkflow(workflow: WorkflowRevision): WorkflowRevision {
  return {
    ...workflow,
    nodes: workflow.nodes.map(({ id, type, position, ariaLabel, data }) => ({ id, type, position, ariaLabel, data })),
    edges: workflow.edges.map(({ id, source, target, sourceHandle, targetHandle, label, data }) => ({
      id, source, target, sourceHandle, targetHandle, label, data,
    })),
  };
}

export function validateWorkflow({ nodes, edges }: WorkflowRevision): string[] {
  const errors: string[] = [];
  const byId = new Map(nodes.map((node) => [node.id, node]));
  const incoming = (id: string) => edges.filter((edge) => edge.target === id).map((edge) => edge.source);
  const visited = new Set<string>();
  const visiting = new Set<string>();

  function cycle(id: string): boolean {
    if (visiting.has(id)) return true;
    if (visited.has(id)) return false;
    visiting.add(id);
    const hasCycle = incoming(id).some(cycle);
    visiting.delete(id);
    visited.add(id);
    return hasCycle;
  }

  const releaseResults = new Map<string, boolean>();
  function released(id: string): boolean {
    const known = releaseResults.get(id);
    if (known !== undefined) return known;
    const sources = incoming(id);
    const isReleased = byId.get(id)?.data.kind === "approval" || (sources.length > 0 && sources.every(released));
    releaseResults.set(id, isReleased);
    return isReleased;
  }

  if (nodes.length === 0) errors.push("Add at least one stage.");
  if (edges.some((edge) => !byId.has(edge.source) || !byId.has(edge.target))) errors.push("Every connection must join two existing stages.");
  const hasCycle = nodes.some((node) => cycle(node.id));
  if (hasCycle) errors.push("The graph contains a cycle. Use a stage's bounded retry limit instead.");
  for (const { id, data } of nodes) {
    if (!data.title.trim()) errors.push(stageKinds[data.kind] + " needs a title.");
    const upstream = upstreamStages({ nodes, edges }, id);
    if ((data.kind === "approval" || data.kind === "condition") && !upstream.some((stage) => stage.data.kind === "plan" && stage.data.output === "plan")) {
      errors.push(data.title + " needs a typed plan from an upstream planning stage.");
    }
    if (data.kind === "accept" && !upstream.some((stage) => stage.data.kind === "verify" && stage.data.output === "checks")) {
      errors.push(data.title + " needs checks from an upstream verification stage.");
    }
    if (data.kind === "commit" && !upstream.some((stage) => stage.data.kind === "accept")) {
      errors.push(data.title + " needs an accepted candidate.");
    }
    if (data.kind === "merge" && !upstream.some((stage) => stage.data.kind === "commit")) {
      errors.push(data.title + " needs a verified source commit.");
    }
    if (data.mode === "write" && (data.kind === "approval" || (!hasCycle && !released(id)))) {
      errors.push(data.title + " must follow Approve implementation on every path.");
    }
    if (["implementation", "commit", "merge"].includes(data.kind) && data.mode !== "write") errors.push(data.title + " requires a writing profile.");
    if (data.goal.trim() && (!data.evidence.trim() || !Number.isInteger(data.goalMinutes) || data.goalMinutes < 1)) {
      errors.push(data.title + " needs completion evidence and a positive time budget for its goal.");
    }
    if (!Number.isInteger(data.retryLimit) || data.retryLimit < 0 || !Number.isInteger(data.timeoutMinutes) || data.timeoutMinutes < 1) {
      errors.push(data.title + " needs a non-negative retry count and a positive timeout.");
    }
  }
  return errors;
}

export type StageStatus = "queued" | "running" | "waiting" | "succeeded" | "failed" | "skipped";
export type GitEvidence = { branch: string; sha: string };
export interface Attempt {
  number: number;
  status: StageStatus;
  detail: string;
  git?: GitEvidence;
}
export interface PipelineRun {
  id: number;
  workflow: WorkflowRevision;
  migration: boolean | null;
  attempts: Record<string, Attempt[]>;
}

export function beginPipelineRun(id: number, workflow: WorkflowRevision): PipelineRun {
  return {
    id, workflow, migration: null,
    attempts: Object.fromEntries(workflow.nodes.map((node) => [
      node.id, [{ number: 1, status: "queued", detail: "Waiting for prerequisite stages." }],
    ])),
  };
}

export function stageAttempt(run: PipelineRun, id: string) {
  return run.attempts[id]?.at(-1);
}

function updateAttempt(run: PipelineRun, id: string, status: StageStatus, detail: string, git?: GitEvidence): PipelineRun {
  const prior = run.attempts[id];
  if (!prior?.length) return run;
  return { ...run, attempts: { ...run.attempts, [id]: [...prior.slice(0, -1), { number: prior.length, status, detail, git }] } };
}

function readyState(run: PipelineRun, id: string): "blocked" | "skip" | "ready" {
  const edges = run.workflow.edges.filter((edge) => edge.target === id);
  if (!edges.length) return "ready";
  const sources = edges.map((edge) => ({
    attempt: stageAttempt(run, edge.source),
    omitted: edge.data?.branch === "migration" && run.migration === false,
  }));
  if (sources.some(({ attempt }) => !attempt || (attempt.status !== "succeeded" && attempt.status !== "skipped"))) return "blocked";
  return sources.every(({ attempt, omitted }) => omitted || attempt?.status === "skipped") ? "skip" : "ready";
}

export function advancePipelineRun(run: PipelineRun): PipelineRun {
  let next = run;
  for (const node of run.workflow.nodes) {
    const attempt = stageAttempt(run, node.id);
    if (!attempt) continue;
    if (attempt.status === "queued") {
      const readiness = readyState(run, node.id);
      if (readiness === "ready") {
        next = node.data.kind === "merge" && !run.workflow.mergeRequired
          ? updateAttempt(next, node.id, "skipped", "The selected delivery policy requires a verified commit.")
          : updateAttempt(next, node.id, "running", "Executing against workflow revision " + run.workflow.revision + ".");
      }
      if (readiness === "skip") next = updateAttempt(next, node.id, "skipped", "Branch was excluded by the planning decision.");
      continue;
    }
    if (attempt.status !== "running") continue;
    const { kind, release, predicate, goal, requiredCapability, roleId } = node.data;
    const role = run.workflow.roles.find((item) => item.id === roleId);
    const effectiveModel = node.data.model ?? role?.model;
    if (goal && node.data.goalContinuation === "native" && effectiveModel?.provider !== "codex") {
      next = updateAttempt(next, node.id, "failed", "Native goal control is unverified for this example profile. Select a qualified profile or managed continuation.");
    } else if (requiredCapability === "Windows host control" || role?.requirements.includes("Windows host control")) {
      next = updateAttempt(next, node.id, "failed", "Windows host control is unverified for this example profile.");
    } else if (kind === "commit" || kind === "merge") {
      next = updateAttempt(next, node.id, "waiting", kind === "commit" ? "Waiting for commit" : "Waiting for merge");
    } else if (kind === "plan" || goal) {
      next = updateAttempt(next, node.id, "waiting", kind === "plan" ? "Planning needs an answer about migration scope." : "Confirm that the goal evidence meets its completion criteria.");
    } else if (kind === "approval" || kind === "accept") {
      const reviews = upstreamStages(run.workflow, node.id).filter((stage) => stage.data.kind === "review" && stageAttempt(run, stage.id)?.status === "succeeded");
      const eligible = predicate === "plan-present" ? run.migration !== null : reviews.length > 0 && run.migration !== null;
      if (kind === "approval" && release === "rule" && eligible) {
        next = updateAttempt(next, node.id, "succeeded", "Release rule matched the pinned plan, findings and worktree scope.");
      } else {
        next = updateAttempt(next, node.id, "waiting", release === "rule" && kind === "approval" ? "Release rule did not match. A human decision is required." : "Waiting for a human decision on the recorded revision.");
      }
    } else if (kind === "verify" && attempt.number === 1) {
      next = updateAttempt(next, node.id, "failed", "Verification suite was interrupted while opening the test fixture.");
    } else {
      next = updateAttempt(next, node.id, "succeeded", "Recorded output: " + node.data.output + ".");
    }
  }
  return next;
}

export function resolveStage(run: PipelineRun, id: string, migration?: boolean): PipelineRun {
  const node = run.workflow.nodes.find((stage) => stage.id === id);
  if (!node || stageAttempt(run, id)?.status !== "waiting") return run;
  if (node.data.kind === "commit" || node.data.kind === "merge") return run;
  const resolved = updateAttempt(run, id, "succeeded", node.data.kind === "approval" ? "Approved on the pinned workflow and plan revision." : "Recorded decision and output: " + node.data.output + ".");
  return node.data.kind === "plan" ? { ...resolved, migration: migration ?? false } : resolved;
}

export function recordPipelineGit(run: PipelineRun, evidence: { commit?: GitEvidence; merge?: GitEvidence }): PipelineRun {
  return run.workflow.nodes.reduce((current, node) => {
    const kind = node.data.kind;
    if ((kind !== "commit" && kind !== "merge") || stageAttempt(current, node.id)?.status !== "waiting") return current;
    const result = evidence[kind];
    return result ? updateAttempt(current, node.id, "succeeded", kind === "commit" ? "Verified source commit recorded." : "Verified integration recorded.", result) : current;
  }, run);
}

export function declineStage(run: PipelineRun, id: string): PipelineRun {
  const stage = run.workflow.nodes.find((node) => node.id === id);
  if (stageAttempt(run, id)?.status !== "waiting" || (stage?.data.kind !== "approval" && stage?.data.kind !== "accept")) return run;
  return updateAttempt(run, id, "failed", "Declined by the reviewer. Dependent stages remain blocked.");
}

export function retryStage(run: PipelineRun, id: string): PipelineRun {
  const stage = run.workflow.nodes.find((node) => node.id === id);
  const attempts = run.attempts[id];
  if (!stage || !attempts || attempts.at(-1)?.status !== "failed" || attempts.length > stage.data.retryLimit) return run;
  return {
    ...run,
    attempts: { ...run.attempts, [id]: [...attempts, { number: attempts.length + 1, status: "queued", detail: "Retry of attempt " + attempts.length + "." }] },
  };
}

export function interruptStage(run: PipelineRun, id: string): PipelineRun {
  const status = stageAttempt(run, id)?.status;
  return status === "running" || status === "waiting" ? updateAttempt(run, id, "failed", "Interrupted by the operator.") : run;
}

export function runStatus(run: PipelineRun): StageStatus {
  const attempts = Object.values(run.attempts).map((attempts) => attempts.at(-1));
  if (attempts.some((attempt) => attempt?.status === "running")) return "running";
  if (attempts.some((attempt) => attempt?.status === "waiting")) return "waiting";
  if (attempts.some((attempt) => attempt?.status === "failed")) return "failed";
  if (attempts.some((attempt) => attempt?.status === "queued")) return "queued";
  return "succeeded";
}
