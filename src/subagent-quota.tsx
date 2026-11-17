import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Activity, Check, ChevronDown, Info, Save, ShieldCheck, Users } from "lucide-react";
import { Dialog } from "radix-ui";
import { useState, type ReactNode } from "react";
import type { ConversationSubagentQuota } from "@/bindings/ConversationSubagentQuota";
import type { NativeSubagentScope } from "@/bindings/NativeSubagentScope";
import type { ProjectSubagentPolicy } from "@/bindings/ProjectSubagentPolicy";
import type { Provider } from "@/bindings/Provider";
import type { SubagentCounts } from "@/bindings/SubagentCounts";
import type { SubagentLimitMode } from "@/bindings/SubagentLimitMode";
import { Button } from "@/components/ui/button";
import { SettingRow } from "@/settings-controls";
import { ConversationSubagentAuditControl } from "@/subagent-audit-controls";
import "./subagent-quota.css";

const modeLabels: Record<SubagentLimitMode, string> = { managed: "Managed limit", strict: "Strict total limit" };
const providerNames: Record<Provider, string> = { codex: "Codex", claude: "Claude Code", antigravity: "Antigravity" };

function modeOptions() {
  return Object.entries(modeLabels).map(([value, label]) => <option key={value} value={value}>{label}</option>);
}

const validLimit = (value: string) => /^[0-9]+$/.test(value) && Number(value) <= 32;

export function ProjectSubagentControls({ projectId }: { projectId: string }) {
  const client = useQueryClient();
  const key = ["project-subagent-policy", projectId];
  const policy = useQuery({
    queryKey: key,
    queryFn: () => invoke<ProjectSubagentPolicy>("project_subagent_policy", { projectId }),
  });
  const [mode, setMode] = useState<SubagentLimitMode>();
  const [maximum, setMaximum] = useState<string>();
  const resolvedMode = mode ?? policy.data?.mode;
  const resolvedMaximum = maximum ?? String(policy.data?.maximum ?? "");
  const save = useMutation({
    mutationFn: (policy: ProjectSubagentPolicy) => invoke("set_project_subagent_policy", { projectId, policy }),
    onSuccess: (_, saved) => {
      client.setQueryData(key, saved);
      setMode(undefined);
      setMaximum(undefined);
      return Promise.all([
        client.invalidateQueries({ queryKey: ["conversation-subagent-quota"] }),
        client.invalidateQueries({ queryKey: ["project-job-queue", projectId] }),
      ]);
    },
  });
  const dirty = resolvedMode !== policy.data?.mode || resolvedMaximum !== String(policy.data?.maximum ?? "");
  const disabled = !policy.data || save.isPending;

  return <form aria-label="Project subagent policy" className="grid gap-2" onSubmit={(event) => {
    event.preventDefault();
    if (!disabled && dirty && resolvedMode && validLimit(resolvedMaximum)) {
      save.mutate({ mode: resolvedMode, maximum: Number(resolvedMaximum) });
    }
  }}>
    <SettingRow id="subagent-policy" label="Default subagent limit mode"
      description="Strict total requires a profile with verified control over native delegation.">
      <select id="subagent-policy" aria-describedby="subagent-policy-description"
        value={resolvedMode ?? ""} disabled={disabled}
        onChange={(event) => setMode(event.currentTarget.value as SubagentLimitMode)}>
        {!resolvedMode && <option value="">Loading policy…</option>}
        {modeOptions()}
      </select>
    </SettingRow>
    <SettingRow id="subagent-cap" label="Project maximum per conversation"
      description="Caps the limit that each conversation can request.">
      <input id="subagent-cap" aria-describedby="subagent-cap-description" type="number" min={0} max={32} step={1}
        value={resolvedMaximum} disabled={disabled} required onChange={(event) => setMaximum(event.currentTarget.value)} />
    </SettingRow>
    {(policy.error || save.error) && <p className="settings-result settings-result-error" role="alert">{String(policy.error || save.error)}</p>}
    <div className="flex justify-end">
      <Button type="submit" variant="outline" disabled={disabled || !dirty || !validLimit(resolvedMaximum)}>Save subagent policy</Button>
    </div>
  </form>;
}

function ActivityCounts({ counts }: { counts: SubagentCounts }) {
  return <span className="tabular-nums">{counts.active} active · {counts.queued} queued · {counts.historical} historical</span>;
}

type ConversationQuotaScope = { conversationId: string; runId?: string };

function nativeEnforcement(scope: NativeSubagentScope) {
  switch (scope.enforcement) {
    case "unbounded": return "Uncapped in this session";
    case "pending": return `Waiting for the CLI to verify the reserved cap of ${scope.allowance}`;
    case "enforced": return `CLI cap verified · ${scope.allowance} allowed`;
    case "stop_unconfirmed": return `Session stop unconfirmed · ${scope.allowance} reserved`;
    case "ended": return scope.allowance === null ? "Attempt ended · Uncapped at launch" : `Attempt ended · ${scope.allowance} reserved at launch`;
    case "unconfirmed": return `Enforcement unconfirmed · ${scope.allowance} allocated`;
  }
}

