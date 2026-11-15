import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { IDockviewPanelProps } from "dockview-react";
import { Activity, ArrowDown, ArrowUp, MessageSquare, Plus, RotateCcw, Search, Target } from "lucide-react";
import { Dialog } from "radix-ui";
import { useCallback, useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type FormEvent } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { AdmissionBlocker } from "@/bindings/AdmissionBlocker";
import type { Job } from "@/bindings/Job";
import type { JobInspection } from "@/bindings/JobInspection";
import type { RecoveryAction } from "@/bindings/RecoveryAction";
import type { ConversationHistory } from "@/bindings/ConversationHistory";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import type { RunRecord } from "@/bindings/RunRecord";
import { ModelPicker } from "@/components/model-picker";
import { EffortSelect, type EffortSet } from "@/components/effort-select";
import { ConversationModeSelect, goalCommand, type ConversationMode } from "@/conversation-mode";
import { Button } from "@/components/ui/button";
import { ConversationActivity, ConversationRequests, StopConversation, isLiveRun, useConversationRun } from "@/conversation-run";
import { ConversationInstructions, useConversationInstructions } from "@/conversation-instructions";
import { ConversationHistoryControls } from "@/conversation-history-controls";
import { ConversationSubagentControls } from "@/subagent-quota";
import { ConversationMarkdown } from "@/conversation-markdown";
import { ConversationRow } from "@/conversation-menu";
import { WorktreeEvidence } from "@/worktree-evidence";
import { openConversation } from "@/workspace-tabs";
import { useWorkspace } from "@/workspace-state";
import "./conversation.css";

const draftWrites = new Map<string, Promise<void>>();
const historyOffsets = new Map<string, number>();

function pendingJob(inspection: JobInspection | null | undefined) {
  return Boolean(inspection && (!inspection.job.latestAttempt || inspection.job.queuedRecovery || inspection.queuedQuestion ||
    (inspection.job.downstreamWait && inspection.job.downstreamWait.state !== "yielding")) &&
    inspection.job.queueState !== "cancelled");
}

function waitReason(blocker: AdmissionBlocker | undefined): string {
  switch (blocker?.type) {
    case "queue_paused": return "The job queue is paused.";
    case "job_paused": return "This job is paused.";
    case "context_changed": return "The conversation changed since this message was queued.";
    case "conversation_busy": return "Waiting for the active conversation run.";
    case "instruction_order": return "Waiting for an earlier instruction.";
    case "capacity": return `Waiting for an execution slot (${blocker.active} of ${blocker.limit} in use).`;
    case "policy": return blocker.reason;
    case "dependency": return "Waiting for a required job to complete.";
    case "downstream": return blocker.needsReview
      ? "The previous attempt needs review before this job can continue."
      : `Waiting for ${blocker.pending} downstream job results.`;
    case "approval": return "Waiting for approval.";
    case "writer": return blocker.needsReview
      ? "The previous worktree writer needs review."
      : "Waiting for the worktree writer.";
    case "app_writer": return "Waiting for an app write to finish.";
    case "pending_worktree_changes": return "The worktree has pending changes for another task scope.";
    case "missing_capability": return "The selected runner is missing a required capability.";
    case "job_cancelled": return "Job cancelled.";
    default: return "Waiting to start.";
  }
}

function saveDraft(conversationId: string, text: string) {
  const pending = draftWrites.get(conversationId);
  const write = (pending?.catch(() => undefined) ?? Promise.resolve()).then(() =>
    invoke<void>("save_conversation_draft", { conversationId, text }),
  );
  draftWrites.set(conversationId, write);
  const release = () => {
    if (draftWrites.get(conversationId) === write) draftWrites.delete(conversationId);
  };
  void write.then(release, release);
  return write;
}

export function ConversationListPanel({ containerApi }: IDockviewPanelProps) {
  const checkoutId = useWorkspace((state) => state.checkout?.checkoutId);
  if (!checkoutId) return null;
  return <ConversationSidebar checkoutId={checkoutId} onOpen={(conversation) => openConversation(containerApi, conversation)} />;
}

