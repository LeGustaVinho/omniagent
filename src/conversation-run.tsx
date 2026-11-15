import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Activity, Square, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { RunRecord } from "@/bindings/RunRecord";
import { Button } from "@/components/ui/button";
import { ConversationChildren, recordChildActivity, type ChildActivity, type ChildLocation, type Delegation } from "@/conversation-children";
import { PendingInput, type ConversationPendingInput } from "@/conversation-inputs";

type EventData =
  | { type: "assistant_delta" | "assistant_message" | "plan_proposed"; text: string }
  | { type: "error"; message: string }
  | { type: "tool"; name: string; phase: string }
  | { type: "reasoning"; text: string; summary: boolean }
  | { type: "usage"; input: number | null; output: number | null; cached_input: number | null }
  | ChildActivity;
type Page = {
  cursor: string;
  more: boolean;
  events: { data: EventData; location: ChildLocation }[];
};
type Projection = {
  cursor: string;
  streamed: string;
  final: string;
  plan: string;
  error: string;
  delegation: Delegation;
  reasoning: Extract<EventData, { type: "reasoning" }>[];
  tools: Extract<EventData, { type: "tool" }>[];
  usage?: Extract<EventData, { type: "usage" }>;
};
const emptyProjection: Projection = {
  cursor: "0", streamed: "", final: "", plan: "", error: "", reasoning: [], tools: [],
  delegation: { children: {}, parentActivity: {} },
};
const labels = {
  starting: "Starting", running: "Running", waiting_for_input: "Waiting for input",
  succeeded: "Completed", failed: "Failed", cancelled: "Cancelled", interrupted: "Interrupted",
};
const liveStatuses = new Set(["starting", "running", "waiting_for_input"]);

export function isLiveRun(run: RunRecord | null | undefined) {
  return Boolean(run && liveStatuses.has(run.status));
}

export function useConversationRun(conversationId: string, run: RunRecord | null | undefined) {
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
      };
      let page: Page;
      do {
        page = await invoke<Page>("conversation_events", { ...target, after: current.cursor });
        current.cursor = page.cursor;
        for (const event of page.events) {
          const data = event.data;
          if (data.type === "child_activity") recordChildActivity(current.delegation, data, event.location);
          if (event.location.parentItemId !== null) continue;
          if (data.type === "assistant_delta") current.streamed += data.text;
          if (data.type === "assistant_message") current.final += data.text;
          if (data.type === "plan_proposed") current.plan = data.text;
          if (data.type === "error") current.error = data.message;
          if (data.type === "reasoning") current.reasoning.push(data);
          if (data.type === "tool") current.tools.push(data);
          if (data.type === "usage") current.usage = data;
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
  async function stop() {
    if (!runId) return;
    setStopState({ runId });
    try {
      await invoke("stop_conversation_turn", target);
    } catch (error) {
      setStopState({ runId, error: String(error) });
    }
  }

  const currentStop = stopState?.runId === runId ? stopState : undefined;
  return {
    projection: result.data,
    output: result.data?.plan || result.data?.final || result.data?.streamed,
    error: currentStop?.error || result.data?.error || result.error || inputs.error,
    inputs: inputs.data ?? [],
    refreshInputs: () => void inputs.refetch(),
    live,
    stop,
    stopping: Boolean(currentStop && !currentStop.error),
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
  if (!activity.live || !activity.inputs.length) return null;
  return (
    <section aria-label="Pending requests" className="conv-requests">
      <p className="conv-waiting" role="status">Waiting for input</p>
      {activity.inputs.map((request) => <PendingInput key={JSON.stringify(request.data.request_id)}
        input={request} conversationId={conversationId} runId={runId} onResolved={activity.refreshInputs} />)}
    </section>
  );
}

export function ConversationActivity({ conversationId, run, projection, onClose }: {
  conversationId: string;
  run: RunRecord;
  projection?: Projection;
  onClose: () => void;
}) {
  const [showInput, setShowInput] = useState(false);
  const heading = useRef<HTMLButtonElement>(null);
  const live = isLiveRun(run);
  useEffect(() => { heading.current?.focus(); }, []);
  const input = useQuery({
    queryKey: ["conversation-run", run.id, "input"],
    enabled: showInput,
    refetchInterval: showInput && live ? 400 : false,
    queryFn: () => invoke<{ delivery: string; frames: string[] } | null>("conversation_input", { conversationId, runId: run.id }),
  });
  const usage = projection?.usage;
  return (
    <aside className="conv-activity" aria-label="Conversation activity" onKeyDown={(event) => {
      if (event.key !== "Escape") return;
      event.stopPropagation();
      onClose();
    }}>
      <header className="conv-activity-heading">
        <Activity className="size-4" aria-hidden /><h2>Activity</h2>
        <button ref={heading} type="button" className="conv-icon-button ml-auto" aria-label="Close activity" onClick={onClose}>
          <X aria-hidden />
        </button>
      </header>
      <div className="conv-activity-content">
      <dl>
        <dt>Execution</dt><dd role="status">{labels[run.status]}</dd>
        <dt>Run ID</dt><dd className="break-all font-mono">{run.id}</dd>
        {usage && <>
          <dt>Input tokens</dt><dd>{usage.input?.toLocaleString() ?? "Unavailable"}</dd>
          <dt>Output tokens</dt><dd>{usage.output?.toLocaleString() ?? "Unavailable"}</dd>
          <dt>Cached input</dt><dd>{usage.cached_input?.toLocaleString() ?? "Unavailable"}</dd>
        </>}
      </dl>
      {projection && <ConversationChildren delegation={projection.delegation} />}
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
          {input.data && <>
            <p className="text-xs text-muted-foreground">Delivery {input.data.delivery}</p>
            <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-all text-xs">{input.data.frames.join("")}</pre>
          </>}
        </section>
      )}
      </div>
    </aside>
  );
}
