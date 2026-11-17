import { useState } from "react";
import { Button } from "@/components/ui/button";
import { GitCheckoutChanges, useGitCheckoutReview } from "@/git-checkout-changes";
import { openWorkspaceFile } from "@/file-context-actions";
import { branchChanges, savedCandidate, workingChanges, type ChangesSource } from "@/git-demo-data";
import { GitDiffFrame, GitDiffReview } from "@/git-diff-view";
import { SavedGitDiffs } from "@/git-saved-diffs";
import { useGitSimulation } from "@/git-simulation";
import { deliveryBase, deliveryBranch, deliveryCommit, deliveryRunId } from "@/tasks-demo-data";
import { DeliveryStatus, InlineIssue } from "@/tasks-git-ui";
import type { WorkspaceProps } from "@/workbench-contract";

const sourceLabels: Record<ChangesSource, string> = {
  working: "Working tree", staged: "Staged", branch: "Branch", saved: "Saved diff",
};
const availableCandidate = ["review", "verified", "accepted", "merged"];

interface ChangesPanelProps extends WorkspaceProps {
  kind?: "saved" | "working";
  source?: ChangesSource;
  baseRef?: string;
  onClose?: () => void;
}

export default function ChangesPanel(props: ChangesPanelProps) {
  const { simulation, target, kind, source, checkout, onClose, openActivity } = props;
  const saved = kind === "saved" || target === "saved" || target?.startsWith("attempt:");
  const activeSource = source ?? (saved ? "saved" : "working");
  if (simulation) return <SimulatedChanges key={activeSource} {...props} simulation={simulation} source={activeSource} />;
  if (activeSource === "saved") return <SavedGitDiffs key={checkout.projectId}
    projectId={checkout.projectId} target={target} openActivity={openActivity} onClose={onClose} />;
  return <LiveChanges key={checkout.checkoutId} {...props} source={activeSource === "staged" ? "staged" : "working"} />;
}

function LiveChanges({ checkout, source, onClose, openWorkspace }: ChangesPanelProps & { source: "working" | "staged" }) {
  const review = useGitCheckoutReview(checkout.checkoutId);
  if (review.data && !review.error) return <GitCheckoutChanges review={review.data} onClose={onClose} initialSource={source}
    openConversation={(id) => openWorkspace("conversations", id)}
    openFile={(path, line) => openWorkspaceFile(openWorkspace, path, line)} />;
  return <GitDiffFrame source={source} title={sourceLabels[source]} onClose={onClose}>
    <div className="p-3">
      {review.error ? <InlineIssue>{String(review.error)}</InlineIssue>
        : <p className="text-sm text-muted-foreground" role="status">Loading Git review…</p>}
    </div>
  </GitDiffFrame>;
}

function SimulatedChanges({ simulation, source, baseRef = "origin/main", onClose, openActivity, openWorkspace }: ChangesPanelProps & {
  simulation: NonNullable<WorkspaceProps["simulation"]>;
  source: ChangesSource;
}) {
  const git = useGitSimulation();
  const stateBranch = git.worktrees.find((tree) => tree.id === git.checkout)?.branch ?? deliveryBranch;
  const liveFiles = git.checkout === "release" ? workingChanges.filter((file) => !git.committed.has(file.path)) : [];
  const files = source === "saved" ? (availableCandidate.includes(simulation.delivery) ? savedCandidate : [])
    : source === "staged" ? liveFiles.filter((file) => git.staged.has(file.path))
    : source === "branch" ? (git.checkout === "release" ? branchChanges : [])
    : liveFiles;
  const [selection, setSelection] = useState<string>();
  const selected = selection === "" ? undefined : files.find((file) => file.path === selection) ?? files[0];
  const ranges = {
    working: { base: "HEAD", revised: `Worktree · ${stateBranch}` },
    staged: { base: "HEAD", revised: "Index" },
    branch: { base: `Merge base of ${baseRef}`, revised: `HEAD · ${stateBranch}` },
    saved: { base: deliveryBase, revised: deliveryCommit },
  }[source];
  return <GitDiffReview source={source} title={sourceLabels[source]} ranges={ranges} files={files}
    selectedPath={selected?.path} select={setSelection} comparison={selected?.kind !== "Binary" ? selected : undefined}
    onClose={onClose} allowSavedPatch={source === "saved"}
    openFile={selected && selected.kind !== "Binary" && source !== "saved"
      ? () => openWorkspace(selected.path.endsWith(".md") ? "knowledge" : "ide", selected.path) : undefined}
    notice={git.drift && <div className="border-b border-border px-3 py-2"><InlineIssue>
      {git.drift === "conflict" ? "Merge conflict. Resolve it in Git and run verification again." : "Destination changed. Recheck this version before committing further changes."}
    </InlineIssue></div>}
    empty={<>
      <h3>{source === "saved" ? "No saved candidate" : "No changes"}</h3>
      <p>{source === "saved" ? "Capture an attempt to review its exact revision." : "Stage changes from the Git workspace to compare them here."}</p>
    </>}
    footer={source === "saved" && <>
      <Button className="px-2.5 py-1.5 text-xs" type="button" variant="outline" onClick={() => openActivity(deliveryRunId)}>View attempt</Button>
      {simulation.delivery === "review" && <Button className="px-2.5 py-1.5 text-xs" type="button" disabled={Boolean(git.drift)} onClick={() => simulation.advanceDelivery("verify")}>Run verification</Button>}
      {simulation.delivery === "verified" && <Button className="px-2.5 py-1.5 text-xs" type="button" disabled={Boolean(git.drift)} onClick={() => git.acceptCandidate(simulation)}>Accept diff</Button>}
      {simulation.delivery === "accepted" && <DeliveryStatus>Accepted</DeliveryStatus>}
      {simulation.delivery === "merged" && <DeliveryStatus>Merged</DeliveryStatus>}
    </>} />;
}
