import { invoke } from "@tauri-apps/api/core";
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { GitCommitHorizontal, Plus, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback, type MenuAction } from "@/components/ui/context-actions";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { GitCheckoutChanges, comparisonFile, useGitCheckoutReview, type CheckoutReview, type Worktree } from "@/git-checkout-changes";
import { GitDiffReview, type GitComparedFile } from "@/git-diff-view";
import { GitFileHistoryView } from "@/git-file-history";
import { SavedGitDiffs } from "@/git-saved-diffs";
import { GitWorkspaceFrame, GitWorkspaceTable } from "@/git-workspace-frame";
import { GitWorktreeDialog, type WorktreeDraft } from "@/git-worktree-dialog";
import { FileDialog } from "@/ide-file-workspace";
import { DeliveryDetailPanel, DeliveryStatus, InlineIssue } from "@/tasks-git-ui";
import { fileContextActions, isGitCheckoutTarget, isNewWorktreeTarget, openWorkspaceFile, readFileHistoryTarget, readWorkingDiffTarget, terminalFolderTarget } from "@/file-context-actions";
import type { WorkspaceProps } from "@/workbench-contract";

type HistoryEntry = { sha: string; author: string; committedAt: string; subject: string };
type CommitRequest = { branch: string | null; expectedHead: string; sha: string; parent: string | null };
type CommitFile = { path: string; previousPath: string | null; status: string };
type CommitDetails = { message: string; parents: string[]; parent: string | null; files: CommitFile[]; comparisonUnavailable?: string | null };
type CommitReference = { id: string; label: string; sha: string };
type CommitComparison = { checkoutId: string; base: CommitReference; targets: CommitReference[]; initialTarget?: string };
type WorktreeBase = { reference: string; label: string; expectedHead: string };

const commitChangeLabels: Record<string, string> = { A: "Added", C: "Copied", D: "Deleted", M: "Modified", R: "Renamed", T: "Type changed" };

function commitDetailsQuery(checkoutId: string, request: CommitRequest) {
  return {
    queryKey: ["git-commit-details", checkoutId, request],
    queryFn: () => invoke<CommitDetails>("git_checkout_commit_details", { checkoutId, request }),
    retry: false,
  };
}

