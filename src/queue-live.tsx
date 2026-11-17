import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ArrowRight, RefreshCw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { AdmissionBlocker } from "@/bindings/AdmissionBlocker";
import type { Job } from "@/bindings/Job";
import type { JobInspection } from "@/bindings/JobInspection";
import type { ModelOption } from "@/bindings/ModelOption";
import type { Provider } from "@/bindings/Provider";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { QueueState } from "@/bindings/QueueState";
import type { RecoveryAction } from "@/bindings/RecoveryAction";
import type { SessionMode } from "@/bindings/SessionMode";
import { ContextActions, type MenuAction, type MenuGroups } from "@/components/ui/context-actions";
import { gitCheckoutTarget } from "@/file-context-actions";
import { JobFailurePanel } from "@/job-failure";
import {
  AddQueueJobButton, QueueButton, QueueEmpty, QueueJobActions, QueueJobFacts, QueueJobForm,
  QueueLayout, QueuePauseButton, type QueueJobDraft,
} from "@/queue-layout";
import { matchesQueueView, projectQueueQuery, queueBlockerReason, queueJobState, type QueueView } from "@/queue-model";
import { QueuePolicy } from "@/queue-policy";
import { QueueWriterReconciliation } from "@/queue-writer-reconciliation";
import { InlineIssue } from "@/tasks-git-ui";
import type { QueueJobFocus, WorkspaceDestination, WorkspaceProps } from "@/workbench-contract";

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

type JobCommand = Extract<MenuAction, { run: () => void }>;

function jobControls(inspection: JobInspection, busy: boolean, run: (action: QueueAction) => void): JobCommand[] {
  const { job } = inspection;
  const attempt = job.latestAttempt;
  const pending = job.queueState !== "cancelled" && (!attempt || job.queuedRecovery !== null ||
    Boolean(job.downstreamWait && job.downstreamWait.state !== "yielding") || inspection.queuedQuestion);
  const running = activeAttempt(attempt);
  const savedQuestions = inspection.questionWait && !running && !pending && job.queueState !== "cancelled";
  const commands: JobCommand[] = [];
  if (pending) commands.push({ label: job.queueState === "paused" ? "Resume job" : "Pause job", disabled: busy,
    run: () => run({ type: "pending_state", jobId: job.id, state: job.queueState === "paused" ? "queued" : "paused" }) });
  if (pending || savedQuestions) commands.push({
    label: savedQuestions ? "Cancel saved questions" : inspection.queuedQuestion ? "Cancel queued answer" : "Cancel pending job",
    disabled: busy, destructive: true, run: () => run({ type: "pending_state", jobId: job.id, state: "cancelled" }),
  });
  if (running) commands.push({ label: running.cancelRequested ? "Interrupting…" : "Interrupt attempt",
    disabled: busy || running.cancelRequested, destructive: true,
    run: () => run({ type: "interrupt", jobId: job.id, runId: running.runId }) });
  if (attempt) for (const action of inspection.recoveryActions) commands.push({
    label: action === "retry" ? "Retry saved input" : "Resume from checkpoint", disabled: busy,
    run: () => run({ type: "recovery", jobId: job.id, runId: attempt.runId, action }),
  });
  return commands;
}

function activeAttempt(attempt: Job["latestAttempt"]) {
  return attempt && ["starting", "running", "waiting_for_input"].includes(attempt.status) ? attempt : undefined;
}

function jobConversation(job: Job) {
  return job.latestAttempt?.conversationId ?? (job.origin.type === "operator" ? job.context?.conversationId : undefined);
}

function blockerReference(blocker: AdmissionBlocker) {
  if ("approvalId" in blocker) return { label: "Copy approval ID", id: blocker.approvalId };
  if ("jobId" in blocker) return { label: "Copy blocking job ID", id: blocker.jobId };
  if ("runId" in blocker) return { label: "Copy blocking run ID", id: blocker.runId };
  if ("operationId" in blocker) return { label: "Copy operation ID", id: blocker.operationId };
  if ("checkoutId" in blocker) return { label: "Copy blocking worktree ID", id: blocker.checkoutId };
  if ("contextId" in blocker) return { label: "Copy context ID", id: blocker.contextId };
  if ("waitId" in blocker) return { label: "Copy downstream wait ID", id: blocker.waitId };
  if ("conversationId" in blocker) return { label: "Copy conversation ID", id: blocker.conversationId };
}

