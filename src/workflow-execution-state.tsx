import type { WorkflowExecutionState } from "@/workflow-execution-model";
import "./workflow-execution.css";

export const workflowExecutionStateLabel: Record<WorkflowExecutionState, string> = {
  queued: "Queued", running: "Running", waiting: "Waiting", skipped: "Skipped", completed: "Completed",
  failed: "Failed", interrupted: "Interrupted", cancelled: "Cancelled",
};

export function WorkflowExecutionStateBadge({ status }: { status: WorkflowExecutionState }) {
  return <span className="workflow-execution-state" data-status={status}>{workflowExecutionStateLabel[status]}</span>;
}
