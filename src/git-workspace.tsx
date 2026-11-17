import { ArrowDownToLine, ArrowRight, ChevronDown, FileDiff, GitBranch, GitCommitHorizontal, GitMerge, Plus, RefreshCw } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import ChangesPanel from "@/changes-panel";
import { LiveGitWorkspace } from "@/git-live-workspace";
import { Button } from "@/components/ui/button";
import { deliveryGitModes, deliveryGitSetting, deliveryGitWait, type DeliveryGitPolicy } from "@/delivery-git-policy";
import { releaseCommits, revisionMessage, workingChanges, type ChangesSource } from "@/git-demo-data";
import { useGitSimulation } from "@/git-simulation";
import { GitDiffSourceSelect, GitWorkspaceFrame, GitWorkspaceTable } from "@/git-workspace-frame";
import { GitWorktreeDialog } from "@/git-worktree-dialog";
import { deliveryBase, deliveryBranch, deliveryCommit, deliveryCommitSha, deliveryMergeSha, deliveryRunId, deliveryTaskId } from "@/tasks-demo-data";
import { DeliveryDetailPanel, DeliveryStatus, InlineIssue, IntegrationNotice } from "@/tasks-git-ui";
import type { WorkspaceProps } from "@/workbench-contract";

const gitViews = ["Changes", "Worktrees", "History"] as const;
type GitView = typeof gitViews[number];
const sources: ChangesSource[] = ["working", "staged", "branch", "saved"];

function EmptyGitView({ view, props }: { view: GitView; props: WorkspaceProps }) {
  const labels = {
    Changes: "Git status and staging will appear when the checkout service is connected.",
    Worktrees: "Registered worktrees, branch bases and writer ownership will appear when worktree management is connected.",
    History: "Repository commits and captured candidate revisions will appear when history is connected.",
  };
  return <div className="delivery-main"><IntegrationNotice title={`${view} integration pending`} onSimulate={props.startSimulation}>{labels[view]}</IntegrationNotice></div>;
}

function WorktreeDialog({ onClose }: { onClose: () => void }) {
  const git = useGitSimulation();
  return <GitWorktreeDialog
    revisions={releaseCommits.map((commit) => ({ value: commit.id, label: `${commit.id} · ${commit.branch}` }))}
    defaultBase={deliveryBase} pathHint="worktrees/my-task"
    defaultPath={(branch) => branch ? `worktrees/${branch.replaceAll("/", "-")}` : ""}
    validate={({ branch, path }) => git.worktrees.some((tree) => tree.branch === branch || tree.path === path)
      ? "A worktree with that branch or path already exists." : undefined}
    create={({ branch, path, base }) => { git.addWorktree(branch, path, base); onClose(); }} onClose={onClose} />;
}

