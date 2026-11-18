import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Boxes, ChevronRight, CircuitBoard, FileCode2, RefreshCw, Search } from "lucide-react";
import { useState, type ReactNode } from "react";
import type { Capability } from "@/bindings/Capability";
import type { CustomizationEntry } from "@/bindings/CustomizationEntry";
import type { PluginInventory } from "@/bindings/PluginInventory";
import type { PluginInventoryStatus } from "@/bindings/PluginInventoryStatus";
import type { Provider } from "@/bindings/Provider";
import type { ReportedPlugin } from "@/bindings/ReportedPlugin";
import type { ReviewState } from "@/bindings/ReviewState";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import { useCustomizationInventory } from "@/customizations-panel";
import type { CliPrerequisite } from "@/desktop-cli-prerequisites";
import type { LiveModel } from "@/live-models";
import { QualificationMatrix } from "@/qualification-matrix";
import "./provider-inventory.css";

type ReportedModel = LiveModel & { inputModalities: string[] | null; outputModalities: string[] | null };
type TransportReport = {
  protocol: string | null;
  capabilities: Capability[] | null;
  promptFormats: { image: boolean | null; audio: boolean | null; embeddedContext: boolean | null } | null;
  sessionModel: string | null;
  tools: string[] | null;
  commands: string[] | null;
};
type ModelInventory = { executable: string; version: string; models: ReportedModel[]; transportReport: TransportReport };
type LocalSkill = { id: string; name: string; source: string; path: string; scope: string | null; enabled: boolean | null; revision: string | null };
type SkillInventory = { checkout: string; catalog: { entries: LocalSkill[]; issues: { path: string | null; message: string }[] } };
type InventoryKind = "all" | "models" | "plugins" | "skills" | "declarations";
type StatusTone = "reported" | "warning" | "unknown";

export const providerModelInventoryKey = (provider: Provider) => ["provider-model-inventory", provider] as const;
export const providerPluginInventoryKey = (provider: Provider) => ["provider-plugin-inventory", provider] as const;
export const providerSkillInventoryKey = (provider: Provider, checkoutId?: string) => checkoutId
  ? ["provider-skill-inventory", provider, checkoutId] as const : ["provider-skill-inventory", provider] as const;

const pluginStatus: Record<PluginInventoryStatus, string> = {
  reported: "Inventory reported",
  partial: "Partial inventory · Some entries could not be read",
  executable_missing: "Executable not found",
  unsupported: "This CLI build does not support JSON plugin inventory",
  probe_failed: "The plugin inventory check failed",
  invalid_response: "The CLI returned an unrecognized inventory",
};
const reviewStatus: Record<ReviewState, string> = {
  approved: "Approved revision",
  changed: "Changed since approval",
  disabled: "Disabled",
  pending: "Approval pending",
  unresolved: "Blocked · source unresolved",
};

function inventoryText(text: string) {
  return text.replaceAll("_", " ");
}

function Status({ tone, children }: { tone: StatusTone; children: ReactNode }) {
  return <span className="provider-report-status" data-tone={tone}>{children}</span>;
}

function ReportedValue({ items }: { items: string[] | null | undefined }) {
  if (items == null) return <Status tone="unknown">Unknown</Status>;
  return items.length ? <span>{items.map(inventoryText).join(", ")} <Status tone="reported">Reported</Status></span>
    : <span>No entries reported</span>;
}

function Availability({ reported }: { reported: boolean | null | undefined }) {
  return reported == null ? <Status tone="unknown">Unknown</Status>
    : reported ? <Status tone="reported">Reported</Status> : <Status tone="warning">Unavailable in report</Status>;
}

