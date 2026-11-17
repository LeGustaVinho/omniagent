import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { GitCommitHorizontal, Plus, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { ContextActions } from "@/components/ui/context-actions";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { GitCheckoutChanges, useGitCheckoutReview, type CheckoutReview, type Worktree } from "@/git-checkout-changes";
import { GitFileHistoryView } from "@/git-file-history";
import { SavedGitDiffs } from "@/git-saved-diffs";
import { GitWorkspaceFrame, GitWorkspaceTable } from "@/git-workspace-frame";
import { GitWorktreeDialog, type WorktreeDraft } from "@/git-worktree-dialog";
import { DeliveryDetailPanel, DeliveryStatus, InlineIssue } from "@/tasks-git-ui";
import { fileContextActions, isGitCheckoutTarget, isNewWorktreeTarget, openWorkspaceFile, readFileHistoryTarget, readWorkingDiffTarget } from "@/file-context-actions";
import type { WorkspaceProps } from "@/workbench-contract";

type HistoryEntry = { sha: string; author: string; committedAt: string; subject: string };

const tabs = ["Changes", "Worktrees", "History", "Saved attempts"] as const;
type Tab = typeof tabs[number];
const tabFor = (target?: string): Tab => target === "worktrees" || isNewWorktreeTarget(target) ? "Worktrees" : target === "history" || readFileHistoryTarget(target) ? "History"
  : target === "saved" || target?.startsWith("attempt:") ? "Saved attempts" : "Changes";

export function LiveGitWorkspace(props: WorkspaceProps) {
  return <GitCheckoutWorkspace key={`${props.checkout.projectId}:${props.checkout.checkoutId}`} {...props} />;
}

function GitCheckoutWorkspace(props: WorkspaceProps) {
  const [checkoutId, setCheckout] = useState(props.checkout.checkoutId);
  const [requestedFile, setRequestedFile] = useState(() => readWorkingDiffTarget(props.target));
  const [historyPath, setHistoryPath] = useState(() => readFileHistoryTarget(props.target));
  const [tab, setTab] = useState<Tab>(() => tabFor(props.target));
  const [creating, setCreating] = useState(() => isNewWorktreeTarget(props.target));
  useEffect(() => {
    setTab(tabFor(props.target));
    const file = readWorkingDiffTarget(props.target);
    const history = readFileHistoryTarget(props.target);
    setRequestedFile(file);
    setHistoryPath(history);
    const create = isNewWorktreeTarget(props.target);
    if (file !== undefined || history !== undefined || create || isGitCheckoutTarget(props.target)) setCheckout(props.checkout.checkoutId);
    if (create) setCreating(true);
  }, [props.checkout.checkoutId, props.target]);
  const review = useGitCheckoutReview(checkoutId, tab !== "Saved attempts");
  const trees = review.data?.worktrees.filter((tree): tree is Worktree & { checkoutId: string } => tree.checkoutId !== null) ?? [];
  const branch = review.data?.revision.branch?.replace(/^refs\/heads\//, "");
  const activeCheckout = checkoutId === props.checkout.checkoutId;

  return <GitWorkspaceFrame tabs={tabs} active={tab} select={setTab} controls={tab !== "Saved attempts" && <>
    <label className="ml-auto flex min-w-0 items-center gap-2"><span className="text-xs text-muted-foreground">Inspect worktree</span>
      <select aria-label="Git worktree" className="delivery-filter w-full max-w-60" value={checkoutId} onChange={(event) => {
        setRequestedFile(undefined);
        setCheckout(event.currentTarget.value);
      }}>
        {!trees.some(({ checkoutId: id }) => id === checkoutId) && <option value={checkoutId}>{branch ?? (activeCheckout ? props.checkout.checkoutRoot : checkoutId)}</option>}
        {trees.map((tree) => <option key={tree.checkoutId} value={tree.checkoutId}>{tree.branch ?? "Detached HEAD"} · {tree.root}</option>)}
      </select>
    </label>
    {tab === "Worktrees" && <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" disabled={!review.data || Boolean(review.error)}
      onClick={() => setCreating(true)}><Plus size={14} /> New worktree</Button>}
    <Button type="button" variant="ghost" aria-label="Refresh Git review" title="Refresh Git review" onClick={() => void review.refetch()}><RefreshCw size={15} aria-hidden /></Button>
  </>}>
    {tab === "Saved attempts" ? <SavedGitDiffs projectId={props.checkout.projectId} target={props.target} openActivity={props.openActivity} /> : <>
      {review.isPending && <p role="status" className="p-4 text-sm text-muted-foreground">Loading Git review…</p>}
      {review.error && <div className="grid justify-items-start gap-2 p-4"><InlineIssue>{String(review.error)}</InlineIssue>
        <Button type="button" variant="outline" onClick={() => void review.refetch()}>Retry review</Button></div>}
      {review.data && !review.error && <>
        {tab === "Changes" && <GitCheckoutChanges key={JSON.stringify([checkoutId, requestedFile])} review={review.data} indexActions initialPath={requestedFile}
          openFile={activeCheckout ? (path, line) => openWorkspaceFile(props.openWorkspace, path, line) : undefined}
          openConversation={activeCheckout ? (id) => props.openWorkspace("conversations", id) : undefined}
          openPanel={activeCheckout ? () => props.openChanges("working") : undefined} />}
        {tab === "Worktrees" && <WorktreeTable review={review.data} project={props.checkout} select={(id, view) => {
          setRequestedFile(undefined);
          setCheckout(id);
          if (view) { setHistoryPath(undefined); setTab(view); }
        }} />}
        {tab === "Worktrees" && creating && <CreateGitWorktree projectId={props.checkout.projectId} review={review.data}
          select={(id) => { setCreating(false); setCheckout(id); }} close={() => setCreating(false)} />}
        {tab === "History" && (historyPath ? <GitFileHistoryView key={JSON.stringify([checkoutId, historyPath])} review={review.data} path={historyPath}
          openFile={activeCheckout ? () => openWorkspaceFile(props.openWorkspace, historyPath) : undefined}
          openConversation={activeCheckout ? (id) => props.openWorkspace("conversations", id) : undefined}
          clear={() => setHistoryPath(undefined)} /> : <CommitHistory key={checkoutId} review={review.data} />)}
      </>}
    </>}
  </GitWorkspaceFrame>;
}

function CreateGitWorktree({ projectId, review, select, close }: {
  projectId: string; review: CheckoutReview; select: (id: string) => void; close: () => void;
}) {
  const client = useQueryClient();
  const checkoutId = review.checkoutId;
  const history = useQuery({
    queryKey: ["git-history", checkoutId, review.revision.head, "", undefined],
    queryFn: () => invoke<HistoryEntry[]>("git_checkout_history", { checkoutId, branch: null }),
    enabled: Boolean(review.revision.head), retry: false,
  });
  const commits = history.data ?? [];
  const localBranches = review.branches.filter(({ reference }) => reference.startsWith("refs/heads/"));
  const usedBranches = new Set(review.worktrees.map(({ branch }) => branch));
  const mutation = useMutation({
    mutationFn: (request: { branch: string; existing: boolean; baseReference: string | null; expectedHead: string; path: string | null }) =>
      invoke<ProjectCheckout>("create_git_worktree", { projectId, checkoutId, request }),
    onSuccess: (checkout) => {
      void client.invalidateQueries({ queryKey: ["git-review"] });
      void client.invalidateQueries({ queryKey: ["registered-checkouts"] });
      select(checkout.checkoutId);
    },
  });
  function expectedHead({ branch, existing, base }: WorktreeDraft) {
    if (existing) return localBranches.find(({ name }) => name === branch)?.sha;
    if (!base) return review.revision.head;
    return review.branches.find(({ reference }) => reference === base)?.sha ?? commits.find(({ sha }) => sha === base)?.sha;
  }

  return <GitWorktreeDialog defaultBase="" pathHint="Project worktrees folder"
    revisions={[
      { value: "", label: "Current checkout HEAD", disabled: !review.revision.head },
      ...review.branches.map(({ reference, name, sha }) => ({ value: reference, label: `${name} · ${sha.slice(0, 12)}` })),
      ...commits.map(({ sha, subject }) => ({ value: sha, label: `${sha.slice(0, 12)} · ${subject}` })),
    ]}
    existingBranches={localBranches.map(({ name }) => ({
      value: name, label: usedBranches.has(name) ? `${name} · Checked out` : name, disabled: usedBranches.has(name),
    }))}
    busy={mutation.isPending} error={mutation.error ? String(mutation.error) : undefined} onClose={close}
    validate={(draft) => {
      if (!draft.existing && localBranches.some(({ name }) => name.toLowerCase() === draft.branch.toLowerCase())) return "This local branch already exists.";
      if (draft.existing && usedBranches.has(draft.branch)) return "This branch is already checked out.";
      if (draft.path && !/^[a-z]:[\\/]/i.test(draft.path)) return "Choose an absolute path on a local drive.";
      if (!expectedHead(draft)) return "Choose a starting revision that exists in this repository.";
    }}
    create={(draft) => {
      const head = expectedHead(draft);
      if (head) mutation.mutate({
        branch: draft.branch, existing: draft.existing, baseReference: draft.existing || !draft.base ? null : draft.base,
        expectedHead: head, path: draft.path || null,
      });
    }}
  />;
}

function WorktreeTable({ review, project, select }: { review: CheckoutReview; project: ProjectCheckout; select: (id: string, view?: Tab) => void }) {
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  return <div className="delivery-main p-4">
    <h3 className="mb-3 text-sm font-semibold">Repository worktrees</h3>
    {status && <p role="status" className="mb-2 text-xs">{status}</p>}
    {error && <InlineIssue>{error}</InlineIssue>}
    <GitWorkspaceTable label="Git worktrees" columns={["Worktree", "Branch", "HEAD", "Scope", "Selection"]}>
      {review.worktrees.map((tree) => {
        const selected = tree.checkoutId === review.checkoutId;
        const { checkoutId, branch, head } = tree;
        const actions = fileContextActions({ checkout: { ...project, checkoutId: checkoutId ?? "", checkoutRoot: tree.root },
          simulated: false, status: setStatus, report: (message) => { setStatus(""); setError(message); } });
        return <tr key={tree.root} className={selected ? "bg-muted" : undefined}>
          <td className="wrap-anywhere font-mono"><ContextActions label={tree.root} groups={[[
            ...(checkoutId ? [
              { label: "Use worktree", disabled: selected, run: () => select(checkoutId) },
              { label: "Review worktree changes", run: () => select(checkoutId, "Changes") },
              { label: "Open worktree history", run: () => select(checkoutId, "History") },
              { label: "Show in File Explorer", run: () => actions.reveal("") },
            ] : []),
          ], [
            { label: "Copy worktree path", run: () => actions.copy(tree.root, "Copied worktree path") },
            ...(branch ? [{ label: "Copy branch name", run: () => actions.copy(branch, "Copied branch name") }] : []),
            ...(head ? [{ label: "Copy HEAD SHA", run: () => actions.copy(head, "Copied HEAD SHA") }] : []),
          ]]}>{tree.root}</ContextActions></td>
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
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  const branchRef = review.branches.find((entry) => entry.reference === branch);
  const history = useQuery({
    queryKey: ["git-history", review.checkoutId, review.revision.head, branch, branchRef?.sha],
    queryFn: () => invoke<HistoryEntry[]>("git_checkout_history", { checkoutId: review.checkoutId, branch: branch || null }), retry: false,
  });
  const commits = history.error ? [] : history.data ?? [];
  const selected = commits.find((commit) => commit.sha === selection) ?? commits[0];
  function copy(text: string, copied: string) {
    setError("");
    setStatus("");
    void Promise.resolve().then(() => navigator.clipboard.writeText(text)).then(() => setStatus(copied), () => setError("Could not copy to the clipboard."));
  }
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
      {error && <InlineIssue>{error}</InlineIssue>}
      {status && <p role="status" className="mb-2 text-xs">{status}</p>}
      {history.data?.length === 0 && <p className="text-sm text-muted-foreground">No commits in this branch.</p>}
      {commits.length > 0 && <GitWorkspaceTable label="Repository commit history" columns={["Commit", "Author", "Recorded"]}>
        {commits.map((commit) => <tr key={commit.sha}>
          <td><ContextActions label={`${commit.subject || "Commit"} ${commit.sha.slice(0, 12)}`} groups={[[
            { label: "Inspect commit", run: () => select(commit.sha) },
          ], [
            { label: "Copy commit SHA", run: () => copy(commit.sha, "Copied commit SHA") },
            { label: "Copy short SHA", run: () => copy(commit.sha.slice(0, 12), "Copied short SHA") },
            ...(commit.subject ? [{ label: "Copy commit subject", run: () => copy(commit.subject, "Copied commit subject") }] : []),
          ]]}><button type="button" onClick={() => select(commit.sha)} aria-pressed={selected?.sha === commit.sha} className="grid gap-1 text-left font-medium hover:underline">
            {commit.subject || "Commit"}<code className="text-xs text-muted-foreground">{commit.sha.slice(0, 12)}</code>
          </button></ContextActions></td>
          <td>{commit.author}</td>
          <td className="whitespace-nowrap text-muted-foreground"><time dateTime={commit.committedAt}>{commit.committedAt}</time></td>
        </tr>)}
      </GitWorkspaceTable>}
    </div>
    {selected && <DeliveryDetailPanel label="Selected commit" resizeKey="git-commit">
      <div className="delivery-detail-content grid content-start gap-3 p-3.5">
        <h3 className="flex items-center gap-2 text-sm font-semibold"><GitCommitHorizontal size={17} aria-hidden /> Selected commit</h3>
        <dl className="grid gap-2 text-xs">
          <div><dt className="text-muted-foreground">Revision</dt><dd className="wrap-anywhere font-mono">{selected.sha}</dd></div>
          <div><dt className="text-muted-foreground">History ref</dt><dd className="wrap-anywhere font-mono">{branchRef?.reference ?? review.revision.branch ?? "Checkout HEAD"}</dd></div>
        </dl>
      </div>
    </DeliveryDetailPanel>}
  </div>;
}