export function useNewConversation(checkoutId: string, onOpen: (conversation: Conversation) => void) {
  const queryClient = useQueryClient();
  const key = ["conversations", checkoutId];
  return useMutation({
    mutationKey: ["new-conversation", checkoutId],
    scope: { id: "new-conversation:" + checkoutId },
    mutationFn: async () => {
      const list = await queryClient.fetchQuery({
        queryKey: key,
        queryFn: () => invoke<Conversation[]>("list_conversations", { checkoutId }),
      });
      return invoke<Conversation>("create_conversation", { checkoutId, title: `Conversation ${list.length + 1}` });
    },
    onSuccess: (conversation) => {
      void queryClient.invalidateQueries({ queryKey: key });
      onOpen(conversation);
    },
  });
}

export function ConversationSidebar({ checkoutId, onOpen, activeId, onRenamed }: {
  checkoutId: string;
  onOpen: (conversation: Conversation) => void;
  activeId?: string;
  onRenamed?: (conversation: Conversation) => void;
}) {
  const queryClient = useQueryClient();
  const [filter, setFilter] = useState("");
  const [notice, setNotice] = useState("");
  const [renaming, setRenaming] = useState<Conversation>();
  const [title, setTitle] = useState("");
  const sidebar = useRef<HTMLElement>(null);
  const listKey = ["conversations", checkoutId] as const;
  const conversations = useQuery({
    queryKey: listKey,
    queryFn: () => invoke<Conversation[]>("list_conversations", { checkoutId }),
    enabled: Boolean(checkoutId),
  });
  const start = useNewConversation(checkoutId, onOpen);
  const rename = useMutation({
    mutationFn: ({ conversationId, title }: { conversationId: string; title: string }) =>
      invoke<Conversation>("rename_conversation", { checkoutId, conversationId, title }),
    onSuccess: (updated) => {
      queryClient.setQueryData<Conversation[]>(listKey, (previous) =>
        previous?.map((conversation) => conversation.id === updated.id ? updated : conversation));
      void queryClient.invalidateQueries({ queryKey: listKey });
      onRenamed?.(updated);
      setRenaming(undefined);
    },
  });
  const error = start.error ?? conversations.error;
  const matching = conversations.data?.filter(({ title }) => title.toLowerCase().includes(filter.trim().toLowerCase()));
  return (
    <section ref={sidebar} className="conv-sessions" aria-label="Conversation list">
      <header className="conv-session-heading">
        <h2>Conversations</h2>
        <button className="conv-icon-button" type="button" title="New conversation" aria-label="New conversation"
          onClick={() => start.mutate()} disabled={conversations.isPending || start.isPending}>
          <Plus aria-hidden /><span className="sr-only">New conversation</span>
        </button>
      </header>
      <label className="conv-search">
        <Search aria-hidden /><input aria-label="Filter conversations" placeholder="Filter conversations" value={filter}
          onChange={(event) => setFilter(event.target.value)} />
      </label>
      {error && <p role="alert" className="conv-alert mt-2">{String(error)}</p>}
      {conversations.isPending && <p role="status" className="text-sm text-muted-foreground">Loading conversations…</p>}
      {matching?.length === 0 && <p className="conv-sidebar-note">{filter ? "No matching conversations." : "New conversations appear here."}</p>}
      <ul className="conv-session-list">
        {matching?.map((conversation) => (
          <li key={conversation.id}>
            <ConversationRow title={conversation.title} available={(action) => action === "copy-title" || action === "rename"}
              onAction={(action) => {
                if (action === "rename") {
                  setRenaming(conversation);
                  setTitle(conversation.title);
                  rename.reset();
                  return;
                }
                void Promise.resolve().then(() => navigator.clipboard.writeText(conversation.title))
                  .then(() => setNotice("Title copied."))
                  .catch(() => setNotice("Couldn’t copy the title."));
              }}>
              <button type="button" className="conv-session-button" data-conversation-id={conversation.id}
                aria-current={activeId === conversation.id ? "page" : undefined}
                onClick={() => onOpen(conversation)}>
                <MessageSquare aria-hidden /><span>{conversation.title}</span>
              </button>
            </ConversationRow>
          </li>
        ))}
      </ul>
      {notice && <p role="status" className="conv-sidebar-note">{notice}</p>}
      {renaming && <Dialog.Root open onOpenChange={(open) => { if (!open && !rename.isPending) setRenaming(undefined); }}>
        <Dialog.Portal>
          <Dialog.Overlay className="conv-dialog-scrim" />
          <Dialog.Content className="conv-dialog" onCloseAutoFocus={(event) => {
            const row = Array.from(sidebar.current?.querySelectorAll<HTMLElement>("[data-conversation-id]") ?? [])
              .find(({ dataset }) => dataset.conversationId === renaming.id);
            const focus = row ?? sidebar.current?.querySelector<HTMLElement>('input[aria-label="Filter conversations"]');
            if (focus) { event.preventDefault(); focus.focus(); }
          }}>
            <Dialog.Title>Rename conversation</Dialog.Title>
            <Dialog.Description>{renaming.title}</Dialog.Description>
            <form className="conv-dialog-form" onSubmit={(event) => {
              event.preventDefault();
              if (title.trim() && !rename.isPending) rename.mutate({ conversationId: renaming.id, title: title.trim() });
            }}>
              <label>Conversation title<input type="text" value={title} required disabled={rename.isPending}
                onChange={(event) => setTitle(event.currentTarget.value)} /></label>
              {rename.error && <p role="alert" className="conv-alert">{String(rename.error)}</p>}
              <footer>
                <Dialog.Close asChild><button type="button" className="conv-quiet-button" disabled={rename.isPending}>Cancel</button></Dialog.Close>
                <button type="submit" className="conv-primary-button" disabled={!title.trim() || rename.isPending}>
                  {rename.isPending ? "Saving…" : "Save"}
                </button>
              </footer>
            </form>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>}
    </section>
  );
}

