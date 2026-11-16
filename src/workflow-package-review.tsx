import { useState, type Dispatch, type SetStateAction } from "react";
import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { WorkflowGraphEditor, editorDefinitions, type WorkflowEditorState } from "@/workflow-graph-editor";
import { graphProblems, type WorkflowCanvasEdit, type WorkflowDefinition, type WorkflowDefinitions,
  type WorkflowEndpoint, type WorkflowGraph, type WorkflowPortType } from "@/workflow-graph-model";
import { PublishedSubflow, ReusableCatalog, reusableEditableFields, reusableNodeQuery,
  type ReusableBody, type ReusableDescription, type ReusableReference } from "@/workflow-reusable";

type Version = { id: string; revision: string; destination: string; kind: string };
type Reference = { source: "draft"; key: string } | { source: "published"; version: Version }
  | { source: "parameter"; name: string; kind: string };
type Member = {
  key: string; name: string; kind: string; base: Version | null;
  changes: { path: string; change: "added" | "removed" | "changed"; before: unknown; after: unknown }[];
  changesTruncated: boolean; dependencies: Record<string, Reference>;
  affectedUses: { consumer: Version; name: string; slot: string; pinnedRevision: string }[];
  diagnostics: string[]; requirements: string[]; simulatable: boolean;
  sampleInputs: string[]; sampleNodes: { nodeId: string; title: string; outputs: string[] }[];
  graphDefinitions?: WorkflowDefinitions;
};
type Preview = { revision: string; revisions: string[]; members: Member[] };
type TraceState = "completed" | "failed" | "waiting" | "skipped";
type TraceSignal = { from: WorkflowEndpoint; state: "selected" | "waiting" | "skipped" };
type TraceExports = Record<string, TraceSignal>;
type Trace = {
  revision: string; memberKey: string; inputs: Record<string, unknown>;
  steps: { nodeId: string; title: string; state: TraceState; reason: string | null;
    inputs: (TraceSignal & { port: string })[]; outputs: Record<string, { portType: WorkflowPortType; value: unknown }> }[];
  subflows: { path: string; name: string; revision: string; nodes: string[]; outputs: TraceExports }[];
  outputs: TraceExports;
};

const kindName = (kind: string) => kind.replaceAll("_", " ");
const jsonClass = "max-h-56 overflow-auto whitespace-pre-wrap break-all rounded bg-muted/50 p-2 text-xs";

function usePackagePreview(projectId: string, draftId: string, revision: string, enabled: boolean) {
  return useQuery({
    queryKey: ["workflow-package-preview", projectId, draftId, revision], enabled,
    queryFn: () => invoke<Preview>("preview_workflow_package_revision", { projectId, draftId, revision }),
  });
}

type GraphMember = { key: string; name: string; graph: WorkflowGraph; kind?: string; references?: Record<string, ReusableReference> };

export function WorkflowPackageGraphEditing({ projectId, checkoutId, draftId, savedRevision, graphs, draftPresets = {}, disabled, edit }: {
  projectId: string; checkoutId: string; draftId: string; savedRevision: string; graphs: GraphMember[]; disabled: boolean;
  draftPresets?: Record<string, ReusableBody>;
  edit: (key: string, graph: WorkflowGraph, references?: Record<string, ReusableReference>) => void;
}) {
  const [open, setOpen] = useState(false);
  const [selection, select] = useState("");
  const graph = graphs.find(({ key }) => key === selection) ?? graphs[0];
  const show = open && Boolean(graph);
  const preview = usePackagePreview(projectId, draftId, savedRevision, show);
  const registry = useQuery({ queryKey: ["workflow-graph-registry"], enabled: show, staleTime: Infinity,
    queryFn: () => invoke<WorkflowDefinition[]>("workflow_graph_registry"),
  });
  if (!graph) return null;
  const pinned = preview.data?.members.find(({ key }) => key === graph.key)?.graphDefinitions ?? {};
  const error = preview.error ?? registry.error;
  return <details onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Edit member graph</summary>
    {show && <div className="pipeline-stack pt-2">
      <label className="grid gap-1">Graph member<select className="rounded border border-border bg-background p-2" value={graph.key}
        onChange={(event) => select(event.currentTarget.value)}>
        {graphs.map((member) => <option key={member.key} value={member.key}>{member.name}</option>)}
      </select></label>
      {error && <p role="alert">{String(error)}</p>}
      {registry.data && preview.isSuccess && <PackageMemberGraph key={graph.key} projectId={projectId} checkoutId={checkoutId}
        graph={graph.graph} references={graph.references ?? {}} draftPresets={draftPresets}
        pinned={pinned} catalog={registry.data} disabled={disabled} edit={(next, references) => edit(graph.key, next, references)}
        openDraftSubflow={(key) => {
          const child = graphs.find((member) => member.key === key && member.kind === "subflow");
          return child ? <Button type="button" variant="outline" onClick={() => select(child.key)}>Open draft subflow</Button> : null;
        }} />}
    </div>}
  </details>;
}

