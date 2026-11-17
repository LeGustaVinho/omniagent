import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { GitDiffReview, type GitTextComparison } from "@/git-diff-view";
import { GitDiffSourceSelect } from "@/git-workspace-frame";
import { DeliveryDetailPanel, InlineIssue } from "@/tasks-git-ui";

type ReviewRevision = { branch: string | null; head: string | null; index: string | null };
type FileStatus = { path: string; staged: string | null; working: string | null };
type Branch = { name: string; reference: string; sha: string };
export type Worktree = { checkoutId: string | null; root: string; branch: string | null; head: string | null; managed: boolean };
export type CheckoutReview = { checkoutId: string; root: string; revision: ReviewRevision; managed: boolean; changes: FileStatus[]; branches: Branch[]; worktrees: Worktree[] };
type FileSide = { status: "missing" } | { status: "text"; content: string } | { status: "unavailable"; reason: string };
type Source = "working" | "staged";
type TextDiff = { path: string; original: FileSide; modified: FileSide };
type BranchRevisions = { base: string; head: string; mergeBase: string };
const reviewSources = ["working", "staged", "branch"] as const;
type ReviewSource = typeof reviewSources[number];

const statusLabels: Record<string, string> = { A: "Added", D: "Deleted", M: "Modified", U: "Unmerged", T: "Type changed", "?": "Untracked" };
const languages: Record<string, string> = {
  md: "markdown", markdown: "markdown", js: "javascript", jsx: "javascript", ts: "typescript", tsx: "typescript", rs: "rust",
  py: "python", json: "json", yaml: "yaml", yml: "yaml", ps1: "powershell", html: "html", css: "css", toml: "toml",
};
const sourceLabels: Record<Source, string> = { working: "Working tree", staged: "Staged" };

export function useGitCheckoutReview(checkoutId: string, enabled = true) {
  return useQuery({
    queryKey: ["git-review", checkoutId], queryFn: () => invoke<CheckoutReview>("git_checkout_review", { checkoutId }),
    enabled, refetchInterval: 2500, retry: false,
  });
}

export function GitCheckoutChanges({ review, indexActions, initialSource = "working", initialPath, openFile, openPanel, onClose }: {
  review: CheckoutReview;
  indexActions?: boolean;
  initialSource?: Source;
  initialPath?: string;
  openFile?: (path: string) => void;
  openPanel?: () => void;
  onClose?: () => void;
}) {
  const [source, setSource] = useState<ReviewSource>(initialSource);
  const [selection, setSelection] = useState(initialPath);
  const changeStage = useChangeStage(review);
  const files = source === "branch" ? [] : review.changes.flatMap((file) => {
    const status = file[source];
    return status ? [{ path: file.path, kind: statusLabels[status] ?? status }] : [];
  });
  const path = selection === undefined ? files[0]?.path : files.find((file) => file.path === selection)?.path;
  const diff = useQuery({
    queryKey: ["git-live-diff", review.checkoutId, source, path, review.revision.branch, review.revision.head, review.revision.index],
    queryFn: () => invoke<TextDiff>("git_checkout_diff", { checkoutId: review.checkoutId, path, source }),
    enabled: !!path, refetchInterval: 2500, retry: false,
  });
  const comparison = useMemo(() => diff.error ? null : comparisonFile(diff.data), [diff.data, diff.error]);
  const unavailable = diff.data && [diff.data.original, diff.data.modified].find((side) => side.status === "unavailable");
  const canOpen = source === "working" && comparison && diff.data?.modified.status === "text" && openFile;
  return <div className="flex min-h-0 min-w-0 flex-1 flex-col">
    <div className="delivery-toolbar delivery-filters">
      <GitDiffSourceSelect sources={reviewSources} value={source} onChange={(next) => { setSource(next); setSelection(undefined); }} />
      {source === "working" && openPanel && <Button className="ml-auto px-2.5 py-1.5 text-xs" type="button" variant="outline" onClick={openPanel}>Open changes panel</Button>}
    </div>
    {source === "branch" ? <GitBranchComparison review={review} onClose={onClose} /> : <div className="delivery-body">
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <GitDiffReview key={source} source={source} title={sourceLabels[source]}
          ranges={source === "working" ? { base: "Index", revised: "Worktree" } : { base: "HEAD", revised: "Index" }}
          files={files} selectedPath={path} select={setSelection} comparison={comparison}
          notice={selection && !path && <p className="p-3 text-sm" role="status">No {source === "working" ? "working" : "staged"} changes for {selection}.</p>}
          preview={path && (diff.isPending ? <p role="status" className="p-3 text-sm">Loading diff…</p>
            : diff.error ? <div className="p-3"><InlineIssue>{String(diff.error)}</InlineIssue></div>
            : unavailable && <div className="p-3"><InlineIssue>{unavailable.reason}</InlineIssue></div>)}
          empty={<p>{source === "working" ? "No working changes." : "No staged changes."}</p>}
          onClose={onClose} openFile={canOpen ? () => openFile(comparison.path) : undefined}
          fileActions={(file) => {
            const entry = review.changes.find((change) => change.path === file.path);
            return [[
              ...(openFile && source === "working" && file.kind !== "Deleted"
                ? [{ label: "Open working file", run: () => openFile(file.path) }] : []),
              ...(indexActions && entry?.working ? [{ label: "Stage file", disabled: changeStage.isPending,
                run: () => changeStage.mutate({ path: file.path, staged: true }) }] : []),
              ...(indexActions && entry?.staged ? [{ label: "Unstage file", disabled: changeStage.isPending,
                run: () => changeStage.mutate({ path: file.path, staged: false }) }] : []),
            ]];
          }} />
      </div>
      {indexActions && <IndexOperations review={review} changeStage={changeStage} />}
    </div>}
  </div>;
}

