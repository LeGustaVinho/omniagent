import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ArrowRight, ListTodo, Pause, Play, Plus, RefreshCw, X } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import type { Job } from "@/bindings/Job";
import type { JobInspection } from "@/bindings/JobInspection";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import type { QueueState } from "@/bindings/QueueState";
import type { RecoveryAction } from "@/bindings/RecoveryAction";
import type { SessionMode } from "@/bindings/SessionMode";
import { JobFailurePanel } from "@/job-failure";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { matchesQueueView, projectQueueQuery, queueBlockerReason, queueJobState, queueViews, type QueueView } from "@/queue-model";
import { QueuePolicy } from "@/queue-policy";
import { DeliveryStatus, InlineIssue } from "@/tasks-git-ui";
import type { QueueJobFocus, WorkspaceProps } from "@/workbench-contract";

type QueueAction =
  | { type: "pause_queue"; paused: boolean }
  | { type: "pending_state"; jobId: string; state: QueueState }
  | { type: "approval"; approvalId: string; approved: boolean }
  | { type: "interrupt"; jobId: string; runId: string }
  | { type: "recovery"; jobId: string; runId: string; action: RecoveryAction };

function applyQueueAction(projectId: string, action: QueueAction) {
  switch (action.type) {
    case "pause_queue":
      return invoke("set_job_queue_paused", { paused: action.paused });
    case "pending_state":
      return invoke("set_pending_job_state", { projectId, jobId: action.jobId, state: action.state });
    case "approval":
      return invoke("decide_job_approval", { projectId, approvalId: action.approvalId, approved: action.approved });
    case "interrupt":
      return invoke("interrupt_queued_job_attempt", { projectId, jobId: action.jobId, runId: action.runId });
    case "recovery":
      return invoke("queue_job_recovery", { projectId, jobId: action.jobId, runId: action.runId, action: action.action });
  }
}

function jobTitle({ job }: JobInspection) {
  if (job.kind === "side") return "Isolated request";
  if (job.kind === "command") return "Command job";
  if (job.context && job.origin.type === "operator") return "Conversation turn";
  if (job.origin.type === "agent") return "Agent job";
  return job.origin.type === "pipeline" ? "Pipeline job" : "Operator job";
}

function jobRequester({ job }: JobInspection) {
  switch (job.origin.type) {
    case "operator": return "Operator";
    case "agent": return `${job.operatorRequested ? "Operator · parent run" : "Agent"} · ${job.origin.runId}`;
    case "pipeline": return "Pipeline";
  }
}

function firstWaitReason(inspection: JobInspection) {
  const { job, blockers } = inspection;
  const blocker = blockers[0];
  if (blocker) return queueBlockerReason(blocker);
  if (job.queuedRecovery) return job.queuedRecovery === "resume" ? "Checkpoint queued for resumption." : "Saved input queued for retry.";
  if (job.downstreamWait) return "Selected downstream results are ready.";
  if (job.latestAttempt?.cancelRequested) return "Cancellation requested.";
  if (job.latestAttempt) return job.latestAttempt.status === "waiting_for_input" ? "The current attempt needs input." : "—";
  if (job.queueState === "queued") return "Waiting to start.";
  return "—";
}

const buttonClass = "px-2.5 py-1.5 text-xs";

type ParentAttempt = { runId: string; mode: SessionMode };

