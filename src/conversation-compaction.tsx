import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { SessionMode } from "@/bindings/SessionMode";
import { Button } from "@/components/ui/button";

type CompactionSettings = { automatic: boolean; thresholdPercent: number };
type CompactionEstimate = {
  historyTokens: number; pendingTokens: number; availableTokens: number;
  projectedHistoryTokens: number | null; overBudget: boolean; automaticNeeded: boolean;
};
type CompactionStatus = {
  contextId: string;
  settings: CompactionSettings;
  estimate: CompactionEstimate | null;
  eligible: boolean;
  providerCompactionObserved: boolean;
  activeSummary: { id: string; sourceContextId: string; run: { runId: string; requestedModel: string | null; observedModel: string | null } } | null;
  request: { artifactId: string; sourceContextId: string; jobId: string; status: "queued" | "activated" | "failed" | "stale"; detail: string | null } | null;
};

const pollInterval = 1000;

export function useConversationCompaction({ conversationId, selection, effort, mode, pendingText, enabled }: {
  conversationId: string;
  selection: ModelSelection | null;
  effort: string | null;
  mode: SessionMode;
  pendingText: string;
  enabled: boolean;
}) {
  const client = useQueryClient();
  const profile = { conversationId, selection, effort, mode };
  const identity = JSON.stringify(profile);
  const queryKey = ["conversation-compaction", identity];
  const latest = useRef({ identity, pendingText, enabled });
  latest.current = { identity, pendingText, enabled };
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  const sending = useRef(false);
  const [checkingSend, setCheckingSend] = useState(false);
  const [sendError, setSendError] = useState("");
  const status = useQuery({
    queryKey,
    queryFn: () => invoke<CompactionStatus>("conversation_compaction_state", { ...profile, pendingText }),
    enabled,
    refetchInterval: ({ state }) => state.data?.request?.status === "queued" ? pollInterval : false,
  });
  const start = useMutation({
    mutationFn: ({ contextId, text, automatic }: { contextId: string; text: string; automatic: boolean }) =>
      invoke<CompactionStatus>("request_conversation_compaction", {
        ...profile, expectedContextId: contextId, pendingText: text, automatic,
      }),
    onSuccess: async (value) => {
      await client.cancelQueries({ queryKey, exact: true });
      client.setQueryData(queryKey, value);
    },
  });
  const save = useMutation({
    mutationFn: (settings: CompactionSettings) => invoke<CompactionSettings>("set_conversation_compaction_settings", { conversationId, ...settings }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["conversation-compaction"] }),
  });

  async function beforeSend(contextId: string, text: string): Promise<string | null> {
    if (!enabled || !selection) return contextId;
    if (sending.current) return null;
    sending.current = true;
    setCheckingSend(true);
    setSendError("");
    const current = () => mounted.current && latest.current.enabled
      && latest.current.identity === identity && latest.current.pendingText === text;
    try {
      let observed = await invoke<CompactionStatus>("conversation_compaction_state", { ...profile, pendingText: text });
      if (!current()) return null;
      if (observed.contextId !== contextId &&
        (observed.request?.status !== "activated" || observed.request.sourceContextId !== contextId)) {
        setSendError("The conversation changed before sending. Review the latest history.");
        return null;
      }
      if (observed.estimate?.automaticNeeded && observed.request?.status !== "queued" && observed.contextId === contextId) {
        observed = await start.mutateAsync({ contextId, text, automatic: true });
      }
      const requestId = observed.request?.status === "queued" ? observed.request.artifactId : undefined;
      const deadline = Date.now() + 30_000;
      while (observed.request?.status === "queued" && current()) {
        if (Date.now() >= deadline) return null;
        await new Promise((resolve) => setTimeout(resolve, pollInterval));
        observed = await invoke<CompactionStatus>("conversation_compaction_state", { ...profile, pendingText: text });
        client.setQueryData(queryKey, observed);
      }
      if (!current()) return null;
      if (requestId && observed.request?.artifactId === requestId && observed.request.status === "stale") {
        setSendError("The conversation changed during compaction. Review the latest history.");
        return null;
      }
      if (observed.estimate?.overBudget) {
        client.setQueryData(queryKey, observed);
        setSendError("This input exceeds the dialogue budget. Shorten the draft, remove an attachment or reference, or retry compaction.");
        return null;
      }
      return observed.contextId;
    } catch (error) {
      if (current()) setSendError(String(error));
      return null;
    } finally {
      sending.current = false;
      setCheckingSend(false);
    }
  }

  return { status, start, save, beforeSend, checkingSend, sendError, pendingText };
}

