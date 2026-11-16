import { createContext, useContext, useState, type ReactNode } from "react";
import { revisionMessage, savedCandidate } from "@/git-demo-data";
import { defaultDeliveryGitPolicy, type DeliveryGitPolicy } from "@/delivery-git-policy";
import type { SimulationSession } from "@/workbench-contract";

export const exampleWorktrees = [
  { id: "release", branch: "agent/release-checklist", path: "worktrees/release-checklist", base: "1ba6dc2" },
  { id: "main", branch: "main", path: "repository", base: "1ba6dc2" },
] as const;

export type WorktreeId = string;
export interface GitWorktree {
  id: WorktreeId;
  branch: string;
  path: string;
  base: string;
}
interface WorktreeChanges {
  staged: ReadonlySet<string>;
  committed: ReadonlySet<string>;
  commitMessage: string;
}

const emptyChanges: WorktreeChanges = { staged: new Set(), committed: new Set(), commitMessage: "" };

interface GitSession {
  epoch: number | undefined;
  checkout: WorktreeId;
  trees: Partial<Record<WorktreeId, WorktreeChanges>>;
  worktrees: readonly GitWorktree[];
  drift: false | "advanced" | "conflict";
  gitMode: DeliveryGitPolicy;
  mergeRequired: boolean;
  candidateCommitted: boolean;
}

function initialGitSession(epoch: number | undefined): GitSession {
  return { epoch, checkout: "release", trees: {}, worktrees: exampleWorktrees, drift: false, gitMode: defaultDeliveryGitPolicy, mergeRequired: false, candidateCommitted: false };
}

function useGitState(epoch?: number) {
  const [saved, setSession] = useState(() => initialGitSession(epoch));
  let session = saved;
  if (saved.epoch !== epoch) {
    session = initialGitSession(epoch);
    setSession(session);
  }

  const { checkout, trees, worktrees, drift, gitMode, mergeRequired } = session;

  function toggleStage(path: string) {
    setSession((current) => {
      const tree = current.trees[checkout] ?? emptyChanges;
      if (tree.committed.has(path)) return current;
      const staged = new Set(tree.staged);
      if (staged.has(path)) staged.delete(path);
      else staged.add(path);
      return { ...current, trees: { ...current.trees, [checkout]: { ...tree, staged } } };
    });
  }

  function commit(paths: Iterable<string>, message: string, worktree: WorktreeId, candidate: boolean) {
    const captured = new Set(paths);
    if (!captured.size || !message.trim()) return;
    setSession((current) => {
      const tree = current.trees[worktree] ?? emptyChanges;
      return { ...current, candidateCommitted: current.candidateCommitted || candidate, trees: { ...current.trees, [worktree]: {
        committed: new Set([...tree.committed, ...captured]),
        staged: new Set([...tree.staged].filter((path) => !captured.has(path))),
        commitMessage: message.trim(),
      } } };
    });
  }

  function addWorktree(branch: string, path: string, base: string) {
    const id = `local-${branch}`;
    setSession((current) => current.worktrees.some((tree) => tree.branch === branch || tree.path === path)
      ? current
      : { ...current, checkout: id, worktrees: [...current.worktrees, { id, branch, path, base }] });
  }

  function commitCandidate() {
    commit(savedCandidate.map(({ path }) => path), revisionMessage, "release", true);
  }

  function acceptCandidate(simulation: SimulationSession | null) {
    if (simulation?.delivery !== "verified" || drift) return;
    simulation.advanceDelivery("accept");
    if (gitMode === "user_commits_and_merges") return;
    commitCandidate();
    if (gitMode === "automatic_local_commit_and_merge") simulation.advanceDelivery("merge");
  }

  return {
    checkout, worktrees, addWorktree,
    staged: trees[checkout]?.staged ?? emptyChanges.staged,
    committed: trees[checkout]?.committed ?? emptyChanges.committed,
    commitMessage: trees[checkout]?.commitMessage ?? emptyChanges.commitMessage,
    candidateCommitted: session.candidateCommitted,
    drift, gitMode, mergeRequired, toggleStage,
    commit: (paths: Iterable<string>, message: string) => commit(paths, message, checkout, false),
    commitCandidate, acceptCandidate,
    setCheckout: (value: WorktreeId) => setSession((current) => ({ ...current, checkout: value })),
    setDrift: (value: GitSession["drift"]) => setSession((current) => ({
      ...current, drift: value, candidateCommitted: value ? false : current.candidateCommitted,
    })),
    setGitMode: (value: DeliveryGitPolicy) => setSession((current) => ({ ...current, gitMode: value })),
    setMergeRequired: (value: boolean) => setSession((current) => ({ ...current, mergeRequired: value })),
  };
}

type GitState = ReturnType<typeof useGitState>;
const GitSimulationContext = createContext<GitState | null>(null);

export function GitSimulationProvider({ children, epoch }: { children: ReactNode; epoch?: number }) {
  const value = useGitState(epoch);
  return <GitSimulationContext.Provider value={value}>{children}</GitSimulationContext.Provider>;
}

export function useGitSimulation() {
  const fallback = useGitState();
  return useContext(GitSimulationContext) ?? fallback;
}