function blockerKey(blocker: AdmissionBlocker) {
  return JSON.stringify([blocker.type, blockerReference(blocker)?.id]);
}

type JobSelection = { id: string; blocker?: string };
type BlockerActions = {
  openActivity: WorkspaceProps["openActivity"];
  openJob: (id: string, blocker?: AdmissionBlocker) => void;
  worktreeAction: (checkoutId: string, label?: string) => JobCommand | undefined;
  copyText: (text: string) => void;
};

function blockerOrigin(blocker: AdmissionBlocker, job: Job, actions: BlockerActions): JobCommand | undefined {
  const conversationId = jobConversation(job);
  switch (blocker.type) {
    case "dependency": return { label: "Inspect dependency", run: () => actions.openJob(blocker.jobId) };
    case "instruction_order": return { label: "Inspect earlier instruction", run: () => actions.openJob(blocker.jobId) };
    case "conversation_busy":
    case "continuation_owner":
    case "instruction_delivery_uncertain": return { label: "Inspect blocking run", run: () => actions.openActivity(blocker.runId, conversationId) };
    case "writer": return { label: "Inspect blocking run", run: () => actions.openActivity(blocker.runId) };
    case "pending_worktree_changes": {
      const runId = blocker.producingRunId;
      return runId ? { label: "Inspect producing run", run: () => actions.openActivity(runId) }
        : actions.worktreeAction(blocker.checkoutId, "Open blocking worktree in Git");
    }
    case "app_writer": return actions.worktreeAction(blocker.checkoutId, "Open blocking worktree in Git");
    case "dispatch_rejected": {
      const runId = job.latestAttempt?.runId;
      if (runId && blocker.rejection.type === "requirements") {
        return { label: "Inspect unmet requirements", run: () => actions.openActivity(runId, conversationId) };
      }
    }
  }
}

function blockerMenu(blocker: AdmissionBlocker, job: Job, actions: BlockerActions, showReason: boolean): MenuGroups {
  const reference = blockerReference(blocker);
  const reason = queueBlockerReason(blocker);
  const origin = blockerOrigin(blocker, job, actions);
  const review = blocker.type === "approval" ? "Review pending approval" : showReason ? "Open wait details" : undefined;
  return [[
    ...(review ? [{ label: review, run: () => actions.openJob(job.id, blocker) }] : []),
    ...(origin ? [origin] : []),
  ], [
    { label: "Copy wait reason", run: () => actions.copyText(reason) },
    ...(reference ? [{ label: reference.label, run: () => actions.copyText(reference.id) }] : []),
  ]];
}

