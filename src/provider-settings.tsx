import { invoke } from "@tauri-apps/api/core";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import type { CliProbe } from "@/bindings/CliProbe";
import type { ConfiguredExecutable } from "@/bindings/ConfiguredExecutable";
import type { PluginInventory } from "@/bindings/PluginInventory";
import type { PluginInventoryStatus } from "@/bindings/PluginInventoryStatus";
import type { Provider } from "@/bindings/Provider";
import { Button } from "@/components/ui/button";

const PROVIDERS: Record<Provider, string> = {
  codex: "Codex",
  claude: "Claude Code",
  antigravity: "Antigravity",
};

const AUTH_LABELS: Record<CliProbe["authentication"], string> = {
  authenticated: "Signed in",
  required: "Sign-in required",
  unknown: "Unverified",
};

const PROTOCOL_LABELS: Record<CliProbe["protocol"], string> = {
  unchecked: "Unchecked",
  verified: "Verified",
  failed: "Negotiation failed",
};

const PLUGIN_STATUS: Record<PluginInventoryStatus, string> = {
  reported: "Inventory reported",
  partial: "Some inventory entries could not be read",
  executable_missing: "Executable not found",
  unsupported: "This CLI build does not support JSON plugin inventory",
  probe_failed: "The plugin inventory check failed",
  invalid_response: "The CLI returned an unrecognized inventory",
};

export function ProviderSettingsPanel() {
  const profiles = useQuery({
    queryKey: ["cli-executables"],
    queryFn: () => invoke<ConfiguredExecutable[]>("cli_executables"),
    staleTime: Infinity,
  });
  return (
    <section className="h-full overflow-auto p-6">
      <h1 className="mb-5 text-xl font-semibold tracking-tight">Provider settings</h1>
      {profiles.isPending && <p role="status" className="text-sm text-muted-foreground">Loading executable paths…</p>}
      {profiles.isError && <p role="alert" className="text-sm text-red-300">{String(profiles.error)}</p>}
      <div className="grid max-w-3xl gap-4">
        {profiles.data?.map((profile) => <ExecutableSettings key={profile.provider} profile={profile} />)}
      </div>
    </section>
  );
}

function ExecutableSettings({ profile }: { profile: ConfiguredExecutable }) {
  const queryClient = useQueryClient();
  const [configured, setConfigured] = useState(profile.executable);
  const [path, setPath] = useState(profile.executable ?? "");
  const [probe, setProbe] = useState<CliProbe | null>(null);
  const [pending, setPending] = useState<"save" | "cli" | "plugins" | null>(null);
  const [error, setError] = useState("");
  const { provider } = profile;
  const pluginKey = ["cli-plugin-inventory", provider];
  const inventory = useQuery({
    queryKey: pluginKey,
    queryFn: () => invoke<PluginInventory | null>("cli_plugin_inventory", { provider, refresh: false }),
    staleTime: Infinity,
  });

  async function run(pendingAction: "save" | "cli" | "plugins", operation: () => Promise<void>) {
    setError("");
    setPending(pendingAction);
    try {
      await operation();
    } catch (failure) {
      setError(String(failure));
    } finally {
      setPending(null);
    }
  }

  async function save(executable: string | null) {
    const updated = await invoke<ConfiguredExecutable>("set_cli_executable", { provider, executable });
    queryClient.setQueryData<ConfiguredExecutable[]>(["cli-executables"], (current) =>
      current?.map((entry) => entry.provider === provider ? updated : entry));
    setConfigured(updated.executable);
    setPath(updated.executable ?? "");
    setProbe(null);
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    void run("save", () => save(path.trim()));
  }

  return (
    <section className="rounded-lg border border-border bg-card p-4">
      <h2 className="text-base font-semibold">{PROVIDERS[provider]}</h2>
      <form onSubmit={submit} className="mt-3 flex flex-wrap items-end gap-2">
        <label className="min-w-52 flex-1 text-sm">
          Executable path
          <input name={provider} value={path} onChange={(event) => setPath(event.target.value)}
            placeholder="Use PATH" className="mt-1 block w-full rounded-md border border-border bg-background p-2 font-mono text-xs" />
        </label>
        <Button type="submit" disabled={pending !== null || !path.trim() || path.trim() === configured}>Save</Button>
        <Button type="button" variant="outline" disabled={pending !== null || configured === null}
          onClick={() => void run("save", () => save(null))}>Use PATH</Button>
        <Button type="button" variant="outline" disabled={pending !== null || path.trim() !== (configured ?? "")}
          onClick={() => void run("cli", async () => setProbe(await invoke<CliProbe>("probe_cli_executable", { provider })))}>
          {pending === "cli" ? "Checking CLI…" : "Check CLI"}
        </Button>
        <Button type="button" variant="outline" disabled={pending !== null || path.trim() !== (configured ?? "")}
          onClick={() => void run("plugins", async () => {
            const result = await invoke<PluginInventory | null>("cli_plugin_inventory", { provider, refresh: true });
            queryClient.setQueryData(pluginKey, result);
          })}>{pending === "plugins" ? "Checking plugins…" : "Check plugins"}</Button>
      </form>
      {error && <p role="alert" className="mt-2 text-sm text-red-300">{error}</p>}
      {probe && (
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          <dt className="text-muted-foreground">Installation</dt>
          <dd>{probe.version ?? (probe.executable ? "Detected, version unverified" : "Executable not found")}</dd>
          <dt className="text-muted-foreground">Protocol</dt><dd>{PROTOCOL_LABELS[probe.protocol]}</dd>
          <dt className="text-muted-foreground">Authentication</dt><dd>{AUTH_LABELS[probe.authentication]}</dd>
          <dt className="text-muted-foreground">Effective permissions</dt><dd>Unverified</dd>
        </dl>
      )}
      {inventory.isError && <p role="alert" className="mt-3 text-sm text-red-300">{String(inventory.error)}</p>}
      {inventory.data && <PluginList inventory={inventory.data} />}
    </section>
  );
}