type PanelParams = { conversationId: string; title: string };
type DesktopModel = EffortSet & { option: ModelOption };
const selectorClass = "conv-select";

export function ConversationPanel({ params, api }: IDockviewPanelProps<PanelParams>) {
  const subscribe = useCallback((update: () => void) => {
    const listener = api.onDidVisibilityChange(update);
    return () => listener.dispose();
  }, [api]);
  const visible = useSyncExternalStore(subscribe, () => api.isVisible);
  useEffect(() => {
    if (api.renderer !== "always") api.setRenderer("always");
  }, [api]);
  return <ConversationContent key={params.conversationId} {...params} visible={visible} />;
}

function ConversationContent({ conversationId, title, visible }: PanelParams & { visible: boolean }) {
  const queryClient = useQueryClient();
  const draftKey = ["conversation", conversationId, "draft"];
  const draft = useQuery({
    queryKey: draftKey,
    queryFn: async () => {
      await draftWrites.get(conversationId);
      return invoke<string>("conversation_draft", { conversationId });
    },
    staleTime: Infinity,
  });
  const historyKey = ["conversation", conversationId, "messages"];
  const history = useQuery({
    queryKey: historyKey,
    queryFn: () => invoke<ConversationHistory>("conversation_history", { conversationId }),
  });
  const historyRegion = useRef<HTMLDivElement>(null);
  const historyContents = useRef<HTMLDivElement>(null);
  const stickToEnd = useRef(!historyOffsets.has(conversationId));
  const [showJump, setShowJump] = useState(false);
  useLayoutEffect(() => {
    if (!visible || !history.data || !historyRegion.current) return;
    const region = historyRegion.current;
    region.scrollTop = stickToEnd.current ? region.scrollHeight : historyOffsets.get(conversationId) ?? 0;
  }, [conversationId, visible, history.data]);
  useEffect(() => {
    const contents = historyContents.current;
    const region = historyRegion.current;
    if (!visible || !contents || !region) return;
    const observer = new ResizeObserver(() => {
      if (stickToEnd.current) region.scrollTop = region.scrollHeight;
    });
    observer.observe(contents);
    return () => observer.disconnect();
  }, [visible]);
  const activeEdit = useRef(0);
  const [draftError, setDraftError] = useState("");
  const [draftStatus, setDraftStatus] = useState("");
  const [provider, setProvider] = useState<Provider>("codex");
  const [selection, setSelection] = useState<ModelSelection | null>(null);
  const [effort, setEffort] = useState("");
  const [mode, setMode] = useState<ConversationMode>("read_only");
  const text = draft.data ?? "";
  const goalUnavailable = mode === "goal" || goalCommand(text) !== undefined;
  const models = useQuery({
    queryKey: ["conversation-models", provider],
    queryFn: () => invoke<DesktopModel[]>("conversation_models", { provider }),
    staleTime: 60_000,
  });
  const selected = models.data?.find(({ option }) => option.selection.modelId === selection?.modelId
    && option.selection.provider === selection?.provider && option.selection.profileId === selection?.profileId);
  const antigravityModel = provider === "antigravity" ? selected?.option.selection.modelId : undefined;
  const modelEfforts = useQuery({
    queryKey: ["conversation-models", provider, antigravityModel],
    queryFn: () => invoke<DesktopModel[]>("conversation_models", { provider, modelId: antigravityModel }),
    enabled: Boolean(antigravityModel),
    staleTime: 60_000,
  });
  const efforts = antigravityModel
    ? modelEfforts.data?.find(({ option }) => option.selection.modelId === antigravityModel)
    : selected;
  const jobKey = ["conversation", conversationId, "latest-job"];
  const latestJob = useQuery({
    queryKey: jobKey,
    queryFn: () => invoke<JobInspection | null>("latest_conversation_job", { conversationId }),
    refetchInterval: (query) => pendingJob(query.state.data) || isLiveRun(query.state.data?.job.latestAttempt) ? 250 : false,
  });
  const job = latestJob.data;
  const queued = pendingJob(job);
  const attempt = job?.job.latestAttempt;
  const runKey = ["conversation", conversationId, "latest-run"];
  const latest = useQuery({
    queryKey: runKey,
    queryFn: () => invoke<RunRecord | null>("latest_conversation_run", { conversationId }),
    refetchInterval: (query) => isLiveRun(query.state.data) ? 250 : false,
  });
  const run = latest.data;
  const instructions = useConversationInstructions(conversationId, visible);
  const cancelRequested = attempt?.runId === run?.id && Boolean(attempt?.cancelRequested);
  const activity = useConversationRun(conversationId, run, cancelRequested);
  const { live } = activity;
  const busy = queued || live || isLiveRun(attempt) || activity.savedQuestions.length > 0;
  const [inspectedRun, setInspectedRun] = useState<string>();
  const activityButton = useRef<HTMLButtonElement>(null);
  const showingActivity = visible && run && inspectedRun === run.id;
  useEffect(() => {
    if (!visible) setInspectedRun(undefined);
  }, [visible]);
  function closeActivity() {
    setInspectedRun(undefined);
    activityButton.current?.focus();
  }
  useEffect(() => {
    if (attempt?.runId) void queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "latest-run"] });
  }, [attempt?.runId, conversationId, queryClient]);
  useEffect(() => {
    if (run?.status) void queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "messages"] });
  }, [run?.id, run?.status, conversationId, queryClient]);
  const send = useMutation({
    mutationFn: async (submitted: string) => {
      if (!selection) throw new Error("Choose a model");
      if (mode === "goal" || goalCommand(submitted) !== undefined) throw new Error("Goal Mode is available in simulation.");
      await saveDraft(conversationId, submitted);
      return invoke<Job>("start_conversation_turn", {
        conversationId, selection, effort: efforts?.efforts.includes(effort) ? effort : null, mode, text: submitted,
      });
    },
    onSuccess: (_, submitted) => {
      activeEdit.current++;
      queryClient.setQueryData<string>(draftKey, (current) => current === submitted ? "" : current);
      setDraftStatus("");
      setDraftError("");
      void instructions.refresh();
    },
  });
  const cancelQueued = useMutation({
    mutationFn: (jobId: string) => invoke("cancel_conversation_job", { conversationId, jobId }),
    onSettled: () => queryClient.invalidateQueries({ queryKey: jobKey }),
  });
  const recover = useMutation({
    mutationFn: ({ jobId, runId, action }: { jobId: string; runId: string; action: RecoveryAction }) =>
      invoke<Job>("recover_conversation_job", { conversationId, jobId, runId, action }),
    onSuccess: (recoveryJob) => {
      queryClient.setQueryData<JobInspection>(jobKey, { job: recoveryJob, blockers: [], recoveryActions: [] });
      void queryClient.invalidateQueries({ queryKey: jobKey });
    },
  });

  function editDraft(text: string) {
    const edit = ++activeEdit.current;
    setDraftError("");
    setDraftStatus("Saving draft…");
    queryClient.setQueryData(draftKey, text);
    void saveDraft(conversationId, text).then(() => {
      if (edit === activeEdit.current) setDraftStatus(text ? "Draft saved" : "");
    }, (error: unknown) => {
      if (edit === activeEdit.current) {
        setDraftStatus("");
        setDraftError(String(error));
      }
    });
  }

  const canSend = Boolean(text.trim() && !goalUnavailable && !latest.isPending && !latestJob.isPending && selected?.option.selectable && !send.isPending && !recover.isPending);
  const queueNext = busy || instructions.data.some((instruction) => !instruction.recordedContext);

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!canSend) return;
    stickToEnd.current = true;
    if (historyRegion.current) historyRegion.current.scrollTop = historyRegion.current.scrollHeight;
    setShowJump(false);
    send.mutate(text);
  }

  return (
    <section className="conv-pane" aria-label={title}>
      <header className="conv-heading">
        <MessageSquare aria-hidden />
        <h1>{title}</h1>
        <div className="ml-auto flex items-center gap-1">
        {run && <button ref={activityButton} type="button" className="conv-quiet-button" aria-expanded={Boolean(showingActivity)}
          onClick={() => showingActivity ? closeActivity() : setInspectedRun(run.id)}>
          <Activity aria-hidden /> Activity
        </button>}
        <ConversationHistoryControls conversationId={conversationId} history={history.data}
          disabled={live || isLiveRun(attempt) || send.isPending} />
        <ConversationSubagentControls conversationId={conversationId} />
        </div>
      </header>
      <div className="conv-layout">
      <div className="conv-chat">
      <div ref={historyRegion} className="conv-history" role="region" aria-label="Conversation history"
        onScroll={(event) => {
          const region = event.currentTarget;
          if (!visible || !region.clientHeight) return;
          historyOffsets.set(conversationId, region.scrollTop);
          stickToEnd.current = region.scrollHeight - region.clientHeight - region.scrollTop < 32;
          setShowJump(!stickToEnd.current);
        }}>
        <div ref={historyContents} className="conv-reading">
        {history.isPending && <p role="status" className="text-sm text-muted-foreground">Loading messages…</p>}
        {history.error && <p role="alert" className="conv-alert">{String(history.error)}</p>}
        {history.data?.messages.length === 0 && !live && !queued && <div className="conv-empty">
          <MessageSquare aria-hidden />
          <h2>Start a conversation</h2>
          <p>Choose a model and ask about this repository.</p>
        </div>}
        <ol className="conv-messages">
          {history.data?.messages.map((message) => (
            <li key={message.revisionId} className={message.role === "user" ? "conv-user" : "conv-assistant"}>
              <p className="sr-only">{message.role === "user" ? "You" : "Assistant"}</p>
              {message.role === "user" ? <p>{message.text}</p> : <ConversationMarkdown>{message.text}</ConversationMarkdown>}
            </li>
          ))}
        </ol>
        {(latestJob.error || cancelQueued.error || recover.error) && <p role="alert" className="conv-alert">{String(latestJob.error || cancelQueued.error || recover.error)}</p>}
        {latest.error && <p role="alert" className="conv-alert">{String(latest.error)}</p>}
        {activity.error && <p role="alert" className="conv-alert">{String(activity.error)}</p>}
        {live && activity.output && <div className="conv-stream" aria-label="Streaming response">
          <ConversationMarkdown>{activity.output}</ConversationMarkdown>
        </div>}
        {run && <ConversationRequests activity={activity} conversationId={conversationId} runId={run.id} />}
        {job?.job.mode === "approved_write" && attempt && !isLiveRun(attempt) && (
          <WorktreeEvidence key={attempt.runId} projectId={job.job.projectId} runId={attempt.runId} ownerTaskId={job.job.worktreeOwnerTaskId} />
        )}
        {job && attempt && !busy && job.recoveryActions.length > 0 && (
          <section className="conv-recovery" aria-label="Recover job">
            <div className="conv-recovery-message"><RotateCcw size={16} aria-hidden="true" /><div>
              <h2>Recover this turn</h2>
              <p>{job.recoveryActions.includes("resume") ? "Continue from a recorded checkpoint or retry with the saved input." : "Start another attempt with the saved input."}</p>
            </div></div>
            <div className="flex flex-wrap gap-2">
            {job.recoveryActions.map((action) => <Button key={action} type="button" variant="outline" disabled={recover.isPending || send.isPending}
              onClick={() => recover.mutate({ jobId: job.job.id, runId: attempt.runId, action })}>
              {action === "retry" ? "Retry saved input" : "Resume from checkpoint"}
            </Button>)}
            </div>
          </section>
        )}
        </div>
      </div>
      <div className="conv-composer-area">
        {showJump && <button type="button" className="conv-jump" onClick={() => {
          stickToEnd.current = true;
          if (historyRegion.current) historyRegion.current.scrollTop = historyRegion.current.scrollHeight;
          setShowJump(false);
        }}><ArrowDown aria-hidden /> Jump to latest</button>}
        {Boolean(activity.inputs.length || activity.savedQuestions.length) && <button className="conv-request-link" type="button" onClick={() => {
          const request = historyRegion.current?.querySelector<HTMLElement>('[aria-label="Pending requests"]');
          request?.scrollIntoView({ block: "nearest" });
          (request?.querySelector<HTMLElement>("button, input, select") ?? request)?.focus({ preventScroll: true });
        }}>{job?.queuedQuestion ? "View answer" : "Waiting for input"} <ArrowUp aria-hidden /></button>}
        <ConversationInstructions conversationId={conversationId} instructions={instructions} />
        {queued && job && (job.job.downstreamWait || job.queuedQuestion || (job.job.queuedRecovery && !instructions.data.some(({ id }) => id === job.job.id))) && <div className="conv-queued">
          <strong>{job.job.downstreamWait ? "Waiting for downstream jobs" : job.queuedQuestion ? "Queued answer" : "Queued message"}</strong>
          <div>
            <p role="status" className="text-muted-foreground">{waitReason(job.blockers[0])}</p>
            <button type="button" className="conv-quiet-button" disabled={cancelQueued.isPending}
              onClick={() => cancelQueued.mutate(job.job.id)}>
              {cancelQueued.isPending ? "Cancelling…" : job.queuedQuestion ? "Cancel queued answer" : job.job.downstreamWait ? "Cancel downstream wait" : "Cancel queued message"}
            </button>
          </div>
        </div>}
      <form onSubmit={submit} className="conv-composer">
          <label htmlFor={`draft-${conversationId}`} className="sr-only">Message draft</label>
          <textarea id={`draft-${conversationId}`} placeholder="Ask about this repository…"
            value={text} onChange={(event) => editDraft(event.target.value)} disabled={draft.isPending || Boolean(draft.error) || send.isPending}
            onKeyDown={(event) => {
              if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && !event.nativeEvent.isComposing) {
                event.preventDefault();
                event.currentTarget.form?.requestSubmit();
              }
            }} />
          <div className="conv-composer-toolbar">
              <select aria-label="Provider" className={selectorClass} value={provider} onChange={(event) => {
                setProvider(event.target.value as Provider);
                setSelection(null);
                setEffort("");
              }}>
                <option value="codex">Codex</option>
                <option value="claude">Claude Code</option>
                <option value="antigravity">Antigravity</option>
              </select>
              <ModelPicker label="Model" layout="inline" options={models.data?.map(({ option }) => option) ?? []} loading={models.isPending}
                selection={{ mode: "single", selected: selection, onChange: (model) => { setSelection(model); setEffort(""); } }} />
              <ConversationModeSelect mode={mode} onChange={setMode} />
              <EffortSelect capabilities={efforts} loading={Boolean(antigravityModel && modelEfforts.isPending)}
                className={selectorClass} value={effort} onChange={setEffort} />
              {live && <StopConversation {...activity} />}
                <Button type="submit" className="conv-send" disabled={!canSend}>
                  {send.isPending ? "Queueing…" : goalUnavailable ? "Start goal" : queueNext ? "Queue for next turn" : "Send"} <ArrowUp className="size-3.5" aria-hidden />
                </Button>
          </div>
          {goalUnavailable && <p className="conv-goal-note" role="status"><Target size={14} aria-hidden />
            Goal Mode is available in Simulation while runtime integration is pending.
          </p>}
          {models.error && <div className="flex flex-wrap items-center gap-2">
            <p role="alert" className="conv-alert">{String(models.error)}</p>
            <Button type="button" variant="outline" onClick={() => void models.refetch()}>Refresh models</Button>
          </div>}
          {antigravityModel && modelEfforts.error && <div className="flex flex-wrap items-center gap-2">
            <p role="alert" className="conv-alert">Could not load effort levels for this model.</p>
            <Button type="button" variant="outline" onClick={() => void modelEfforts.refetch()}>Retry effort lookup</Button>
          </div>}
          {draftStatus && <p role="status" className="text-xs text-muted-foreground">{draftStatus}</p>}
          {(draftError || draft.error) && <p role="alert" className="conv-alert">{draftError || String(draft.error)}</p>}
          {send.error && <p role="alert" className="conv-alert">{String(send.error)}</p>}
      </form>
      </div>
      </div>
      {showingActivity && <ConversationActivity key={run.id} conversationId={conversationId} run={run} projection={activity.projection} onClose={closeActivity} />}
      </div>
    </section>
  );
}
