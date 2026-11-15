import type { AdmissionBlocker } from "@/bindings/AdmissionBlocker";
import type { JobInspection } from "@/bindings/JobInspection";
import type { RunStatus } from "@/bindings/RunStatus";

export type JobState = "Queued" | "Starting" | "Running" | "Waiting for input" | "Waiting for approval" | "Paused" | "Interrupted" | "Completed" | "Failed" | "Cancelled";

export const queueViews = ["All", "Waiting", "Active", "Needs input", "History"] as const;
export type QueueView = typeof queueViews[number];

const runLabels: Record<RunStatus, JobState> = {
  starting: "Starting",
  running: "Running",
  waiting_for_input: "Waiting for input",
  succeeded: "Completed",
  failed: "Failed",
  cancelled: "Cancelled",
  interrupted: "Interrupted",
};

export function matchesQueueView(state: JobState, view: QueueView) {
  switch (view) {
    case "All": return true;
    case "Waiting": return state === "Queued" || state === "Paused";
    case "Active": return state === "Starting" || state === "Running" || state === "Waiting for input";
    case "Needs input": return state === "Waiting for input" || state === "Waiting for approval";
    case "History": return state === "Completed" || state === "Failed" || state === "Interrupted" || state === "Cancelled";
  }
}

export function queueJobState({ job, blockers }: JobInspection): JobState {
  if (job.latestAttempt) return runLabels[job.latestAttempt.status];
  if (job.queueState === "cancelled") return "Cancelled";
  if (job.queueState === "paused") return "Paused";
  if (blockers.some(({ type }) => type === "approval")) return "Waiting for approval";
  return "Queued";
}

export function queueBlockerReason(blocker: AdmissionBlocker): string {
  switch (blocker.type) {
    case "queue_paused": return "The job queue is paused.";
    case "job_paused": return "This job is paused.";
    case "job_cancelled": return "Job cancelled.";
    case "dependency": {
      const state = blocker.status ? runLabels[blocker.status] : blocker.queueState;
      return `Dependency ${blocker.jobId} is ${state.toLowerCase()}.`;
    }
    case "approval": return `Waiting for approval ${blocker.approvalId}.`;
    case "policy": return blocker.reason;
    case "context_changed": return `The conversation changed since context ${blocker.contextId} was queued.`;
    case "conversation_busy": return "Waiting for the active conversation run.";
    case "missing_capability": return `Required capability unavailable: ${blocker.capability.replaceAll("_", " ")}.`;
    case "writer": return "Waiting for the worktree writer.";
    case "capacity": return `Waiting for an execution slot (${blocker.active} of ${blocker.limit} in use).`;
  }
}