function ManagedJobForm({ checkout, parent, onQueued, onClose }: Pick<WorkspaceProps, "checkout"> & {
  parent?: ParentAttempt;
  onQueued: (id: string) => void;
  onClose: () => void;
}) {
  const [provider, setProvider] = useState<Provider>("codex");
  const [selection, setSelection] = useState<ModelSelection | null>(null);
  const [mode, setMode] = useState<SessionMode>("read_only");
  const [instructions, setInstructions] = useState("");
  const models = useQuery({
    queryKey: ["conversation-models", provider],
    queryFn: () => invoke<{ option: ModelOption }[]>("conversation_models", { provider }),
    staleTime: 60_000,
  });
  const options = models.data?.map(({ option }) => option) ?? [];
  const selected = options.find(({ selection: model, selectable }) => selectable && model.provider === selection?.provider
    && model.profileId === selection.profileId && model.modelId === selection.modelId);
  const enqueue = useMutation({
    mutationFn: (model: ModelSelection) => invoke<Job>("enqueue_managed_agent_job", {
      projectId: checkout.projectId, checkoutId: checkout.checkoutId, parentRunId: parent?.runId ?? null,
      selection: model, mode, instructions: instructions.trim(),
    }),
    onSuccess: ({ id }) => onQueued(id),
  });

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (selected && instructions.trim() && !enqueue.isPending) enqueue.mutate(selected.selection);
  }

  return (
    <form aria-label={parent ? "Managed subagent" : "Managed job"} className="grid gap-3 border-b border-border p-3" onSubmit={submit}>
      <label className="delivery-field">Instructions<textarea required className="delivery-filter min-h-20" value={instructions}
        onChange={(event) => setInstructions(event.target.value)} disabled={enqueue.isPending} /></label>
      <div className="grid gap-3 sm:grid-cols-3">
        <label className="delivery-field">CLI<select className="delivery-filter" value={provider} disabled={enqueue.isPending} onChange={(event) => {
          setProvider(event.target.value as Provider);
          setSelection(null);
        }}><option value="codex">Codex</option><option value="claude">Claude Code</option><option value="antigravity">Antigravity</option></select></label>
        <ModelPicker label="Model" options={options} loading={models.isPending} disabled={enqueue.isPending}
          selection={{ mode: "single", selected: selection, onChange: setSelection }} />
        <label className="delivery-field">Operation<select className="delivery-filter" value={mode} disabled={enqueue.isPending}
          onChange={(event) => setMode(event.target.value as SessionMode)}>
          <option value="read_only">Read only</option><option value="plan">Plan</option>
          {(!parent || parent.mode === "approved_write") && <option value="approved_write">Repository write</option>}
        </select></label>
      </div>
      {models.isError && <div className="flex flex-wrap items-center gap-2"><InlineIssue>{String(models.error)}</InlineIssue>
        <Button type="button" variant="outline" className={buttonClass} disabled={models.isFetching} onClick={() => void models.refetch()}>Refresh models</Button></div>}
      {enqueue.isError && <InlineIssue>{String(enqueue.error)}</InlineIssue>}
      <div className="flex gap-2">
        <Button type="submit" className={buttonClass} disabled={!selected || !instructions.trim() || enqueue.isPending}>{parent ? "Queue subagent" : "Queue job"}</Button>
        <Button type="button" variant="outline" className={buttonClass} disabled={enqueue.isPending} onClick={onClose}>Cancel</Button>
      </div>
    </form>
  );
}

function ApprovalReview({ projectId, approvalId, busy, onAction }: {
  projectId: string;
  approvalId: string;
  busy: boolean;
  onAction: (action: QueueAction) => void;
}) {
  const request = useQuery({
    queryKey: ["job-approval-request", projectId, approvalId],
    queryFn: () => invoke<{ instructions: string }>("pending_job_approval_request", { projectId, approvalId }),
  });
  return (
    <div className="grid gap-2">
      {request.isPending && <p role="status">Loading the approval request…</p>}
      {request.isError && <p role="alert">{String(request.error)}</p>}
      {request.data && <pre aria-label="Requested instructions" className="max-h-48 overflow-auto whitespace-pre-wrap break-words rounded bg-muted p-2">{request.data.instructions}</pre>}
      <div className="flex flex-wrap gap-2">
        <Button type="button" className={buttonClass} disabled={busy || !request.isSuccess}
          onClick={() => onAction({ type: "approval", approvalId, approved: true })}>Approve job</Button>
        <Button type="button" variant="outline" className={buttonClass} disabled={busy}
          onClick={() => onAction({ type: "approval", approvalId, approved: false })}>Decline job</Button>
      </div>
    </div>
  );
}