export function ConversationCompactionControl({ compaction, contextId, disabled }: {
  compaction: ReturnType<typeof useConversationCompaction>;
  contextId: string | undefined;
  disabled?: boolean;
}) {
  const { status, start, save, pendingText } = compaction;
  const value = status.data;
  const busy = start.isPending || value?.request?.status === "queued";
  const error = start.error ?? status.error ?? save.error;
  return <>
    <details className="relative">
      <summary className="conv-quiet-button cursor-pointer">Compact context</summary>
      <section aria-label="Context compaction settings" className="absolute right-0 z-30 grid w-72 max-w-[90vw] gap-2 rounded border border-border bg-card p-3 text-xs shadow">
        <p className="font-medium">OmniAgent compaction</p>
        {value?.providerCompactionObserved && <p>Provider-managed compaction has also been observed for this profile.</p>}
        {status.isPending && <p role="status">Loading context…</p>}
        {error && <p role="alert">{String(error)}</p>}
        {value && <>
          <label className="flex items-center gap-2">
            <input type="checkbox" checked={value.settings.automatic} disabled={save.isPending}
              onChange={(event) => save.mutate({ ...value.settings, automatic: event.currentTarget.checked })} />
            Compact automatically before sending
          </label>
          <Threshold value={value.settings.thresholdPercent} disabled={save.isPending}
            onSave={(thresholdPercent) => save.mutate({ ...value.settings, thresholdPercent })} />
          {value.estimate ? <>
            <p>Estimated history {value.estimate.historyTokens.toLocaleString()} tokens · Available dialogue {Math.max(0, value.estimate.availableTokens - value.estimate.pendingTokens).toLocaleString()}</p>
            {value.estimate.projectedHistoryTokens !== null && <p>Estimated after compaction {value.estimate.projectedHistoryTokens.toLocaleString()} tokens</p>}
          </> : <p>No qualified dialogue budget is saved for this model. Manual compaction is available.</p>}
          {value.activeSummary && <p className="break-all">Active summary <code>{value.activeSummary.id}</code>
            {(value.activeSummary.run.observedModel ?? value.activeSummary.run.requestedModel) && <> · {value.activeSummary.run.observedModel ?? value.activeSummary.run.requestedModel}</>}
          </p>}
          {value.request?.detail && <p role="alert">{value.request.detail}</p>}
          {busy && <p role="status">The read-only summary job is queued or running.</p>}
          <Button type="button" variant="outline" disabled={disabled || busy || !contextId || !value.eligible}
            onClick={() => { if (contextId) start.mutate({ contextId, text: pendingText, automatic: false }); }}>Compact now</Button>
        </>}
      </section>
    </details>
    {compaction.sendError && <p role="alert" className="conv-alert">{compaction.sendError}</p>}
  </>;
}

function Threshold({ value, disabled, onSave }: { value: number; disabled: boolean; onSave: (value: number) => void }) {
  return <label className="flex items-center gap-2">
    Automatic threshold (%)
    <input key={value} type="number" min={1} max={100} defaultValue={value} disabled={disabled} className="conv-select w-16"
      onBlur={(event) => {
        const threshold = event.currentTarget.valueAsNumber;
        if (Number.isInteger(threshold) && threshold >= 1 && threshold <= 100 && threshold !== value) onSave(threshold);
        else event.currentTarget.value = String(value);
      }} />
  </label>;
}
