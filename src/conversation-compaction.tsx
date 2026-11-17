import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ChevronDown, CircleAlert, FileText, ScanLine, SlidersHorizontal, Terminal, X } from "lucide-react";
import { Popover } from "radix-ui";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { SessionMode } from "@/bindings/SessionMode";
import { Button } from "@/components/ui/button";
import "./conversation-compaction.css";

type CompactionSettings = { automatic: boolean; thresholdPercent: number };
type CompactionEstimate = {
  historyTokens: number; pendingTokens: number; availableTokens: number;
  projectedHistoryTokens: number | null; overBudget: boolean; automaticNeeded: boolean;
};
type NativeCompactionRequest = {
  runId: string;
  sourceContextId: string;
  status: "starting" | "running" | "succeeded" | "failed" | "interrupted";
  detail: string | null;
  boundary: { turnId: string; itemId: string | null } | null;
  usage: { input: number | null; output: number | null; cachedInput: number | null; reasoningOutput: number | null } | null;
};
type CompactionStatus = {
  contextId: string;
  settings: CompactionSettings;
  estimate: CompactionEstimate | null;
  eligible: boolean;
  providerCompactionObserved: boolean;
  native: { eligible: boolean; reason: string | null; request: NativeCompactionRequest | null };
  activeSummary: { id: string; sourceContextId: string; run: { runId: string; requestedModel: string | null; observedModel: string | null } } | null;
  request: { artifactId: string; sourceContextId: string; jobId: string; status: "queued" | "activated" | "failed" | "stale"; detail: string | null } | null;
};

const pollInterval = 1000;