function JobDetails({ inspection, checkout, onClose, openWorkspace, openActivity, onDependency, onAction, onAddChild, busy }: Pick<WorkspaceProps, "checkout" | "openActivity" | "openWorkspace"> & {
  inspection: JobInspection;
  onClose: () => void;
  onDependency: (id: string) => void;
  onAction: (action: QueueAction) => void;
  onAddChild: (parent: ParentAttempt) => void;
  busy: boolean;
}) {
  const { job, blockers } = inspection;
  const attempt = job.latestAttempt;
  const ownCheckout = job.checkoutId === checkout.checkoutId;
  const conversationId = attempt?.conversationId
    ?? (job.origin.type === "operator" ? job.context?.conversationId : undefined);
  const requestingRunId = job.origin.type === "agent" ? job.origin.runId : undefined;
  const pending = job.queueState !== "cancelled" && (!attempt || job.queuedRecovery !== null ||
    Boolean(job.downstreamWait && job.downstreamWait.state !== "yielding") || inspection.queuedQuestion);
  const running = attempt && ["starting", "running", "waiting_for_input"].includes(attempt.status);
  const savedQuestions = inspection.questionWait && !running && !pending && job.queueState !== "cancelled";

  return (
    <aside className="delivery-detail" aria-label="Selected job">
      <div className="delivery-detail-content grid content-start gap-3 p-3.5">
        <div className="flex items-start justify-between gap-2"><h3 className="text-sm font-semibold">{jobTitle(inspection)}</h3><button type="button" aria-label="Close job details" className="rounded p-1 text-muted-foreground hover:bg-muted" onClick={onClose}><X size={15} /></button></div>
        <DeliveryStatus>{queueJobState(inspection)}</DeliveryStatus>
        {attempt?.cancelRequested && <InlineIssue>Cancellation requested. Waiting for the runtime to confirm the outcome.</InlineIssue>}
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-sm">
          <dt className="text-muted-foreground">Job ID</dt><dd className="break-all font-mono text-xs">{job.id}</dd>
          <dt className="text-muted-foreground">Requester</dt><dd className="break-all">{jobRequester(inspection)}</dd>
          <dt className="text-muted-foreground">Worktree</dt><dd className="break-all font-mono text-xs">{ownCheckout ? checkout.checkoutRoot : job.checkoutId}</dd>
          <dt className="text-muted-foreground">Profile mode</dt><dd>{job.mode.replaceAll("_", " ")}</dd>
          <dt className="text-muted-foreground">Priority</dt><dd>{job.priority}</dd>
          {job.pipeline && <>
            <dt className="text-muted-foreground">Pipeline run</dt><dd className="break-all">{job.pipeline.pipelineRunId}</dd>
            <dt className="text-muted-foreground">Node instance</dt><dd className="break-all">{job.pipeline.nodeInstanceId}</dd>
            {job.pipeline.planRevision && <><dt className="text-muted-foreground">Plan revision</dt><dd className="break-all">{job.pipeline.planRevision}</dd></>}
            {job.pipeline.release && <>
              <dt className="text-muted-foreground">Implementation release</dt><dd className="break-all">{job.pipeline.release.id}</dd>
              <dt className="text-muted-foreground">Released scope</dt><dd className="break-all">{job.pipeline.release.taskId} · {job.pipeline.release.checkoutId}</dd>
            </>}
          </>}
          <dt className="text-muted-foreground">Attempt</dt><dd>{attempt ? `#${attempt.ordinal}` : "Awaiting dispatch"}</dd>
          {attempt && <><dt className="text-muted-foreground">Run ID</dt><dd className="break-all font-mono text-xs">{attempt.runId}</dd></>}
          {attempt?.recovery && <><dt className="text-muted-foreground">Recovery</dt><dd>{attempt.recovery === "resume" ? "Resumed from checkpoint" : "Retried saved input"}</dd></>}
        </dl>
        {attempt && !inspection.questionWait && (!job.downstreamWait || job.downstreamWait.state === "needs_review")
          && (attempt.status === "failed" || attempt.status === "interrupted") &&
          <JobFailurePanel projectId={job.projectId} runId={attempt.runId}
            openLogs={() => openActivity(attempt.runId, attempt.conversationId ?? undefined)} />}
        {job.downstreamWait && <section className="grid gap-1.5" aria-label="Downstream jobs">
          <h4 className="text-xs font-semibold">Downstream jobs</h4>
          <ul className="divide-y divide-border rounded border border-border">
            {job.downstreamWait.inputs.map((input) => <li key={input.childJobId} className="grid gap-1 p-2 text-xs">
              <button type="button" className="break-all text-left text-primary hover:underline" onClick={() => onDependency(input.childJobId)}>
                {input.childJobId}
              </button>
              <span className="capitalize">{input.status ?? "Pending result"}</span>
            </li>)}
          </ul>
        </section>}
        {blockers.length > 0 && <section className="grid gap-1.5" aria-label="Admission blockers">
          <h4 className="text-xs font-semibold">Admission blockers</h4>
          <ul className="divide-y divide-border rounded border border-border">
            {blockers.map((blocker, index) => <li key={`${blocker.type}-${index}`} className="grid gap-1.5 p-2 text-xs leading-5">
              <p className="break-words">{queueBlockerReason(blocker)}</p>
              {attempt && blocker.type === "dispatch_rejected" && blocker.rejection.type === "requirements" &&
                <button type="button" className="justify-self-start text-primary hover:underline"
                  onClick={() => openActivity(attempt.runId, conversationId)}>Inspect unmet requirements</button>}
              {blocker.type === "approval" && <ApprovalReview projectId={job.projectId} approvalId={blocker.approvalId} busy={busy} onAction={onAction} />}
              {blocker.type === "dependency" && <button type="button" className="justify-self-start text-primary hover:underline" onClick={() => onDependency(blocker.jobId)}>Inspect dependency</button>}
              {blocker.type === "instruction_order" && <button type="button" className="justify-self-start text-primary hover:underline" onClick={() => onDependency(blocker.jobId)}>Inspect earlier instruction</button>}
              {(blocker.type === "conversation_busy" || blocker.type === "continuation_owner" || blocker.type === "instruction_delivery_uncertain" || blocker.type === "writer") && <button type="button" className="justify-self-start text-primary hover:underline" onClick={() => openActivity(blocker.runId, blocker.type === "writer" ? undefined : conversationId)}>Inspect blocking run</button>}
            </li>)}
          </ul>
        </section>}
        <div className="flex flex-wrap gap-2">
          {pending && <>
            <Button type="button" variant="outline" className={buttonClass} disabled={busy}
              onClick={() => onAction({ type: "pending_state", jobId: job.id, state: job.queueState === "paused" ? "queued" : "paused" })}>
              {job.queueState === "paused" ? "Resume job" : "Pause job"}
            </Button>
            <Button type="button" variant="outline" className={buttonClass} disabled={busy}
              onClick={() => onAction({ type: "pending_state", jobId: job.id, state: "cancelled" })}>
              {inspection.queuedQuestion ? "Cancel queued answer" : "Cancel pending job"}
            </Button>
          </>}
          {savedQuestions && <Button type="button" variant="outline" className={buttonClass} disabled={busy}
            onClick={() => onAction({ type: "pending_state", jobId: job.id, state: "cancelled" })}>Cancel saved questions</Button>}
          {running && <Button type="button" variant="outline" className={buttonClass} disabled={busy || attempt.cancelRequested}
            onClick={() => onAction({ type: "interrupt", jobId: job.id, runId: attempt.runId })}>
            {attempt.cancelRequested ? "Interrupting…" : "Interrupt attempt"}
          </Button>}
          {running && ownCheckout && job.kind === "agent" && <Button type="button" variant="outline" className={buttonClass}
            disabled={busy || attempt.cancelRequested} onClick={() => onAddChild({ runId: attempt.runId, mode: job.mode })}>Add subagent</Button>}
          {attempt && inspection.recoveryActions.map((action) => <Button type="button" variant="outline" key={action}
            className={buttonClass} disabled={busy} onClick={() => onAction({ type: "recovery", jobId: job.id, runId: attempt.runId, action })}>
            {action === "retry" ? "Retry saved input" : "Resume from checkpoint"}
          </Button>)}
          {conversationId && ownCheckout && <Button className={buttonClass} type="button" variant="outline" onClick={() => openWorkspace("conversations", conversationId)}>Open conversation <ArrowRight size={13} /></Button>}
          {conversationId && !ownCheckout && <p className="w-full text-xs text-muted-foreground">Select the linked worktree to open this conversation.</p>}
          {job.taskId && <Button className={buttonClass} type="button" variant="outline" onClick={() => openWorkspace("tasks", job.taskId ?? undefined)}>Open task <ArrowRight size={13} /></Button>}
          {attempt && <Button className={buttonClass} type="button" variant="outline" onClick={() => openActivity(attempt.runId, conversationId)}>
            {inspection.questionWait ? "Open originating turn" : "Inspect attempt"}
          </Button>}
          {attempt?.previousRunId && <Button className={buttonClass} type="button" variant="outline" onClick={() => openActivity(attempt.previousRunId!, conversationId)}>Inspect previous attempt</Button>}
          {requestingRunId && <Button className={buttonClass} type="button" variant="outline" onClick={() => openActivity(requestingRunId, job.context?.conversationId)}>{job.operatorRequested ? "Open parent run" : "Open requesting run"}</Button>}
        </div>
      </div>
    </aside>
  );
}

