import { createContext, useContext, useState, type ReactNode } from "react";
import { savedCandidate } from "@/git-demo-data";

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
type GitMode = "manual" | "commit" | "merge";

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
  gitMode: GitMode;
}

function initialGitSession(epoch: number | undefined): GitSession {
  return { epoch, checkout: "release", trees: {}, worktrees: exampleWorktrees, drift: false, gitMode: "manual" };
}

function useGitState(epoch?: number) {
  const [saved, setSession] = useState(() => initialGitSession(epoch));
  let session = saved;
  if (saved.epoch !== epoch) {
    session = initialGitSession(epoch);
    setSession(session);
  }

  const { checkout, trees, worktrees, drift, gitMode } = session;

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

  function commit(paths: Iterable<string>, message: string, worktree: WorktreeId = checkout) {
    const captured = new Set(paths);
    if (!captured.size || !message.trim()) return;
    setSession((current) => {
      const tree = current.trees[worktree] ?? emptyChanges;
      return { ...current, trees: { ...current.trees, [worktree]: {
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

  return {
    checkout, worktrees, addWorktree,
    staged: trees[checkout]?.staged ?? emptyChanges.staged,
    committed: trees[checkout]?.committed ?? emptyChanges.committed,
    commitMessage: trees[checkout]?.commitMessage ?? emptyChanges.commitMessage,
    candidateCommitted: savedCandidateIsCommitted(trees.release),
    drift, gitMode, toggleStage, commit,
    setCheckout: (value: WorktreeId) => setSession((current) => ({ ...current, checkout: value })),
    setDrift: (value: GitSession["drift"]) => setSession((current) => ({ ...current, drift: value })),
    setGitMode: (value: GitMode) => setSession((current) => ({ ...current, gitMode: value })),
  };
}

function savedCandidateIsCommitted(tree: WorktreeChanges | undefined) {
  return savedCandidate.every(({ path }) => tree?.committed.has(path));
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
