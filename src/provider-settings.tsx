import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import type { AttachmentLimits } from "@/bindings/AttachmentLimits";
import type { CliProbe } from "@/bindings/CliProbe";
import type { ConfiguredExecutable } from "@/bindings/ConfiguredExecutable";
import type { PluginInventory } from "@/bindings/PluginInventory";
import type { PluginInventoryStatus } from "@/bindings/PluginInventoryStatus";
import type { Provider } from "@/bindings/Provider";
import { UserPluginManagement } from "@/cli-plugin-management";
import { Button } from "@/components/ui/button";
import { desktopCliPrerequisitesKey, ExecutableInstallation, providerExecutableLabels, useDesktopCliPrerequisites, type CliPrerequisite } from "@/desktop-cli-prerequisites";
import { ExecutionProfileModelSettings } from "@/model-default-settings";
import { useWorkspace } from "@/workspace-state";

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
  const projectId = useWorkspace((state) => state.checkout?.projectId);
  return (
    <section className="h-full overflow-auto p-6">
      <h1 className="mb-5 text-xl font-semibold tracking-tight">Provider settings</h1>
      <div className="grid max-w-3xl gap-4">
        {projectId && <ProjectAttachmentSettings key={projectId} projectId={projectId} />}
        <ProviderSettings />
      </div>
    </section>
  );
}

export function ProviderSettings() {
  const prerequisites = useDesktopCliPrerequisites();
  const profiles = useQuery({
    queryKey: ["cli-executables"],
    queryFn: () => invoke<ConfiguredExecutable[]>("cli_executables"),
    staleTime: Infinity,
  });
  return (
    <div className="grid max-w-3xl gap-4" data-setting="provider-paths">
      {profiles.isPending && <p role="status" className="text-sm text-muted-foreground">Loading executable paths…</p>}
      {profiles.isError && <p role="alert" className="text-sm text-red-300">{String(profiles.error)}</p>}
      {prerequisites.isPending && <p role="status" className="text-sm text-muted-foreground">Checking installed executables…</p>}
      {prerequisites.error && <p role="alert" className="text-sm text-red-300">{String(prerequisites.error)}</p>}
      {profiles.data?.map((profile) => <ExecutableSettings key={profile.provider} profile={profile}
        installation={prerequisites.data?.providers.find(({ provider, configuredExecutable }) =>
          provider === profile.provider && configuredExecutable === profile.executable)?.status} />)}
    </div>
  );
}

const ATTACHMENT_FIELDS = {
  filesPerTurn: "Files per turn",
  bytesPerFile: "Bytes per native file or staged copy",
  bytesPerTurn: "Combined native and staged bytes per turn",
  utf8BytesPerExcerpt: "UTF-8 bytes per excerpt",
} satisfies Record<keyof AttachmentLimits, string>;
const ATTACHMENT_KEYS = Object.keys(ATTACHMENT_FIELDS) as (keyof AttachmentLimits)[];

export function ProjectAttachmentSettings({ projectId }: { projectId: string }) {
  const queryClient = useQueryClient();
  const queryKey = ["project-attachment-limits", projectId];
  const settings = useQuery({
    queryKey,
    queryFn: () => invoke<AttachmentLimits>("project_attachment_limits", { projectId }),
    staleTime: Infinity,
  });
  const save = useMutation({
    mutationFn: (limits: AttachmentLimits) => invoke<void>("set_project_attachment_limits", { projectId, limits }),
    onSuccess: (_, limits) => queryClient.setQueryData(queryKey, limits),
  });

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const data = new FormData(event.currentTarget);
    save.mutate(Object.fromEntries(ATTACHMENT_KEYS.map((key) => [key, Number(data.get(key))])) as AttachmentLimits);
  }

  return (
    <section className="rounded-lg border border-border bg-card p-4" data-setting="attachment-limits">
      <h2 className="text-base font-semibold">Project attachment limits</h2>
      {settings.isPending && <p role="status" className="mt-2 text-sm text-muted-foreground">Loading limits…</p>}
      {settings.isError && <p role="alert" className="mt-2 text-sm text-red-300">{String(settings.error)}</p>}
      {settings.data && (
        <form onSubmit={submit} onChange={() => save.reset()} className="mt-3 grid gap-3 sm:grid-cols-2">
          {ATTACHMENT_KEYS.map((key) => (
            <label key={key} className="text-sm">
              {ATTACHMENT_FIELDS[key]}
              <input name={key} type="number" min={0}
                max={key === "filesPerTurn" ? 4_294_967_295 : Number.MAX_SAFE_INTEGER}
                step={1} required disabled={save.isPending} defaultValue={settings.data[key]}
                className="mt-1 block w-full rounded-md border border-border bg-background p-2 font-mono text-xs" />
            </label>
          ))}
          <div className="flex items-center gap-3 sm:col-span-2">
            <Button type="submit" disabled={save.isPending}>{save.isPending ? "Saving…" : "Save limits"}</Button>
            {save.isSuccess && <p role="status" className="text-sm text-muted-foreground">Limits saved</p>}
            {save.isError && <p role="alert" className="text-sm text-red-300">{String(save.error)}</p>}
          </div>
        </form>
      )}
    </section>
  );
}

