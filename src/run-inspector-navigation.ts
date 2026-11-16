import { createContext } from "react";
import type { WorkspaceProps } from "@/workbench-contract";

export const RunInspectorNavigation = createContext<{
  openActivity: WorkspaceProps["openActivity"];
  activeRunId?: string;
  openGitAttempt?: (runId: string) => void;
} | undefined>(undefined);
