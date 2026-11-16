import { useEffect, useId, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { RunRecord } from "@/bindings/RunRecord";
import type { SessionMode } from "@/bindings/SessionMode";
import { isLiveRun } from "@/conversation-run";
import { useSideRequests, useSuggestionMode, type SideSubmission } from "@/conversation-side-requests";

type NativeOutcome = { kind: "available"; text: string } |
  { kind: "unavailable" | "empty" | "failed" | "timed_out" | "cancelled" | "ineligible" };

export type ConversationSuggestion = { source: "native" | "omniagent"; text: string };

type WatchedTurn = {
  runId: string;
  scope: string;
  draftVersion: number;
  invalidated: boolean;
  lookupStarted?: boolean;
  completedContext?: string;
};

export function useConversationSuggestion({ conversationId, contextId, run, selection, effort, mode, draft, draftVersion, eligible, visible, liveFresh = true }: {
  conversationId: string;
  contextId?: string;
  run?: RunRecord | null;
  selection: ModelSelection | null;
  effort: string | null;
  mode: SessionMode | "goal";
  draft: string;
  draftVersion: number;
  eligible: boolean;
  visible: boolean;
  liveFresh?: boolean;
}): ConversationSuggestion | undefined {
  const viewId = useId();
  const [watched, setWatched] = useState<WatchedTurn>();
  const configured = useSuggestionMode(visible).data;
  const runId = run?.id;
  const live = liveFresh && isLiveRun(run);
  const empty = draft.trim().length === 0;
  const enabled = visible && Boolean(selection && configured && configured !== "off" && mode !== "goal" && mode !== "plan");
  const scope = JSON.stringify([conversationId, selection?.provider, selection?.profileId, selection?.modelId, effort, mode, configured]);

  useEffect(() => {
    setWatched((previous) => {
      if (previous?.runId !== runId && runId && live && enabled) {
        return { runId, scope, draftVersion, invalidated: !empty };
      }
      if (!previous || previous.invalidated) return previous;
      if (!enabled || !empty || previous.scope !== scope || previous.draftVersion !== draftVersion ||
          (previous.lookupStarted && !eligible) || (previous.completedContext && previous.completedContext !== contextId)) {
        return { ...previous, invalidated: true };
      }
      return previous;
    });
  }, [runId, live, enabled, empty, scope, draftVersion, contextId, eligible]);

  const current = Boolean(enabled && eligible && empty && contextId && run?.status === "succeeded" &&
    watched && !watched.invalidated && watched.runId === runId && watched.scope === scope && watched.draftVersion === draftVersion &&
    (!watched.completedContext || watched.completedContext === contextId));
  const queryKey = ["composer-suggestion", conversationId, viewId, runId, contextId, scope, draftVersion];
  const native = useQuery({
    queryKey: [...queryKey, "native"], enabled: current, retry: false, staleTime: Infinity,
    queryFn: () => invoke<NativeOutcome>("composer_native_suggestion", { conversationId, contextId, runId, selection, effort, mode }),
  });
  const nativeSettled = native.data !== undefined && native.data.kind !== "ineligible";
  useEffect(() => {
    if (!current || !contextId) return;
    setWatched((previous) => {
      if (!previous || (previous.lookupStarted && (!nativeSettled || previous.completedContext))) return previous;
      return { ...previous, lookupStarted: true, completedContext: nativeSettled ? contextId : undefined };
    });
  }, [current, nativeSettled, contextId]);

  const fallbackAllowed = current && configured === "omniagent" && native.data?.kind === "unavailable";
  const fallback = useQuery({
    queryKey: [...queryKey, "fallback"], enabled: fallbackAllowed, retry: false, staleTime: Infinity,
    queryFn: () => invoke<SideSubmission>("enqueue_conversation_side_request", {
      conversationId, expectedContextId: contextId, expectedSourceRunId: runId, selection, effort, mode,
      purpose: "suggestion", question: "",
    }),
  });
  const fallbackJob = fallbackAllowed && fallback.data?.status === "queued" ? fallback.data.jobId : undefined;
  const side = useSideRequests(conversationId, Boolean(fallbackJob));
  if (!current) return;
  if (native.data?.kind === "available") return { source: "native", text: native.data.text };
  const answer = fallbackJob ? side.data?.find((item) => item.job.id === fallbackJob && item.purpose === "suggestion" &&
    item.sourceRunId === runId && item.contextId === contextId)?.response?.outcome : undefined;
  if (answer?.kind === "answered") return { source: "omniagent", text: answer.text };
}

export function ConversationSuggestionButton({ suggestion, onAccept }: { suggestion: ConversationSuggestion; onAccept: () => void }) {
  return <button type="button" className="conv-quiet-button text-left" onClick={onAccept}>
    <span>Use suggested draft · {suggestion.source === "native" ? "Native CLI" : "OmniAgent"}</span>
    <span className="block whitespace-pre-wrap break-words">{suggestion.text}</span>
  </button>;
}
