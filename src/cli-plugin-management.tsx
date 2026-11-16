import { useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState, type FormEvent } from "react";
import type { PluginInventory } from "@/bindings/PluginInventory";
import { Button } from "@/components/ui/button";

const ACTIONS = { install: "Install", enable: "Enable", disable: "Disable" } as const;
type Action = keyof typeof ACTIONS;

type Preview = {
  id: string;
  action: Action;
  pluginId: string;
  configurationRoot: string;
  executable: string;
  cliVersion: string;
  executionScope: "user";
  persists: boolean;
  marketplace?: string | null;
  source?: string | null;
  version?: string | null;
};

const OUTCOMES = {
  applied: "Change confirmed in the refreshed inventory.",
  stale: "The CLI or inventory changed. Review the action again.",
  needs_confirmation: "Claude Code requires additional confirmation.",
  failed: "Claude Code reported that the change failed.",
  unconfirmed: "The result could not be confirmed. Check the inventory.",
} as const;
type Outcome = {
  status: keyof typeof OUTCOMES;
  inventoryAfter?: PluginInventory | null;
  reason?: string | null;
  commandFingerprint?: string | null;
};

const pluginKey = ["cli-plugin-inventory", "claude"];

export function ClaudePluginManagement({ disabled }: { disabled: boolean }) {
  const queryClient = useQueryClient();
  const [action, setAction] = useState<Action>("install");
  const [pluginId, setPluginId] = useState("");
  const preview = useMutation({
    mutationFn: () => invoke<Preview>("preview_claude_user_plugin_action", {
      request: { action, pluginId: pluginId.trim() },
    }),
  });
  const apply = useMutation({
    mutationFn: (previewId: string) => invoke<Outcome>("apply_claude_user_plugin_action", { previewId }),
    onSuccess: (result) => {
      if (result.inventoryAfter) {
        queryClient.setQueryData(pluginKey, result.inventoryAfter);
      } else {
        void queryClient.invalidateQueries({ queryKey: pluginKey });
      }
    },
  });
  const busy = disabled || preview.isPending || apply.isPending;
  const selection = preview.data;

  function review(event: FormEvent) {
    event.preventDefault();
    apply.reset();
    preview.mutate();
  }

  function changeSelection() {
    apply.reset();
    preview.reset();
  }

  return <details className="mt-3 border-t border-border pt-3 text-sm">
    <summary className="cursor-pointer font-medium">Manage user plugins</summary>
    <form onSubmit={review} className="mt-2 flex flex-wrap items-end gap-2">
      <label className="grid gap-1">Action
        <select value={action} disabled={busy} className="rounded-md border border-border bg-background p-2"
          onChange={(event) => { setAction(event.currentTarget.value as Action); changeSelection(); }}>
          {Object.entries(ACTIONS).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
        </select>
      </label>
      <label className="grid min-w-52 flex-1 gap-1">Plugin ID
        <input required value={pluginId} placeholder="plugin@marketplace" disabled={busy}
          className="rounded-md border border-border bg-background p-2 font-mono text-xs"
          onChange={(event) => { setPluginId(event.currentTarget.value); changeSelection(); }} />
      </label>
      <Button type="submit" variant="outline" disabled={busy || !pluginId.trim()}>
        {preview.isPending ? "Checking…" : "Review change"}
      </Button>
    </form>
    {selection && !apply.data && !apply.error && <section aria-label="Plugin change preview" className="mt-3 rounded-md border border-border p-3">
      <p className="font-medium">{ACTIONS[selection.action]} <code className="break-all">{selection.pluginId}</code></p>
      <dl className="my-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1">
        <dt>CLI</dt><dd>Claude Code · {selection.cliVersion}</dd>
        <dt>Executable</dt><dd className="break-all font-mono text-xs">{selection.executable}</dd>
        <dt>Configuration scope</dt><dd>{selection.executionScope === "user" ? "User" : selection.executionScope}{selection.persists && " · persists across sessions"}</dd>
        <dt>Configuration path</dt><dd className="break-all font-mono text-xs">{selection.configurationRoot}</dd>
        {selection.marketplace && <><dt>Marketplace</dt><dd className="break-all">{selection.marketplace}</dd></>}
        {selection.source && <><dt>Source</dt><dd className="break-all">{selection.source}</dd></>}
        {selection.version && <><dt>Plugin version</dt><dd>{selection.version}</dd></>}
      </dl>
      <div className="flex gap-2">
        <Button type="button" disabled={busy} onClick={() => apply.mutate(selection.id)}>
          {apply.isPending ? "Applying…" : `Confirm ${selection.action}`}
        </Button>
        <Button type="button" variant="outline" disabled={busy} onClick={changeSelection}>Cancel</Button>
      </div>
    </section>}
    {apply.data && <div className="mt-2 break-words" role="status">
      <p>{OUTCOMES[apply.data.status]}</p>
      {apply.data.reason && <p>{apply.data.reason}</p>}
      {apply.data.commandFingerprint && <p>Command fingerprint <code className="break-all">{apply.data.commandFingerprint}</code></p>}
    </div>}
    {(preview.error || apply.error) && <p className="mt-2 break-words text-red-300" role="alert">{String(preview.error || apply.error)}</p>}
  </details>;
}
