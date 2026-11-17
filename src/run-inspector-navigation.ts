import { createContext } from "react";
import type { WorkspaceProps } from "@/workbench-contract";

export const RunInspectorNavigation = createContext<{
  openActivity: WorkspaceProps["openActivity"];
  openWorkspace?: WorkspaceProps["openWorkspace"];
  activeRunId?: string;
  openGitAttempt?: (runId: string) => void;
  openProviders?: () => void;
} | undefined>(undefined);
