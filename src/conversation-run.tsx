import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import type { RunRecord } from "@/bindings/RunRecord";
import { Button } from "@/components/ui/button";
import { ConversationChildren, recordChildActivity, type ChildActivity, type ChildLocation, type Delegation } from "@/conversation-children";
import { PendingInput, type ConversationPendingInput } from "@/conversation-inputs";

type EventData =
  | { type: "assistant_delta" | "assistant_message" | "plan_proposed"; text: string }
  | { type: "error"; message: string }
  | { type: "tool" }
  | ChildActivity;
type Page = {
  cursor: string;
  more: boolean;
  events: { data: EventData; location: ChildLocation }[];
};
type Projection = { cursor: string; streamed: string; final: string; plan: string; error: string; delegation: Delegation };
const emptyProjection: Projection = {
  cursor: "0", streamed: "", final: "", plan: "", error: "",
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

export function ConversationRun({ conversationId, run }: { conversationId: string; run: RunRecord }) {
  const queryClient = useQueryClient();
  const runId = run.id;
  const target = { conversationId, runId };
  const live = isLiveRun(run);
  const projectionKey = ["conversation-run", runId, "output", live];
  const inputKey = ["conversation-run", runId, "pending"];
  const [showInput, setShowInput] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [stopError, setStopError] = useState("");
  const result = useQuery({
    queryKey: projectionKey,
    refetchInterval: live ? 200 : false,
    queryFn: async () => {
      const prior = queryClient.getQueryData<Projection>(projectionKey)
        ?? queryClient.getQueryData<Projection>(["conversation-run", runId, "output", true])
        ?? emptyProjection;
      const current = {
        ...prior,
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
  const input = useQuery({
    queryKey: ["conversation-run", runId, "input"],
    enabled: showInput,
    refetchInterval: showInput && live ? 400 : false,
    queryFn: () => invoke<{ delivery: string; frames: string[] } | null>("conversation_input", target),
  });

  async function stop() {
    setStopping(true);
    setStopError("");
    try {
      await invoke("stop_conversation_turn", target);
    } catch (error) {
      setStopping(false);
      setStopError(String(error));
    }
  }

  const output = result.data?.plan || result.data?.final || result.data?.streamed;
  const error = stopError || result.data?.error || result.error || inputs.error;
  return (
    <section className="mx-auto mt-4 grid max-w-3xl gap-2" aria-label="Conversation run">
      <div className="flex flex-wrap items-center gap-2">
        <span role="status" className="text-xs text-muted-foreground">{labels[run.status]}</span>
        {live && <Button variant="outline" disabled={stopping} onClick={() => void stop()}>{stopping ? "Stopping…" : "Stop"}</Button>}
        <Button variant="outline" aria-expanded={showInput} onClick={() => setShowInput((visible) => !visible)}>Exact input</Button>
      </div>
      {error && <p role="alert" className="text-sm text-red-300">{String(error)}</p>}
      {live && output && (
        <div className="rounded-md border border-border bg-card px-4 py-3" aria-label="Streaming response">
          <p className="mb-1 text-xs font-medium text-muted-foreground">Assistant</p>
          <p className="whitespace-pre-wrap break-words text-sm">{output}</p>
        </div>
      )}
      {result.data && <ConversationChildren delegation={result.data.delegation} />}
      {live && inputs.data?.map((request) => <PendingInput key={JSON.stringify(request.data.request_id)}
        input={request} conversationId={conversationId} runId={runId} onResolved={() => void inputs.refetch()} />)}
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
    </section>
  );
}