function loadCommitFile(checkoutId: string, request: CommitRequest, path: string) {
  return invoke<GitComparedFile>("git_checkout_commit_diff", { checkoutId, request, path });
}

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
  const [savedWorktree, setSavedWorktree] = useState<Pick<ProjectCheckout, "checkoutId" | "checkoutRoot">>();
  const [creating, setCreating] = useState<{ checkoutId: string; base?: WorktreeBase }>();
  const [creatingBranch, setCreatingBranch] = useState<{ checkoutId: string; base: CommitReference }>();
  const [createdBranch, setCreatedBranch] = useState<string>();
  useEffect(() => {
    setTab(tabFor(props.target));
    setSavedWorktree(undefined);
    setCreatingBranch(undefined);
    setCreatedBranch(undefined);
    const file = readWorkingDiffTarget(props.target);
    const history = readFileHistoryTarget(props.target);
    setRequestedFile(file);
    setHistoryPath(history);
    const create = isNewWorktreeTarget(props.target);
    if (file !== undefined || history !== undefined || create || isGitCheckoutTarget(props.target)) setCheckout(props.checkout.checkoutId);
    setCreating(create ? { checkoutId: props.checkout.checkoutId } : undefined);
  }, [props.checkout.checkoutId, props.target]);
  const review = useGitCheckoutReview(checkoutId, tab !== "Saved attempts");
  const trees = review.data?.worktrees.filter((tree): tree is Worktree & { checkoutId: string } => tree.checkoutId !== null) ?? [];
  const branch = review.data?.revision.branch?.replace(/^refs\/heads\//, "");
  const activeCheckout = checkoutId === props.checkout.checkoutId;
  const openWorktreeSetup = (source: string, base?: WorktreeBase) => {
    setCheckout(source);
    setCreating({ checkoutId: source, base });
  };
  const openBranchSetup = (checkoutId: string, base: CommitReference) => {
    setCreatedBranch(undefined);
    setCreatingBranch({ checkoutId, base });
  };

  return <GitWorkspaceFrame tabs={tabs} active={tab} select={(view) => { setSavedWorktree(undefined); setTab(view); }} controls={tab !== "Saved attempts" && <>
    <label className="ml-auto min-w-0"><span className="sr-only">Git worktree</span>
      <select className="delivery-filter w-full max-w-60" value={checkoutId} onChange={(event) => {
        setRequestedFile(undefined);
        setCheckout(event.currentTarget.value);
      }}>
        {!trees.some(({ checkoutId: id }) => id === checkoutId) && <option value={checkoutId}>{branch ?? (activeCheckout ? props.checkout.checkoutRoot : checkoutId)}</option>}
        {trees.map((tree) => <option key={tree.checkoutId} value={tree.checkoutId}>{tree.branch ?? "Detached HEAD"} · {tree.root}</option>)}
      </select>
    </label>
    {tab === "Worktrees" && <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" disabled={!review.data || Boolean(review.error)}
      onClick={() => openWorktreeSetup(checkoutId)}><Plus size={14} /> New worktree</Button>}
    <Button type="button" variant="ghost" aria-label="Refresh Git review" title="Refresh Git review" onClick={() => void review.refetch()}><RefreshCw size={15} aria-hidden /></Button>
  </>}>
    {createdBranch && <p role="status" className="px-4 py-2 text-xs">Created local branch <code>{createdBranch}</code>.</p>}
    {creatingBranch && <CreateLocalBranch {...creatingBranch} close={() => setCreatingBranch(undefined)} created={(name) => {
      setCreatingBranch(undefined);
      setCreatedBranch(name);
    }} />}
    {tab === "Saved attempts" ? <SavedGitDiffs projectId={props.checkout.projectId} target={savedWorktree ? undefined : props.target}
      worktree={savedWorktree} workspace={props} openActivity={props.openActivity} /> : <>
      {review.isPending && <p role="status" className="p-4 text-sm text-muted-foreground">Loading Git review…</p>}
      {review.error && <div className="p-4"><InlineIssue>{String(review.error)}</InlineIssue></div>}
      {review.data && !review.error && <>
        {tab === "Changes" && <GitCheckoutChanges key={JSON.stringify([checkoutId, requestedFile])} review={review.data} indexActions initialPath={requestedFile}
          openFile={activeCheckout ? (path, line) => openWorkspaceFile(props.openWorkspace, path, line) : undefined}
          openConversation={activeCheckout ? (id) => props.openWorkspace("conversations", id) : undefined}
          openPanel={activeCheckout ? () => props.openChanges("working") : undefined} />}
        {tab === "Worktrees" && <WorktreeTable review={review.data} project={props.checkout} select={(id, view) => {
          setRequestedFile(undefined);
          setCheckout(id);
          if (view) { setHistoryPath(undefined); setTab(view); }
        }} createFrom={openWorktreeSetup} createBranch={openBranchSetup} openCheckoutWorkspace={props.openCheckoutWorkspace}
          openAttempts={(selected) => { setSavedWorktree(selected); setTab("Saved attempts"); }} />}
        {creating && creating.checkoutId === review.data.checkoutId && <CreateGitWorktree projectId={props.checkout.projectId}
          review={review.data} startingPoint={creating.base}
          select={(id) => { setCreating(undefined); setCheckout(id); setTab("Worktrees"); }} close={() => setCreating(undefined)} />}
        {tab === "History" && (historyPath ? <GitFileHistoryView key={JSON.stringify([checkoutId, historyPath])} review={review.data} path={historyPath}
          openFile={activeCheckout ? () => openWorkspaceFile(props.openWorkspace, historyPath) : undefined}
          openConversation={activeCheckout ? (id) => props.openWorkspace("conversations", id) : undefined}
          clear={() => setHistoryPath(undefined)} /> : <CommitHistory key={checkoutId} review={review.data}
            createFrom={(base) => openWorktreeSetup(checkoutId, base)} createBranch={(base) => openBranchSetup(checkoutId, base)} />)}
      </>}
    </>}
  </GitWorkspaceFrame>;
}

