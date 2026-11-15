import type { ProjectCheckout } from "@/bindings/ProjectCheckout";

export type WorkspaceId = "conversations" | "ide" | "tasks" | "pipelines" | "knowledge" | "git" | "diagnostics";
export type ScenarioId = "delivery" | "recovery" | "pipelines" | "windows";
export type DeliveryStep = "note" | "question" | "plan" | "queued" | "running" | "review" | "verified" | "accepted" | "merged" | "interrupted" | "failed";
export type DeliveryAction = "start" | "answer" | "approve" | "dispatch" | "finish" | "verify" | "accept" | "merge" | "interrupt" | "resume" | "fail" | "retry" | "invalidate";

export interface SimulationSession {
  scenario: ScenarioId;
  epoch: number;
  delivery: DeliveryStep;
  advanceDelivery: (action: DeliveryAction) => void;
}

export interface WorkspaceProps {
  checkout: ProjectCheckout;
  target?: string;
  simulation: SimulationSession | null;
  startSimulation: (scenario?: ScenarioId) => void;
  openWorkspace: (workspace: WorkspaceId, target?: string) => void;
  openActivity: (runId: string) => void;
  openChanges: (target: "saved" | "working") => void;
}

export const exampleCheckout: ProjectCheckout = {
  projectId: "example-project",
  repositoryRoot: "Example repository",
  checkoutId: "example-worktree",
  checkoutRoot: "Example repository",
};

export const scenarios = {
  delivery: { name: "Delivery", workspace: "knowledge", target: "docs/release.md", initialStep: "note" },
  recovery: { name: "Recovery", workspace: "tasks", target: "release", initialStep: "interrupted" },
  pipelines: { name: "Pipelines", workspace: "pipelines", target: undefined, initialStep: "plan" },
  windows: { name: "Windows", workspace: "diagnostics", target: "system", initialStep: "note" },
} satisfies Record<ScenarioId, { name: string; workspace: WorkspaceId; target?: string; initialStep: DeliveryStep }>;

const deliveryTransitions: Partial<Record<DeliveryStep, Partial<Record<DeliveryAction, DeliveryStep>>>> = {
  note: { start: "question" },
  question: { answer: "plan" },
  plan: { approve: "queued" },
  queued: { dispatch: "running" },
  running: { finish: "review", interrupt: "interrupted", fail: "failed" },
  review: { verify: "verified" },
  verified: { accept: "accepted", invalidate: "review" },
  accepted: { merge: "merged", invalidate: "review" },
  interrupted: { resume: "queued" },
  failed: { retry: "queued" },
};

export function nextDeliveryStep(step: DeliveryStep, action: DeliveryAction): DeliveryStep {
  return deliveryTransitions[step]?.[action] ?? step;
}