function PackageMemberGraph({ projectId, checkoutId, graph, references, draftPresets, pinned, catalog, disabled, edit, openDraftSubflow }: {
  projectId: string; checkoutId: string; graph: WorkflowGraph; pinned: WorkflowDefinitions; catalog: WorkflowDefinition[]; disabled: boolean;
  references: Record<string, ReusableReference>; draftPresets: Record<string, ReusableBody>;
  edit: (graph: WorkflowGraph, references?: Record<string, ReusableReference>) => void;
  openDraftSubflow: (key: string) => React.ReactNode;
}) {
  const client = useQueryClient();
  const [canvas, setCanvas] = useState<WorkflowCanvasEdit>({ layout: {}, updateDefinitions: {} });
  const published = graph.nodes.flatMap(({ id }) => {
    const reference = references[id];
    return reference?.source === "published" && (reference.version.kind === "preset" || reference.version.kind === "subflow")
      ? [{ id, version: reference.version }] : [];
  });
  const queries = useQueries({ queries: published.map(({ version }) => reusableNodeQuery(projectId, version)) });
  const reusable = new Map(published.flatMap(({ id }, index) => {
    const loaded = queries[index]?.data;
    return loaded ? [[id, loaded] as const] : [];
  }));
  const error = queries.find((query) => query.error)?.error;
  const discovered: WorkflowDefinitions = { ...pinned };
  const presets = new Map<string, { configuration: NonNullable<ReusableBody["configuration"]>; fields: string[] }>();
  const nodes = graph.nodes.map((node) => {
    const known = reusable.get(node.id);
    if (known) discovered[node.id] = known.definition;
    const reference = references[node.id];
    const local = reference?.source === "draft" ? draftPresets[reference.key] : undefined;
    const base = known?.artifact.body.kind === "preset" ? known.artifact.body : local;
    if (!base?.configuration) return node;
    const fields = known?.editableOverrides ?? reusableEditableFields(base.configuration)
      .filter((field) => base.editableFields?.includes(field));
    presets.set(node.id, { configuration: base.configuration, fields });
    const stored = node.configuration.kind === "preset_overrides" && node.configuration.fields &&
      typeof node.configuration.fields === "object" ? node.configuration.fields as Record<string, unknown> : {};
    return { ...node, configuration: { ...base.configuration, ...stored } };
  });
  const presented = { ...graph, nodes };
  const state = { ...canvas, graph: presented };
  const definitions = editorDefinitions(state, discovered, catalog);
  const update: Dispatch<SetStateAction<WorkflowEditorState>> = (change) => {
    const next = typeof change === "function" ? change(state) : change;
    setCanvas({ layout: next.layout, updateDefinitions: next.updateDefinitions });
    if (next.graph === state.graph) return;
    const before = new Map(graph.nodes.map((node) => [node.id, node]));
    const shown = new Map(nodes.map((node) => [node.id, node]));
    const updated = next.graph.nodes.map((node) => {
      const preset = presets.get(node.id);
      if (!preset) return node;
      if (shown.get(node.id)?.configuration === node.configuration) return before.get(node.id) ?? node;
      const fields = Object.fromEntries(preset.fields.filter((field) =>
        JSON.stringify(preset.configuration[field]) !== JSON.stringify(node.configuration[field]))
        .map((field) => [field, node.configuration[field]]));
      return { ...node, configuration: { kind: "preset_overrides", fields } };
    });
    const after = new Set(updated.map(({ id }) => id));
    const retained = Object.fromEntries(Object.entries(references).filter(([slot]) => !before.has(slot) || after.has(slot)));
    edit({ ...next.graph, nodes: updated }, retained);
  };
  function add({ artifact, definition, ...details }: ReusableDescription) {
    client.setQueryData(reusableNodeQuery(projectId, artifact.version).queryKey, { artifact, definition, ...details });
    const occupied = new Set(graph.nodes.map(({ id }) => id));
    const stem = `reusable-${definition.kind.replaceAll("_", "-")}`;
    let id = stem;
    for (let ordinal = 2; occupied.has(id); ordinal += 1) id = `${stem}-${ordinal}`;
    const configuration = artifact.version.kind === "preset" ? { kind: "preset_overrides", fields: {} } : { kind: "none" };
    edit({ ...graph, nodes: [...graph.nodes, { id, kind: definition.kind, configuration }] },
      { ...references, [id]: { source: "published", version: artifact.version } });
  }
  return <>
    <ReusableCatalog projectId={projectId} disabled={disabled} onAdd={add} />
    {error && <p role="alert">{String(error)}</p>}
    <WorkflowGraphEditor projectId={projectId} checkoutId={checkoutId} state={state} setState={update} definitions={definitions} catalog={catalog}
      problems={graphProblems(presented, definitions)} disabled={disabled} allowDefinitionUpdates={false}
      editablePresetFields={Object.fromEntries([...presets.entries()].map(([id, preset]) => [id, preset.fields]))}
      inspectReference={(id) => {
        const reference = references[id];
        if (reference?.source === "draft") return openDraftSubflow(reference.key);
        if (reference?.source !== "published") return null;
        if (reference.version.kind === "subflow") return <PublishedSubflow key={JSON.stringify(reference.version)}
          projectId={projectId} checkoutId={checkoutId} version={reference.version} />;
        if (reference.version.kind === "preset") return <p className="break-all">
          Pinned preset revision <code>{reference.version.revision}</code></p>;
        return null;
      }} />
  </>;
}