export function ProviderInventory({ provider, checkoutId, inventory, installation, refreshPlugins, pluginsRefreshing }: {
  provider: Provider;
  checkoutId?: string;
  inventory: PluginInventory | null | undefined;
  installation?: CliPrerequisite;
  refreshPlugins: () => Promise<void>;
  pluginsRefreshing: boolean;
}) {
  const [search, setSearch] = useState("");
  const [kind, setKind] = useState<InventoryKind>("all");
  const modelInventory = useQuery({
    queryKey: [...providerModelInventoryKey(provider), installation?.executable, installation?.version],
    queryFn: () => invoke<ModelInventory>("provider_model_inventory", { provider }),
    retry: false,
    staleTime: 60_000,
  });
  const skillInventory = useQuery({
    queryKey: providerSkillInventoryKey(provider, checkoutId),
    queryFn: () => invoke<SkillInventory>("provider_skill_inventory", { provider, checkoutId }),
    enabled: Boolean(checkoutId),
    retry: false,
    staleTime: 60_000,
  });
  const declarations = useCustomizationInventory(checkoutId);
  const terms = search.toLocaleLowerCase().trim().split(/\s+/).filter(Boolean);
  const providerEntries = declarations.data?.entries.filter((entry) => entry.provider === provider) ?? [];
  function matches(...fields: unknown[]) {
    const row = fields.filter((field) => field != null).flat(Infinity).join(" ").toLocaleLowerCase();
    return terms.every((term) => row.includes(term));
  }
  const models = modelInventory.data?.models.filter(({ option, inputModalities, outputModalities, efforts }) =>
    matches(option.name, option.selection.modelId, option.selection.profileId, option.unavailableReason,
      option.selectable ? "selectable" : "blocked", inputModalities, outputModalities, efforts)) ?? [];
  const plugins = inventory?.report.plugins.filter((plugin) => matches(
    plugin.name, plugin.id, plugin.version, plugin.scope, plugin.marketplace, plugin.source,
    plugin.sourceLocation, plugin.installPath, plugin.skills, plugin.hooks, plugin.mcpServers,
    plugin.enabled === null ? "unknown" : plugin.enabled ? "enabled" : "disabled",
  )) ?? [];
  const skills = skillInventory.data?.catalog.entries.filter((skill) => matches(
    skill.name, skill.path, skill.source, skill.scope, skill.revision, skill.enabled === false ? "disabled" : "discovered",
  )) ?? [];
  const declared = providerEntries.filter((entry) => matches(
    entry.label, inventoryText(entry.kind), entry.source, entry.sourceRevision, entry.target, entry.issue,
    reviewStatus[entry.review], entry.requestedScope,
  ));
  const isRefreshing = pluginsRefreshing || modelInventory.isFetching || skillInventory.isFetching || declarations.isFetching;
  const displayed = { models: models.length, plugins: plugins.length, skills: skills.length, declarations: declared.length };
  const searching = terms.length > 0;
  const show = (section: Exclude<InventoryKind, "all">) => (kind === section || kind === "all") && (!searching || displayed[section] > 0);
  const report = modelInventory.data?.transportReport;
  const visibleTransport = (kind === "all" || kind === "models") && report && (!searching ||
    matches(report.protocol, report.sessionModel, report.tools, report.commands, report.capabilities?.map(inventoryText))) ? report : undefined;
  const entries = kind === "all" ? Object.values(displayed).reduce((sum, value) => sum + value, 0) : displayed[kind];
  const count = entries + Number(searching && Boolean(visibleTransport));
  async function refresh() {
    await Promise.allSettled([
      modelInventory.refetch(), refreshPlugins(),
      ...(checkoutId ? [skillInventory.refetch(), declarations.refetch()] : []),
    ]);
  }

  return <div className="provider-inventory">
    <div className="provider-inventory-tools">
      <label className="provider-inventory-search"><Search size={16} aria-hidden />
        <input type="search" aria-label="Search provider inventory" placeholder="Search models, plugins or skills" value={search}
          onChange={(event) => setSearch(event.currentTarget.value)} />
      </label>
      <select aria-label="Inventory type" value={kind} onChange={(event) => setKind(event.currentTarget.value as InventoryKind)}>
        <option value="all">Everything</option><option value="models">Models</option><option value="plugins">Plugins</option>
        <option value="skills">Local skills</option><option value="declarations">Checkout declarations</option>
      </select>
      <Button type="button" variant="outline" disabled={isRefreshing} onClick={() => void refresh()}>
        <RefreshCw size={15} aria-hidden /> {isRefreshing ? "Checking…" : "Refresh inventory"}
      </Button>
    </div>
    <p role="status" className="provider-inventory-note">{searching ? `${count} ${count === 1 ? "match" : "matches"}` :
      "Reports identify their source and scope. Recorded qualifications include evidence from prior runs."}</p>
    {(kind === "all" || kind === "models") && modelInventory.isPending && <p role="status">Querying the CLI for models and capabilities…</p>}
    {(kind === "all" || kind === "models") && modelInventory.error && <p role="alert">Model inventory unavailable · {String(modelInventory.error)}</p>}
    {(kind === "all" || kind === "skills") && skillInventory.error && <p role="alert">Local skills could not be read · {String(skillInventory.error)}</p>}
    {(kind === "all" || kind === "declarations") && declarations.error && <p role="alert">Checkout declarations could not be read · {String(declarations.error)}</p>}
    {searching && !count && <p className="provider-inventory-empty">No entries match your search.</p>}

    {(show("models") || visibleTransport) && modelInventory.data && <section className="provider-inventory-section" aria-label="Models and transport">
      <h3><CircuitBoard size={16} aria-hidden /> Models <span>{models.length}</span></h3>
      <p className="provider-inventory-note" title={modelInventory.data.executable}>CLI {modelInventory.data.version} · Neutral read-only probe</p>
      {models.length ? <ul className="provider-inventory-results">{models.map((model) => <li key={model.option.selection.modelId}><ModelEntry model={model} /></li>)}</ul>
        : !searching && <p>The CLI returned no model entries.</p>}
      {visibleTransport && <TransportCapabilities report={visibleTransport} searching={searching} />}
    </section>}

    {show("plugins") && (inventory ? <PluginReport inventory={inventory} entries={plugins} searching={searching} />
      : <p className="provider-inventory-note" role="status">{pluginsRefreshing ? "Querying the CLI for installed plugins…" : "Plugin inventory unavailable."}</p>)}

    {show("skills") && <section className="provider-inventory-section" aria-label="Local skills">
      <h3><FileCode2 size={16} aria-hidden /> Local skills <span>{skills.length}</span></h3>
      {!checkoutId && <p>Open a repository to inspect skills for its worktree.</p>}
      {checkoutId && skillInventory.isPending && <p role="status">Reading local skill files…</p>}
      {skillInventory.data && <>
        <p className="provider-inventory-note provider-inventory-path">{skillInventory.data.checkout}</p>
        {skillInventory.data.catalog.issues.length > 0 && <details>
          <summary>Partial discovery · {skillInventory.data.catalog.issues.length} unresolved sources</summary>
          <ul>{skillInventory.data.catalog.issues.map(({ path, message }, index) => <li key={`${path}:${index}`}>{path ?? message}</li>)}</ul>
        </details>}
        {skills.length ? <ul className="provider-inventory-results">{skills.map((skill) => <li key={skill.id}>
          <div className="provider-inventory-row-title"><strong>{skill.name}</strong>
            <Status tone={skill.enabled === false ? "warning" : "unknown"}>{skill.enabled === false ? "Disabled" : "Discovered"}</Status></div>
          <p className="provider-inventory-note provider-inventory-path">{skill.path}</p>
          <p className="provider-inventory-note" title={skill.revision ?? undefined}>{skill.source === "filesystem" ? "Filesystem" : skill.source}
            {skill.scope && ` · ${inventoryText(skill.scope)}`}{skill.revision && ` · ${skill.revision.slice(0, 12)}`}</p>
        </li>)}</ul> : <p>No local skill files found.</p>}
      </>}
    </section>}

    {show("declarations") && declarations.data && <section className="provider-inventory-section" aria-label="Checkout declarations">
      <h3><Boxes size={16} aria-hidden /> Checkout declarations <span>{declared.length}</span></h3>
      {declared.length ? <ul className="provider-inventory-results">{declared.map((entry) => <li key={entry.id}><DeclarationEntry entry={entry} /></li>)}</ul>
        : <p>No declarations found for this service.</p>}
    </section>}

    {!searching && (kind === "all" || kind === "models") && <QualificationMatrix provider={provider} validationCheckoutId={checkoutId} />}
  </div>;
}

