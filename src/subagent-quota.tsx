import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Users } from "lucide-react";
import { Dialog } from "radix-ui";
import { useState } from "react";
import type { ConversationSubagentQuota } from "@/bindings/ConversationSubagentQuota";
import type { ProjectSubagentPolicy } from "@/bindings/ProjectSubagentPolicy";
import type { Provider } from "@/bindings/Provider";
import type { SubagentCounts } from "@/bindings/SubagentCounts";
import type { SubagentLimitMode } from "@/bindings/SubagentLimitMode";
import { Button } from "@/components/ui/button";
import { SettingRow } from "@/settings-controls";

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

export function ConversationSubagentControls({ conversationId }: { conversationId: string }) {
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<SubagentLimitMode | null>();
  const [requested, setRequested] = useState<string>();
  const [requestedNative, setRequestedNative] = useState<string>();
  const client = useQueryClient();
  const key = ["conversation-subagent-quota", conversationId];
  const quota = useQuery({
    queryKey: key,
    queryFn: () => invoke<ConversationSubagentQuota>("conversation_subagent_quota", { conversationId }),
    enabled: open,
    refetchInterval: open ? 1_000 : false,
  });
  const { data } = quota;
  const resolvedMode = mode === undefined ? data?.modeOverride ?? null : mode;
  const resolvedRequested = requested ?? String(data?.requested ?? "");
  const resolvedNative = requestedNative ?? String(data?.nativeRequested ?? "");
  const editingStrict = (resolvedMode ?? data?.project.mode) === "strict";
  const save = useMutation({
    mutationFn: (settings: { modeOverride: SubagentLimitMode | null; requested: number; nativeRequested: number }) =>
      invoke<ConversationSubagentQuota>("set_conversation_subagent_quota", { conversationId, ...settings }),
    onSuccess: async (updated) => {
      await client.cancelQueries({ queryKey: key });
      client.setQueryData(key, updated);
      setMode(undefined);
      setRequested(undefined);
      setRequestedNative(undefined);
      void client.invalidateQueries({ queryKey: ["project-job-queue"] });
    },
  });
  const dirty = resolvedMode !== data?.modeOverride
    || resolvedRequested !== String(data?.requested ?? "")
    || resolvedNative !== String(data?.nativeRequested ?? "");
  const disabled = !data || save.isPending;
  const canSave = !disabled && dirty && validLimit(resolvedRequested) && validLimit(resolvedNative);

  return <Dialog.Root open={open} onOpenChange={(next) => {
    if (!next && save.isPending) return;
    setOpen(next);
    if (!next) {
      setMode(undefined);
      setRequested(undefined);
      setRequestedNative(undefined);
      save.reset();
    }
  }}>
    <Dialog.Trigger type="button" className="conv-quiet-button" aria-label="Subagent limits" title="Subagent limits">
      <Users aria-hidden />
    </Dialog.Trigger>
    <Dialog.Portal>
      <Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog">
        <Dialog.Title>Subagent limits</Dialog.Title>
        <Dialog.Description>The project maximum caps the requested limit for this conversation.</Dialog.Description>
        <form className="conv-dialog-form" onSubmit={(event) => {
          event.preventDefault();
          if (canSave) {
            save.mutate({
              modeOverride: resolvedMode, requested: Number(resolvedRequested), nativeRequested: Number(resolvedNative),
            });
          }
        }}>
          <label>
            Limit mode
            <select value={resolvedMode ?? ""} disabled={disabled}
              onChange={(event) => setMode(event.currentTarget.value ? event.currentTarget.value as SubagentLimitMode : null)}>
              <option value="">{data ? `Use project default · ${modeLabels[data.project.mode]}` : "Loading policy…"}</option>
              {modeOptions()}
            </select>
          </label>
          {editingStrict && <label>
            Requested native allowance per CLI session
            <input type="number" min={0} max={32} step={1} required value={resolvedNative}
              disabled={disabled} onChange={(event) => setRequestedNative(event.currentTarget.value)} />
            <span className="text-muted-foreground">Set zero to use managed subagents only. Native reservations share the conversation limit.</span>
          </label>}
          <label>
            Requested simultaneous subagents
            <input type="number" min={0} max={32} step={1} required value={resolvedRequested}
              disabled={disabled} onChange={(event) => setRequested(event.currentTarget.value)} />
          </label>
          {data && <dl className="grid grid-cols-[1fr_auto] gap-x-3 gap-y-1">
            <dt>Authorized simultaneous subagents</dt>
            <dd className="text-right tabular-nums">{data.authorized}</dd>
            <dt>Project maximum</dt>
            <dd className="text-right tabular-nums">{data.project.maximum}</dd>
            <dt>Managed by OmniAgent</dt>
            <dd className="text-right"><ActivityCounts counts={data.managed} /></dd>
          </dl>}
          {data?.configurationPending && <p role="status" className="text-muted-foreground">
            Subagent policy changes are pending. Existing sessions keep their allowances until they stop.
          </p>}
          {Boolean(data?.nativeSessions.length) && <section aria-label="Native subagent reservations" className="grid gap-1">
            <h3 className="font-medium">Native subagent reservations</h3>
            {data?.nativeSessions.map((session) => <div className="flex flex-wrap justify-between gap-x-3" key={session.sessionId}>
              <span>{providerNames[session.provider]} session</span>
              <span className="tabular-nums">{session.allowance} reserved · {session.needsReview ? "Stop unconfirmed" : session.verified ? "Verified" : "Verifying"}</span>
            </div>)}
          </section>}
          {data && <section className="grid gap-2" aria-label="Provider-reported subagents">
            <h3 className="font-medium">Native activity reported by providers</h3>
            {data.native.length === 0 && <p className="text-muted-foreground">No native subagent activity has been reported.</p>}
            {data.native.map(({ provider, reported, unknownState, unidentifiedEvents }) => <div key={provider}>
              <div className="flex flex-wrap justify-between gap-x-3">
                <span>{providerNames[provider]}</span><ActivityCounts counts={reported} />
              </div>
              {unknownState > 0 && <p className="text-muted-foreground">{unknownState} reported with unknown status.</p>}
              {unidentifiedEvents > 0 && <p className="text-muted-foreground">{unidentifiedEvents} events without a child identity.</p>}
            </div>)}
          </section>}
          {(quota.error || save.error) && <p className="conv-alert" role="alert">{String(quota.error || save.error)}</p>}
          <footer>
            <Dialog.Close asChild><Button type="button" variant="outline" disabled={save.isPending}>Close</Button></Dialog.Close>
            <Button type="submit" disabled={!canSave}>Save limit</Button>
          </footer>
        </form>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