function JsonValue({ value }: { value: unknown }) {
  return <pre className={jsonClass}>{JSON.stringify(value, null, 2)}</pre>;
}

function MemberReview({ member }: { member: Member }) {
  const changes = [
    ["Graph changes", member.changes.filter(({ path }) => path.startsWith("/body/graph"))],
    ["Field changes", member.changes.filter(({ path }) => !path.startsWith("/body/graph"))],
  ] as const;
  return <div className="pipeline-stack">
    {member.base && <p className="break-all">Published base <code>{member.base.revision}</code></p>}
    {changes.map(([label, items]) => items.length > 0 && <details key={label}>
      <summary>{label} ({items.length})</summary>
      <ul className="pipeline-stack">{items.map(({ change, path, before, after }) => <li key={path} className="pipeline-evidence">
        <p className="break-all"><strong className="capitalize">{change}</strong> <code>{path}</code></p>
        {change !== "added" && <><p>Before</p><JsonValue value={before} /></>}
        {change !== "removed" && <><p>After</p><JsonValue value={after} /></>}
      </li>)}</ul>
    </details>)}
    {member.changes.length === 0 && <p>The selected artifact matches its published base.</p>}
    {member.changesTruncated && <p>The preview reached its field limit. Open the package document to review the remaining fields.</p>}
    {Object.keys(member.dependencies).length > 0 && <details><summary>Dependencies</summary>
      <ul>{Object.entries(member.dependencies).map(([slot, reference]) => <li key={slot} className="break-all">
        <code>{slot}</code> · {reference.source === "draft" ? `Draft ${reference.key}` : reference.source === "parameter"
          ? `${kindName(reference.kind)} binding ${reference.name}` : <>{kindName(reference.version.kind)} <code>{reference.version.id}</code> · <code>{reference.version.revision}</code></>}
      </li>)}</ul>
    </details>}
    {!!member.affectedUses.length && <details><summary>Affected published uses</summary><ul>
      {member.affectedUses.map(({ consumer, name, slot, pinnedRevision }) => <li key={`${consumer.destination}:${consumer.id}:${consumer.revision}:${slot}`} className="break-all">
        {name} · {slot} · consumer <code>{consumer.revision}</code> · dependency <code>{pinnedRevision}</code>
      </li>)}
    </ul></details>}
    {!!member.requirements.length && <section aria-label="Execution requirements"><h4 className="font-medium">Execution requirements</h4>
      <ul className="list-disc pl-4">{member.requirements.map((item) => <li key={item}>{item}</li>)}</ul>
    </section>}
    {!!member.diagnostics.length && <section aria-label="Validation diagnostics"><h4 className="font-medium">Validation diagnostics</h4>
      <ul className="list-disc pl-4">{member.diagnostics.map((item) => <li key={item}>{item}</li>)}</ul>
    </section>}
  </div>;
}