function ModelEntry({ model }: { model: ReportedModel }) {
  const { option } = model;
  return <details>
    <summary className="provider-inventory-row-title"><strong>{option.name}</strong>
      <Status tone={option.selectable ? "reported" : "warning"}>{option.selectable ? "Selectable" : "Blocked"}</Status>
      <ChevronRight className="provider-inventory-chevron" size={14} aria-hidden />
    </summary>
    <dl className="provider-inventory-facts">
      <dt>Model ID</dt><dd>{option.selection.modelId}</dd>
      <dt>Profile</dt><dd>{option.selection.profileId}</dd>
      <dt>Input modalities</dt><dd><ReportedValue items={model.inputModalities} /></dd>
      <dt>Output modalities</dt><dd><ReportedValue items={model.outputModalities} /></dd>
      <dt>Image generation</dt><dd><Availability reported={model.outputModalities?.some((item) => item.toLowerCase() === "image")} /></dd>
      <dt>Audio generation</dt><dd><Availability reported={model.outputModalities?.some((item) => item.toLowerCase() === "audio")} /></dd>
      <dt>Thinking levels</dt><dd><ReportedValue items={model.efforts.length ? model.efforts : null} /></dd>
    </dl>
    {!option.selectable && option.unavailableReason && <p className="provider-inventory-note">{option.unavailableReason}</p>}
  </details>;
}