function JobDetails({ inspection, checkout, openWorkspace, openActivity, actions, focus, worktreePath, onAction, onAddChild, busy }:
  Pick<WorkspaceProps, "checkout" | "openActivity" | "openWorkspace"> & {
  inspection: JobInspection;
  actions: BlockerActions;
  focus: JobSelection;
  worktreePath?: string;
  onAction: (action: QueueAction) => void;
  onAddChild: (parent: ParentAttempt) => void;
  busy: boolean;
}) {
  const { job, blockers } = inspection;
  const attempt = job.latestAttempt;
  const ownCheckout = job.checkoutId === checkout.checkoutId;
  const conversationId = jobConversation(job);
  const requestingRunId = job.origin.type === "agent" ? job.origin.runId : undefined;
  const running = activeAttempt(attempt);
  const pipeline = job.pipeline;
  const blockerTarget = useRef<HTMLParagraphElement>(null);
  const focusAvailable = !focus.blocker || blockers.some((blocker) => blockerKey(blocker) === focus.blocker);
  const worktree = actions.worktreeAction(job.checkoutId);
  useEffect(() => {
    if (!focus.blocker) return;
    let frame: number;
    // Let the menu's focus scope finish restoring focus before focusing the blocker.
    const timer = window.setTimeout(() => { frame = requestAnimationFrame(() => blockerTarget.current?.focus()); });
    return () => { window.clearTimeout(timer); cancelAnimationFrame(frame); };
  }, [focus, focusAvailable]);

  return <>
        {attempt?.cancelRequested && <InlineIssue>Cancellation requested. Waiting for the runtime to confirm the outcome.</InlineIssue>}
        <QueueJobFacts facts={[
          { label: "Job ID", value: job.id, mono: true },
          { label: "Requester", value: jobRequester(inspection) },
          { label: "Worktree", value: worktreePath ?? job.checkoutId, mono: true },
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
              <ContextActions label={`Downstream job ${input.childJobId}`} groups={[[
                { label: "Open downstream job", run: () => actions.openJob(input.childJobId) },
                { label: "Copy downstream job ID", run: () => actions.copyText(input.childJobId) },
              ]]}>
                <button type="button" className="break-all text-left text-primary hover:underline" onClick={() => actions.openJob(input.childJobId)}>
                  {input.childJobId}
                </button>
              </ContextActions>
              <span className="capitalize">{input.status ?? "Pending result"}</span>
            </li>)}
          </ul>
        </section>}
        {!focusAvailable && <p ref={blockerTarget} tabIndex={-1} role="status">This blocker is no longer present for the selected job.</p>}
        {blockers.length > 0 && <section className="grid gap-1.5" aria-label="Admission blockers">
          <h4 className="text-xs font-semibold">Admission blockers</h4>
          <ul className="divide-y divide-border rounded border border-border">
            {blockers.map((blocker, index) => {
              const key = blockerKey(blocker);
              const current = focus.blocker === key;
              const origin = blockerOrigin(blocker, job, actions);
              const reason = queueBlockerReason(blocker);
              return <li key={`${key}-${index}`} className="grid gap-1.5 p-2 text-xs leading-5">
                <ContextActions label={reason} groups={blockerMenu(blocker, job, actions, false)}>
                  <p ref={current ? blockerTarget : undefined} tabIndex={-1} className="break-words">{reason}</p>
                </ContextActions>
                {origin && <button type="button" className="justify-self-start text-primary hover:underline"
                  disabled={origin.disabled} onClick={origin.run}>{origin.label}</button>}
                {blocker.type === "approval" && <ApprovalReview projectId={job.projectId} approvalId={blocker.approvalId} busy={busy} onAction={onAction} />}
              </li>;
            })}
          </ul>
        </section>}
        <QueueJobActions>
          {jobControls(inspection, busy, onAction).map(({ label, disabled, run }) =>
            <QueueButton key={label} variant="outline" disabled={disabled} onClick={run}>{label}</QueueButton>)}
          {running && ownCheckout && job.kind === "agent" && <QueueButton variant="outline"
            disabled={busy || running.cancelRequested} onClick={() => onAddChild({ runId: running.runId, mode: job.mode })}>Add subagent</QueueButton>}
          {conversationId && ownCheckout && <QueueButton variant="outline" onClick={() => openWorkspace("conversations", conversationId)}>Open conversation <ArrowRight size={13} aria-hidden /></QueueButton>}
          {conversationId && !ownCheckout && <p className="w-full text-xs text-muted-foreground">Select the linked worktree to open this conversation.</p>}
          {job.taskId && <QueueButton variant="outline" onClick={() => openWorkspace("tasks", job.taskId ?? undefined)}>Open task <ArrowRight size={13} aria-hidden /></QueueButton>}
          {worktree && <QueueButton variant="outline" onClick={worktree.run} disabled={worktree.disabled}>{worktree.label}</QueueButton>}
          {attempt && <QueueButton variant="outline" onClick={() => openActivity(attempt.runId, conversationId)}>
            {inspection.questionWait ? "Open originating turn" : "Inspect attempt"}
          </QueueButton>}
          {attempt?.previousRunId && <QueueButton variant="outline" onClick={() => openActivity(attempt.previousRunId!, conversationId)}>Inspect previous attempt</QueueButton>}
          {requestingRunId && <QueueButton variant="outline" onClick={() => openActivity(requestingRunId, job.context?.conversationId)}>{job.operatorRequested ? "Open parent run" : "Open requesting run"}</QueueButton>}
        </QueueJobActions>
  </>;
}

export type LiveQueueProps = WorkspaceProps & {
  onClose?: () => void;
  active?: boolean;
  focusJob?: QueueJobFocus;
  onOpenRegisteredWorktree?: (checkout: ProjectCheckout, destination: WorkspaceDestination) => Promise<void>;
};

