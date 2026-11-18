import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { RequestId } from "@/conversation-inputs";

export type WorkspaceId = "conversations" | "ide" | "tasks" | "pipelines" | "knowledge" | "git" | "diagnostics";
export type WorkspaceDestination = { workspace: WorkspaceId; target?: string };
export type QuestionFocus = { requestId: RequestId };

export interface WorkspaceProps {
  checkout: ProjectCheckout;
  target?: string;
  openWorkspace: (workspace: WorkspaceId, target?: string) => void;
  openCheckoutWorkspace?: (checkout: ProjectCheckout, destination: WorkspaceDestination) => Promise<void>;
  openActivity: (runId: string, conversationId?: string, requestId?: RequestId) => void;
  openChanges: (target: "saved" | "working") => void;
  openQueueJob?: (jobId: string) => void;
}

export type QueueJobFocus = { projectId: string; jobId: string };

export const exampleCheckout: ProjectCheckout = {
  projectId: "example-project",
  repositoryRoot: "Example repository",
  checkoutId: "example-worktree",
  checkoutRoot: "Example repository",
};
