import type { Job } from "@/bindings/Job";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";

export type DeliveryNode = "plan" | "review-plan" | "implement" | "review-candidate";
export type DeliveryInputs = Record<string, string>;
type TaskReference = { projectId: string; taskId: string };
type ScopedEvidence = { task: TaskReference; id: string };
export type DeliverySource = { id: string; checkoutId: string; relativePath: string; sha256: string };

export type DeliveryArtifact = {
  id: string;
  nodeInstanceId: string;
  port: string;
  unitId: string | null;
  value:
    | { kind: "markdown"; evidence: DeliverySource }
    | { kind: "plan"; evidence: { run_id: string; event_sequence: number } }
    | { kind: "review"; evidence: { run_id: string } }
    | { kind: "task"; evidence: TaskReference }
    | { kind: "approval"; evidence: { planRevision: string } }
    | { kind: "release"; evidence: { decisionArtifactId: string; release: { id: string; revoked: boolean } } }
    | { kind: "worktree"; evidence: { checkout_id: string; task: TaskReference } }
    | { kind: "candidate"; evidence: { id: string; tree: string; target: { runId: string; unitId: string } } }
    | { kind: "checks" | "git"; evidence: ScopedEvidence }
    | { kind: "merge"; evidence: ScopedEvidence & { commitEvidenceId: string } }
    | { kind: "acceptance"; evidence: { evidence: ScopedEvidence; reviewArtifactId: string; completionCriteria: string[] } };
};

export type DeliveryRun = { id: string; planRevision: string | null };
export type DeliveryJob = { nodeId: DeliveryNode; unitId: string | null; inputs: DeliveryInputs; job: Job };
export type DeliveryWorkflowDefinition = {
  nodes: { instance: { id: string; kind: string }; definition: { title: string } }[];
  wires: { from: { node: string; port: string }; to: { node: string; port: string } }[];
};
export type DeliverySnapshot = {
  run: DeliveryRun; artifacts: DeliveryArtifact[]; jobs: DeliveryJob[];
  definition?: DeliveryWorkflowDefinition; hostOutputs?: DeliveryInputs;
};
export type DeliveryUnit = {
  checkoutId: string;
  taskId: string;
  unit: { id: string; ordinal: number; purpose: string; completionCriteria: string[] };
  commitEvidenceId: string | null;
};
export type DeliveryTarget = { projectId: string; runId: string };
export type DeliveryAction = (operation: () => Promise<unknown>) => void;
export type DeliveryActions = { execute: DeliveryAction; busy: boolean };
export type DeliveryModelChoice = ModelSelection | null;

export const deliveryProviders: Provider[] = ["codex", "claude", "antigravity"];

export function deliveryGitControlId(runId: string, nodeId: string, unitId: string) {
  return `delivery-${runId}-${nodeId}-${unitId}`;
}

export function matchingJob(delivery: DeliverySnapshot, nodeId: DeliveryNode, inputs: DeliveryInputs, unitId: string | null = null) {
  return delivery.jobs.findLast((entry) =>
    entry.nodeId === nodeId && entry.unitId === unitId &&
    Object.entries(inputs).every(([port, id]) => entry.inputs[port] === id),
  )?.job;
}

export function matchingReview(delivery: DeliverySnapshot, job: Job | undefined) {
  return delivery.artifacts.find(({ value }) =>
    value.kind === "review" && value.evidence.run_id === job?.latestAttempt?.runId,
  );
}
