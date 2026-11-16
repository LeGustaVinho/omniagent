import { queryOptions } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { RequestId } from "@/conversation-inputs";
import type { VerifiedMerge } from "@/delivery-merge-model";

export const workflowStates = [
  { value: "backlog", label: "Backlog" }, { value: "ready", label: "Ready" },
  { value: "in_progress", label: "In Progress" }, { value: "in_review", label: "In Review" },
  { value: "blocked", label: "Blocked" }, { value: "completed", label: "Completed" },
  { value: "failed", label: "Failed" }, { value: "cancelled", label: "Cancelled" },
] as const;

export type WorkflowStatus = typeof workflowStates[number]["value"];
export type WorkKind = "epic" | "task" | "subtask";
export type Work = { id: string; title: string };
export type Hierarchy = {
  epics: Work[];
  work: { task: Work; placement: { kind: "task"; epicId: string | null } | { kind: "subtask"; parentTaskId: string } }[];
};
export type Item = { id: string; revision: number; position: number; body: string; checked: boolean };
export type TaskQuestionBlocker = {
  runId: string; conversationId: string; requestId: RequestId;
  state: "live" | "saving" | "saved" | "needs_review"; answerRecorded: boolean;
};
export type WorkflowState = {
  state: WorkflowStatus;
  hasDirectWork: boolean;
  waitingForCommit: boolean;
  waitingForMerge?: boolean;
  pendingMerge?: { workflowRunId: string; commitEvidenceIds: string[]; sourceBranch: string; sourceSha: string; destinationBranch: string } | null;
  checklistResolved: boolean;
  pendingRepositoryChanges: boolean;
  activeWorkflowRunId: string | null;
  questionBlocker: TaskQuestionBlocker | null;
  prerequisiteBlockers?: { taskId: string; title: string }[];
  completion?: Delivery | null;
};
export type Delivery = { id: string; recordedAt: string; merge?: VerifiedMerge | null; evidence: { kind: "manual"; note: string | null } |
  { kind: "verified_commit"; completionSha: string; workflowRunId: string; unitEvidenceIds: string[] } };
export type VerifiedCommit = {
  id: string; projectId: string; taskId: string; unitId: string; branch: string; sha: string;
  captureRunId: string; purpose: string; verifiedAt: string; candidateRevision: string; tree: string;
  acceptanceId: string; acceptanceArtifactId?: string; verificationId: string; pendingChangesRetained: boolean;
};
export type DeliveryAttempt = { runId: string; conversationId: string };
export type VerifiedDeliveryUnitHistory = {
  commit: VerifiedCommit; workflowRunId: string; planRevision: string; approvalArtifactId: string | null;
  implementation: DeliveryAttempt; review: DeliveryAttempt | null;
};
export type EvidenceTarget = { taskId: string; evidenceId: string };
export type AgileRow = Work & {
  kind: WorkKind;
  parentId?: string;
  status?: WorkflowStatus;
  workflow?: WorkflowState | null;
  checklist: Item[];
  commit?: VerifiedCommit | null;
  evidence?: EvidenceTarget;
  deliveries?: Delivery[];
  hasChildren: boolean;
};

export const workLevels = [
  { value: "task", label: "Task" }, { value: "epic", label: "Epic" }, { value: "subtask", label: "Sub-Task" },
] as const;

export const statusLabel = (status: WorkflowStatus) => workflowStates.find(({ value }) => value === status)?.label ?? status;
export const workLabel = (kind: WorkKind) => workLevels.find(({ value }) => value === kind)?.label ?? kind;
export const terminalStatus = (status: WorkflowStatus) => status === "completed" || status === "cancelled";

export function questionBlockerLabel(question: TaskQuestionBlocker) {
  if (question.state === "needs_review") return "Question needs review";
  if (question.answerRecorded) return "Answer recorded · Waiting to continue";
  if (question.state === "saving") return "Saving the turn checkpoint…";
  return "Waiting for input";
}

export function canComplete(workflow: WorkflowState, hasChildren: boolean) {
  return !terminalStatus(workflow.state) && !workflow.activeWorkflowRunId && !workflow.pendingRepositoryChanges &&
    !workflow.waitingForMerge && !workflow.questionBlocker && !workflow.prerequisiteBlockers?.length && workflow.checklistResolved &&
    (!hasChildren || workflow.hasDirectWork);
}

export function canMove(row: AgileRow, next: WorkflowStatus) {
  const workflow = row.workflow;
  if (!workflow || workflow.state === next) return false;
  if (terminalStatus(workflow.state)) return next === "ready" && (!row.hasChildren || workflow.hasDirectWork);
  if (next === "cancelled") return true;
  if (workflow.questionBlocker || workflow.prerequisiteBlockers?.length || workflow.waitingForCommit || workflow.waitingForMerge) return false;
  return next !== "completed" || canComplete(workflow, row.hasChildren);
}

export function taskStateQuery(projectId: string, taskId: string) {
  return queryOptions({ queryKey: ["project-tasks", projectId, taskId, "state"],
    queryFn: () => invoke<WorkflowState | null>("project_task_delivery_state", { projectId, taskId }), refetchInterval: 2000 });
}

export function epicStateQuery(projectId: string, epicId: string) {
  return queryOptions({ queryKey: ["project-tasks", projectId, epicId, "state"],
    queryFn: () => invoke<WorkflowStatus | null>("project_epic_delivery_state", { projectId, epicId }), refetchInterval: 2000 });
}

export function checklistQuery(projectId: string, taskId: string) {
  return queryOptions({ queryKey: ["project-tasks", projectId, taskId, "checklist"],
    queryFn: () => invoke<{ items: Item[] } | null>("project_task_checklist", { projectId, taskId }), refetchInterval: 2000 });
}

export function deliveryQuery(projectId: string, taskId: string) {
  return queryOptions({ queryKey: ["project-tasks", projectId, taskId, "deliveries"],
    queryFn: () => invoke<Delivery[]>("project_task_deliveries", { projectId, taskId }), refetchInterval: 2500 });
}

export function commitQuery(projectId: string, taskId: string, evidenceId: string) {
  return queryOptions({ queryKey: ["delivery-verified-commit", projectId, taskId, evidenceId],
    queryFn: () => invoke<VerifiedCommit | null>("verified_delivery_commit", { projectId, taskId, evidenceId }), staleTime: Infinity });
}

export function workDeliveryUnitsQuery(projectId: string, taskId: string) {
  return queryOptions({ queryKey: ["project-tasks", projectId, taskId, "verified-units"],
    queryFn: () => invoke<VerifiedDeliveryUnitHistory[]>("verified_work_delivery_units", { projectId, taskId }), refetchInterval: 2500 });
}

export function latestEvidence(taskId: string, deliveries: readonly Delivery[], completion?: Delivery | null): EvidenceTarget | undefined {
  const delivery = completion?.evidence.kind === "verified_commit" ? completion :
    deliveries.findLast(({ evidence }) => evidence.kind === "verified_commit");
  if (delivery?.evidence.kind !== "verified_commit") return;
  const evidenceId = delivery.evidence.unitEvidenceIds.at(-1);
  return evidenceId ? { taskId, evidenceId } : undefined;
}
