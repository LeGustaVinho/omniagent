import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Activity, Square, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { RunRecord } from "@/bindings/RunRecord";
import type { SubagentAuditGap } from "@/bindings/SubagentAuditGap";
import { Button } from "@/components/ui/button";
import { ConversationChildren, recordChildActivity, type ChildActivity, type ChildLocation, type Delegation } from "@/conversation-children";
import { ConversationDeliveries, type ConversationInput } from "@/conversation-deliveries";
import { PendingInput, type ConversationPendingInput } from "@/conversation-inputs";

type EventData =
  | { type: "assistant_delta" | "assistant_message" | "plan_proposed"; text: string }
  | { type: "error"; message: string }
  | { type: "tool"; name: string; phase: string }
  | { type: "runtime_inspected" }
  | { type: "downstream_wait_requested"; wait_id: string }
  | { type: "downstream_continuation"; wait_id: string; parent_run_id: string }
  | { type: "reasoning"; text: string; summary: boolean }
  | { type: "usage"; input: number | null; output: number | null; cached_input: number | null }
  | { type: "context_window_usage"; used: number; size: number }
  | { type: "subagent_audit_gap"; report: SubagentAuditGap }
  | ChildActivity;
type Page = {
  cursor: string;
  more: boolean;
  events: { data: EventData; location: ChildLocation }[];
};
type SavedQuestion = {
  event: ConversationPendingInput;
  state: "saving" | "saved" | "needs_review";
  reason: "timeout" | "provider_expired" | "turn_stopped" | "runtime_restart" | "transport_closed" | "structured_output";
  answerRecorded: boolean;
};
const saveReasons: Record<SavedQuestion["reason"], string> = {
  timeout: "The live reply deadline expired.",
  provider_expired: "The provider closed the live request.",
  turn_stopped: "The turn ended with this question open.",
  runtime_restart: "The runtime restarted with this question open.",
  transport_closed: "The provider connection closed.",
  structured_output: "The turn returned a question.",
};
type Projection = {
  cursor: string;
  streamed: string;
  final: string;
  plan: string;
  error: string;
  delegation: Delegation;
  auditGaps: SubagentAuditGap[];
  reasoning: Extract<EventData, { type: "reasoning" }>[];
  tools: Extract<EventData, { type: "tool" }>[];
  usage?: Extract<EventData, { type: "usage" }>;
  contextWindow?: Extract<EventData, { type: "context_window_usage" }>;
};
const emptyProjection: Projection = {
  cursor: "0", streamed: "", final: "", plan: "", error: "", reasoning: [], tools: [],
  delegation: { children: {}, parentActivity: {} }, auditGaps: [],
};
const labels = {
  starting: "Starting", running: "Running", waiting_for_input: "Waiting for input",
  succeeded: "Completed", failed: "Failed", cancelled: "Cancelled", interrupted: "Interrupted",
};
const liveStatuses = new Set(["starting", "running", "waiting_for_input"]);

export function isLiveRun(run: Pick<RunRecord, "status"> | null | undefined) {
  return Boolean(run && liveStatuses.has(run.status));
}

