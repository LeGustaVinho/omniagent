import { useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState, type FormEvent } from "react";
import type { PluginInventory } from "@/bindings/PluginInventory";
import type { Provider } from "@/bindings/Provider";
import { Button } from "@/components/ui/button";
import { providerPluginInventoryKey } from "@/provider-inventory";

const ACTIONS = { install: "Install", enable: "Enable", disable: "Disable" } as const;
type Action = keyof typeof ACTIONS;
type CatalogEntry = { pluginId: string; version: string; source: string };
const PROVIDER_LABELS = { claude: "Claude Code", codex: "Codex", antigravity: "Antigravity" } as const;

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
  needs_confirmation: "The CLI requires additional confirmation.",
  failed: "The CLI reported that the change failed.",
  unconfirmed: "The result could not be confirmed. Check the inventory.",
} as const;
type Outcome = {
  status: keyof typeof OUTCOMES;
  inventoryAfter?: PluginInventory | null;
  reason?: string | null;
  commandFingerprint?: string | null;
};

export function UserPluginManagement({ disabled, provider }: { disabled: boolean; provider: Provider }) {
  const queryClient = useQueryClient();
  const pluginKey = ["cli-plugin-inventory", provider];
  const [action, setAction] = useState<Action>("install");
  const [pluginId, setPluginId] = useState("");
  const [marketplace, setMarketplace] = useState("");
  const catalog = useMutation({
    mutationFn: () => invoke<CatalogEntry[]>("native_codex_plugin_catalog", { marketplace: marketplace.trim() }),
  });
  const preview = useMutation({
    mutationFn: () => provider === "claude" ? invoke<Preview>("preview_claude_user_plugin_action", {
      request: { action, pluginId: pluginId.trim() },
    }) : invoke<Preview>("preview_native_user_plugin_action", {
      request: { provider, action, target: pluginId.trim() },
    }),
  });
  const apply = useMutation({
    mutationFn: (previewId: string) => invoke<Outcome>(
      provider === "claude" ? "apply_claude_user_plugin_action" : "apply_native_user_plugin_action", { previewId },
    ),
    onSuccess: (result) => {
      if (result.inventoryAfter) {
        queryClient.setQueryData(pluginKey, result.inventoryAfter);
      } else {
        void queryClient.invalidateQueries({ queryKey: pluginKey });
      }
      void queryClient.invalidateQueries({ queryKey: providerPluginInventoryKey(provider) });
    },
  });
  const busy = disabled || catalog.isPending || preview.isPending || apply.isPending;
  const selection = preview.data;
  const localFolder = provider === "antigravity" && action === "install";

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
    {provider === "codex" && <p className="mt-2 text-muted-foreground">
      Install from a configured local marketplace. Toggle installed plugins in Codex’s interactive /plugins menu.
    </p>}
    <form onSubmit={review} className="mt-2 flex flex-wrap items-end gap-2">
      {provider !== "codex" && <label className="grid gap-1">Action
        <select value={action} disabled={busy} className="rounded-md border border-border bg-background p-2"
          onChange={(event) => { setAction(event.currentTarget.value as Action); setPluginId(""); changeSelection(); }}>
          {Object.entries(ACTIONS).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
        </select>
      </label>}
      {provider === "codex" ? <>
        <label className="grid gap-1">Configured marketplace
          <input value={marketplace} disabled={busy} className="rounded-md border border-border bg-background p-2"
            onChange={(event) => { setMarketplace(event.currentTarget.value); setPluginId(""); catalog.reset(); changeSelection(); }} />
        </label>
        <Button type="button" variant="outline" disabled={busy || !marketplace.trim()} onClick={() => {
          setPluginId(""); changeSelection(); catalog.mutate();
        }}>{catalog.isPending ? "Loading…" : "Load catalog"}</Button>
        <label className="grid min-w-52 flex-1 gap-1">Plugin
          <select value={pluginId} disabled={busy || !catalog.data?.length} className="rounded-md border border-border bg-background p-2"
            onChange={(event) => { setPluginId(event.currentTarget.value); changeSelection(); }}>
            <option value="">Select a plugin</option>
            {catalog.data?.map((entry) => <option key={entry.pluginId} value={entry.pluginId}>{entry.pluginId} · {entry.version}</option>)}
          </select>
        </label>
      </> : <label className="grid min-w-52 flex-1 gap-1">{localFolder ? "Local plugin folder" : "Plugin ID"}
        <input required value={pluginId} placeholder={localFolder ? "C:\\Plugins\\my-plugin" : provider === "claude" ? "plugin@marketplace" : "plugin-name"} disabled={busy}
          className="rounded-md border border-border bg-background p-2 font-mono text-xs"
          onChange={(event) => { setPluginId(event.currentTarget.value); changeSelection(); }} />
      </label>}
      <Button type="submit" variant="outline" disabled={busy || !pluginId.trim()}>
        {preview.isPending ? "Checking…" : "Review change"}
      </Button>
    </form>
    {catalog.isSuccess && !catalog.data.length && <p className="mt-2" role="status">No available plugins in this configured local marketplace.</p>}
    {selection && !apply.data && !apply.error && <section aria-label="Plugin change preview" className="mt-3 rounded-md border border-border p-3">
      <p className="font-medium">{ACTIONS[selection.action]} <code className="break-all">{selection.pluginId}</code></p>
      {selection.action === "install" && <p className="mt-1 text-muted-foreground">
        The CLI may enable this plugin for future sessions when it installs it.
      </p>}
      <dl className="my-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1">
        <dt>CLI</dt><dd>{PROVIDER_LABELS[provider]} · {selection.cliVersion}</dd>
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
    {(catalog.error || preview.error || apply.error) && <p className="mt-2 break-words text-red-300" role="alert">{String(catalog.error || preview.error || apply.error)}</p>}
  </details>;
}
