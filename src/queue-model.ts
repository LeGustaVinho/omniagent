import type { AdmissionBlocker } from "@/bindings/AdmissionBlocker";
import type { JobInspection } from "@/bindings/JobInspection";
import type { ReleaseBlocker } from "@/bindings/ReleaseBlocker";
import type { RunStatus } from "@/bindings/RunStatus";
import type { RequiredCustomization } from "@/bindings/RequiredCustomization";

export type JobState = "Queued" | "Starting" | "Running" | "Yielding" | "Waiting for jobs" | "Needs review" | "Waiting for input" | "Waiting for approval" | "Paused" | "Interrupted" | "Completed" | "Failed" | "Cancelled";

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
    case "Waiting": return state === "Queued" || state === "Paused" || state === "Waiting for jobs";
    case "Active": return state === "Starting" || state === "Running" || state === "Waiting for input" || state === "Yielding";
    case "Needs input": return state === "Waiting for input" || state === "Waiting for approval" || state === "Needs review";
    case "History": return state === "Completed" || state === "Failed" || state === "Interrupted" || state === "Cancelled";
  }
}

export function queueJobState({ job, blockers, questionWait, queuedQuestion }: JobInspection): JobState {
  if (job.queueState === "cancelled") return "Cancelled";
  if (questionWait && (!queuedQuestion || questionWait.needsReview > 0)) {
    return questionWait.count === questionWait.needsReview ? "Needs review" : "Waiting for input";
  }
  if (job.latestAttempt && !job.queuedRecovery && !job.downstreamWait && !queuedQuestion) return runLabels[job.latestAttempt.status];
  if (job.queueState === "paused") return "Paused";
  if (blockers.some(({ type }) => type === "approval")) return "Waiting for approval";
  if (job.downstreamWait?.state === "yielding") return "Yielding";
  if (job.downstreamWait?.state === "needs_review") return "Needs review";
  if (job.downstreamWait?.state === "waiting") return "Waiting for jobs";
  return "Queued";
}

export function customizationLabel(requirement: RequiredCustomization) {
  return requirement.type === "instruction" ? requirement.path : `${requirement.type.replaceAll("_", " ")} ${requirement.id}`;
}

const releaseReasons: Record<ReleaseBlocker, string> = {
  missing: "This pipeline write needs an implementation release.",
  stale_plan: "The implementation release does not cover the current plan revision.",
  wrong_scope: "The implementation release does not cover this task and worktree.",
  revoked: "The implementation release was revoked.",
};

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
    case "instruction_order": return "Waiting for an earlier instruction.";
    case "continuation_owner": return "Waiting for the owning run to continue.";
    case "instruction_delivery_uncertain": return "An instruction in this context needs delivery review.";
    case "downstream":
      if (blocker.needsReview) return "The parent attempt needs review before it can continue.";
      return blocker.pending > 0 ? `Waiting for ${blocker.pending} downstream job results.` : "Waiting for the parent attempt to settle.";
    case "missing_capability": return `Required capability unavailable: ${blocker.capability.replaceAll("_", " ")}.`;
    case "pipeline_release": return releaseReasons[blocker.reason];
    case "dispatch_rejected": {
      const rejection = blocker.rejection;
      switch (rejection.type) {
        case "policy_changed": return "The project rule changed before dispatch. Review the current rule before retrying.";
        case "goal": return rejection.reason;
        case "pipeline_release": return releaseReasons[rejection.reason];
        case "profile_unavailable": return "The selected CLI profile changed or could not be verified.";
        case "worktree_unavailable": return "The worktree identity could not be verified.";
        case "native_delegation_unverified": return "The CLI did not confirm its reserved native subagent limit.";
        case "native_goal_unverified": return "The selected CLI profile no longer matches its native goal qualification.";
        case "subagent_audit_unverified": return "The effective CLI profile changed after its native subagent audit was verified.";
        case "requirements": {
          const missing = [
            ...rejection.capabilities.map((capability) => capability.replaceAll("_", " ")),
            ...rejection.customizations.map(customizationLabel),
          ];
          return `Could not verify required dispatch evidence: ${missing.join(", ")}.`;
        }
      }
    }
    case "writer": return blocker.needsReview ? "The previous worktree writer needs review." : "Waiting for the worktree writer.";
    case "app_writer": return "Waiting for an app write to finish.";
    case "pending_worktree_changes": return `The worktree has pending changes for task scope ${blocker.scopeTaskId}.`;
    case "capacity": return `Waiting for an execution slot (${blocker.active} of ${blocker.limit} in use).`;
    case "subagent_quota": return `Conversation subagent limit reached (${blocker.active} of ${blocker.limit} in use).`;
    case "subagent_audit": return `Complete subagent audit requires verified evidence for ${blocker.missing.map((field) => field.replaceAll("_", " ")).join(", ")} or verified native delegation disablement.`;
  }
}