function QuotaDisclosure({ label, icon: Icon, expanded, status, children }: {
  label: string;
  icon: typeof Activity;
  expanded?: boolean;
  status?: string;
  children: ReactNode;
}) {
  return <details className="subagent-quota-disclosure" open={expanded}>
    <summary>
      <Icon size={16} aria-hidden />
      <span>{label}</span>
      {status && <small>{status}</small>}
      <ChevronDown className="subagent-quota-chevron" size={15} aria-hidden />
    </summary>
    <div className="subagent-quota-detail">{children}</div>
  </details>;
}

function QuotaActivity({ quota }: { quota: ConversationSubagentQuota }) {
  const { managed, nativeScope, nativeSessions, native } = quota;
  return <>
    <dl className="subagent-quota-facts">
      <div><dt>Configured limit mode</dt><dd>{modeLabels[quota.modeOverride ?? quota.project.mode]}</dd></div>
      <div><dt>Managed by OmniAgent</dt><dd><ActivityCounts counts={managed} /></dd></div>
    </dl>
    <section aria-label="CLI delegation cap">
      <h3>CLI delegation cap</h3>
      <p>{nativeScope ? <>{providerNames[nativeScope.provider]} · {nativeEnforcement(nativeScope)}</>
        : "Awaiting an active CLI attempt."}</p>
    </section>
    {nativeSessions.length > 0 && <section aria-label="Native subagent reservations">
      <h3>Native subagent reservations</h3>
      <dl className="subagent-quota-facts">
        {nativeSessions.map((session) => <div key={session.sessionId}>
          <dt>{providerNames[session.provider]} session</dt>
          <dd className="tabular-nums">{session.allowance} reserved · {session.needsReview ? "Stop unconfirmed" : session.verified ? "Verified" : "Verifying"}</dd>
        </div>)}
      </dl>
    </section>}
    <section aria-label="Provider-reported subagents">
      <h3>Provider reports</h3>
      {native.length === 0 && <p>No native subagent activity has been reported.</p>}
      {native.map(({ provider, reported, unknownState, unidentifiedEvents }) => <div key={provider}>
        <dl className="subagent-quota-facts"><div>
          <dt>{providerNames[provider]}</dt><dd><ActivityCounts counts={reported} /></dd>
        </div></dl>
        {unknownState > 0 && <p>{unknownState} reported with unknown status.</p>}
        {unidentifiedEvents > 0 && <p>{unidentifiedEvents} events without a child identity.</p>}
      </div>)}
    </section>
  </>;
}