function nativeRunning(request: NativeCompactionRequest | null | undefined) {
  return request?.status === "starting" || request?.status === "running";
}

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
    refetchInterval: ({ state }) => state.data?.request?.status === "queued" || nativeRunning(state.data?.native?.request) ? pollInterval : false,
  });
  async function updateStatus(value: CompactionStatus) {
    await client.cancelQueries({ queryKey, exact: true });
    client.setQueryData(queryKey, value);
  }
  const start = useMutation({
    mutationKey: ["conversation-compaction-managed", identity],
    mutationFn: ({ contextId, text, automatic }: { contextId: string; text: string; automatic: boolean }) =>
      invoke<CompactionStatus>("request_conversation_compaction", {
        ...profile, expectedContextId: contextId, pendingText: text, automatic,
      }),
    onSuccess: updateStatus,
  });
  const startNative = useMutation({
    mutationKey: ["conversation-compaction-native", identity],
    mutationFn: ({ contextId, text }: { contextId: string; text: string }) =>
      invoke<CompactionStatus>("request_native_conversation_compaction", {
        ...profile, expectedContextId: contextId, pendingText: text,
      }),
    onSuccess: updateStatus,
    onError: () => client.invalidateQueries({ queryKey, exact: true }),
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
      if (nativeRunning(observed.native?.request)) {
        setSendError("Wait for the Codex context compaction to finish before sending.");
        return null;
      }
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

  return { status, start, startNative, save, beforeSend, checkingSend, sendError, pendingText };
}

export function ConversationCompactionControl({ compaction, contextId, disabled }: {
  compaction: ReturnType<typeof useConversationCompaction>;
  contextId: string | undefined;
  disabled?: boolean;
}) {
  const { status, start, startNative, save, pendingText } = compaction;
  const panel = useRef<HTMLDivElement>(null);
  const value = status.data;
  const managedBusy = start.isPending || value?.request?.status === "queued";
  const native = value?.native;
  const nativeBusy = startNative.isPending || nativeRunning(native?.request);
  const busy = managedBusy || nativeBusy;
  const error = start.error ?? startNative.error ?? status.error ?? save.error;
  const nativeAction = native && <Button type="button" variant="outline" disabled={disabled || busy || !contextId || !native.eligible}
    onClick={() => { if (contextId) startNative.mutate({ contextId, text: pendingText }); }}>
    <Terminal size={15} aria-hidden /> Compact Codex session
  </Button>;

  return <>
    <Popover.Root>
      <Popover.Trigger asChild>
        <Button type="button" variant="ghost" className="conv-compaction-trigger">
          <ScanLine size={15} aria-hidden /> Compact context <ChevronDown size={13} aria-hidden />
        </Button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content ref={panel} className="conv-compaction-popover" aria-label="Context compaction settings"
          side="top" align="end" sideOffset={8} collisionPadding={12}
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            panel.current?.focus({ preventScroll: true });
          }}>
          <header className="conv-compaction-header">
            <span className="conv-compaction-icon"><ScanLine size={18} aria-hidden /></span>
            <div>
              <h2>Context compaction</h2>
              <p>Summarize earlier messages to make room for your next prompt.</p>
            </div>
            <Popover.Close asChild><Button type="button" variant="ghost" className="ui-icon-button" aria-label="Close context compaction"><X size={16} aria-hidden /></Button></Popover.Close>
          </header>
          <div className="conv-compaction-body">
            {status.isPending && <p role="status">Loading context…</p>}
            {error && <p role="alert" className="conv-compaction-alert"><CircleAlert size={15} aria-hidden /> {String(error)}</p>}
            {value && <>
              <label className="conv-compaction-auto">
                <span>Compact automatically before sending</span>
                <input type="checkbox" checked={value.settings.automatic} disabled={save.isPending}
                  onChange={(event) => save.mutate({ ...value.settings, automatic: event.currentTarget.checked })} />
              </label>
              {value.estimate ? <dl className="conv-compaction-metrics" aria-label="Estimated dialogue budget">
                <div><dt>History</dt><dd>{value.estimate.historyTokens.toLocaleString()} tokens</dd></div>
                <div><dt>Budget after draft</dt><dd>{Math.max(0, value.estimate.availableTokens - value.estimate.pendingTokens).toLocaleString()} tokens</dd></div>
              </dl> : <p className="conv-compaction-muted">Dialogue budget unavailable for this model.</p>}
              {value.estimate?.overBudget && <p className="conv-compaction-alert" role="status"><CircleAlert size={15} aria-hidden /> The draft and history exceed the dialogue budget.</p>}
              {value.request?.detail && <p role="alert" className="conv-compaction-alert">{value.request.detail}</p>}
              {managedBusy && <p role="status">The read-only summary job is queued or running.</p>}
              {nativeBusy && <p role="status">Codex is compacting its session.</p>}
              {native?.request?.detail && !nativeBusy && <p role="alert" className="conv-compaction-alert">{native.request.detail}</p>}
              {native?.request?.status === "succeeded" && !nativeBusy && <p role="status">Codex confirmed session compaction.</p>}
              <div className="conv-compaction-actions" aria-label="Compact now">
                <Button type="button" disabled={disabled || busy || !contextId || !value.eligible}
                  onClick={() => { if (contextId) start.mutate({ contextId, text: pendingText, automatic: false }); }}>
                  <FileText size={15} aria-hidden /> Summarize with OmniAgent
                </Button>
                {(native?.eligible || nativeBusy) && nativeAction}
              </div>
              <details className="conv-compaction-details">
                <summary><SlidersHorizontal size={15} aria-hidden /> Settings and evidence <ChevronDown size={14} aria-hidden /></summary>
                <div className="conv-compaction-evidence">
                  <Threshold value={value.settings.thresholdPercent} disabled={save.isPending}
                    onSave={(thresholdPercent) => save.mutate({ ...value.settings, thresholdPercent })} />
                  {value.estimate?.projectedHistoryTokens != null && <p>Estimated after compaction {value.estimate.projectedHistoryTokens.toLocaleString()} tokens</p>}
                  {value.activeSummary && <p>Active summary <code>{value.activeSummary.id}</code>
                    {(value.activeSummary.run.observedModel ?? value.activeSummary.run.requestedModel) && <> · {value.activeSummary.run.observedModel ?? value.activeSummary.run.requestedModel}</>}
                  </p>}
                  {value.providerCompactionObserved && <p>Provider-managed compaction observed for this profile.</p>}
                  {native?.reason && !nativeBusy && <p>{native.reason}</p>}
                  {native?.request?.boundary && !nativeBusy && <p>Provider turn <code>{native.request.boundary.turnId}</code>
                    {native.request.boundary.itemId && <> · Item <code>{native.request.boundary.itemId}</code></>}
                  </p>}
                  {native?.request?.usage && !nativeBusy && <p>Provider usage · {[
                    native.request.usage.input === null ? "" : `${native.request.usage.input.toLocaleString()} input`,
                    native.request.usage.output === null ? "" : `${native.request.usage.output.toLocaleString()} output`,
                    native.request.usage.cachedInput === null ? "" : `${native.request.usage.cachedInput.toLocaleString()} cached`,
                    native.request.usage.reasoningOutput === null ? "" : `${native.request.usage.reasoningOutput.toLocaleString()} reasoning`,
                  ].filter(Boolean).join(" · ")}</p>}
                  {native && !native.eligible && !nativeBusy && nativeAction}
                </div>
              </details>
            </>}
          </div>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
    {compaction.sendError && <p role="alert" className="conv-alert">{compaction.sendError}</p>}
  </>;
}

function Threshold({ value, disabled, onSave }: { value: number; disabled: boolean; onSave: (value: number) => void }) {
  return <label className="conv-compaction-threshold">
    Automatic threshold (%)
    <input key={value} type="number" min={1} max={100} defaultValue={value} disabled={disabled}
      onBlur={(event) => {
        const threshold = event.currentTarget.valueAsNumber;
        if (Number.isInteger(threshold) && threshold >= 1 && threshold <= 100 && threshold !== value) onSave(threshold);
        else event.currentTarget.value = String(value);
      }} />
  </label>;
}
