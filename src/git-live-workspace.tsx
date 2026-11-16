import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FileDiff, GitBranch, History, Layers3, RefreshCw } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { MonacoComparison } from "@/changes-panel";
import { Button } from "@/components/ui/button";
import type { ChangedFile } from "@/git-demo-data";
import { SavedGitDiffs } from "@/git-saved-diffs";
import type { WorkspaceProps } from "@/workbench-contract";

type ReviewRevision = { branch: string | null; head: string | null; index: string | null };
type FileStatus = { path: string; staged: string | null; working: string | null };
type Branch = { name: string; reference: string; sha: string };
type Worktree = { checkoutId: string | null; root: string; branch: string | null; head: string | null; managed: boolean };
type CheckoutReview = { checkoutId: string; root: string; revision: ReviewRevision; managed: boolean; changes: FileStatus[]; branches: Branch[]; worktrees: Worktree[] };
type HistoryEntry = { sha: string; author: string; committedAt: string; subject: string };
type FileSide = { status: "missing" } | { status: "text"; content: string } | { status: "unavailable"; reason: string };
type Source = "working" | "staged";
type LiveDiff = { path: string; source: Source; original: FileSide; modified: FileSide };
type Tab = "Changes" | "Worktrees" | "History" | "Saved attempts";

const tabs: readonly { name: Tab; Icon: typeof GitBranch }[] = [
  { name: "Changes", Icon: FileDiff }, { name: "Worktrees", Icon: Layers3 },
  { name: "History", Icon: History }, { name: "Saved attempts", Icon: FileDiff },
];
const noDiffControls = () => undefined;
const statusLabels: Record<string, string> = { A: "Added", D: "Deleted", M: "Modified", U: "Unmerged", T: "Type changed", "?": "Untracked" };
const tabFor = (target?: string): Tab => target === "worktrees" ? "Worktrees" : target === "history" ? "History"
  : target === "saved" || target?.startsWith("attempt:") ? "Saved attempts" : "Changes";