function Worktrees({ props }: { props: WorkspaceProps }) {
  const git = useGitSimulation();
  const simulation = props.simulation;
  if (!simulation) return <EmptyGitView view="Worktrees" props={props} />;

  return (
    <div className="delivery-main p-4">
      <div className="mb-4 flex flex-wrap items-center justify-between gap-2">
        <div><h3 className="text-sm font-semibold">Registered worktrees</h3><p className="mt-1 text-xs text-muted-foreground">Writer reservations and captured candidates</p></div>
        <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" onClick={() => props.openWorkspace("tasks", deliveryTaskId)}>Open linked task <ArrowRight size={14} /></Button>
      </div>
      <GitWorkspaceTable label="Git worktrees" columns={["Branch / worktree", "Base", "Writer", "State", "Action"]}>
        {git.worktrees.map((tree) => {
            const activeWriter = tree.id === "release" && simulation.delivery === "running";
            const interrupted = tree.id === "release" && simulation.delivery === "interrupted";
            const state = activeWriter ? "Running" : interrupted ? "Interrupted" : git.checkout === tree.id ? "Selected" : "Ready";
            return <tr key={tree.id}>
              <td><div className="flex items-center gap-2"><GitBranch size={14} className="text-muted-foreground" /><span className="font-medium">{tree.branch}</span></div><p className="mt-1 font-mono text-xs text-muted-foreground">{tree.path}</p></td>
              <td className="font-mono">{tree.base}</td>
              <td>{activeWriter ? "GPT-6.1 Sol" : interrupted ? "Checkpoint captured" : "Available"}</td>
              <td><DeliveryStatus>{state}</DeliveryStatus></td>
              <td>{interrupted
                ? <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" onClick={() => simulation.advanceDelivery("resume")}>Resume interrupted writer</Button>
                : <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" disabled={git.checkout === tree.id} onClick={() => git.setCheckout(tree.id)}>{git.checkout === tree.id ? "Selected" : "Use worktree"}</Button>}</td>
            </tr>;
        })}
      </GitWorkspaceTable>
      {simulation.delivery === "interrupted" && <div className="mt-4 grid gap-2"><InlineIssue>The release writer disconnected. Resume from its retained checkpoint to avoid an ambiguous write.</InlineIssue><Button type="button" variant="outline" className="justify-self-start px-2.5 py-1.5 text-xs" onClick={() => props.openActivity(deliveryRunId)}>Review checkpoint</Button></div>}
    </div>
  );
}

function GitHistory({ props }: { props: WorkspaceProps }) {
  const git = useGitSimulation();
  const [selected, setSelected] = useState(deliveryCommit);
  const simulation = props.simulation;
  if (!simulation) return <EmptyGitView view="History" props={props} />;
  const commits = git.commitMessage && git.commitMessage !== revisionMessage
    ? [{ id: "61cf502", title: git.commitMessage, branch: git.worktrees.find((tree) => tree.id === git.checkout)?.branch ?? deliveryBranch, author: "Operator", date: "Current session" }, ...releaseCommits]
    : releaseCommits;
  const commit = commits.find((entry) => entry.id === selected) ?? commits[0];
  const isCandidate = commit?.id === deliveryCommit && ["review", "verified", "accepted", "merged"].includes(simulation.delivery);

  return (
    <div className="delivery-body">
      <div className="delivery-main p-4">
        <h3 className="mb-3 text-sm font-semibold">Commit history</h3>
        <GitWorkspaceTable label="Repository commit history" columns={["Commit", "Branch", "Author", "Recorded"]}>
          {commits.map((entry) => <tr key={entry.id}>
            <td><button type="button" onClick={() => setSelected(entry.id)} aria-pressed={entry.id === commit?.id} className="grid gap-1 text-left font-medium hover:underline">{entry.title}<span className="text-xs text-muted-foreground font-mono">{entry.id}</span></button></td>
            <td className="whitespace-nowrap">{entry.branch}</td><td>{entry.author}</td><td className="whitespace-nowrap text-muted-foreground">{entry.date}</td>
          </tr>)}
        </GitWorkspaceTable>
      </div>
      {commit && <DeliveryDetailPanel label="Selected commit" resizeKey="git-commit">
        <div className="delivery-detail-content grid content-start gap-3 p-3.5">
          <GitCommitHorizontal size={19} className="text-muted-foreground" />
          <h3 className="text-sm font-semibold">{commit.title}</h3>
          <p className="font-mono text-xs">{commit.id} · {commit.branch}</p>
          <p className="text-sm text-muted-foreground">{isCandidate ? "Captured candidate associated with the release task." : "No attempt snapshot is attached to this revision."}</p>
          {isCandidate && <Button type="button" variant="outline" className="justify-self-start px-2.5 py-1.5 text-xs" onClick={() => props.openChanges("saved")}><FileDiff size={15} /> Inspect saved diff</Button>}
        </div>
      </DeliveryDetailPanel>}
    </div>
  );
}

function ChangeOperations({ props, source, baseRef }: { props: WorkspaceProps; source: ChangesSource; baseRef: string }) {
  const git = useGitSimulation();
  const [commitMessage, setCommitMessage] = useState("");
  const simulation = props.simulation;
  if (!simulation) return <EmptyGitView view="Changes" props={props} />;
  const live = git.checkout === "release" ? workingChanges.filter(({ path }) => !git.committed.has(path)) : [];
  const writerBusy = git.checkout === "release" && ["running", "interrupted"].includes(simulation.delivery);
  const candidateAccepted = simulation.delivery === "accepted" || simulation.delivery === "merged";
  const mergeReady = simulation.delivery === "accepted" && !git.drift && git.candidateCommitted;

  function commitStaged(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!writerBusy && !git.drift && git.staged.size && commitMessage.trim()) {
      git.commit(git.staged, commitMessage);
      setCommitMessage("");
    }
  }

  function moveDestination(reason: "advanced" | "conflict") {
    git.setDrift(reason);
    simulation?.advanceDelivery("invalidate");
  }

  return (
    <div className="delivery-body">
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <ChangesPanel {...props} source={source} baseRef={baseRef} />
      </div>
      <DeliveryDetailPanel label="Git operations" resizeKey="git-operations">
        <div className="delivery-detail-content grid content-start gap-3 p-3.5">
          <h3 className="text-sm font-semibold">{source === "saved" ? "Candidate review" : "Git operations"}</h3>
          {writerBusy && <InlineIssue>This worktree has a reserved writer. Review the active or interrupted attempt to release it.</InlineIssue>}
          {git.drift === "advanced" && <InlineIssue>The destination advanced to <code>4a71b3e</code>. Refresh the comparison and recheck the saved candidate before merging.</InlineIssue>}
          {git.drift === "conflict" && <InlineIssue>Merge conflict in <code>docs/release.md</code>. Review the saved candidate and confirm the recovery edit before rerunning verification.</InlineIssue>}
          {source === "saved" ? <>
            <dl className="grid grid-cols-[auto_1fr] gap-2 text-sm">
              <dt className="text-muted-foreground">Base</dt><dd className="font-mono">{deliveryBase}</dd>
              <dt className="text-muted-foreground">Candidate</dt><dd className="font-mono">{deliveryCommit}</dd>
              <dt className="text-muted-foreground">Commit</dt><dd>{git.candidateCommitted ? <><span>{deliveryBranch}</span><code className="block wrap-anywhere text-xs">{deliveryCommitSha}</code></> : candidateAccepted ? deliveryGitWait.commit : "Waiting for acceptance"}</dd>
              <dt className="text-muted-foreground">Merge</dt><dd>{simulation.delivery === "merged" ? <><span>Merged into destination</span><code className="block wrap-anywhere text-xs">main · {deliveryMergeSha}</code></> : git.candidateCommitted && candidateAccepted ? deliveryGitWait.merge : "Requires a verified commit"}</dd>
            </dl>
            <label className="delivery-field">{deliveryGitSetting.label}<select className="delivery-filter" value={git.gitMode} onChange={(event) => git.setGitMode(event.target.value as DeliveryGitPolicy)}>
              {deliveryGitModes.map(([mode, label]) => <option key={mode} value={mode}>{label}</option>)}
            </select></label>
            {simulation.delivery === "accepted" && <>
              <Button className="justify-self-start px-2.5 py-1.5 text-xs" type="button" disabled={Boolean(git.drift) || git.candidateCommitted} onClick={git.commitCandidate}><GitCommitHorizontal size={15} /> Commit accepted candidate</Button>
              <Button className="justify-self-start px-2.5 py-1.5 text-xs" type="button" variant="outline" disabled={!mergeReady} onClick={() => simulation.advanceDelivery("merge")}><GitMerge size={15} /> Merge into destination</Button>
            </>}
            <Button className="justify-self-start px-2.5 py-1.5 text-xs" type="button" variant="outline" onClick={() => props.openWorkspace("tasks", deliveryTaskId)}>Open linked task <ArrowRight size={14} /></Button>
          </> : <>
            <h4 className="text-xs font-semibold">Stage files <span className="text-muted-foreground">({git.staged.size})</span></h4>
            {live.length
              ? <ul className="grid gap-1.5">{live.map((file) => <li key={file.path} className="flex items-center gap-1.5 rounded border border-border p-1.5"><FileDiff size={14} className="shrink-0 text-muted-foreground" /><span className="min-w-0 flex-1 break-all font-mono text-xs">{file.path}</span><button type="button" className="delivery-tab text-xs" disabled={writerBusy} onClick={() => git.toggleStage(file.path)}>{git.staged.has(file.path) ? "Unstage" : "Stage"}</button></li>)}</ul>
              : <p className="text-sm text-muted-foreground">This worktree has a clean index and working tree.</p>}
            <form className="grid gap-2" onSubmit={commitStaged}>
              <label className="delivery-field">Commit message<input value={commitMessage} onChange={(event) => setCommitMessage(event.target.value)} className="delivery-filter" disabled={!git.staged.size || writerBusy} /></label>
              <Button className="justify-self-start px-2.5 py-1.5 text-xs" type="submit" variant="outline" disabled={!git.staged.size || !commitMessage.trim() || writerBusy || Boolean(git.drift)}><ArrowDownToLine size={15} /> Commit staged</Button>
            </form>
          </>}
          {["review", "verified", "accepted"].includes(simulation.delivery) && (
            git.drift
              ? <Button className="justify-self-start px-2.5 py-1.5 text-xs" variant="outline" type="button" onClick={() => git.setDrift(false)}><RefreshCw size={15} /> {git.drift === "conflict" ? "Resolve with candidate edit" : "Refresh destination"}</Button>
              : <div className="flex flex-wrap gap-2">
                <Button className="px-2.5 py-1.5 text-xs" variant="outline" type="button" onClick={() => moveDestination("advanced")}>Simulate destination change</Button>
                <Button className="px-2.5 py-1.5 text-xs" variant="outline" type="button" onClick={() => moveDestination("conflict")}>Simulate merge conflict</Button>
              </div>
          )}
        </div>
      </DeliveryDetailPanel>
    </div>
  );
}

