import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";

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

export function workKind(work: DeliveryTask): WorkKind {
  return work.kind ?? "Task";
}

export function workCode(work: DeliveryTask): string {
  return work.id === deliveryTaskId ? "OPS-24" : work.id;
}