function ExecutableSettings({ profile, installation }: { profile: ConfiguredExecutable; installation?: CliPrerequisite }) {
  const queryClient = useQueryClient();
  const [configured, setConfigured] = useState(profile.executable);
  const [path, setPath] = useState(profile.executable ?? "");
  const [probe, setProbe] = useState<CliProbe | null>(null);
  const [pending, setPending] = useState<"save" | "cli" | "plugins" | null>(null);
  const [error, setError] = useState("");
  const requested = path.trim().replace(/^"(.*)"$/, "$1");
  const observedInstallation = probe?.version && probe.executable
    ? { executable: probe.executable, version: probe.version, issue: null }
    : installation;
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
    void queryClient.invalidateQueries({ queryKey: ["conversation-models", provider] });
    void queryClient.invalidateQueries({ queryKey: desktopCliPrerequisitesKey });
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    if (requested && requested !== configured) void run("save", () => save(requested));
  }

  return (
    <section className="rounded-lg border border-border bg-card p-4" aria-labelledby={`${provider}-executable-title`}>
      <h2 id={`${provider}-executable-title`} className="text-base font-semibold">{providerExecutableLabels[provider]}</h2>
      <form onSubmit={submit} className="mt-3 flex flex-wrap items-end gap-2">
        <label className="min-w-52 flex-1 text-sm">
          Executable path
          <input name={provider} value={path} onChange={(event) => setPath(event.target.value)}
            placeholder="Use PATH" className="mt-1 block w-full rounded-md border border-border bg-background p-2 font-mono text-xs" />
        </label>
        <Button type="submit" disabled={pending !== null || !requested || requested === configured}>Save</Button>
        <Button type="button" variant="outline" disabled={pending !== null || configured === null}
          onClick={() => void run("save", () => save(null))}>Use PATH</Button>
        <Button type="button" variant="outline" disabled={pending !== null || requested !== (configured ?? "")}
          onClick={() => void run("cli", async () => setProbe(await invoke<CliProbe>("probe_cli_executable", { provider })))}>
          {pending === "cli" ? "Checking CLI…" : "Check CLI"}
        </Button>
        <Button type="button" variant="outline" disabled={pending !== null || requested !== (configured ?? "")}
          onClick={() => void run("plugins", async () => {
            const result = await invoke<PluginInventory | null>("cli_plugin_inventory", { provider, refresh: true });
            queryClient.setQueryData(pluginKey, result);
          })}>{pending === "plugins" ? "Checking plugins…" : "Check plugins"}</Button>
      </form>
      {observedInstallation && <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <dt className="text-muted-foreground">Installation</dt><dd><ExecutableInstallation status={observedInstallation} /></dd>
      </dl>}
      {error && <p role="alert" className="mt-2 text-sm text-red-300">{error}</p>}
      {probe && (
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          <dt className="text-muted-foreground">Protocol</dt><dd>{PROTOCOL_LABELS[probe.protocol]}</dd>
          <dt className="text-muted-foreground">Authentication</dt><dd>{AUTH_LABELS[probe.authentication]}</dd>
          <dt className="text-muted-foreground">Effective CLI permissions</dt>
          <dd>{probe.effectivePermissions.status === "mode_reported" ? `${probe.effectivePermissions.mode} · reported for the probe session` : "Unverified"}</dd>
          <dt className="text-muted-foreground">Windows command sandbox</dt>
          <dd>{probe.osSandboxSupport === "unsupported" ? "Unsupported for this CLI on the Windows host" : probe.osSandboxSupport === "prerequisites_met" ? "Windows prerequisites met · activation unverified" : "Unverified on the Windows host"}</dd>
        </dl>
      )}
      {inventory.isError && <p role="alert" className="mt-3 text-sm text-red-300">{String(inventory.error)}</p>}
      {inventory.data && <PluginList inventory={inventory.data} />}
      <UserPluginManagement key={configured ?? ""} provider={provider} disabled={pending !== null || requested !== (configured ?? "")} />
      <ExecutionProfileModelSettings provider={provider} />
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
