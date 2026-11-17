import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ArrowRight, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import type { Job } from "@/bindings/Job";
import type { JobInspection } from "@/bindings/JobInspection";
import type { ModelOption } from "@/bindings/ModelOption";
import type { Provider } from "@/bindings/Provider";
import type { QueueState } from "@/bindings/QueueState";
import type { RecoveryAction } from "@/bindings/RecoveryAction";
import type { SessionMode } from "@/bindings/SessionMode";
import { JobFailurePanel } from "@/job-failure";
import {
  AddQueueJobButton, PendingQueueJobActions, QueueButton, QueueEmpty, QueueJobActions, QueueJobFacts, QueueJobForm,
  QueueLayout, QueuePauseButton, QueueRecoveryButton, type QueueJobDraft,
} from "@/queue-layout";
import { matchesQueueView, projectQueueQuery, queueBlockerReason, queueJobState, type QueueView } from "@/queue-model";
import { QueuePolicy } from "@/queue-policy";
import { QueueWriterReconciliation } from "@/queue-writer-reconciliation";
import { InlineIssue } from "@/tasks-git-ui";
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

type ParentAttempt = { runId: string; mode: SessionMode };

function ManagedJobForm({ checkout, parent, onQueued, onClose }: Pick<WorkspaceProps, "checkout"> & {
  parent?: ParentAttempt;
  onQueued: (id: string) => void;
  onClose: () => void;
}) {
  const [provider, setProvider] = useState<Provider>("codex");
  const models = useQuery({
    queryKey: ["conversation-models", provider],
    queryFn: () => invoke<{ option: ModelOption }[]>("conversation_models", { provider }),
    staleTime: 60_000,
  });
  const options = models.data?.map(({ option }) => option) ?? [];
  const enqueue = useMutation({
    mutationFn: (draft: QueueJobDraft) => invoke<Job>("enqueue_managed_agent_job", {
      projectId: checkout.projectId, checkoutId: checkout.checkoutId, parentRunId: parent?.runId ?? null,
      ...draft,
    }),
    onSuccess: ({ id }) => onQueued(id),
  });

  return <QueueJobForm provider={provider} onProvider={setProvider} options={options} loading={models.isPending}
    pending={enqueue.isPending} child={Boolean(parent)} allowWrite={!parent || parent.mode === "approved_write"}
    onSubmit={enqueue.mutate} onCancel={onClose} feedback={<>
      {models.isError && <QueueJobActions><InlineIssue>{String(models.error)}</InlineIssue>
        <QueueButton variant="outline" disabled={models.isFetching} onClick={() => void models.refetch()}>Refresh models</QueueButton>
      </QueueJobActions>}
      {enqueue.isError && <InlineIssue>{String(enqueue.error)}</InlineIssue>}
    </>} />;
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
      <QueueJobActions>
        <QueueButton disabled={busy || !request.isSuccess}
          onClick={() => onAction({ type: "approval", approvalId, approved: true })}>Approve job</QueueButton>
        <QueueButton variant="outline" disabled={busy}
          onClick={() => onAction({ type: "approval", approvalId, approved: false })}>Decline job</QueueButton>
      </QueueJobActions>
    </div>
  );
}