function PluginList({ inventory }: { inventory: PluginInventory }) {
  const { report, checkedAt } = inventory;
  const hasInventory = report.status === "reported" || report.status === "partial";
  return (
    <details className="mt-3 border-t border-border pt-3 text-sm">
      <summary className="cursor-pointer font-medium">
        {hasInventory ? `Installed plugins (${report.plugins.length})` : "Plugin inventory"}
      </summary>
      <p className="mt-2 text-muted-foreground" role="status">
        {PLUGIN_STATUS[report.status]} · {new Date(checkedAt).toLocaleString()}
      </p>
      {report.executable && (
        <p className="mt-1 break-all text-xs text-muted-foreground">
          {report.executable}{report.version && ` · ${report.version}`}
        </p>
      )}
      {hasInventory && !report.loadTelemetryReported && <p className="mt-1 text-muted-foreground">CLI profile inventory. In-session plugin loads are unconfirmed.</p>}
      {report.plugins.length > 0 && (
        <ul className="mt-2 max-h-64 space-y-2 overflow-auto pr-2">
          {report.plugins.map((plugin, index) => (
            <li key={`${plugin.id}-${plugin.scope ?? ""}-${index}`} className="rounded-md border border-border p-2">
              <p className="break-words font-medium">{plugin.name ?? plugin.id}{plugin.version && ` · ${plugin.version}`}</p>
              <p className="break-words text-xs text-muted-foreground">
                {[plugin.enabled === null ? "Enablement unreported" : plugin.enabled ? "Enabled" : "Disabled",
                  plugin.scope, plugin.marketplace, plugin.source, plugin.sourceLocation].filter(Boolean).join(" · ")}
              </p>
              {plugin.skills?.length ? <p className="mt-1 break-words text-xs">Skills · {plugin.skills.join(", ")}</p> : null}
              {plugin.hooks?.length ? <p className="mt-1 break-words text-xs">Hooks · {plugin.hooks.join(", ")}</p> : null}
              {plugin.mcpServers?.length ? <p className="mt-1 break-words text-xs">MCP servers · {plugin.mcpServers.join(", ")}</p> : null}
            </li>
          ))}
        </ul>
      )}
    </details>
  );
}