function Outputs({ values }: { values: TraceExports }) {
  return <ul aria-label="Forwarded outputs">{Object.entries(values).map(([port, { from, state }]) => <li key={port} className="break-all">
    {port} · {state} · <code>{from.node}.{from.port}</code>
  </li>)}</ul>;
}

function SimulationTrace({ trace }: { trace: Trace }) {
  const byNode = new Map(trace.steps.map((step) => [step.nodeId, step]));
  return <section className="pipeline-stack" aria-label="Simulation trace">
    <p className="break-all">Simulated revision <code>{trace.revision}</code></p>
    <details><summary>Sample inputs used</summary><JsonValue value={trace.inputs} /></details>
    {!!trace.subflows.length && <section aria-label="Simulated subflows" className="pipeline-stack">
      <h4 className="font-medium">Subflows</h4>
      {trace.subflows.map(({ path, name, revision, nodes, outputs }) => <details key={path}>
        <summary>{name} · <code>{path}</code></summary>
        <p className="break-all">Pinned revision <code>{revision}</code></p>
        <ul>{nodes.map((node) => <li key={node}><code>{node}</code> · {byNode.get(node)?.state}</li>)}</ul>
        {Object.keys(outputs).length > 0 && <Outputs values={outputs} />}
      </details>)}
    </section>}
    <ol className="pipeline-stack" aria-label="Simulated stages">{trace.steps.map(({ nodeId, title, state, inputs, outputs, reason }) => <li key={nodeId} className="pipeline-evidence">
      <details>
        <summary><strong>{title}</strong> · <code>{nodeId}</code> · <span className="capitalize">{state}</span></summary>
        {reason && <p>{reason}</p>}
        {!!inputs.length && <ul aria-label="Stage inputs">{inputs.map(({ port, from, state: signal }) => {
          const value = byNode.get(from.node)?.outputs[from.port];
          return <li key={`${port}:${from.node}:${from.port}`}>
            {port} · {signal} · <code>{from.node}.{from.port}</code>
            {value && value.portType.kind !== "control" && <details><summary>Show sample input</summary><JsonValue value={value.value} /></details>}
          </li>;
        })}</ul>}
        {!!Object.keys(outputs).length && <ul aria-label="Stage outputs">{Object.entries(outputs).map(([port, output]) => <li key={port}>
          {port} · {output.portType.kind === "data" ? kindName(output.portType.artifact) : output.portType.kind}
          {output.portType.kind !== "control" && <JsonValue value={output.value} />}
        </li>)}</ul>}
      </details>
    </li>)}</ol>
    {Object.keys(trace.outputs).length > 0 && <Outputs values={trace.outputs} />}
  </section>;
}

function parseObject(source: string, label: string) {
  const value: unknown = JSON.parse(source);
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error(`${label} needs a JSON object.`);
  return value;
}