function GitBranchComparison({ review, onClose }: { review: CheckoutReview; onClose?: () => void }) {
  const [baseReference, setBase] = useState(() => review.branches.find((branch) => branch.reference === "refs/remotes/origin/main")?.reference
    ?? review.branches.find((branch) => branch.reference === "refs/heads/main")?.reference
    ?? review.branches[0]?.reference ?? "");
  const [headReference, setHead] = useState("");
  const [selection, setSelection] = useState<string>();
  const base = review.branches.find((branch) => branch.reference === baseReference);
  const head = headReference && review.branches.find((branch) => branch.reference === headReference);
  const headSha = headReference ? (head || undefined)?.sha : review.revision.head;
  const range = { baseReference, headReference: headReference || null };
  const checkoutId = review.checkoutId;
  const summary = useQuery({
    queryKey: ["git-branch-comparison", checkoutId, baseReference, base?.sha, headReference, headSha],
    queryFn: () => invoke<{ revisions: BranchRevisions; changes: { path: string; status: string }[] }>("git_branch_comparison", { checkoutId, range }),
    enabled: Boolean(base && headSha), retry: false,
  });
  const files = summary.data?.changes.map(({ path, status }) => ({ path, kind: statusLabels[status] ?? status })) ?? [];
  const path = selection === "" ? undefined : files.find((file) => file.path === selection)?.path ?? files[0]?.path;
  const revisions = summary.data?.revisions;
  const diff = useQuery({
    queryKey: ["git-branch-comparison-diff", checkoutId, baseReference, headReference, revisions, path],
    queryFn: () => invoke<TextDiff>("git_branch_comparison_diff", { checkoutId, range, expected: revisions, path }),
    enabled: Boolean(path && revisions), retry: false,
  });
  const comparison = useMemo(() => diff.error ? null : comparisonFile(diff.data), [diff.data, diff.error]);
  const unavailable = diff.data && [diff.data.original, diff.data.modified].find((side) => side.status === "unavailable");
  const options = review.branches.map(({ name, reference }) => <option key={reference} value={reference}>{name}</option>);
  return <div className="flex min-h-0 min-w-0 flex-1 flex-col">
    <div className="delivery-toolbar delivery-filters">
      <label className="delivery-field">Base reference
        <select aria-label="Base reference" className="delivery-filter" value={baseReference} onChange={(event) => {
          setBase(event.currentTarget.value); setSelection(undefined);
        }}>
          {!base && <option value={baseReference} disabled>{baseReference ? "Base reference is unavailable" : "Select a branch"}</option>}
          {options}
        </select>
      </label>
      <label className="delivery-field">Compare reference
        <select aria-label="Compare reference" className="delivery-filter" value={headReference} onChange={(event) => {
          setHead(event.currentTarget.value); setSelection(undefined);
        }}>
          <option value="">Current checkout HEAD</option>
          {headReference && !head && <option value={headReference} disabled>Compare reference is unavailable</option>}
          {options}
        </select>
      </label>
    </div>
    <GitDiffReview source="branch" title="Branch comparison" files={files} selectedPath={path} select={setSelection} comparison={comparison}
      ranges={{
        base: `Merge base of ${base?.name ?? "selected branch"}${revisions ? ` · ${revisions.mergeBase.slice(0, 12)}` : ""}`,
        revised: `${head ? head.name : "Checkout HEAD"}${revisions ? ` · ${revisions.head.slice(0, 12)}` : ""}`,
      }}
      preview={path && (diff.isPending ? <p role="status" className="p-3 text-sm">Loading diff…</p>
        : diff.error ? <div className="p-3"><InlineIssue>{String(diff.error)}</InlineIssue></div>
        : unavailable && <div className="p-3"><InlineIssue>{unavailable.reason}</InlineIssue></div>)}
      empty={!base || !headSha ? <p>Select two available revisions to compare.</p>
        : summary.error ? <InlineIssue>{String(summary.error)}</InlineIssue>
        : summary.isPending ? <p role="status">Loading branch comparison…</p>
        : <p>No committed changes since the merge base.</p>}
      onClose={onClose} />
  </div>;
}

