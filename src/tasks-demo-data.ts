import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { SimulationSession } from "@/workbench-contract";

export const taskStates = ["Backlog", "Ready", "In Progress", "In Review", "Blocked", "Completed", "Failed", "Cancelled"] as const;
export type TaskState = typeof taskStates[number];
export type TaskPriority = "Urgent" | "High" | "Normal" | "Low";
export type WorkKind = "Epic" | "Task" | "Sub-Task";
export type ChecklistMode = "Track progress" | "Run by item";
export type WorktreeStrategy = "isolated" | "shared";
export type ChecklistItem = { id: string; title: string; checked: boolean };

export interface DeliveryTask {
  id: string;
  kind?: WorkKind;
  parentId?: string;
  title: string;
  description: string;
  state: TaskState;
  priority: TaskPriority;
  assignment: ModelSelection | null;
  criteria: string[];
  dependencies: string[];
  checklist?: ChecklistItem[];
  checklistMode?: ChecklistMode;
  worktreeStrategy?: WorktreeStrategy;
  blocker: string;
}

export const deliveryTaskId = "release";
export const deliveryRunId = "demo-release-run";
export const deliveryNotePath = "docs/release.md";
export const deliveryCommit = "d73b42a";
export const deliveryCommitSha = "d73b42a91be3c8c63cf128379192f229061cca70e";
export const deliveryMergeSha = "13a63c87cbb034dbf6a6d4deda4f93facbe28daae";
export const deliveryBase = "1ba6dc2";
export const deliveryBranch = "agent/release-checklist";

export const deliveryModels: ModelOption[] = [
  { selection: { provider: "codex", profileId: "example-codex", modelId: "gpt-6.1-sol" }, name: "GPT-6.1 Sol", selectable: true, unavailableReason: null, rating: { intelligenceTenths: 738, apiReferenceCents: 400, priceScore: 65 }, catalogVersion: 1 },
  { selection: { provider: "claude", profileId: "example-claude", modelId: "claude-sonnet-5-5" }, name: "Claude Sonnet 5.5", selectable: true, unavailableReason: null, rating: { intelligenceTenths: 739, apiReferenceCents: 400, priceScore: 65 }, catalogVersion: 1 },
  { selection: { provider: "antigravity", profileId: "example-gemini", modelId: "gemini-4-argon" }, name: "Gemini 4 Argon", selectable: true, unavailableReason: null, rating: { intelligenceTenths: 796, apiReferenceCents: 400, priceScore: 65 }, catalogVersion: 1 },
];

export function modelName(assignment: ModelSelection | null) {
  return deliveryModels.find((option) => option.selection.modelId === assignment?.modelId && option.selection.profileId === assignment.profileId)?.name ?? "Unassigned";
}

export const releaseTask: DeliveryTask = {
  id: deliveryTaskId,
  parentId: "EPIC-01",
  title: "Add release readiness checklist",
  description: "Turn the release note into a repeatable checklist and verify it in an isolated worktree before accepting the diff.",
  state: "Backlog",
  priority: "High",
  assignment: deliveryModels[0]?.selection ?? null,
  criteria: ["Record the Windows smoke test requirements", "Include a rollback check", "Pass the isolated verification run"],
  dependencies: [],
  checklist: [
    { id: "release-read", title: "Read the release note", checked: true },
    { id: "release-review", title: "Review the candidate diff", checked: false },
  ],
  blocker: "",
};