function ConversationQuotaForm({ conversationId, runId, live = true, modal = false }: ConversationQuotaScope & {
  live?: boolean;
  modal?: boolean;
}) {
  const [mode, setMode] = useState<SubagentLimitMode | null>();
  const [requested, setRequested] = useState<string>();
  const [requestedNative, setRequestedNative] = useState<string>();
  const client = useQueryClient();
  const key = ["conversation-subagent-quota", conversationId, runId];
  const quota = useQuery({
    queryKey: key,
    queryFn: () => invoke<ConversationSubagentQuota>("conversation_subagent_quota", runId ? { conversationId, runId } : { conversationId }),
    refetchInterval: live ? 1_000 : 3_000,
  });
  const { data } = quota;
  const resolvedMode = mode === undefined ? data?.modeOverride ?? null : mode;
  const resolvedRequested = requested ?? String(data?.requested ?? "");
  const resolvedNative = requestedNative ?? String(data?.nativeRequested ?? "");
  const editingStrict = (resolvedMode ?? data?.project.mode) === "strict";
  const save = useMutation({
    mutationKey: ["save-conversation-subagent-quota", conversationId],
    mutationFn: (settings: { conversationId: string; modeOverride: SubagentLimitMode | null; requested: number; nativeRequested: number }) =>
      invoke<ConversationSubagentQuota>("set_conversation_subagent_quota", settings),
    onSuccess: async (updated) => {
      await client.cancelQueries({ queryKey: key });
      client.setQueryData<ConversationSubagentQuota>(key, (previous) => runId && previous
        ? { ...updated, nativeScope: previous.nativeScope }
        : updated);
      setMode(undefined);
      setRequested(undefined);
      setRequestedNative(undefined);
      void client.invalidateQueries({ queryKey: ["conversation-subagent-quota"] });
      void client.invalidateQueries({ queryKey: ["project-job-queue"] });
    },
  });
  const dirty = resolvedMode !== data?.modeOverride
    || resolvedRequested !== String(data?.requested ?? "")
    || resolvedNative !== String(data?.nativeRequested ?? "");
  const disabled = !data || save.isPending;
  const canSave = !disabled && dirty && validLimit(resolvedRequested) && validLimit(resolvedNative);
  const aboveProjectMaximum = data && validLimit(resolvedRequested) && Number(resolvedRequested) > data.project.maximum;
  const activityStatus = data && (data.managed.active || data.managed.queued)
    ? `${data.managed.active} active · ${data.managed.queued} queued` : undefined;

  return <form aria-label="Subagent quota settings" className={modal ? "subagent-quota-form subagent-quota-modal-form" : "subagent-quota-form"} onSubmit={(event) => {
    event.preventDefault();
    if (canSave && data) {
      save.mutate({
        conversationId: data.conversationId, modeOverride: resolvedMode,
        requested: Number(resolvedRequested), nativeRequested: Number(resolvedNative),
      });
    }
  }}>
    <div className={modal ? "conv-dialog-body conv-dialog-form subagent-quota-body" : "conv-dialog-form subagent-quota-body"}>
      {data && <dl className="subagent-quota-totals">
        <div><dt>Authorized now</dt><dd>{data.authorized}</dd></div>
        <div><dt>Project maximum</dt><dd>{data.project.maximum}</dd></div>
      </dl>}
      {quota.isPending && <p role="status">Loading subagent limits…</p>}
      {data && data.conversationId !== conversationId && <p className="text-muted-foreground">
        This attempt shares the originating conversation’s subagent limit.
      </p>}
      <div className="subagent-quota-fields">
        <label>
          Requested limit
          <input type="number" min={0} max={32} step={1} required value={resolvedRequested}
            disabled={disabled} onChange={(event) => setRequested(event.currentTarget.value)} />
        </label>
        <label>
          Limit mode
          <select value={resolvedMode ?? ""} disabled={disabled}
            onChange={(event) => setMode(event.currentTarget.value ? event.currentTarget.value as SubagentLimitMode : null)}>
            <option value="">{data ? `Project default · ${modeLabels[data.project.mode]}` : "Loading policy…"}</option>
            {modeOptions()}
          </select>
        </label>
      </div>
      {editingStrict && <label>
        Requested native allowance per CLI session
        <input type="number" min={0} max={32} step={1} required value={resolvedNative}
          disabled={disabled} onChange={(event) => setRequestedNative(event.currentTarget.value)} />
        <span className="text-muted-foreground">Native reservations share the limit. Use 0 for managed subagents only.</span>
      </label>}
      {aboveProjectMaximum && <p className="subagent-quota-notice">
        <Info size={15} aria-hidden /> This request exceeds the project maximum of {data.project.maximum}.
      </p>}
      {data?.configurationPending && <p role="status" className="subagent-quota-notice">
        <Info size={15} aria-hidden /> Subagent policy changes are pending. Existing sessions keep their allowances until they stop.
      </p>}
      {(quota.error || save.error) && <p className="conv-alert" role="alert">{String(quota.error || save.error)}</p>}
      {(data || modal) && <div className="subagent-quota-disclosures">
        {modal && <QuotaDisclosure label="Audit policy" icon={ShieldCheck}>
          <ConversationSubagentAuditControl conversationId={conversationId} open />
        </QuotaDisclosure>}
        {data && <QuotaDisclosure label="Activity and CLI limits" icon={Activity} expanded={!modal || undefined} status={activityStatus}>
          <QuotaActivity quota={data} />
        </QuotaDisclosure>}
      </div>}
    </div>
    <footer className={modal ? "conv-dialog-actions" : "subagent-quota-actions"}>
      {save.isSuccess && !dirty && !data?.configurationPending && <span className="subagent-quota-saved" role="status">
        <Check size={14} aria-hidden /> Limit saved
      </span>}
      {modal && <Dialog.Close asChild><Button type="button" variant="outline" disabled={save.isPending}>Close</Button></Dialog.Close>}
      <Button type="submit" disabled={!canSave}><Save size={15} aria-hidden />{save.isPending ? "Saving…" : "Save limit"}</Button>
    </footer>
  </form>;
}

export function ConversationSubagentQuotaPanel({ conversationId, runId, live }: ConversationQuotaScope & { live: boolean }) {
  return <section aria-label="Subagent limits" className="grid gap-2">
    <h2 className="text-sm font-medium">Subagent limits</h2>
    <ConversationQuotaForm key={`${conversationId}:${runId ?? ""}`} conversationId={conversationId} runId={runId} live={live} />
  </section>;
}

export function ConversationSubagentControls({ conversationId, runId }: ConversationQuotaScope) {
  const [open, setOpen] = useState(false);
  const saving = useIsMutating({ mutationKey: ["save-conversation-subagent-quota", conversationId] }) > 0;
  return <Dialog.Root open={open} onOpenChange={(next) => {
    if (next || !saving) setOpen(next);
  }}>
    <Dialog.Trigger type="button" className="conv-quiet-button" aria-label="Subagent limits" title="Subagent limits">
      <Users aria-hidden />
    </Dialog.Trigger>
    <Dialog.Portal>
      <Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog conv-dialog-layout subagent-quota-dialog">
        <header className="conv-dialog-header">
          <span className="conv-dialog-mark"><Users size={19} aria-hidden /></span>
          <Dialog.Title>Subagent limits</Dialog.Title>
          <Dialog.Description className="sr-only">Configure the conversation limit and review subagent activity.</Dialog.Description>
        </header>
        <ConversationQuotaForm key={`${conversationId}:${runId ?? ""}`} conversationId={conversationId} runId={runId} modal />
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
