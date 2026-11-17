import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { GitCommitHorizontal, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { GitCheckoutChanges, useGitCheckoutReview, type CheckoutReview, type Worktree } from "@/git-checkout-changes";
import { SavedGitDiffs } from "@/git-saved-diffs";
import { GitWorkspaceFrame, GitWorkspaceTable } from "@/git-workspace-frame";
import { DeliveryStatus, InlineIssue } from "@/tasks-git-ui";
import type { WorkspaceProps } from "@/workbench-contract";

type HistoryEntry = { sha: string; author: string; committedAt: string; subject: string };

const tabs = ["Changes", "Worktrees", "History", "Saved attempts"] as const;
type Tab = typeof tabs[number];
const tabFor = (target?: string): Tab => target === "worktrees" ? "Worktrees" : target === "history" ? "History"
  : target === "saved" || target?.startsWith("attempt:") ? "Saved attempts" : "Changes";

export function LiveGitWorkspace(props: WorkspaceProps) {
  return <GitCheckoutWorkspace key={`${props.checkout.projectId}:${props.checkout.checkoutId}`} {...props} />;
}

function GitCheckoutWorkspace(props: WorkspaceProps) {
  const [checkoutId, setCheckout] = useState(props.checkout.checkoutId);
  const [tab, setTab] = useState<Tab>(() => tabFor(props.target));
  useEffect(() => { setTab(tabFor(props.target)); }, [props.target]);
  const review = useGitCheckoutReview(checkoutId, tab !== "Saved attempts");
  const trees = review.data?.worktrees.filter((tree): tree is Worktree & { checkoutId: string } => tree.checkoutId !== null) ?? [];
  const branch = review.data?.revision.branch?.replace(/^refs\/heads\//, "");
  const activeCheckout = checkoutId === props.checkout.checkoutId;

  return <GitWorkspaceFrame tabs={tabs} active={tab} select={setTab} controls={tab !== "Saved attempts" && <>
    <label className="ml-auto min-w-0"><span className="sr-only">Git worktree</span>
      <select className="delivery-filter w-full max-w-60" value={checkoutId} onChange={(event) => setCheckout(event.currentTarget.value)}>
        {!trees.some(({ checkoutId: id }) => id === checkoutId) && <option value={checkoutId}>{branch ?? (activeCheckout ? props.checkout.checkoutRoot : checkoutId)}</option>}
        {trees.map((tree) => <option key={tree.checkoutId} value={tree.checkoutId}>{tree.branch ?? "Detached HEAD"} · {tree.root}</option>)}
      </select>
    </label>
    <Button type="button" variant="ghost" aria-label="Refresh Git review" title="Refresh Git review" onClick={() => void review.refetch()}><RefreshCw size={15} aria-hidden /></Button>
  </>}>
    {tab === "Saved attempts" ? <SavedGitDiffs projectId={props.checkout.projectId} target={props.target} openActivity={props.openActivity} /> : <>
      {review.isPending && <p role="status" className="p-4 text-sm text-muted-foreground">Loading Git review…</p>}
      {review.error && <div className="p-4"><InlineIssue>{String(review.error)}</InlineIssue></div>}
      {review.data && !review.error && <>
        {tab === "Changes" && <GitCheckoutChanges key={checkoutId} review={review.data} indexActions
          openFile={activeCheckout ? (path) => props.openWorkspace(/\.(md|markdown)$/i.test(path) ? "knowledge" : "ide", path) : undefined}
          openPanel={activeCheckout ? () => props.openChanges("working") : undefined} />}
        {tab === "Worktrees" && <WorktreeTable review={review.data} select={setCheckout} />}
        {tab === "History" && <CommitHistory key={checkoutId} review={review.data} />}
      </>}
    </>}
  </GitWorkspaceFrame>;
}

function WorktreeTable({ review, select }: { review: CheckoutReview; select: (id: string) => void }) {
  return <div className="delivery-main p-4">
    <h3 className="mb-3 text-sm font-semibold">Repository worktrees</h3>
    <GitWorkspaceTable label="Git worktrees" columns={["Worktree", "Branch", "HEAD", "Scope", "Selection"]}>
      {review.worktrees.map((tree) => {
        const selected = tree.checkoutId === review.checkoutId;
        return <tr key={tree.root} className={selected ? "bg-muted" : undefined}>
          <td className="wrap-anywhere font-mono">{tree.root}</td>
          <td>{tree.branch ?? "Detached HEAD"}</td>
          <td className="wrap-anywhere font-mono">{tree.head ?? "Unborn"}</td>
          <td><DeliveryStatus>{tree.checkoutId ? tree.managed ? "Managed" : "Registered" : "Unregistered"}</DeliveryStatus></td>
          <td><Button type="button" variant="outline" className="whitespace-nowrap px-2.5 py-1.5 text-xs" disabled={!tree.checkoutId || selected}
            onClick={() => { if (tree.checkoutId) select(tree.checkoutId); }}>{selected ? "Selected" : "Use worktree"}</Button></td>
        </tr>;
      })}
    </GitWorkspaceTable>
  </div>;
}

function CommitHistory({ review }: { review: CheckoutReview }) {
  const [branch, setBranch] = useState("");
  const [selection, select] = useState<string>();
  const branchRef = review.branches.find((entry) => entry.reference === branch);
  const history = useQuery({
    queryKey: ["git-history", review.checkoutId, review.revision.head, branch, branchRef?.sha],
    queryFn: () => invoke<HistoryEntry[]>("git_checkout_history", { checkoutId: review.checkoutId, branch: branch || null }), retry: false,
  });
  const commits = history.error ? [] : history.data ?? [];
  const selected = commits.find((commit) => commit.sha === selection) ?? commits[0];
  return <div className="delivery-body">
    <div className="delivery-main p-4">
      <label className="delivery-field mb-3 max-w-72">History branch
        <select className="delivery-filter" value={branch} onChange={(event) => { setBranch(event.currentTarget.value); select(undefined); }}>
          <option value="">Current checkout HEAD</option>
          {review.branches.map((entry) => <option key={entry.reference} value={entry.reference}>{entry.name}</option>)}
        </select>
      </label>
      {history.isPending && <p role="status" className="text-sm">Loading commits…</p>}
      {history.error && <InlineIssue>{String(history.error)}</InlineIssue>}
      {history.data?.length === 0 && <p className="text-sm text-muted-foreground">No commits in this branch.</p>}
      {commits.length > 0 && <GitWorkspaceTable label="Repository commit history" columns={["Commit", "Author", "Recorded"]}>
        {commits.map((commit) => <tr key={commit.sha}>
          <td><button type="button" onClick={() => select(commit.sha)} aria-pressed={selected?.sha === commit.sha} className="grid gap-1 text-left font-medium hover:underline">
            {commit.subject || "Commit"}<code className="text-xs text-muted-foreground">{commit.sha.slice(0, 12)}</code>
          </button></td>
          <td>{commit.author}</td>
          <td className="whitespace-nowrap text-muted-foreground"><time dateTime={commit.committedAt}>{commit.committedAt}</time></td>
        </tr>)}
      </GitWorkspaceTable>}
    </div>
    {selected && <aside className="delivery-detail" aria-label="Selected commit">
      <div className="delivery-detail-content grid content-start gap-3 p-3.5">
        <h3 className="flex items-center gap-2 text-sm font-semibold"><GitCommitHorizontal size={17} aria-hidden /> Selected commit</h3>
        <dl className="grid gap-2 text-xs">
          <div><dt className="text-muted-foreground">Revision</dt><dd className="wrap-anywhere font-mono">{selected.sha}</dd></div>
          <div><dt className="text-muted-foreground">History ref</dt><dd className="wrap-anywhere font-mono">{branchRef?.reference ?? review.revision.branch ?? "Checkout HEAD"}</dd></div>
        </dl>
      </div>
    </aside>}
  </div>;
}