function JobDetails({ inspection, checkout, openWorkspace, openActivity, onDependency, onAction, onAddChild, busy }: Pick<WorkspaceProps, "checkout" | "openActivity" | "openWorkspace"> & {
  inspection: JobInspection;
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
  const pipeline = job.pipeline;

  return <>
        {attempt?.cancelRequested && <InlineIssue>Cancellation requested. Waiting for the runtime to confirm the outcome.</InlineIssue>}
        <QueueJobFacts facts={[
          { label: "Job ID", value: job.id, mono: true },
          { label: "Requester", value: jobRequester(inspection) },
          { label: "Worktree", value: ownCheckout ? checkout.checkoutRoot : job.checkoutId, mono: true },
          { label: "Profile mode", value: job.mode.replaceAll("_", " ") },
          { label: "Priority", value: job.priority },
          ...(pipeline ? [
            { label: "Pipeline run", value: pipeline.pipelineRunId, mono: true },
            { label: "Node instance", value: pipeline.nodeInstanceId, mono: true },
          ] : []),
          ...(pipeline?.planRevision ? [{ label: "Plan revision", value: pipeline.planRevision, mono: true }] : []),
          ...(pipeline?.release ? [
            { label: "Implementation release", value: pipeline.release.id, mono: true },
            { label: "Released scope", value: `${pipeline.release.taskId} · ${pipeline.release.checkoutId}`, mono: true },
          ] : []),
          { label: "Attempt", value: attempt ? `#${attempt.ordinal}` : "Awaiting dispatch" },
          ...(attempt ? [{ label: "Run ID", value: attempt.runId, mono: true }] : []),
          ...(attempt?.recovery ? [{ label: "Recovery", value: attempt.recovery === "resume" ? "Resumed from checkpoint" : "Retried saved input" }] : []),
        ]} />
        {attempt && !inspection.questionWait && (!job.downstreamWait || job.downstreamWait.state === "needs_review")
          && (attempt.status === "failed" || attempt.status === "interrupted") &&
          <JobFailurePanel projectId={job.projectId} runId={attempt.runId}
            openLogs={() => openActivity(attempt.runId, attempt.conversationId ?? undefined)} />}
        {attempt?.status === "interrupted" && job.mode === "approved_write" && job.kind === "agent" &&
          <QueueWriterReconciliation key={attempt.runId} projectId={job.projectId} jobId={job.id} runId={attempt.runId} />}
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
        <QueueJobActions>
          {pending && <PendingQueueJobActions paused={job.queueState === "paused"} queuedAnswer={inspection.queuedQuestion} disabled={busy}
            onPauseToggle={() => onAction({ type: "pending_state", jobId: job.id, state: job.queueState === "paused" ? "queued" : "paused" })}
            onCancel={() => onAction({ type: "pending_state", jobId: job.id, state: "cancelled" })} />}
          {savedQuestions && <QueueButton variant="outline" disabled={busy}
            onClick={() => onAction({ type: "pending_state", jobId: job.id, state: "cancelled" })}>Cancel saved questions</QueueButton>}
          {running && <QueueButton variant="outline" disabled={busy || attempt.cancelRequested}
            onClick={() => onAction({ type: "interrupt", jobId: job.id, runId: attempt.runId })}>
            {attempt.cancelRequested ? "Interrupting…" : "Interrupt attempt"}
          </QueueButton>}
          {running && ownCheckout && job.kind === "agent" && <QueueButton variant="outline"
            disabled={busy || attempt.cancelRequested} onClick={() => onAddChild({ runId: attempt.runId, mode: job.mode })}>Add subagent</QueueButton>}
          {attempt && inspection.recoveryActions.map((action) => <QueueRecoveryButton key={action} action={action}
            disabled={busy} onClick={() => onAction({ type: "recovery", jobId: job.id, runId: attempt.runId, action })} />)}
          {conversationId && ownCheckout && <QueueButton variant="outline" onClick={() => openWorkspace("conversations", conversationId)}>Open conversation <ArrowRight size={13} aria-hidden /></QueueButton>}
          {conversationId && !ownCheckout && <p className="w-full text-xs text-muted-foreground">Select the linked worktree to open this conversation.</p>}
          {job.taskId && <QueueButton variant="outline" onClick={() => openWorkspace("tasks", job.taskId ?? undefined)}>Open task <ArrowRight size={13} aria-hidden /></QueueButton>}
          {attempt && <QueueButton variant="outline" onClick={() => openActivity(attempt.runId, conversationId)}>
            {inspection.questionWait ? "Open originating turn" : "Inspect attempt"}
          </QueueButton>}
          {attempt?.previousRunId && <QueueButton variant="outline" onClick={() => openActivity(attempt.previousRunId!, conversationId)}>Inspect previous attempt</QueueButton>}
          {requestingRunId && <QueueButton variant="outline" onClick={() => openActivity(requestingRunId, job.context?.conversationId)}>{job.operatorRequested ? "Open parent run" : "Open requesting run"}</QueueButton>}
        </QueueJobActions>
  </>;
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

  const occupancy = query.data && { active: query.data.active, capacity: query.data.settings.capacity, description: "Scheduler occupancy across projects" };

  return <QueueLayout occupancy={occupancy} paused={settings?.paused ?? false} onClose={onClose}
    view={view} onViewChange={setView} onSelect={setSelected} onDeselect={() => setSelected(undefined)}
    actions={<>
      {active && <AddQueueJobButton disabled={showSetup} onClick={() => setSetup({ checkoutId: checkout.checkoutId })} />}
      {settings && <QueuePauseButton paused={settings.paused} disabled={control.isPending}
        onClick={() => control.mutate({ type: "pause_queue", paused: !settings.paused })} />}
      <QueueButton variant="outline" disabled={query.isFetching} onClick={() => void query.refetch()} aria-label="Refresh queue">
        <RefreshCw size={14} aria-hidden="true" /> Refresh
      </QueueButton>
    </>}
    controls={<>
      {control.isError && <div className="px-3 py-2"><InlineIssue>{String(control.error)}</InlineIssue></div>}
      {showSetup && <ManagedJobForm key={setup.parent?.runId ?? setup.checkoutId} checkout={checkout} parent={setup.parent}
        onClose={() => setSetup(undefined)} onQueued={(id) => {
          setSetup(undefined);
          setView("All");
          setSelected(id);
          void queryClient.invalidateQueries({ queryKey: ["project-job-queue", checkout.projectId] });
        }} />}
      {settings && <QueuePolicy key={checkout.projectId} projectId={checkout.projectId} checkoutId={checkout.checkoutId} capacity={settings.capacity} />}
    </>}
    state={<>
      {query.isPending && active && <p className="px-4 py-3 text-sm text-muted-foreground" role="status">Loading project queue…</p>}
      {query.isError && <div className="p-3"><InlineIssue>Could not refresh the project queue. {String(query.error)}</InlineIssue></div>}
      {query.data && !visible.length && <QueueEmpty filtered={query.data.jobs.length > 0} />}
    </>}
    jobs={visible.map((inspection) => ({
      id: inspection.job.id, title: jobTitle(inspection), requester: jobRequester(inspection), status: queueJobState(inspection),
      priority: inspection.job.priority, attempt: inspection.job.latestAttempt?.ordinal ?? "—", reason: firstWaitReason(inspection),
    }))}
    selected={selectedJob && {
      id: selectedJob.job.id, title: jobTitle(selectedJob), status: queueJobState(selectedJob),
      content: <JobDetails inspection={selectedJob} checkout={checkout} openActivity={openActivity}
        openWorkspace={(workspace, target) => { openWorkspace(workspace, target); onClose?.(); }}
        onDependency={(id) => { setView("All"); setSelected(id); }}
        onAction={control.mutate} onAddChild={(parent) => setSetup({ checkoutId: checkout.checkoutId, parent })} busy={control.isPending} />,
    }}
  />;
}