function SimulateMember({ projectId, draftId, revision, member }: { projectId: string; draftId: string; revision: string; member: Member }) {
  const [inputs, setInputs] = useState(member.kind === "pipeline" ? '{\n  "document": {"relativePath": "sample.md"}\n}' : "{}");
  const [outcomes, setOutcomes] = useState("{}");
  const run = useMutation({ mutationFn: () => invoke<Trace>("simulate_workflow_package_revision", {
    projectId, draftId, revision, memberKey: member.key, scenario: {
      inputs: parseObject(inputs, "Sample inputs"), outcomes: parseObject(outcomes, "Sample outcomes"),
    },
  }) });
  const stale = run.data && (run.data.revision !== revision || run.data.memberKey !== member.key);
  return <section className="pipeline-stack" aria-label="Simulate selected artifact">
    <div className="grid gap-2 md:grid-cols-2">
      <label className="grid gap-1">Sample inputs<textarea rows={4} className="rounded border border-border bg-background p-2 font-mono text-xs" spellCheck={false}
        value={inputs} onChange={(event) => { run.reset(); setInputs(event.currentTarget.value); }} /></label>
      <label className="grid gap-1">Sample outcomes<textarea rows={4} className="rounded border border-border bg-background p-2 font-mono text-xs" spellCheck={false}
        value={outcomes} onChange={(event) => { run.reset(); setOutcomes(event.currentTarget.value); }} /></label>
    </div>
    <details><summary>Sample fields and outputs</summary>
      {member.sampleInputs.length > 0 && <p>Input names · {member.sampleInputs.join(" · ")}</p>}
      <p>Use the stage ID as the outcome key. Supply a completed, failed, or waiting state.</p>
      {member.sampleNodes.length > 0 && <JsonValue value={{ [member.sampleNodes[0]!.nodeId]: { state: "waiting", reason: "Waiting for sample evidence" } }} />}
      <ul>{member.sampleNodes.map(({ nodeId, title, outputs }) => <li key={nodeId} className="break-all">
        <code>{nodeId}</code> · {title}{outputs.length > 0 && ` · Completed outputs ${outputs.join(", ")}`}
      </li>)}</ul>
    </details>
    <Button type="button" variant="outline" className="w-fit" disabled={run.isPending} onClick={() => run.mutate()}>Simulate</Button>
    {run.isPending && <p role="status">Simulating saved revision…</p>}
    {run.error && <p role="alert">{String(run.error)}</p>}
    {run.data && !stale && <SimulationTrace trace={run.data} />}
  </section>;
}

export function WorkflowPackageReview({ projectId, draftId, savedRevision }: { projectId: string; draftId: string; savedRevision: string }) {
  const [open, setOpen] = useState(false);
  const [revision, setRevision] = useState(savedRevision);
  const [selection, select] = useState("");
  const preview = usePackagePreview(projectId, draftId, revision, open);
  const member = preview.data?.members.find(({ key }) => key === selection) ?? preview.data?.members[0];
  return <section className="pipeline-evidence pipeline-stack" aria-label="Review workflow package">
    <Button type="button" variant="outline" className="w-fit" aria-expanded={open} onClick={() => setOpen(!open)}>Review changes and simulate</Button>
    {open && <>
      <label className="grid gap-1">Review saved revision<select className="rounded border border-border bg-background p-2" value={revision}
        onChange={(event) => setRevision(event.currentTarget.value)}>
        {(preview.data?.revisions ?? [revision]).map((item) => <option key={item} value={item}>{item}{item === savedRevision && " · Current"}</option>)}
      </select></label>
      {preview.isPending && <p role="status">Loading revision preview…</p>}
      {preview.error && <p role="alert">{String(preview.error)}</p>}
      {preview.data?.members.length === 0 && <p>This saved revision has no package members.</p>}
      {member && <>
        <label className="grid gap-1">Review artifact<select className="rounded border border-border bg-background p-2" value={member.key}
          onChange={(event) => select(event.currentTarget.value)}>
          {preview.data?.members.map((item) => <option key={item.key} value={item.key}>{item.name} · {kindName(item.kind)}</option>)}
        </select></label>
        <MemberReview key={`${revision}:${member.key}`} member={member} />
        {member.simulatable && <SimulateMember key={`${revision}:${member.key}`} projectId={projectId} draftId={draftId} revision={revision} member={member} />}
      </>}
    </>}
  </section>;
}