export function useConversationRun(conversationId: string, run: RunRecord | null | undefined, cancelRequested: boolean) {
  const queryClient = useQueryClient();
  const runId = run?.id;
  const target = { conversationId, runId };
  const live = isLiveRun(run);
  const projectionKey = ["conversation-run", runId, "output", live];
  const inputKey = ["conversation-run", runId, "pending"];
  const [stopState, setStopState] = useState<{ runId: string; error?: string }>();
  const result = useQuery({
    queryKey: projectionKey,
    enabled: Boolean(runId),
    refetchInterval: live ? 200 : false,
    queryFn: async () => {
      const prior = queryClient.getQueryData<Projection>(projectionKey)
        ?? queryClient.getQueryData<Projection>(["conversation-run", runId, "output", true])
        ?? emptyProjection;
      const current = {
        ...prior,
        reasoning: [...prior.reasoning],
        tools: [...prior.tools],
        delegation: { children: { ...prior.delegation.children }, parentActivity: { ...prior.delegation.parentActivity } },
        auditGaps: [...prior.auditGaps],
      };
      let page: Page;
      do {
        page = await invoke<Page>("conversation_events", { ...target, after: current.cursor });
        current.cursor = page.cursor;
        for (const event of page.events) {
          const data = event.data;
          if (data.type === "child_activity") recordChildActivity(current.delegation, data, event.location);
          if (data.type === "subagent_audit_gap") current.auditGaps.push(data.report);
          if (event.location.parentItemId !== null) continue;
          if (data.type === "assistant_delta") current.streamed += data.text;
          if (data.type === "assistant_message") current.final += data.text;
          if (data.type === "plan_proposed") current.plan = data.text;
          if (data.type === "error") current.error = data.message;
          if (data.type === "reasoning") current.reasoning.push(data);
          if (data.type === "tool") current.tools.push(data);
          if (data.type === "runtime_inspected") current.tools.push({ type: "tool", name: "inspect_omniagent", phase: "Scope inspected" });
          if (data.type === "downstream_wait_requested") current.tools.push({ type: "tool", name: "wait_for_jobs", phase: `Wait ${data.wait_id} recorded` });
          if (data.type === "downstream_continuation") current.tools.push({ type: "tool", name: "wait_for_jobs", phase: `Continued wait ${data.wait_id}` });
          if (data.type === "usage") current.usage = data;
          if (data.type === "context_window_usage") current.contextWindow = data;
        }
      } while (page.more);
      return current;
    },
  });
  const inputs = useQuery({
    queryKey: inputKey,
    enabled: live,
    refetchInterval: live ? 200 : false,
    queryFn: () => invoke<ConversationPendingInput[]>("pending_conversation_inputs", target),
  });
  const saved = useQuery({
    queryKey: ["conversation-run", runId, "saved-questions", live],
    enabled: Boolean(runId),
    refetchInterval: (query) => live ? 200 : query.state.data?.length ? 800 : false,
    queryFn: () => invoke<SavedQuestion[]>("saved_conversation_questions", target),
  });
  const currentStop = stopState?.runId === runId ? stopState : undefined;
  const stopping = cancelRequested || Boolean(currentStop && !currentStop.error);
  async function stop() {
    if (!runId || stopping) return;
    setStopState({ runId });
    try {
      await invoke("stop_conversation_turn", target);
    } catch (error) {
      setStopState({ runId, error: String(error) });
    }
  }

  return {
    projection: result.data,
    output: result.data?.plan || result.data?.final || result.data?.streamed,
    error: currentStop?.error || result.data?.error || result.error || inputs.error || saved.error,
    inputs: live ? inputs.data ?? [] : [],
    savedQuestions: saved.data ?? [],
    refreshRequests: () => {
      if (live) void inputs.refetch();
      void saved.refetch();
      void queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "latest-job"] });
      void queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "latest-run"] });
      void queryClient.invalidateQueries({ queryKey: ["project-job-queue"] });
    },
    live,
    stop,
    stopping,
  };
}

export function StopConversation({ stop, stopping }: ReturnType<typeof useConversationRun>) {
  return <Button type="button" variant="outline" className="conv-stop" disabled={stopping} onClick={() => void stop()}>
    <Square className="size-3.5" aria-hidden /> {stopping ? "Stopping…" : "Stop"}
  </Button>;
}

export function ConversationRequests({ activity, conversationId, runId }: {
  activity: ReturnType<typeof useConversationRun>;
  conversationId: string;
  runId: string;
}) {
  if (!activity.inputs.length && !activity.savedQuestions.length) return null;
  const needsReview = !activity.inputs.length &&
    activity.savedQuestions.every((question) => question.state === "needs_review");
  const answered = activity.savedQuestions.length > 0 && !activity.inputs.length &&
    activity.savedQuestions.every((question) => question.answerRecorded);
  const savedIds = new Set(activity.savedQuestions.map(({ event }) => event.data.request_id));
  return (
    <section aria-label="Pending requests" className="conv-requests" tabIndex={-1}>
      <p className="conv-waiting" role="status">{needsReview ? "Question needs review" : answered ? "Waiting to continue" : "Waiting for input"}</p>
      {activity.inputs.filter(({ data }) => !savedIds.has(data.request_id)).map((request) => <PendingInput key={JSON.stringify(request.data.request_id)}
        input={request} conversationId={conversationId} runId={runId} onResolved={activity.refreshRequests} />)}
      {activity.savedQuestions.map(({ event, state, reason, answerRecorded }) => event.data.type === "question" && (
        state === "saved" && !answerRecorded
          ? <PendingInput key={JSON.stringify(event.data.request_id)} input={event} savedReason={saveReasons[reason]}
              conversationId={conversationId} runId={runId} onResolved={activity.refreshRequests} />
          :
        <div key={JSON.stringify(event.data.request_id)} className="grid gap-2 rounded-md border border-border bg-card p-3">
          <p className="text-sm font-medium">{event.data.normalized?.message || "Input requested"}</p>
          <p className="text-xs text-muted-foreground">{answerRecorded ? "Answer recorded" : state === "saving" ? "Saving the turn checkpoint…" : saveReasons[reason]}</p>
          {!answerRecorded && event.data.normalized?.fields.map((field) => <div key={field.id} className="grid gap-1 text-sm">
            <p>{field.label}</p>
            {field.description && <p className="text-xs text-muted-foreground">{field.description}</p>}
            {field.input.kind === "choice" && <ul className="list-disc pl-5 text-xs">
              {field.input.options.map(({ id, label }) => <li key={id}>{label}</li>)}
            </ul>}
          </div>)}
        </div>))}
    </section>
  );
}