export default function LiveQueue({ checkout, openActivity, openWorkspace, onClose, active = true, focusJob, onOpenRegisteredWorktree }: LiveQueueProps) {
  const queryClient = useQueryClient();
  const [view, setView] = useState<QueueView>("All");
  const [selection, setSelection] = useState<JobSelection>();
  const [copyError, setCopyError] = useState("");
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
    setSelection({ id: focusJob.jobId });
  }, [checkout.projectId, focusJob]);
  const jobs = query.data?.jobs.filter(({ job }) => job.projectId === checkout.projectId) ?? [];
  const visible = jobs.filter((inspection) => {
    const state = queueJobState(inspection);
    return matchesQueueView(state, view) && (view !== "Active" || !inspection.questionWait ||
      inspection.job.latestAttempt?.status === "waiting_for_input");
  });
  const selectedJob = selection && jobs.find(({ job }) => job.id === selection.id);
  const registered = useQuery({
    queryKey: ["registered-checkouts"],
    queryFn: () => invoke<ProjectCheckout[]>("registered_checkouts"),
    enabled: active && Boolean(onOpenRegisteredWorktree) && jobs.some(({ job, blockers }) => job.checkoutId !== checkout.checkoutId ||
      blockers.some((blocker) => "checkoutId" in blocker && blocker.checkoutId !== checkout.checkoutId)),
  });
  const settings = query.data?.settings;
  const control = useMutation({
    mutationFn: (action: QueueAction) => applyQueueAction(checkout.projectId, action),
    onSettled: () => queryClient.invalidateQueries({ queryKey: ["project-job-queue"] }),
  });
  function copyText(text: string) {
    setCopyError("");
    void Promise.resolve().then(() => navigator.clipboard.writeText(text)).catch(() => setCopyError("Could not copy to the clipboard."));
  }
  const navigate: typeof openWorkspace = (workspace, target) => { openWorkspace(workspace, target); onClose?.(); };
  const openingWorktree = useMutation({
    mutationFn: async (source: ProjectCheckout) => {
      if (!onOpenRegisteredWorktree || source.projectId !== checkout.projectId) throw new Error("This worktree is unavailable in the current project.");
      await onOpenRegisteredWorktree(source, { workspace: "git", target: gitCheckoutTarget() });
    },
    onSuccess: () => onClose?.(),
  });
  function worktreeFor(id: string) {
    return id === checkout.checkoutId ? checkout : registered.data?.find((source) => source.projectId === checkout.projectId && source.checkoutId === id);
  }
  function worktreeAction(id: string, label = "Open worktree in Git"): JobCommand | undefined {
    if (id === checkout.checkoutId) return { label, run: () => navigate("git", gitCheckoutTarget()) };
    const source = worktreeFor(id);
    if (!source || !onOpenRegisteredWorktree) return;
    return { label, disabled: openingWorktree.isPending, run: () => openingWorktree.mutate(source) };
  }
  const blockerActions: BlockerActions = {
    openActivity, worktreeAction, copyText,
    openJob: (id, blocker) => { setView("All"); setSelection({ id, blocker: blocker && blockerKey(blocker) }); },
  };

  const occupancy = query.data && { active: query.data.active, capacity: query.data.settings.capacity, description: "Scheduler occupancy across projects" };

  return <QueueLayout occupancy={occupancy} paused={settings?.paused ?? false} onClose={onClose}
    view={view} onViewChange={setView} onSelect={(id) => setSelection({ id })} onDeselect={() => setSelection(undefined)}
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
      {copyError && <div className="px-3 py-2"><InlineIssue>{copyError}</InlineIssue></div>}
      {openingWorktree.isError && <div className="px-3 py-2"><InlineIssue>Could not open the worktree. {String(openingWorktree.error)}</InlineIssue></div>}
      {showSetup && <ManagedJobForm key={setup.parent?.runId ?? setup.checkoutId} checkout={checkout} parent={setup.parent}
        onClose={() => setSetup(undefined)} onQueued={(id) => {
          setSetup(undefined);
          setView("All");
          setSelection({ id });
          void queryClient.invalidateQueries({ queryKey: ["project-job-queue", checkout.projectId] });
        }} />}
      {settings && <QueuePolicy key={checkout.projectId} projectId={checkout.projectId} checkoutId={checkout.checkoutId} capacity={settings.capacity} />}
    </>}
    state={<>
      {query.isPending && active && <p className="px-4 py-3 text-sm text-muted-foreground" role="status">Loading project queue…</p>}
      {query.isError && <div className="p-3"><InlineIssue>Could not refresh the project queue. {String(query.error)}</InlineIssue></div>}
      {query.isSuccess && selection && !selectedJob && <p className="px-4 py-3 text-sm" role="status">The requested job is unavailable in this queue.</p>}
      {query.data && !visible.length && <QueueEmpty filtered={query.data.jobs.length > 0} />}
    </>}
    jobs={visible.map((inspection) => {
      const { job } = inspection;
      const { id, priority, latestAttempt, taskId, pipeline } = job;
      const conversationId = jobConversation(job);
      const worktree = worktreeFor(job.checkoutId);
      const openWorktree = worktreeAction(job.checkoutId);
      const approvals = inspection.blockers.filter((blocker) => blocker.type === "approval");
      const approvalItems = approvals.map((approval) => ({ label: approval.approvalId, run: () => blockerActions.openJob(id, approval) }));
      const approval = approvalItems[0];
      const blocker = inspection.blockers[0];
      const reason = firstWaitReason(inspection);
      const reasonMenu = blocker ? blockerMenu(blocker, job, blockerActions, true) : reason !== "—" ? [[
        { label: "Open job details", run: () => setSelection({ id }) },
        { label: "Copy wait reason", run: () => copyText(reason) },
      ]] : undefined;
      const references = [
        ["Worktree", job.checkoutId], ["Task", taskId], ["Conversation", conversationId], ["Pipeline run", pipeline?.pipelineRunId],
      ] as const;
      return { id, title: jobTitle(inspection), requester: jobRequester(inspection), status: queueJobState(inspection),
        priority, attempt: latestAttempt?.ordinal ?? "—", reason, reasonMenu, menu: [[
          { label: "Open job details", run: () => setSelection({ id }) },
          ...(approvalItems.length > 1 ? [{ label: "Review pending approval", items: approvalItems }]
            : approval ? [{ label: "Review pending approval", run: approval.run }] : []),
          ...(blocker && blocker.type !== "approval" ? [{ label: "Open wait details", run: () => blockerActions.openJob(id, blocker) }] : []),
          ...(latestAttempt ? [{ label: "Inspect attempt", run: () => openActivity(latestAttempt.runId, conversationId) }] : []),
          ...(latestAttempt?.previousRunId ? [{ label: "Inspect previous attempt", run: () => openActivity(latestAttempt.previousRunId!, conversationId) }] : []),
          ...(conversationId && job.checkoutId === checkout.checkoutId ? [{ label: "Open conversation", run: () => navigate("conversations", conversationId) }] : []),
          ...(taskId ? [{ label: "Open task", run: () => navigate("tasks", taskId) }] : []),
          ...(pipeline ? [{ label: "Open pipeline run", run: () => navigate("pipelines", `run:${pipeline.pipelineRunId}`) }] : []),
          ...(openWorktree ? [openWorktree] : []),
        ], jobControls(inspection, control.isPending, control.mutate), [
          { label: "Copy job ID", run: () => copyText(id) },
          ...(latestAttempt ? [{ label: "Copy run ID", run: () => copyText(latestAttempt.runId) }] : []),
          { label: "Copy linked ID", items: references.flatMap(([label, value]) => value ? [{ label, run: () => copyText(value) }] : []) },
          ...(worktree ? [{ label: "Copy worktree path", run: () => copyText(worktree.checkoutRoot) }] : []),
          { label: "Copy job reference", run: () => copyText([
            `Job ${id}`, ...(latestAttempt ? [`Run ${latestAttempt.runId}`] : []),
            ...references.flatMap(([label, value]) => value ? [`${label} ${value}`] : []),
          ].join("\n")) },
          ...(blocker ? [{ label: "Copy wait reasons", run: () => copyText(inspection.blockers.map(queueBlockerReason).join("\n")) }] : []),
        ]] };
    })}
    selected={selection && selectedJob && {
      id: selectedJob.job.id, title: jobTitle(selectedJob), status: queueJobState(selectedJob),
      content: <JobDetails inspection={selectedJob} checkout={checkout} openActivity={openActivity}
        openWorkspace={navigate} focus={selection} actions={blockerActions} worktreePath={worktreeFor(selectedJob.job.checkoutId)?.checkoutRoot}
        onAction={control.mutate} onAddChild={(parent) => setSetup({ checkoutId: checkout.checkoutId, parent })} busy={control.isPending} />,
    }}
  />;
}