function useChangeStage(review: CheckoutReview) {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ path, staged }: { path: string; staged: boolean }) => invoke("set_git_path_staged", {
      checkoutId: review.checkoutId, path, staged, expected: review.revision,
    }),
    onSettled: () => Promise.all([
      client.invalidateQueries({ queryKey: ["git-review", review.checkoutId] }),
      client.invalidateQueries({ queryKey: ["git-live-diff", review.checkoutId] }),
    ]),
  });
}

function IndexOperations({ review, changeStage }: { review: CheckoutReview; changeStage: ReturnType<typeof useChangeStage> }) {
  const stagedCount = review.changes.filter((file) => file.staged).length;
  return <DeliveryDetailPanel label="Stage files" resizeKey="git-stage">
    <div className="delivery-detail-content grid content-start gap-3 p-3.5">
      <div>
        <h3 className="text-sm font-semibold">Index</h3>
        <p className="mt-1 wrap-anywhere font-mono text-xs">{review.revision.branch?.replace(/^refs\/heads\//, "") ?? "Detached HEAD"}</p>
        <code className="block wrap-anywhere text-xs text-muted-foreground">{review.revision.head ?? "Unborn branch"}</code>
      </div>
      <h4 className="text-xs font-semibold">Stage files <span className="text-muted-foreground">({stagedCount})</span></h4>
      {changeStage.error && <InlineIssue>{String(changeStage.error)}</InlineIssue>}
      {review.changes.length ? <ul className="grid gap-1.5">
        {review.changes.map((entry) => <li className="grid gap-1 rounded border border-border p-2" key={entry.path}>
          <span className="wrap-anywhere font-mono text-xs">{entry.path}</span>
          <div className="flex flex-wrap gap-2">
            {entry.working && <Button type="button" className="px-2.5 py-1.5 text-xs" variant="outline" disabled={changeStage.isPending}
              aria-label={`Stage ${entry.path}`} onClick={() => changeStage.mutate({ path: entry.path, staged: true })}>Stage</Button>}
            {entry.staged && <Button type="button" className="px-2.5 py-1.5 text-xs" variant="outline" disabled={changeStage.isPending}
              aria-label={`Unstage ${entry.path}`} onClick={() => changeStage.mutate({ path: entry.path, staged: false })}>Unstage</Button>}
          </div>
        </li>)}
      </ul> : <p className="text-sm text-muted-foreground">This worktree has a clean index and working tree.</p>}
      {review.managed ? <p className="text-xs text-muted-foreground">Commit approved changes from the linked Task delivery.</p>
        : <CommitIndex review={review} />}
    </div>
  </DeliveryDetailPanel>;
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
    <Button type="submit" className="px-2.5 py-1.5 text-xs" disabled={!ready}>{commit.isPending ? "Committing…" : "Commit staged changes"}</Button>
    {commit.error && <InlineIssue>{String(commit.error)}</InlineIssue>}
    {commit.data && <p role="status" className="wrap-anywhere text-xs">Commit saved <code>{commit.data}</code></p>}
  </form>;
}

function comparisonFile(diff?: TextDiff): GitTextComparison | null {
  if (!diff || diff.original.status === "unavailable" || diff.modified.status === "unavailable") return null;
  const content = (side: FileSide) => side.status === "text" ? side.content : "";
  const extension = diff.path.split(".").at(-1)?.toLowerCase() ?? "";
  return { path: diff.path, language: languages[extension] ?? "plaintext", original: content(diff.original), modified: content(diff.modified) };
}