export function ConversationActivity({ conversationId, run, projection, onClose, embedded }: {
  conversationId: string;
  run: RunRecord;
  projection?: Projection;
  onClose: () => void;
  embedded?: boolean;
}) {
  const [showInput, setShowInput] = useState(false);
  const heading = useRef<HTMLButtonElement>(null);
  const live = isLiveRun(run);
  useEffect(() => { heading.current?.focus(); }, []);
  const input = useQuery({
    queryKey: ["conversation-run", run.id, "input"],
    enabled: showInput,
    refetchInterval: showInput && live ? 400 : false,
    queryFn: () => invoke<ConversationInput | null>("conversation_input", { conversationId, runId: run.id }),
  });
  const usage = projection?.usage;
  return (
    <aside className={embedded ? "conv-activity conv-activity-embedded" : "conv-activity"} aria-label="Conversation activity" onKeyDown={(event) => {
      if (event.key !== "Escape") return;
      event.stopPropagation();
      onClose();
    }}>
      {!embedded && <header className="conv-activity-heading">
        <Activity className="size-4" aria-hidden /><h2>Activity</h2>
        <button ref={heading} type="button" className="conv-icon-button ml-auto" aria-label="Close activity" onClick={onClose}>
          <X aria-hidden />
        </button>
      </header>}
      <div className="conv-activity-content">
      <dl>
        <dt>Execution</dt><dd role="status">{labels[run.status]}</dd>
        <dt>Run ID</dt><dd className="break-all font-mono">{run.id}</dd>
        {usage && <>
          <dt>Input tokens</dt><dd>{usage.input?.toLocaleString() ?? "Unavailable"}</dd>
          <dt>Output tokens</dt><dd>{usage.output?.toLocaleString() ?? "Unavailable"}</dd>
          <dt>Cached input</dt><dd>{usage.cached_input?.toLocaleString() ?? "Unavailable"}</dd>
        </>}
        {projection?.contextWindow && <>
          <dt>Context tokens</dt>
          <dd>{projection.contextWindow.used.toLocaleString()} / {projection.contextWindow.size.toLocaleString()}</dd>
        </>}
      </dl>
      {projection && <ConversationChildren delegation={projection.delegation} />}
      {Boolean(projection?.auditGaps.length) && <section aria-label="Subagent audit gaps" className="grid gap-2">
        <h2 className="text-sm font-medium">Subagent audit gaps</h2>
        {projection?.auditGaps.map((gap) => <div key={gap.source} className="grid gap-1 text-xs text-muted-foreground">
          {gap.nativeDelegationDetected && <p role="alert">The CLI reported a native child after native delegation was disabled.</p>}
          {gap.missing.length > 0 && <p>
            {gap.source === "native" ? "Native delegation" : "Managed child"} · Missing {gap.missing.map((field) => field.replaceAll("_", " ")).join(", ")}
            {gap.source === "native" && ` · ${gap.affectedChildren} identified with gaps · ${gap.unidentified} without confirmed identity`}
          </p>}
        </div>)}
      </section>}
      {Boolean(projection?.tools.length) && <details>
        <summary>Tool activity</summary>
        <ol>{projection?.tools.map((tool, index) => <li key={index}>{tool.name} · {tool.phase}</li>)}</ol>
      </details>}
      {Boolean(projection?.reasoning.length) && <details>
        <summary>Reported reasoning</summary>
        <ol>{projection?.reasoning.map((entry, index) =>
          <li key={index}><small>{entry.summary ? "Summary" : "Provider reasoning"}</small><p>{entry.text}</p></li>)}</ol>
      </details>}
      <Button type="button" variant="outline" aria-expanded={showInput} onClick={() => setShowInput((visible) => !visible)}>Exact input</Button>
      {showInput && (
        <section className="min-w-0 rounded-md border border-border bg-card p-3" aria-label="Exact provider input">
          {input.isPending && <p className="text-xs text-muted-foreground">Loading input…</p>}
          {input.error && <p role="alert" className="text-sm text-red-300">{String(input.error)}</p>}
          {input.data === null && <p className="text-xs text-muted-foreground">No input has been prepared.</p>}
          {input.data && <ConversationDeliveries input={input.data} conversationId={conversationId} runId={run.id} live={live} />}
        </section>
      )}
      </div>
    </aside>
  );
}