export default function GitWorkspace(props: WorkspaceProps) {
  return props.simulation ? <SimulatedGitWorkspace {...props} /> : <LiveGitWorkspace {...props} />;
}

function SimulatedGitWorkspace(props: WorkspaceProps) {
  const { simulation, target } = props;
  const git = useGitSimulation();
  const [view, setView] = useState<GitView>("Changes");
  const [source, setSource] = useState<ChangesSource>("working");
  const [baseRef, setBaseRef] = useState("origin/main");
  const [creatingWorktree, setCreatingWorktree] = useState(false);

  useEffect(() => {
    if (target === "worktrees") setView("Worktrees");
    else if (target === "history") setView("History");
    else if (target === "saved" || target === "working") { setSource(target); setView("Changes"); }
  }, [target]);

  return (
    <GitWorkspaceFrame tabs={gitViews} active={view} select={setView} controls={<>
        {simulation && <label className="relative ml-auto"><span className="sr-only">Git worktree</span><select className="delivery-filter appearance-none pr-7" value={git.checkout} onChange={(event) => git.setCheckout(event.target.value)}>{git.worktrees.map((tree) => <option key={tree.id} value={tree.id}>{tree.branch}</option>)}</select><ChevronDown size={13} className="pointer-events-none absolute right-2 top-2 text-muted-foreground" /></label>}
        {view === "Worktrees" && <Button type="button" variant="outline" className="ml-auto px-2.5 py-1.5 text-xs" disabled={!simulation} title={simulation ? undefined : "Worktree integration pending"} onClick={() => setCreatingWorktree(true)}><Plus size={14} /> New worktree</Button>}
    </>}>
      {view === "Changes" && <div className="delivery-toolbar min-h-0 py-2">
        <GitDiffSourceSelect sources={sources} value={source} onChange={setSource} />
        {source === "branch" && <label><span className="sr-only">Base reference</span><select className="delivery-filter" value={baseRef} onChange={(event) => setBaseRef(event.target.value)}><option>origin/main</option><option>main</option></select></label>}
        {simulation && (source === "working" || source === "saved") && <Button type="button" variant="outline" className="ml-auto px-2.5 py-1.5 text-xs" onClick={() => props.openChanges(source === "saved" ? "saved" : "working")}>Open changes panel</Button>}
      </div>}
      {view === "Changes" && <ChangeOperations props={props} source={source} baseRef={baseRef} />}
      {view === "Worktrees" && <Worktrees props={props} />}
      {view === "History" && <GitHistory props={props} />}
      {simulation && creatingWorktree && <WorktreeDialog onClose={() => setCreatingWorktree(false)} />}
    </GitWorkspaceFrame>
  );
}