function TransportCapabilities({ report, searching }: { report: TransportReport; searching: boolean }) {
  const modes = report.capabilities?.filter((value) => ["native_plan_mode", "native_goal_command", "workspace_write"].includes(value));
  return <details className="provider-inventory-transport" open={searching || undefined}>
    <summary>Capabilities reported by the transport</summary>
    <dl className="provider-inventory-facts">
      <dt>Protocol</dt><dd>{report.protocol ?? "Unknown"}</dd>
      <dt>Session model</dt><dd>{report.sessionModel ?? "Unknown"}</dd>
      <dt>Image prompt</dt><dd><Availability reported={report.promptFormats?.image} /></dd>
      <dt>Audio prompt</dt><dd><Availability reported={report.promptFormats?.audio} /></dd>
      <dt>Embedded context</dt><dd><Availability reported={report.promptFormats?.embeddedContext} /></dd>
      <dt>Provider modes</dt><dd><ReportedValue items={modes} /></dd>
      <dt>Repository write</dt><dd><Status tone="warning">Blocked in this read-only probe</Status></dd>
      <dt>Tools</dt><dd><ReportedValue items={report.tools} /></dd>
      <dt>Commands</dt><dd><ReportedValue items={report.commands} /></dd>
      <dt>Protocol capabilities</dt><dd><ReportedValue items={report.capabilities} /></dd>
    </dl>
  </details>;
}

function PluginReport({ inventory, entries, searching }: { inventory: PluginInventory; entries: ReportedPlugin[]; searching: boolean }) {
  const { report, checkedAt } = inventory;
  const { copy, feedback } = useContextActionFeedback();
  const hasInventory = report.status === "reported" || report.status === "partial";
  return <details className="provider-inventory-section" open={searching || undefined}>
    <summary>{hasInventory ? `Installed plugins (${report.plugins.length})` : "Plugin inventory"}</summary>
    <div className="provider-inventory-report">
      <p><Status tone={report.status === "reported" ? "reported" : "warning"}>{pluginStatus[report.status]}</Status>
        <span className="provider-inventory-note"> · {new Date(checkedAt).toLocaleString()}</span></p>
      {report.executable && <p className="provider-inventory-note provider-inventory-path">{report.executable}{report.version && ` · ${report.version}`}</p>}
      {hasInventory && !report.loadTelemetryReported && <p className="provider-inventory-note">CLI profile inventory. In-session plugin loads are unconfirmed.</p>}
      {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
      {entries.length > 0 && <ul className="provider-inventory-results">{entries.map((plugin, index) => <li key={`${plugin.id}:${plugin.scope ?? ""}:${index}`}>
        <ContextActions label={plugin.id} groups={[[
          { label: "Copy plugin ID", run: () => copy(plugin.id) },
          ...(plugin.version ? [{ label: "Copy version", run: () => copy(plugin.version!) }] : []),
          ...(plugin.sourceLocation ? [{ label: "Copy plugin source", run: () => copy(plugin.sourceLocation!) }] : []),
          ...(plugin.skills?.length ? [{ label: "Copy skill names", run: () => copy(plugin.skills!.join("\n")) }] : []),
          ...(plugin.mcpServers?.length ? [{ label: "Copy MCP server names", run: () => copy(plugin.mcpServers!.join("\n")) }] : []),
        ]]}><div className="provider-inventory-row-title"><strong>{plugin.name ?? plugin.id}</strong>
          <Status tone={plugin.enabled == null ? "unknown" : plugin.enabled ? "reported" : "warning"}>
            {plugin.enabled == null ? "Enablement unknown" : plugin.enabled ? "Enabled" : "Disabled"}
          </Status></div></ContextActions>
        <p className="provider-inventory-note provider-inventory-path">
          {[plugin.version, plugin.scope, plugin.marketplace, plugin.source, plugin.sourceLocation ?? plugin.installPath].filter(Boolean).join(" · ")}
        </p>
        {plugin.skills?.length ? <p>Skills · {plugin.skills.join(", ")}</p> : null}
        {plugin.hooks?.length ? <p>Hooks · {plugin.hooks.join(", ")}</p> : null}
        {plugin.mcpServers?.length ? <p>MCP servers · {plugin.mcpServers.join(", ")}</p> : null}
      </li>)}</ul>}
    </div>
  </details>;
}

function DeclarationEntry({ entry }: { entry: CustomizationEntry }) {
  return <>
    <div className="provider-inventory-row-title"><strong>{entry.label}</strong>
      <Status tone={entry.review === "approved" ? "reported" : "warning"}>{reviewStatus[entry.review]}</Status></div>
    <p className="provider-inventory-note provider-inventory-path" title={entry.sourceRevision}>
      {inventoryText(entry.kind)} · {entry.source} · {entry.sourceRevision.slice(0, 12)}</p>
    {entry.issue && <p>{entry.issue}</p>}
  </>;
}
