import { GitCheckoutChanges, useGitCheckoutReview } from "@/git-checkout-changes";
import { openWorkspaceFile } from "@/file-context-actions";
import { GitDiffFrame } from "@/git-diff-view";
import { SavedGitDiffs } from "@/git-saved-diffs";
import { InlineIssue } from "@/tasks-git-ui";
import type { WorkspaceProps } from "@/workbench-contract";

const sourceLabels = { working: "Working tree", staged: "Staged" };

interface ChangesPanelProps extends WorkspaceProps {
  kind?: "saved" | "working";
  source?: "working" | "staged" | "saved";
  onClose?: () => void;
}

export default function ChangesPanel(props: ChangesPanelProps) {
  const { target, kind, source, checkout, onClose, openActivity } = props;
  const saved = kind === "saved" || target === "saved" || target?.startsWith("attempt:");
  const activeSource = source ?? (saved ? "saved" : "working");
  if (activeSource === "saved") return <SavedGitDiffs key={checkout.projectId}
    projectId={checkout.projectId} target={target} workspace={props} openActivity={openActivity} onClose={onClose} />;
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
