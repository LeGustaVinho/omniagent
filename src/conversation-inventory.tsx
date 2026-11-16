import { useId, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { PluginInventory } from "@/bindings/PluginInventory";
import { Button } from "@/components/ui/button";

type InstructionSource = {
  path: string;
  scope: string;
  revision: string | null;
  selection: string;
  importedFrom: string | null;
  includedBytes: number | null;
};

type SkillSource = {
  id: string;
  source: "filesystem" | "provider";
  name: string;
  path: string;
  scope: string | null;
  pluginId: string | null;
  enabled: boolean | null;
  revision: string | null;
};

type InventoryTarget =
  | { kind: "instruction" | "plugin"; index: number }
  | { kind: "skill"; id: string };

type InventoryReport =
  | { kind: "file_read"; path: string; tool: string }
  | { kind: "skill_use" | "session_skill"; name: string }
  | { kind: "session_plugin"; name: string; path: string | null }
  | { kind: "hook"; trigger: string; status: string; name: string | null; path: string | null };

type InventoryObservation = {
  eventSequence: string;
  location: { sessionId: string | null; turnId: string | null; itemId: string | null };
  report: InventoryReport;
  targets: InventoryTarget[];
};

type RecordedCheckout = {
  runId: string;
  checkoutId: string;
  workingDirectory: string;
  worktreeRoot: string | null;
  primaryRoot: string | null;
};

type NativeHistory =
  | { mode: "fresh" }
  | { mode: "provider_cursor"; sessionId: string; contextId: string }
  | { mode: "provider_fork"; sessionId: string; contextId: string; sourceRunId: string; sourceTurnId: string }
  | { mode: "provider_recovery"; sessionId: string; contextId: string; sourceRunId: string; sourceTurnId: string | null };

type DispatchInventory = {
  dispatchId: string;
  checkout: RecordedCheckout;
  inheritedCheckout: RecordedCheckout | null;
  nativeHistory: NativeHistory;
  historyReports: { eventSequence: string; turns: number | null; itemKind: string | null }[];
  inventory: {
    instructions: { sources: InstructionSource[]; unresolved: string[] } | null;
    filesystemSkills: SkillSource[];
    reportedSkills: { entries: SkillSource[]; reported: boolean } | null;
    requestedSkill: { name: string; matchingIds: string[]; selections?: {
      id: string; name: string; namespace: string | null; path: string; revision: string;
    }[] } | null;
    plugins: PluginInventory | null;
  };
  observations: InventoryObservation[];
};

function words(value: string) {
  return value.replaceAll("_", " ");
}

function enabled(value: boolean | null) {
  return value === null ? "Availability unreported" : value ? "Enabled" : "Disabled";
}

function reportLabel(report: InventoryReport) {
  switch (report.kind) {
    case "file_read": return `File read via ${report.tool} · ${report.path}`;
    case "skill_use": return `Skill use reported · ${report.name}`;
    case "session_skill": return `Skill listed by the native session · ${report.name}`;
    case "session_plugin":
      return `Plugin listed by the native session · ${report.name}${report.path ? ` · ${report.path}` : ""}`;
    case "hook":
      return `Hook reported · ${[report.trigger, words(report.status), report.name, report.path].filter(Boolean).join(" · ")}`;
  }
}

function CheckoutLocation({ checkout }: { checkout: RecordedCheckout }) {
  return <dl className="grid break-all text-muted-foreground">
    <dt>Checkout ID</dt><dd>{checkout.checkoutId}</dd>
    <dt>Working directory</dt><dd>{checkout.workingDirectory}</dd>
    {checkout.worktreeRoot && <><dt>Worktree root</dt><dd>{checkout.worktreeRoot}</dd></>}
    {checkout.primaryRoot && checkout.primaryRoot !== checkout.worktreeRoot &&
      <><dt>Primary checkout root</dt><dd>{checkout.primaryRoot}</dd></>}
  </dl>;
}

function NativeContext({ evidence }: { evidence: DispatchInventory }) {
  const { nativeHistory, inheritedCheckout, historyReports } = evidence;
  const sourceRunId = inheritedCheckout?.runId ??
    ("sourceRunId" in nativeHistory ? nativeHistory.sourceRunId : undefined);
  return <div className="grid gap-1">
    <h3 className="font-medium">Provider-managed context</h3>
    {nativeHistory.mode === "fresh" ? <p className="text-muted-foreground">Fresh session requested.</p> : <>
      <dl className="grid break-all text-muted-foreground">
        <dt>Requested history</dt><dd>{nativeHistory.mode === "provider_cursor" ? "Resume session" :
          nativeHistory.mode === "provider_fork" ? "Fork session" : "Recover session"}</dd>
        <dt>Source session</dt><dd>{nativeHistory.sessionId}</dd>
        <dt>Context boundary</dt><dd>{nativeHistory.contextId}</dd>
        {"sourceTurnId" in nativeHistory && nativeHistory.sourceTurnId &&
          <><dt>Source turn</dt><dd>{nativeHistory.sourceTurnId}</dd></>}
        {sourceRunId && <><dt>Source attempt</dt><dd>{sourceRunId}</dd></>}
      </dl>
      {inheritedCheckout ? <div>
        <h4 className="font-medium">Inherited checkout source</h4>
        <CheckoutLocation checkout={inheritedCheckout} />
      </div> : <p className="text-muted-foreground">Inherited checkout source unavailable.</p>}
    </>}
    {historyReports.length > 0 && <ol aria-label="Provider history reports">
      {historyReports.map(({ eventSequence, turns, itemKind }) => <li key={eventSequence} className="break-all text-muted-foreground">
        {[
          "History reported",
          turns !== null && `${turns} ${turns === 1 ? "turn" : "turns"}`,
          itemKind && words(itemKind),
          `Event ${eventSequence}`,
        ].filter(Boolean).join(" · ")}
      </li>)}
    </ol>}
    <p className="text-muted-foreground">Native context contents are unconfirmed. Open Exact input for the application’s recorded prompt.</p>
  </div>;
}

function SavedInventory({ evidence }: { evidence: DispatchInventory }) {
  const idPrefix = useId();
  const { inventory, observations } = evidence;
  const instructions = inventory.instructions?.sources ?? [];
  const plugins = inventory.plugins?.report.plugins ?? [];
  const skills = new Map<string, SkillSource[]>();
  for (const skill of [...inventory.filesystemSkills, ...(inventory.reportedSkills?.entries ?? [])]) {
    const sources = skills.get(skill.id) ?? [];
    sources.push(skill);
    skills.set(skill.id, sources);
  }
  const reportedUses = new Set(observations.flatMap(({ report, targets }) =>
    report.kind === "skill_use" ? targets.filter((target) => target.kind === "skill").map(({ id }) => id) : [],
  ));
  const targetId = (target: InventoryTarget) =>
    `${idPrefix}-${target.kind}-${target.kind === "skill" ? target.id : target.index}`;
  const targetLabel = (target: InventoryTarget) => {
    switch (target.kind) {
      case "instruction": return instructions[target.index]?.path ?? "Saved instruction";
      case "skill": return skills.get(target.id)?.[0]?.name ?? "Saved skill";
      case "plugin": return plugins[target.index]?.name ?? plugins[target.index]?.id ?? "Saved plugin";
    }
  };

  return <div className="grid gap-3 text-xs">
    <p className="break-all text-muted-foreground">Dispatch {evidence.dispatchId}</p>
    <div>
      <h3 className="font-medium">Dispatch checkout</h3>
      <CheckoutLocation checkout={evidence.checkout} />
    </div>
    <NativeContext evidence={evidence} />
    <div>
      <h3 className="font-medium">Detected instruction sources</h3>
      {instructions.length ? <ul className="mt-1 grid gap-2">
        {instructions.map((instruction, index) => <li id={targetId({ kind: "instruction", index })} key={index} className="break-all">
          <p>{instruction.path}</p>
          <p className="text-muted-foreground">
            {words(instruction.scope)} · {words(instruction.selection)}
            {instruction.includedBytes !== null && ` · ${instruction.includedBytes} bytes`}
          </p>
          <p className="text-muted-foreground">{instruction.revision ? `Recorded revision ${instruction.revision}` : "Revision unavailable"}</p>
          {instruction.importedFrom && <p className="text-muted-foreground">Imported from {instruction.importedFrom}</p>}
        </li>)}
      </ul> : <p className="text-muted-foreground">Instruction entries unavailable.</p>}
      {inventory.instructions?.unresolved.map((path) => <p key={path} className="break-all text-muted-foreground">Unresolved path {path}</p>)}
    </div>
    <div>
      <h3 className="font-medium">Skills</h3>
      {inventory.requestedSkill && <p className="text-muted-foreground">Requested skill {inventory.requestedSkill.name}</p>}
      {!!inventory.requestedSkill?.selections?.length && <ul aria-label="Selected native skill inputs" className="grid gap-1">
        {inventory.requestedSkill.selections.map(({ id, name, namespace, path, revision }) => <li key={id} className="break-all">
          <strong>{namespace ? `${namespace} · ${name}` : name}</strong><p>{path}</p><p>Recorded revision <code>{revision}</code></p>
        </li>)}
      </ul>}
      {inventory.reportedSkills && <p className="text-muted-foreground">
        {inventory.reportedSkills.reported ? "CLI reported skill availability" : "CLI skill availability unreported"}
      </p>}
      {skills.size ? <ul className="mt-1 grid gap-2">
        {Array.from(skills, ([id, sources]) => <li id={targetId({ kind: "skill", id })} key={id} className="break-all">
          <p>{sources[0]?.name} · {reportedUses.has(id) ? "Use reported" : "Use unconfirmed"}</p>
          {sources.map((skill, index) => <div key={index} className="text-muted-foreground">
            <p>{skill.path}</p>
            <p>{[
              skill.source === "filesystem" ? "Filesystem discovery" : "Provider catalog",
              skill.scope, skill.pluginId, enabled(skill.enabled),
            ].filter(Boolean).join(" · ")}</p>
            <p>{skill.revision ? `Recorded revision ${skill.revision}` : "Revision unavailable"}</p>
          </div>)}
        </li>)}
      </ul> : <p className="text-muted-foreground">Skill entries unavailable.</p>}
    </div>
    <div>
      <h3 className="font-medium">Plugins</h3>
      {inventory.plugins && <p className="text-muted-foreground">Registry inventory · {words(inventory.plugins.report.status)}</p>}
      {plugins.length ? <ul className="mt-1 grid gap-2">
        {plugins.map((plugin, index) => <li id={targetId({ kind: "plugin", index })} key={index} className="break-all">
          <p>{plugin.id}{plugin.version && ` · Version ${plugin.version}`}</p>
          <p className="text-muted-foreground">{[plugin.scope, enabled(plugin.enabled)].filter(Boolean).join(" · ")}</p>
          {plugin.installPath && <p className="text-muted-foreground">{plugin.installPath}</p>}
          {plugin.sourceLocation && <p className="text-muted-foreground">Source {plugin.sourceLocation}</p>}
        </li>)}
      </ul> : <p className="text-muted-foreground">Plugin entries unavailable.</p>}
    </div>
    <div>
      <h3 className="font-medium">Provider observations</h3>
      {observations.length ? <ol aria-label="Provider inventory observations" className="mt-1 grid gap-2">
        {observations.map(({ eventSequence, report, targets }, index) => <li key={`${eventSequence}-${index}`} className="break-all">
          <p>{reportLabel(report)}</p>
          <p className="text-muted-foreground">Event {eventSequence}</p>
          {targets.length ? <ul aria-label="Saved inventory links">
            {targets.map((target) => <li key={targetId(target)}>
              <a href={`#${targetId(target)}`} className="underline">{targetLabel(target)}</a>
            </li>)}
          </ul> : <p className="text-muted-foreground">Saved identity unresolved</p>}
        </li>)}
      </ol> : <p className="text-muted-foreground">Observations unavailable.</p>}
    </div>
  </div>;
}

export function ConversationInventory({ conversationId, runId }: { conversationId: string; runId: string }) {
  const [open, setOpen] = useState(false);
  const query = useQuery({
    queryKey: ["conversation-run", runId, "dispatch-inventory", conversationId],
    enabled: open,
    queryFn: () => invoke<DispatchInventory | null>("conversation_dispatch_inventory", { conversationId, runId }),
  });
  return <>
    <Button type="button" variant="outline" aria-expanded={open} onClick={() => setOpen(!open)}>Dispatch inventory</Button>
    {open && <section aria-label="Saved dispatch inventory" className="min-w-0 rounded-md border border-border bg-card p-3">
      {query.isPending && <p className="text-xs text-muted-foreground">Loading inventory…</p>}
      {query.error && <p role="alert" className="text-sm text-red-300">{String(query.error)}</p>}
      {query.data === null && <p className="text-xs text-muted-foreground">Dispatch evidence unavailable.</p>}
      {query.data && <SavedInventory evidence={query.data} />}
    </section>}
  </>;
}