export const exampleTasks: DeliveryTask[] = [
  { id: "EPIC-01", kind: "Epic", title: "Windows release readiness", description: "Prepare the checklist and validate the Windows installation.", state: "Backlog", priority: "Normal", assignment: null, criteria: [], dependencies: [], blocker: "" },
  { id: "OPS-24A", kind: "Sub-Task", parentId: deliveryTaskId, title: "Document the rollback exercise", description: "Record the expected recovery behavior for the next release.", state: "Ready", priority: "Normal", assignment: null, criteria: [], dependencies: [], blocker: "",
    checklist: [{ id: "rollback-snapshot", title: "List the backup and restore requirements", checked: false }] },
  { id: "OPS-16", title: "Write the upgrade troubleshooting guide", description: "Publish the supported recovery paths.", state: "Backlog", priority: "Low", assignment: null, criteria: ["Describe an interrupted update"], dependencies: [], blocker: "" },
  { id: "OPS-19", title: "Check the WebView2 prerequisite", description: "Confirm the dependency is detected before setup.", state: "Ready", priority: "Normal", assignment: deliveryModels[1]?.selection ?? null, criteria: ["Recognize an installed runtime"], dependencies: [], blocker: "" },
  { id: "OPS-20", title: "Rebuild the documentation index", description: "Reconcile indexed headings with the checkout.", state: "In Progress", priority: "Normal", assignment: deliveryModels[2]?.selection ?? null, criteria: [], dependencies: [], blocker: "" },
  { id: "OPS-17", title: "Review the installer smoke report", description: "Match the reported build against the verification run.", state: "In Review", priority: "High", assignment: deliveryModels[1]?.selection ?? null, criteria: [], dependencies: [], blocker: "" },
  { id: "OPS-18", title: "Test recovery on the guest runner", description: "Recover the interrupted checkout in a registered guest.", state: "Blocked", priority: "High", assignment: null, criteria: [], dependencies: ["Check the WebView2 prerequisite"], blocker: "Waiting for a qualified guest runner" },
  { id: "OPS-11", title: "Pin the build metadata", description: "Record the exact accepted build and commit.", state: "Completed", priority: "Normal", assignment: deliveryModels[0]?.selection ?? null, criteria: [], dependencies: [], blocker: "" },
  { id: "OPS-09", title: "Exercise the interrupted writer", description: "Capture partial changes after a stop.", state: "Failed", priority: "Normal", assignment: deliveryModels[2]?.selection ?? null, criteria: [], dependencies: [], blocker: "Writer disconnected before capture" },
  { id: "OPS-05", title: "Repeat the superseded fixture", description: "Retained for attempt history.", state: "Cancelled", priority: "Low", assignment: null, criteria: [], dependencies: [], blocker: "" },
];

export function workKind(work: DeliveryTask): WorkKind {
  return work.kind ?? "Task";
}

export function workCode(work: DeliveryTask): string {
  return work.id === deliveryTaskId ? "OPS-24" : work.id;
}

export function deliveryStatus(step: SimulationSession["delivery"], mergeRequired: boolean, committed: boolean): TaskState {
  switch (step) {
    case "note": return "Backlog";
    case "question":
    case "interrupted": return "Blocked";
    case "plan":
    case "queued": return "Ready";
    case "running": return "In Progress";
    case "review":
    case "verified": return "In Review";
    case "accepted": return !committed || mergeRequired ? "In Review" : "Completed";
    case "merged": return "Completed";
    case "failed": return "Failed";
  }
}

export function deliveryBlocker(step: SimulationSession["delivery"], mergeRequired: boolean, committed: boolean) {
  if (step === "question") return "Waiting for input";
  if (step === "interrupted") return "Writer interrupted. Review its checkpoint before resuming.";
  if (step === "failed") return "The smoke check failed. Inspect the attempt before retrying.";
  if (step === "accepted" && !committed) return "Waiting for commit";
  if (step === "accepted" && mergeRequired) return "Waiting for merge";
  return "";
}

export const deliveryStages = [
  { name: "Plan", role: "Planning agent", model: "Claude Sonnet 5.5", info: "Question and versioned proposal" },
  { name: "Review plan", role: "Cross-review", model: "Gemini 4 Argon", info: "Scope and verification criteria" },
  { name: "Approve implementation", role: "Human approval", model: "Operator", info: "Release is tied to the exact plan revision" },
  { name: "Implement", role: "Implementation agent", model: "GPT-6.1 Sol", info: "Isolated release worktree" },
  { name: "Verify", role: "Verification agent", model: "Claude Sonnet 5.5", info: "Smoke check and rollback case" },
  { name: "Accept changes", role: "Human review", model: "Operator", info: "Acceptance bound to the saved candidate" },
] as const;

export const deliveryStageIndex: Record<SimulationSession["delivery"], number> = {
  note: 0, question: 0, plan: 2, queued: 3, running: 3, review: 4,
  verified: 5, accepted: 6, merged: 6, interrupted: 3, failed: 3,
};