export function LiveGitWorkspace(props: WorkspaceProps) {
  const [checkoutId, setCheckout] = useState(props.checkout.checkoutId);
  const [tab, setTab] = useState<Tab>(() => tabFor(props.target));
  useEffect(() => { setTab(tabFor(props.target)); }, [props.target]);
  const review = useQuery({
    queryKey: ["git-review", checkoutId], queryFn: () => invoke<CheckoutReview>("git_checkout_review", { checkoutId }),
    enabled: tab !== "Saved attempts", refetchInterval: 2500, retry: false,
  });
  const trees = review.data?.worktrees.filter((tree): tree is Worktree & { checkoutId: string } => tree.checkoutId !== null) ?? [];
  const branch = review.data?.revision.branch?.replace(/^refs\/heads\//, "");

  return <section className="delivery-view" aria-label="Git workspace">
    <header className="delivery-toolbar">
      <GitBranch size={16} aria-hidden className="text-muted-foreground" />
      <h2 className="mr-2">Git</h2>
      {tabs.map(({ name, Icon }) => <button key={name} type="button" className="delivery-tab" aria-pressed={tab === name} onClick={() => setTab(name)}>
        <Icon size={14} aria-hidden className="mr-1 inline-block" />{name}
      </button>)}
      {tab !== "Saved attempts" && <label className="ml-auto"><span className="sr-only">Git worktree</span>
        <select aria-label="Git worktree" className="delivery-filter max-w-60" value={checkoutId} onChange={(event) => setCheckout(event.target.value)}>
          {!trees.some(({ checkoutId: id }) => id === checkoutId) && <option value={checkoutId}>{branch ?? props.checkout.checkoutRoot}</option>}
          {trees.map((tree) => <option key={tree.checkoutId} value={tree.checkoutId}>{tree.branch ?? "Detached HEAD"} · {tree.root}</option>)}
        </select>
      </label>}
      {tab !== "Saved attempts" && <Button type="button" variant="ghost" aria-label="Refresh Git review" onClick={() => void review.refetch()}><RefreshCw size={15} /></Button>}
    </header>
    {tab === "Saved attempts" ? <SavedGitDiffs projectId={props.checkout.projectId} target={props.target} openActivity={props.openActivity} /> : <>
      {review.isPending && <p role="status" className="p-4 text-sm text-muted-foreground">Loading Git review…</p>}
      {review.error && <p role="alert" className="p-4 text-sm">{String(review.error)}</p>}
      {review.data && tab === "Changes" && <LiveChanges key={checkoutId} review={review.data} />}
      {review.data && tab === "Worktrees" && <WorktreeTable review={review.data} select={setCheckout} />}
      {review.data && tab === "History" && <CommitHistory key={checkoutId} review={review.data} />}
    </>}
  </section>;
}

function WorktreeTable({ review, select }: { review: CheckoutReview; select: (id: string) => void }) {
  return <div className="delivery-main p-4">
    <div className="overflow-auto rounded border border-border">
      <table className="delivery-table" aria-label="Git worktrees">
        <thead><tr><th>Worktree</th><th>Branch</th><th>HEAD</th><th>Scope</th><th>Selection</th></tr></thead>
        <tbody>{review.worktrees.map((tree) => <tr key={tree.root}>
          <td className="break-all font-mono">{tree.root}</td><td>{tree.branch ?? "Detached HEAD"}</td>
          <td className="break-all font-mono">{tree.head ?? "Unborn"}</td><td>{tree.checkoutId ? tree.managed ? "Managed" : "Registered" : "Unregistered"}</td>
          <td><Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" disabled={!tree.checkoutId || tree.checkoutId === review.checkoutId} onClick={() => { if (tree.checkoutId) select(tree.checkoutId); }}>
            {tree.checkoutId === review.checkoutId ? "Selected" : "Use worktree"}
          </Button></td>
        </tr>)}</tbody>
      </table>
    </div>
  </div>;
}

function CommitHistory({ review }: { review: CheckoutReview }) {
  const [branch, setBranch] = useState("");
  const history = useQuery({
    queryKey: ["git-history", review.checkoutId, review.revision.head, branch, review.branches.find((entry) => entry.reference === branch)?.sha],
    queryFn: () => invoke<HistoryEntry[]>("git_checkout_history", { checkoutId: review.checkoutId, branch: branch || null }), retry: false,
  });
  return <div className="delivery-main p-4">
    <label className="delivery-field mb-3 max-w-72">History branch
      <select className="delivery-filter" value={branch} onChange={(event) => setBranch(event.target.value)}>
        <option value="">Current checkout HEAD</option>
        {review.branches.map((entry) => <option key={entry.reference} value={entry.reference}>{entry.name}</option>)}
      </select>
    </label>
    {history.isPending && <p role="status">Loading commits…</p>}
    {history.error && <p role="alert">{String(history.error)}</p>}
    {history.data?.length === 0 && <p>No commits in this branch.</p>}
    {!!history.data?.length && <div className="overflow-auto rounded border border-border">
      <table className="delivery-table" aria-label="Repository commit history">
        <thead><tr><th>Commit</th><th>Author</th><th>Recorded</th></tr></thead>
        <tbody>{history.data.map((commit) => <tr key={commit.sha}>
          <td><p className="font-medium">{commit.subject}</p><code className="break-all text-xs">{commit.sha}</code></td>
          <td>{commit.author}</td><td>{commit.committedAt}</td>
        </tr>)}</tbody>
      </table>
    </div>}
  </div>;
}

function LiveChanges({ review }: { review: CheckoutReview }) {
  const client = useQueryClient();
  const [source, setSource] = useState<Source>("working");
  const [selected, setSelected] = useState<string>();
  const files = review.changes.filter((file) => file[source]);
  const path = files.some((file) => file.path === selected) ? selected : files[0]?.path;
  const diff = useQuery({
    queryKey: ["git-live-diff", review.checkoutId, source, path, review.revision.head, review.revision.index],
    queryFn: () => invoke<LiveDiff>("git_checkout_diff", { checkoutId: review.checkoutId, path, source }),
    enabled: !!path, refetchInterval: 2500, retry: false,
  });
  const changeStage = useMutation({
    mutationFn: ({ path, staged }: { path: string; staged: boolean }) => invoke("set_git_path_staged", {
      checkoutId: review.checkoutId, path, staged, expected: review.revision,
    }),
    onSettled: () => Promise.all([
      client.invalidateQueries({ queryKey: ["git-review", review.checkoutId] }),
      client.invalidateQueries({ queryKey: ["git-live-diff", review.checkoutId] }),
    ]),
  });
  const file = useMemo(() => comparisonFile(diff.data), [diff.data]);
  const blocked = diff.data && [diff.data.original, diff.data.modified].find((side) => side.status === "unavailable");
  return <div className="delivery-body">
    <div className="delivery-main flex min-w-0 flex-col">
      <div className="delivery-toolbar flex-wrap py-2">
        <label className="delivery-field">Diff source<select className="delivery-filter" value={source} onChange={(event) => { setSource(event.target.value as Source); setSelected(undefined); }}>
          <option value="working">Working tree</option><option value="staged">Staged</option>
        </select></label>
        <p className="text-xs text-muted-foreground" aria-label="Comparison revisions">{source === "working" ? "Index → Worktree" : "HEAD → Index"}</p>
      </div>
      <section className="max-h-44 shrink-0 overflow-auto border-b border-border" aria-label="Changed files">
        {files.map((entry) => <button key={entry.path} type="button" className="diff-file-row" aria-pressed={path === entry.path} onClick={() => setSelected(entry.path)}>
          <FileDiff size={14} aria-hidden /><span className="min-w-0 flex-1 break-all font-mono">{entry.path}</span>
          <span>{statusLabel(entry[source])}</span>
        </button>)}
        {!files.length && <p className="p-4 text-sm">{source === "working" ? "No working changes." : "No staged changes."}</p>}
      </section>
      {path && diff.isPending && <p role="status" className="p-4">Loading diff…</p>}
      {diff.error && <p role="alert" className="p-4">{String(diff.error)}</p>}
      {blocked?.status === "unavailable" && <p className="p-4">{blocked.reason}</p>}
      {file && <div className="min-h-64 flex-1"><MonacoComparison key={`${source}:${file.path}`} file={file} layout="auto" ready={noDiffControls} /></div>}
    </div>
    <aside className="delivery-detail" aria-label="Stage files">
      <div className="delivery-detail-content grid content-start gap-3 p-3.5">
        <div><h3 className="text-sm font-semibold">Index</h3><p className="mt-1 break-all font-mono text-xs">{review.revision.branch?.replace(/^refs\/heads\//, "") ?? "Detached HEAD"}</p>
          <p className="break-all font-mono text-xs">{review.revision.head ?? "Unborn branch"}</p></div>
        {changeStage.error && <p role="alert" className="text-sm">{String(changeStage.error)}</p>}
        {review.changes.map((entry) => <div className="grid gap-1 rounded border border-border p-2" key={entry.path}>
          <span className="break-all font-mono text-xs">{entry.path}</span>
          <div className="flex flex-wrap gap-2">
            {entry.working && <Button type="button" className="px-2.5 py-1.5 text-xs" variant="outline" disabled={changeStage.isPending} aria-label={`Stage ${entry.path}`} onClick={() => changeStage.mutate({ path: entry.path, staged: true })}>Stage</Button>}
            {entry.staged && <Button type="button" className="px-2.5 py-1.5 text-xs" variant="outline" disabled={changeStage.isPending} aria-label={`Unstage ${entry.path}`} onClick={() => changeStage.mutate({ path: entry.path, staged: false })}>Unstage</Button>}
          </div>
        </div>)}
        {review.managed ? <p className="text-xs text-muted-foreground">Commit approved changes from the linked Task delivery.</p>
          : <CommitIndex review={review} />}
      </div>
    </aside>
  </div>;
}

function CommitIndex({ review }: { review: CheckoutReview }) {
  const client = useQueryClient();
  const [message, setMessage] = useState("");
  const [expected, setExpected] = useState<ReviewRevision>();
  const changed = !!expected && (expected.branch !== review.revision.branch || expected.head !== review.revision.head || expected.index !== review.revision.index);
  const commit = useMutation({
    mutationFn: (snapshot: ReviewRevision) => invoke<string>("commit_git_index", { checkoutId: review.checkoutId, message, expected: snapshot }),
    onSuccess: () => { setMessage(""); setExpected(undefined); },
    onSettled: () => Promise.all([
      client.invalidateQueries({ queryKey: ["git-review", review.checkoutId] }),
      client.invalidateQueries({ queryKey: ["git-live-diff", review.checkoutId] }),
      client.invalidateQueries({ queryKey: ["git-history", review.checkoutId] }),
    ]),
  });
  const hasStaged = review.changes.some((entry) => entry.staged);
  const ready = expected && !changed && hasStaged && review.revision.branch && message.trim() && !commit.isPending;
  return <form className="grid gap-2 border-t border-border pt-3" onSubmit={(event) => {
    event.preventDefault();
    if (ready) commit.mutate(expected);
  }}>
    <label className="delivery-field">Commit message
      <textarea rows={3} className="delivery-filter min-w-0 resize-y" value={message} maxLength={4096} disabled={commit.isPending}
        onChange={(event) => { setMessage(event.target.value); setExpected(event.target.value ? expected ?? review.revision : undefined); if (commit.error) commit.reset(); }} />
    </label>
    {changed && <>
      <p role="status" className="text-xs">The staged index or branch changed. Review the current staged changes before committing.</p>
      <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" onClick={() => { setExpected(review.revision); commit.reset(); }}>Use current index</Button>
    </>}
    <Button type="submit" className="px-2.5 py-1.5 text-xs" disabled={!ready}>
      {commit.isPending ? "Committing…" : "Commit staged changes"}
    </Button>
    {commit.error && <p role="alert" className="text-xs">{String(commit.error)}</p>}
    {commit.data && <p role="status" className="break-all text-xs">Commit saved <code>{commit.data}</code></p>}
  </form>;
}

function statusLabel(value: string | null) {
  return value ? statusLabels[value] ?? value : "";
}

function comparisonFile(diff?: LiveDiff): ChangedFile | null {
  if (!diff || diff.original.status === "unavailable" || diff.modified.status === "unavailable") return null;
  const content = (side: FileSide) => side.status === "text" ? side.content : "";
  const extension = diff.path.split(".").at(-1)?.toLowerCase() ?? "";
  const languages: Record<string, string> = { md: "markdown", markdown: "markdown", js: "javascript", jsx: "javascript", ts: "typescript", tsx: "typescript", rs: "rust", py: "python", json: "json", yaml: "yaml", yml: "yaml", ps1: "powershell", html: "html", css: "css", toml: "toml" };
  return { path: diff.path, kind: diff.original.status === "missing" ? "Added" : diff.modified.status === "missing" ? "Deleted" : "Modified",
    language: languages[extension] ?? "plaintext", original: content(diff.original), modified: content(diff.modified), additions: 0, deletions: 0 };
}
