import type { Job } from "@/bindings/Job";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";

export type DeliveryNode = "plan" | "review-plan" | "implement" | "review-candidate" | "review-diff" | "agent";
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
    | { kind: "approval"; evidence: { planRevision: string; source?: "operator" | "automatic_rule" } }
    | { kind: "release"; evidence: { decisionArtifactId: string; release: { id: string; revoked: boolean } } }
    | { kind: "worktree"; evidence: { checkout_id: string; task: TaskReference } }
    | { kind: "candidate"; evidence: { id: string; tree: string; target: { runId: string; unitId: string } } }
    | { kind: "checks" | "git"; evidence: ScopedEvidence }
    | { kind: "merge"; evidence: ScopedEvidence & { commitEvidenceId: string } }
    | { kind: "acceptance"; evidence: { evidence: ScopedEvidence; reviewArtifactId: string; completionCriteria: string[] } };
};

export type DeliveryRun = { id: string; planRevision: string | null; version?: { id: string; revision: string } };
export type DeliveryJob = { nodeId: DeliveryNode; unitId: string | null; inputs: DeliveryInputs; job: Job };
export type DeliverySnapshot = {
  run: DeliveryRun; artifacts: DeliveryArtifact[]; jobs: DeliveryJob[];
};
export type DeliveryTarget = { projectId: string; runId: string };
export type DeliveryAction = (operation: () => Promise<unknown>) => void;
export type DeliveryActions = { execute: DeliveryAction; busy: boolean };
export type DeliveryModelChoice = ModelSelection | null;

export const deliveryProviders: Provider[] = ["codex", "claude", "antigravity"];