function CreateGitWorktree({ projectId, review, startingPoint, select, close }: {
  projectId: string; review: CheckoutReview; startingPoint?: WorktreeBase; select: (id: string) => void; close: () => void;
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
  const revisions = [
    { value: "", label: "Current checkout HEAD", disabled: !review.revision.head },
    ...review.branches.map(({ reference, name, sha }) => ({ value: reference, label: `${name} · ${sha.slice(0, 12)}` })),
    ...commits.map(({ sha, subject }) => ({ value: sha, label: `${sha.slice(0, 12)} · ${subject}` })),
  ];
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
    if (startingPoint && base === startingPoint.reference) return startingPoint.expectedHead;
    if (!base) return review.revision.head;
    return review.branches.find(({ reference }) => reference === base)?.sha ?? commits.find(({ sha }) => sha === base)?.sha;
  }

  return <GitWorktreeDialog defaultBase={startingPoint?.reference ?? ""} pathHint="Project worktrees folder"
    revisions={startingPoint ? [{ value: startingPoint.reference, label: startingPoint.label },
      ...revisions.filter(({ value }) => value !== startingPoint.reference)] : revisions}
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

function CreateLocalBranch({ checkoutId, base, close, created }: {
  checkoutId: string; base: CommitReference; close: () => void; created: (name: string) => void;
}) {
  const client = useQueryClient();
  const [name, setName] = useState("");
  const mutation = useMutation({
    mutationFn: (branch: string) => invoke<void>("create_git_branch", { checkoutId, branch, expectedHead: base.sha }),
    onSuccess: (_, branch) => {
      void client.invalidateQueries({ queryKey: ["git-review"] });
      created(branch);
    },
  });
  const branch = name.trim();
  return <FileDialog title="Create local branch" description={base.label} onClose={() => { if (!mutation.isPending) close(); }}>
    <dl className="text-xs"><dt className="text-muted-foreground">Starting commit</dt><dd className="wrap-anywhere font-mono">{base.sha}</dd></dl>
    <form className="grid gap-3" onSubmit={(event) => {
      event.preventDefault();
      if (branch && !mutation.isPending) mutation.mutate(branch);
    }}>
      <label className="delivery-field">Branch name
        <input className="delivery-filter" value={name} autoCapitalize="none" spellCheck={false} required disabled={mutation.isPending}
          onChange={(event) => { setName(event.currentTarget.value); if (mutation.error) mutation.reset(); }} />
      </label>
      {mutation.error && <InlineIssue>{String(mutation.error)}</InlineIssue>}
      <Button type="submit" className="justify-self-start" disabled={!branch || mutation.isPending}>
        {mutation.isPending ? "Creating branch…" : "Create branch"}
      </Button>
    </form>
  </FileDialog>;
}

function WorktreeTable({ review, project, select, createFrom, createBranch, openCheckoutWorkspace, openAttempts }: {
  review: CheckoutReview; project: ProjectCheckout; select: (id: string, view?: Tab) => void;
  createFrom: (checkoutId: string, base: WorktreeBase) => void;
  createBranch: (checkoutId: string, base: CommitReference) => void;
  openCheckoutWorkspace: WorkspaceProps["openCheckoutWorkspace"];
  openAttempts: (worktree: Pick<ProjectCheckout, "checkoutId" | "checkoutRoot">) => void;
}) {
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  async function openWorktree(checkoutId: string, target?: string) {
    if (!openCheckoutWorkspace) return;
    setError("");
    setStatus("");
    try {
      const checkouts = await invoke<ProjectCheckout[]>("registered_checkouts");
      const checkout = checkouts.find((item) => item.checkoutId === checkoutId && item.projectId === project.projectId);
      if (!checkout) throw new Error("This worktree is no longer registered in the current project.");
      await openCheckoutWorkspace(checkout, { workspace: "ide", target });
    } catch (error) {
      setError(String(error));
    }
  }
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
              ...(openCheckoutWorkspace ? [
                { label: "Open worktree in IDE", run: () => void openWorktree(checkoutId) },
                { label: "Open terminal in worktree", run: () => void openWorktree(checkoutId, terminalFolderTarget("")) },
              ] : []),
              { label: "View saved attempts for worktree", run: () => openAttempts({ checkoutId, checkoutRoot: tree.root }) },
              ...(head ? [
                { label: "Create branch from this HEAD…", run: () => createBranch(checkoutId, { id: head, sha: head, label: branch ?? "Detached HEAD" }) },
                { label: "Create worktree from this HEAD…", run: () => createFrom(checkoutId, {
                  reference: head, expectedHead: head, label: `${branch ?? "Detached HEAD"} · ${head.slice(0, 12)}`,
                }) },
              ] : []),
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

function CommitHistory({ review, createFrom, createBranch }: {
  review: CheckoutReview; createFrom: (base: WorktreeBase) => void; createBranch: (base: CommitReference) => void;
}) {
  const [branch, setBranch] = useState("");
  const [selection, select] = useState<string>();
  const [comparison, compare] = useState<CommitComparison>();
  const clipboard = useContextActionFeedback();
  const client = useQueryClient();
  const branchRef = review.branches.find((entry) => entry.reference === branch);
  const expectedHead = branch ? branchRef?.sha : review.revision.head;
  const history = useQuery({
    queryKey: ["git-history", review.checkoutId, review.revision.head, branch, branchRef?.sha],
    queryFn: () => invoke<HistoryEntry[]>("git_checkout_history", { checkoutId: review.checkoutId, branch: branch || null }), retry: false,
  });
  const commits = history.error ? [] : history.data ?? [];
  const selected = commits.find((commit) => commit.sha === selection) ?? commits[0];
  const requestFor = (sha: string): CommitRequest | undefined => expectedHead ? { branch: branch || null, expectedHead, sha, parent: null } : undefined;
  const selectedRequest = selected && requestFor(selected.sha);
  const revisionOf = ({ sha, subject }: HistoryEntry): CommitReference => ({ id: sha, sha, label: subject || "Commit" });
  const references: CommitReference[] = [
    ...(review.revision.head ? [{ id: "HEAD", label: "Checkout HEAD", sha: review.revision.head }] : []),
    ...review.branches.map(({ reference, name, sha }) => ({ id: reference, label: name, sha })),
    ...commits.map(revisionOf),
  ];
  const historyReference = references.find(({ id }) => id === (branch || "HEAD"));
  function comparisonActions(base: CommitReference): MenuAction[] {
    const targets = references.filter(({ sha }) => sha !== base.sha);
    const checkoutId = review.checkoutId;
    return [
      ...(targets.some(({ id }) => id === "HEAD") ? [{ label: "Compare with checkout HEAD", run: () => compare({ checkoutId, base, targets, initialTarget: "HEAD" }) }] : []),
      ...(targets.some(({ id }) => id !== "HEAD") ? [{ label: "Compare with another reference…", run: () => compare({ checkoutId, base, targets }) }] : []),
    ];
  }
  function copy(text: string, copied: string) {
    void clipboard.run(() => navigator.clipboard.writeText(text), copied);
  }
  return <div className="delivery-body">
    <div className="delivery-main p-4">
      <ContextActions label="History branch" groups={historyReference ? [[
        ...comparisonActions(historyReference),
        { label: branchRef ? "Create branch from this ref…" : "Create branch from checkout HEAD…", run: () => createBranch(historyReference) },
        ...(branchRef ? [{ label: "Create worktree from this ref…", run: () => createFrom({
          reference: branchRef.reference, expectedHead: branchRef.sha, label: `${branchRef.name} · ${branchRef.sha.slice(0, 12)}`,
        }) }, { label: "Copy ref name", run: () => copy(branchRef.reference, "Copied ref name") }] : []),
      ]] : []}>
        <label className="delivery-field mb-3 max-w-72">History branch
          <select className="delivery-filter" value={branch} onChange={(event) => { setBranch(event.currentTarget.value); select(undefined); }}>
            <option value="">Current checkout HEAD</option>
            {review.branches.map((entry) => <option key={entry.reference} value={entry.reference}>{entry.name}</option>)}
          </select>
        </label>
      </ContextActions>
      {history.isPending && <p role="status" className="text-sm">Loading commits…</p>}
      {history.error && <InlineIssue>{String(history.error)}</InlineIssue>}
      {clipboard.feedback?.failed && <InlineIssue>{clipboard.feedback.text}</InlineIssue>}
      {clipboard.feedback && !clipboard.feedback.failed && <p role="status" className="mb-2 text-xs">{clipboard.feedback.text}</p>}
      {history.data?.length === 0 && <p className="text-sm text-muted-foreground">No commits in this branch.</p>}
      {commits.length > 0 && <GitWorkspaceTable label="Repository commit history" columns={["Commit", "Author", "Recorded"]}>
        {commits.map((commit) => <tr key={commit.sha}>
          <td><ContextActions label={`${commit.subject || "Commit"} ${commit.sha.slice(0, 12)}`} groups={[[
            { label: "Inspect commit", run: () => select(commit.sha) },
            ...comparisonActions(revisionOf(commit)),
            { label: "Create branch from this commit…", run: () => createBranch(revisionOf(commit)) },
            { label: "Create worktree from this commit…", run: () => createFrom({
              reference: commit.sha, expectedHead: commit.sha, label: `${commit.sha.slice(0, 12)} · ${commit.subject || "Commit"}`,
            }) },
          ], [
            { label: "Copy commit SHA", run: () => copy(commit.sha, "Copied commit SHA") },
            { label: "Copy short SHA", run: () => copy(commit.sha.slice(0, 12), "Copied short SHA") },
            ...(commit.subject ? [{ label: "Copy commit subject", run: () => copy(commit.subject, "Copied commit subject") }] : []),
            ...(expectedHead ? [{ label: "Copy full commit message", run: () => void clipboard.run(async () => {
              const request = requestFor(commit.sha)!;
              const details = await client.fetchQuery(commitDetailsQuery(review.checkoutId, request));
              await navigator.clipboard.writeText(details.message);
            }, "Copied commit message") }] : []),
          ]]}><button type="button" onClick={() => select(commit.sha)} aria-pressed={selected?.sha === commit.sha} className="grid gap-1 text-left font-medium hover:underline">
            {commit.subject || "Commit"}<code className="text-xs text-muted-foreground">{commit.sha.slice(0, 12)}</code>
          </button></ContextActions></td>
          <td>{commit.author}</td>
          <td className="whitespace-nowrap text-muted-foreground"><time dateTime={commit.committedAt}>{commit.committedAt}</time></td>
        </tr>)}
      </GitWorkspaceTable>}
    </div>
    {selectedRequest && <CommitDetailPanel key={JSON.stringify(selectedRequest)} checkoutId={review.checkoutId} selection={selectedRequest}
      historyRef={branchRef?.reference ?? review.revision.branch ?? "Checkout HEAD"} />}
    {comparison && <CommitRangeDialog comparison={comparison} close={() => compare(undefined)} />}
  </div>;
}

function CommitDetailPanel({ checkoutId, selection, historyRef }: { checkoutId: string; selection: CommitRequest; historyRef: string }) {
  const [parent, setParent] = useState<string | null>(null);
  const [filePath, showFile] = useState<string>();
  const clipboard = useContextActionFeedback();
  const request = { ...selection, parent };
  const details = useQuery({ ...commitDetailsQuery(checkoutId, request), placeholderData: keepPreviousData });
  const commit = details.error ? undefined : details.data;
  return <DeliveryDetailPanel label="Selected commit" resizeKey="git-commit">
    <div className="delivery-detail-content grid content-start gap-3 p-3.5">
      <h3 className="flex items-center gap-2 text-sm font-semibold"><GitCommitHorizontal size={17} aria-hidden /> Selected commit</h3>
      <dl className="grid gap-2 text-xs">
        <div><dt className="text-muted-foreground">Revision</dt><dd className="wrap-anywhere font-mono">{selection.sha}</dd></div>
        <div><dt className="text-muted-foreground">History ref</dt><dd className="wrap-anywhere font-mono">{historyRef}</dd></div>
      </dl>
      {(details.isPending || details.isPlaceholderData) && <p role="status" className="text-xs">Loading commit details…</p>}
      {details.error && <InlineIssue>{String(details.error)}</InlineIssue>}
      {clipboard.feedback?.failed && <InlineIssue>{clipboard.feedback.text}</InlineIssue>}
      {clipboard.feedback && !clipboard.feedback.failed && <p role="status" className="text-xs">{clipboard.feedback.text}</p>}
      {commit && <>
        <ContextActions label="Commit message" groups={[[
          { label: "Copy full commit message", run: () => void clipboard.run(() => navigator.clipboard.writeText(commit.message), "Copied commit message") },
        ]]}>
          <pre aria-label="Commit message" className="min-w-0 whitespace-pre-wrap break-words font-mono text-xs">{commit.message}</pre>
        </ContextActions>
        <section aria-label="Files changed in this commit" className="grid gap-2">
          <h4 className="text-sm font-semibold">Changed files</h4>
          {commit.parents.length > 1 ? <label className="delivery-field">Compare with parent
            <select className="delivery-filter max-w-full" value={parent ?? commit.parent ?? ""} onChange={(event) => { setParent(event.currentTarget.value); showFile(undefined); }}>
              {commit.parents.map((sha, index) => <option key={sha} value={sha}>Parent {index + 1} · {sha.slice(0, 12)}</option>)}
            </select>
          </label> : <p className="break-words font-mono text-xs text-muted-foreground">{commit.parent ? `Parent ${commit.parent.slice(0, 12)}` : "Root commit · Empty tree"}</p>}
          {!details.isPlaceholderData && (commit.comparisonUnavailable ? <InlineIssue>{commit.comparisonUnavailable}</InlineIssue>
            : commit.files.length === 0 ? <p className="text-xs text-muted-foreground">No files changed against this base.</p> : <ul className="divide-y divide-border rounded border border-border">
            {commit.files.map((file) => <li key={file.path}><ContextActions label={file.path} groups={[[
              { label: commit.parent ? "Compare with parent" : "Compare with empty tree", run: () => showFile(file.path) },
            ], [
              { label: "Copy relative path", run: () => clipboard.copy(file.path) },
              ...(file.previousPath ? [{ label: "Copy previous relative path", run: () => clipboard.copy(file.previousPath!) }] : []),
            ]]}>
              <button type="button" onClick={() => showFile(file.path)} className="flex min-w-0 flex-1 flex-wrap items-start justify-between gap-1 px-2 py-1.5 text-left text-xs hover:bg-muted">
                <code className="min-w-0 wrap-anywhere">{file.previousPath && `${file.previousPath} → `}{file.path}</code>
                <span className="text-muted-foreground">{commitChangeLabels[file.status] ?? file.status}</span>
              </button>
            </ContextActions></li>)}
          </ul>)}
        </section>
        {filePath && <CommitFileDiff checkoutId={checkoutId} request={{ ...request, parent: commit.parent }} files={commit.files}
          initialPath={filePath} close={() => showFile(undefined)} />}
      </>}
    </div>
  </DeliveryDetailPanel>;
}

function CommitFileDiff({ checkoutId, request, files, initialPath, close }: {
  checkoutId: string; request: CommitRequest; files: CommitFile[]; initialPath: string; close: () => void;
}) {
  return <FileDialog title="Committed file comparison" description={request.sha} className="file-comparison-dialog" onClose={close}>
    <div className="file-comparison-body">
      <CommittedDiff files={files} initialPath={initialPath} fileKey={["git-commit-diff", checkoutId, request]}
        ranges={{ base: request.parent ? `Parent ${request.parent.slice(0, 12)}` : "Empty tree", revised: `Commit ${request.sha.slice(0, 12)}` }}
        load={(path) => loadCommitFile(checkoutId, request, path)} />
    </div>
  </FileDialog>;
}

function CommitRangeDialog({ comparison, close }: { comparison: CommitComparison; close: () => void }) {
  const { checkoutId, base, targets, initialTarget } = comparison;
  const [target, setTarget] = useState(initialTarget ?? "");
  const head = targets.find(({ id }) => id === target);
  const range = head && { base: base.sha, head: head.sha };
  const files = useQuery({
    queryKey: ["git-commit-range", checkoutId, range],
    queryFn: () => invoke<CommitFile[]>("git_compare_commits", { checkoutId, range }),
    enabled: Boolean(range), retry: false,
  });
  const label = (ref: CommitReference) => `${ref.label} · ${ref.sha.slice(0, 12)}`;
  return <FileDialog title="Compare commits" description={label(base)} className="file-comparison-dialog" onClose={close}>
    <label className="delivery-field">Compare with
      <select className="delivery-filter max-w-full" value={target} onChange={(event) => setTarget(event.currentTarget.value)}>
        <option value="" disabled>Select a reference or commit</option>
        {targets.map((ref) => <option key={ref.id} value={ref.id}>{label(ref)}</option>)}
      </select>
    </label>
    {head && range && (files.error ? <InlineIssue>{String(files.error)}</InlineIssue> : files.isPending ?
      <p role="status" className="text-sm">Loading committed changes…</p> : <div className="file-comparison-body">
        <CommittedDiff key={head.id} files={files.data} fileKey={["git-commit-range-diff", checkoutId, range]}
          ranges={{ base: label(base), revised: label(head) }} load={(path) => invoke("git_compare_commit_file", { checkoutId, range, path })} />
      </div>)}
  </FileDialog>;
}

function CommittedDiff({ files, initialPath, fileKey, ranges, load }: {
  files: CommitFile[]; initialPath?: string; fileKey: readonly unknown[]; ranges: { base: string; revised: string };
  load: (path: string) => Promise<GitComparedFile>;
}) {
  const [path, select] = useState(initialPath ?? files[0]?.path ?? "");
  const diff = useQuery({
    queryKey: [...fileKey, path],
    queryFn: () => load(path),
    enabled: Boolean(path), retry: false,
  });
  const snapshot = diff.error ? undefined : diff.data;
  const previousPath = files.find((file) => file.path === path)?.previousPath ?? undefined;
  const unavailable = snapshot && [snapshot.original, snapshot.modified].find((side) => side.status === "unavailable");
  return <GitDiffReview source="commit" title="Commit changes" ranges={ranges}
    files={files.map((file) => ({ path: file.path, previousPath: file.previousPath ?? undefined, kind: commitChangeLabels[file.status] ?? file.status }))}
    selectedPath={path} select={select} comparison={comparisonFile(snapshot, previousPath)} empty={<p>No committed changes.</p>}
    comparedFiles={{ selected: snapshot, load }}
    preview={path && (diff.isPending ? <p role="status" className="p-3 text-sm">Loading committed file versions…</p>
      : diff.error || unavailable ? <div className="p-3"><InlineIssue>{diff.error ? String(diff.error) : unavailable?.reason}</InlineIssue></div> : undefined)} />;
}