export default function LiveQueue({ checkout, openActivity, openWorkspace, onClose, active = true, focusJob }: WorkspaceProps & {
  onClose?: () => void;
  active?: boolean;
  focusJob?: QueueJobFocus;
}) {
  const queryClient = useQueryClient();
  const [view, setView] = useState<QueueView>("All");
  const [selected, setSelected] = useState<string>();
  const [setup, setSetup] = useState<{ checkoutId: string; parent?: ParentAttempt }>();
  const showSetup = active && setup?.checkoutId === checkout.checkoutId;
  const query = useQuery({
    ...projectQueueQuery(checkout.projectId),
    enabled: active,
    refetchInterval: active ? 1000 : false,
    refetchOnWindowFocus: active,
  });
  useEffect(() => {
    if (focusJob?.projectId !== checkout.projectId) return;
    setView("All");
    setSelected(focusJob.jobId);
  }, [checkout.projectId, focusJob]);
  const visible = query.data?.jobs.filter((inspection) => {
    const state = queueJobState(inspection);
    return matchesQueueView(state, view) && (view !== "Active" || !inspection.questionWait ||
      inspection.job.latestAttempt?.status === "waiting_for_input");
  }) ?? [];
  const selectedJob = query.data?.jobs.find(({ job }) => job.id === selected && job.projectId === checkout.projectId);
  const settings = query.data?.settings;
  const control = useMutation({
    mutationFn: (action: QueueAction) => applyQueueAction(checkout.projectId, action),
    onSettled: () => queryClient.invalidateQueries({ queryKey: ["project-job-queue"] }),
  });

  return (
    <section className="delivery-view" aria-label="Execution queue">
      <header className="delivery-toolbar">
        <ListTodo size={16} className="text-muted-foreground" aria-hidden="true" /><h2>Queue</h2>
        {query.data && <span className="rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground tabular-nums" title="Scheduler occupancy across projects">{query.data.active} / {query.data.settings.capacity} slots</span>}
        {query.data?.settings.paused && <DeliveryStatus>Paused</DeliveryStatus>}
        <div className="ml-auto flex items-center gap-1.5">
          {active && <Button type="button" className={buttonClass} disabled={showSetup} onClick={() => setSetup({ checkoutId: checkout.checkoutId })}>
            <Plus size={14} aria-hidden="true" /> Add job
          </Button>}
          {settings && <Button type="button" variant="outline" className={buttonClass} disabled={control.isPending}
            onClick={() => control.mutate({ type: "pause_queue", paused: !settings.paused })}>
            {settings.paused ? <><Play size={14} aria-hidden="true" /> Resume queue</> : <><Pause size={14} aria-hidden="true" /> Pause queue</>}
          </Button>}
          <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs" disabled={query.isFetching} onClick={() => void query.refetch()} aria-label="Refresh queue"><RefreshCw size={14} aria-hidden="true" /> Refresh</Button>
          {onClose && <button type="button" aria-label="Close queue" className="rounded p-1 text-muted-foreground hover:bg-muted" onClick={onClose}><X size={17} /></button>}
        </div>
      </header>
      {control.isError && <div className="px-3 py-2"><InlineIssue>{String(control.error)}</InlineIssue></div>}
      {showSetup && <ManagedJobForm key={setup.parent?.runId ?? setup.checkoutId} checkout={checkout} parent={setup.parent}
        onClose={() => setSetup(undefined)} onQueued={(id) => {
          setSetup(undefined);
          setView("All");
          setSelected(id);
          void queryClient.invalidateQueries({ queryKey: ["project-job-queue", checkout.projectId] });
        }} />}
      {settings && <QueuePolicy key={checkout.projectId} projectId={checkout.projectId} checkoutId={checkout.checkoutId} capacity={settings.capacity} />}
      <nav className="flex gap-0.5 overflow-x-auto border-b border-border px-3 py-1" aria-label="Queue states">
        {queueViews.map((entry) => <button type="button" key={entry} className="delivery-tab" aria-pressed={view === entry} onClick={() => setView(entry)}>{entry}</button>)}
      </nav>
      <div className="delivery-body">
        <div className="delivery-main">
          {query.isPending && active && <p className="px-4 py-3 text-sm text-muted-foreground" role="status">Loading project queue…</p>}
          {query.isError && <div className="p-3"><InlineIssue>Could not refresh the project queue. {String(query.error)}</InlineIssue></div>}
          {visible.length > 0 && <table className="delivery-table"><thead><tr><th>Job</th><th>Status</th><th>Priority</th><th>Attempt</th><th>Wait reason</th></tr></thead>
            <tbody>{visible.map((inspection) => (
              <tr key={inspection.job.id}>
                <td><button type="button" onClick={() => setSelected(inspection.job.id)} className="text-left font-medium hover:underline" aria-pressed={selected === inspection.job.id}>{jobTitle(inspection)}</button><div className="mt-0.5 break-all text-xs text-muted-foreground">{jobRequester(inspection)}</div></td>
                <td><DeliveryStatus>{queueJobState(inspection)}</DeliveryStatus></td>
                <td className="tabular-nums">{inspection.job.priority}</td>
                <td className="tabular-nums">{inspection.job.latestAttempt?.ordinal ?? "—"}</td>
                <td className="break-words text-muted-foreground">{firstWaitReason(inspection)}</td>
              </tr>
            ))}</tbody>
          </table>}
          {query.data && !visible.length && <div className="delivery-empty">
            <span className="work-empty-icon"><ListTodo size={20} aria-hidden="true" /></span>
            <h3>{query.data.jobs.length ? "No matching jobs" : "No jobs in this project"}</h3>
            <p>{query.data.jobs.length ? "Choose another status to see more jobs." : "Conversation turns appear here when you send a message."}</p>
          </div>}
        </div>
        {selectedJob && <JobDetails inspection={selectedJob} checkout={checkout} onClose={() => setSelected(undefined)}
          openActivity={openActivity} openWorkspace={(workspace, target) => { openWorkspace(workspace, target); onClose?.(); }}
          onDependency={(id) => { setView("All"); setSelected(id); }}
          onAction={control.mutate} onAddChild={(parent) => setSetup({ checkoutId: checkout.checkoutId, parent })} busy={control.isPending} />}
      </div>
    </section>
  );
}
