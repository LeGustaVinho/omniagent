import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { IDockviewPanelProps } from "dockview-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type FormEvent } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { AdmissionBlocker } from "@/bindings/AdmissionBlocker";
import type { Job } from "@/bindings/Job";
import type { JobInspection } from "@/bindings/JobInspection";
import type { RecoveryAction } from "@/bindings/RecoveryAction";
import type { MessageRevision } from "@/bindings/MessageRevision";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import type { RunRecord } from "@/bindings/RunRecord";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { ConversationRun, isLiveRun } from "@/conversation-run";
import { WorktreeEvidence } from "@/worktree-evidence";
import { openConversation } from "@/workspace-tabs";
import { useWorkspace } from "@/workspace-state";

const draftWrites = new Map<string, Promise<void>>();
const historyOffsets = new Map<string, number>();

function pendingJob(inspection: JobInspection | null | undefined) {
  return Boolean(inspection && (!inspection.job.latestAttempt || inspection.job.queuedRecovery) && inspection.job.queueState !== "cancelled");
}

function waitReason(blocker: AdmissionBlocker | undefined): string {
  switch (blocker?.type) {
    case "queue_paused": return "The job queue is paused.";
    case "job_paused": return "This job is paused.";
    case "context_changed": return "The conversation changed since this message was queued.";
    case "conversation_busy": return "Waiting for the active conversation run.";
    case "capacity": return `Waiting for an execution slot (${blocker.active} of ${blocker.limit} in use).`;
    case "policy": return blocker.reason;
    case "dependency": return "Waiting for a required job to complete.";
    case "approval": return "Waiting for approval.";
    case "writer": return "Waiting for the worktree writer.";
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
  const queryClient = useQueryClient();
  const conversations = useQuery({
    queryKey: ["conversations", checkoutId],
    queryFn: () => invoke<Conversation[]>("list_conversations", { checkoutId }),
    enabled: Boolean(checkoutId),
  });
  const start = useMutation({
    mutationFn: () => invoke<Conversation>("create_conversation", {
      checkoutId,
      title: `Conversation ${(conversations.data?.length ?? 0) + 1}`,
    }),
    onSuccess: (conversation) => {
      void queryClient.invalidateQueries({ queryKey: ["conversations", checkoutId] });
      openConversation(containerApi, conversation);
    },
  });
  const error = start.error ?? conversations.error;
  return (
    <section className="h-full overflow-auto p-6">
      <header className="mb-5 flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-xl font-semibold">Conversations</h1>
        <Button onClick={() => start.mutate()} disabled={conversations.isPending || start.isPending}>
          New conversation
        </Button>
      </header>
      {error && <p role="alert" className="mb-4 text-sm text-red-300">{String(error)}</p>}
      {conversations.isPending && <p role="status" className="text-sm text-muted-foreground">Loading conversations…</p>}
      <ul className="grid max-w-2xl gap-2">
        {conversations.data?.map((conversation) => (
          <li key={conversation.id}>
            <Button className="w-full justify-start" variant="outline" onClick={() => openConversation(containerApi, conversation)}>
              {conversation.title}
            </Button>
          </li>
        ))}
      </ul>
    </section>
  );
}

type PanelParams = { conversationId: string; title: string };
type DesktopModel = { option: ModelOption; efforts: string[] };
type ProfileMode = "read_only" | "plan";
const selectorClass = "rounded-md border border-input bg-background px-2 py-2 text-sm";

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
    queryFn: () => invoke<MessageRevision[]>("conversation_messages", { conversationId }),
  });
  const historyRegion = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    if (!visible || !history.data || !historyRegion.current) return;
    historyRegion.current.scrollTop = historyOffsets.get(conversationId) ?? 0;
  }, [conversationId, visible, history.data]);
  const activeEdit = useRef(0);
  const [draftError, setDraftError] = useState("");
  const [draftStatus, setDraftStatus] = useState("");
  const [provider, setProvider] = useState<Provider>("codex");
  const [selection, setSelection] = useState<ModelSelection | null>(null);
  const [effort, setEffort] = useState("");
  const [mode, setMode] = useState<ProfileMode>("read_only");
  const text = draft.data ?? "";
  const models = useQuery({
    queryKey: ["conversation-models", provider],
    queryFn: () => invoke<DesktopModel[]>("conversation_models", { provider }),
    staleTime: 60_000,
  });
  const selected = models.data?.find(({ option }) => option.selection.modelId === selection?.modelId && option.selection.provider === selection?.provider);
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
  const busy = queued || isLiveRun(run) || isLiveRun(attempt);
  useEffect(() => {
    if (attempt?.runId) void queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "latest-run"] });
  }, [attempt?.runId, conversationId, queryClient]);
  useEffect(() => {
    if (run?.status) void queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "messages"] });
  }, [run?.id, run?.status, conversationId, queryClient]);
  const send = useMutation({
    mutationFn: async (submitted: string) => {
      if (!selection) throw new Error("Choose a model");
      await saveDraft(conversationId, submitted);
      return invoke<Job>("start_conversation_turn", {
        conversationId, selection, effort: effort || null, mode, text: submitted,
      });
    },
    onSuccess: (queuedJob, submitted) => {
      activeEdit.current++;
      queryClient.setQueryData<string>(draftKey, (current) => current === submitted ? "" : current);
      setDraftStatus("");
      setDraftError("");
      queryClient.setQueryData<JobInspection>(jobKey, { job: queuedJob, blockers: [], recoveryActions: [] });
      void queryClient.invalidateQueries({ queryKey: jobKey });
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

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!busy && text.trim() && selected?.option.selectable && !send.isPending && !recover.isPending) send.mutate(text);
  }

  return (
    <section className="flex h-full min-h-0 flex-col" aria-label={title}>
      <header className="border-b border-border px-6 py-4">
        <h1 className="text-xl font-semibold">{title}</h1>
      </header>
      <div ref={historyRegion} className="min-h-0 flex-1 overflow-auto px-6 py-4" role="region" aria-label="Conversation history"
        onScroll={(event) => {
          if (visible && event.currentTarget.clientHeight > 0) historyOffsets.set(conversationId, event.currentTarget.scrollTop);
        }}>
        {history.isPending && <p role="status" className="text-sm text-muted-foreground">Loading messages…</p>}
        {history.error && <p role="alert" className="text-sm text-red-300">{String(history.error)}</p>}
        <ol className="mx-auto grid max-w-3xl gap-4">
          {history.data?.map((message) => (
            <li key={message.revisionId} className="rounded-md border border-border bg-card px-4 py-3">
              <p className="mb-1 text-xs font-medium text-muted-foreground">{message.role === "user" ? "You" : "Assistant"}</p>
              <p className="whitespace-pre-wrap break-words text-sm">{message.text}</p>
            </li>
          ))}
        </ol>
        {queued && job && <div className="mx-auto mt-4 flex max-w-3xl flex-wrap items-center gap-2">
          <p role="status" className="text-sm text-muted-foreground">{waitReason(job.blockers[0])}</p>
          <Button variant="outline" disabled={cancelQueued.isPending} onClick={() => cancelQueued.mutate(job.job.id)}>Cancel queued message</Button>
        </div>}
        {(latestJob.error || cancelQueued.error || recover.error) && <p role="alert" className="text-sm text-red-300">{String(latestJob.error || cancelQueued.error || recover.error)}</p>}
        {latest.error && <p role="alert" className="text-sm text-red-300">{String(latest.error)}</p>}
        {run && <ConversationRun key={run.id} conversationId={conversationId} run={run} cancelRequested={attempt?.runId === run.id && attempt.cancelRequested} />}
        {job?.job.mode === "approved_write" && attempt && !isLiveRun(attempt) && (
          <WorktreeEvidence key={attempt.runId} projectId={job.job.projectId} runId={attempt.runId} ownerTaskId={job.job.taskId} />
        )}
        {job && attempt && !busy && job.recoveryActions.length > 0 && (
          <div className="mx-auto mt-3 flex max-w-3xl flex-wrap gap-2" aria-label="Recover job">
            {job.recoveryActions.map((action) => <Button key={action} variant="outline" disabled={recover.isPending || send.isPending}
              onClick={() => recover.mutate({ jobId: job.job.id, runId: attempt.runId, action })}>
              {action === "retry" ? "Retry saved input" : "Resume from checkpoint"}
            </Button>)}
          </div>
        )}
      </div>
      <form onSubmit={submit} className="max-h-[55%] overflow-auto border-t border-border p-4">
        <div className="mx-auto grid max-w-3xl gap-2">
          <div className="flex flex-wrap items-end gap-2">
            <label className="grid gap-1 text-xs">Provider
              <select className={selectorClass} value={provider} onChange={(event) => {
                setProvider(event.target.value as Provider);
                setSelection(null);
                setEffort("");
              }}>
                <option value="codex">Codex</option>
                <option value="claude">Claude Code</option>
                <option value="antigravity">Antigravity</option>
              </select>
            </label>
            <label className="grid gap-1 text-xs">Thinking level
              <select className={selectorClass} value={effort} onChange={(event) => setEffort(event.target.value)}>
                <option value="">Default</option>
                {selected?.efforts.map((level) => <option key={level} value={level}>{level}</option>)}
              </select>
            </label>
            <label className="grid gap-1 text-xs">Profile
              <select className={selectorClass} value={mode} onChange={(event) => setMode(event.target.value as ProfileMode)}>
                <option value="read_only">Read-only</option>
                <option value="plan">Planning</option>
              </select>
            </label>
          </div>
          <ModelPicker label="Model" options={models.data?.map(({ option }) => option) ?? []}
            selection={{ mode: "single", selected: selection, onChange: (selection) => { setSelection(selection); setEffort(""); } }} />
          {models.isPending && <p role="status" className="text-xs text-muted-foreground">Checking CLI models…</p>}
          {models.error && <div className="flex flex-wrap items-center gap-2">
            <p role="alert" className="text-xs text-red-300">{String(models.error)}</p>
            <Button type="button" variant="outline" onClick={() => void models.refetch()}>Refresh models</Button>
          </div>}
          <label htmlFor={`draft-${conversationId}`} className="text-sm font-medium">Message draft</label>
          <textarea id={`draft-${conversationId}`} className="min-h-24 rounded-md border border-input bg-background p-3 text-sm focus-visible:outline-ring"
            value={text} onChange={(event) => editDraft(event.target.value)} disabled={draft.isPending || Boolean(draft.error) || send.isPending} />
          <div className="flex flex-wrap items-center justify-between gap-2">
            <Button type="submit" disabled={!text.trim() || busy || latest.isPending || latestJob.isPending || !selected?.option.selectable || send.isPending || recover.isPending}>
              {send.isPending ? "Starting…" : "Send"}
            </Button>
            {draftStatus && <p role="status" className="text-xs text-muted-foreground">{draftStatus}</p>}
          </div>
          {(draftError || draft.error) && <p role="alert" className="text-sm text-red-300">{draftError || String(draft.error)}</p>}
          {send.error && <p role="alert" className="text-sm text-red-300">{String(send.error)}</p>}
        </div>
      </form>
    </section>
  );
}
