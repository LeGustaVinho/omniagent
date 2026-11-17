import { useState, type Dispatch, type SetStateAction } from "react";
import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { workflowGraphRegistryQuery } from "@/pipeline-catalog";
import { Button } from "@/components/ui/button";
import { WorkflowSimulationCanvas, type WorkflowSimulationGraph } from "@/workflow-simulation-canvas";
import { WorkflowStartValueField, initialWorkflowStartFields, initialWorkflowStartValue,
  type WorkflowStartSchema } from "@/workflow-launch-fields";
import { StructuredValueField } from "@/workflow-structured-fields";
import { WorkflowGraphEditor, editorDefinitions, type WorkflowEditorState } from "@/workflow-graph-editor";
import { graphProblems, type WorkflowCanvasEdit, type WorkflowDefinition, type WorkflowDefinitions,
  type WorkflowEndpoint, type WorkflowGraph, type WorkflowLayout, type WorkflowPortType } from "@/workflow-graph-model";
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
  sampleInputs: string[]; startSchema?: WorkflowStartSchema;
  sampleNodes: { nodeId: string; title: string; outputs: string[]; failurePorts?: string[] }[];
  graphDefinitions?: WorkflowDefinitions;
};
type Preview = { revision: string; revisions: string[]; members: Member[] };
type TraceState = "completed" | "failed" | "waiting" | "skipped";
type TraceSignal = { from: WorkflowEndpoint; state: "selected" | "waiting" | "skipped" };
type TraceExports = Record<string, TraceSignal>;
type Trace = {
  graph?: WorkflowSimulationGraph;
  revision: string; memberKey: string; inputs: Record<string, unknown>;
  steps: { nodeId: string; title: string; state: TraceState; reason: string | null;
    inputs: (TraceSignal & { port: string })[]; outputs: Record<string, { portType: WorkflowPortType; value: unknown }> }[];
  subflows: { path: string; name: string; revision: string; nodes: string[]; outputs: TraceExports }[];
  outputs: TraceExports;
};

const kindName = (kind: string) => kind.replaceAll("_", " ");
const jsonClass = "max-h-56 overflow-auto whitespace-pre-wrap break-all rounded bg-muted/50 p-2 text-xs";

export function usePackagePreview(projectId: string, draftId: string, revision: string, enabled: boolean) {
  return useQuery({
    queryKey: ["workflow-package-preview", projectId, draftId, revision], enabled,
    queryFn: () => invoke<Preview>("preview_workflow_package_revision", { projectId, draftId, revision }),
  });
}

type GraphMember = {
  key: string; name: string; graph: WorkflowGraph; kind?: string; references?: Record<string, ReusableReference>;
  inputs?: Record<string, WorkflowEndpoint>; outputs?: Record<string, WorkflowEndpoint>;
};

export function WorkflowPackageGraphEditing({ projectId, checkoutId, draftId, savedRevision, graphs, draftPresets = {}, disabled, edit,
  layouts = {}, canvasByMember, changeCanvas }: {
  projectId: string; checkoutId: string; draftId: string; savedRevision: string; graphs: GraphMember[]; disabled: boolean;
  draftPresets?: Record<string, ReusableBody>;
  layouts?: Record<string, WorkflowLayout>;
  canvasByMember: Record<string, WorkflowCanvasEdit>;
  changeCanvas: (key: string, canvas: WorkflowCanvasEdit) => void;
  edit: (key: string, graph: WorkflowGraph, references?: Record<string, ReusableReference>) => void;
}) {
  const [selection, select] = useState("");
  const graph = graphs.find(({ key }) => key === selection) ?? graphs[0];
  const show = Boolean(graph);
  const preview = usePackagePreview(projectId, draftId, savedRevision, show);
  const registry = useQuery({ ...workflowGraphRegistryQuery, enabled: show });
  if (!graph) return null;
  const pinned = preview.data?.members.find(({ key }) => key === graph.key)?.graphDefinitions ?? {};
  const error = preview.error ?? registry.error;
  return <section className="pipeline-stack" aria-label="Build workflow">
      {graphs.length > 1 && <label className="grid gap-1">Graph member<select className="rounded border border-border bg-background p-2" value={graph.key}
        onChange={(event) => select(event.currentTarget.value)}>
        {graphs.map((member) => <option key={member.key} value={member.key}>{member.name}</option>)}
      </select></label>}
      {error && <p role="alert">{String(error)}</p>}
      {show && (registry.isPending || preview.isPending) && <p role="status">Loading pipeline graph…</p>}
      {registry.data && preview.isSuccess && <PackageMemberGraph key={graph.key} projectId={projectId} checkoutId={checkoutId}
        graph={graph.graph} references={graph.references ?? {}} draftPresets={draftPresets}
        canvas={canvasByMember[graph.key] ?? { layout: layouts[graph.key] ?? {}, updateDefinitions: {} }}
        setCanvas={(canvas) => changeCanvas(graph.key, canvas)}
        pinned={pinned} catalog={registry.data} disabled={disabled} graphs={graphs} memberKey={graph.key} edit={(next, references) => edit(graph.key, next, references)}
        openDraftSubflow={(key) => {
          const child = graphs.find((member) => member.key === key && ["subflow", "pipeline"].includes(member.kind ?? ""));
          return child ? <Button type="button" variant="outline" onClick={() => select(child.key)}>
            {child.kind === "subflow" ? "Open draft subflow" : "Open draft child workflow"}
          </Button> : null;
        }} />}
  </section>;
}

function PackageMemberGraph({ projectId, checkoutId, graph, references, draftPresets, pinned, catalog, disabled, edit, openDraftSubflow, canvas, setCanvas, graphs, memberKey }: {
  projectId: string; checkoutId: string; graph: WorkflowGraph; pinned: WorkflowDefinitions; catalog: WorkflowDefinition[]; disabled: boolean;
  references: Record<string, ReusableReference>; draftPresets: Record<string, ReusableBody>;
  edit: (graph: WorkflowGraph, references?: Record<string, ReusableReference>) => void;
  openDraftSubflow: (key: string) => React.ReactNode;
  canvas: WorkflowCanvasEdit;
  setCanvas: (canvas: WorkflowCanvasEdit) => void;
  graphs: GraphMember[];
  memberKey: string;
}) {
  const client = useQueryClient();
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
  const bindings = useQuery({
    queryKey: ["workflow-package-catalog", projectId],
    queryFn: () => invoke<{ version: Version & { destination: "project" | "library" }; name: string }[]>("workflow_package_catalog", { projectId }),
    enabled: graph.nodes.some(({ kind, configuration }) => ["subflow", "repeat_until", "for_each", "run_backlog"].includes(kind)
      || configuration.kind === "agent_role"),
  });
  const error = queries.find((query) => query.error)?.error;
  const discovered: WorkflowDefinitions = { ...pinned };
  const presets = new Map<string, { configuration: NonNullable<ReusableBody["configuration"]>; fields: string[] }>();
  const nodes = graph.nodes.map((node) => {
    const known = reusable.get(node.id);
    if (known?.definition.kind === node.kind) discovered[node.id] = known.definition;
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
  function subflowDefinition(id: string): WorkflowDefinition | undefined {
    const reference = references[id];
    if (reference?.source === "published" && reference.version.kind === "subflow") return reusable.get(id)?.definition;
    if (reference?.source !== "draft") return undefined;
    const child = graphs.find(({ key, kind }) => key === reference.key && kind === "subflow");
    const base = catalog.find(({ kind }) => kind === "subflow");
    if (!child || !base) return undefined;
    const childDefinitions = editorDefinitions({ graph: child.graph, layout: {}, updateDefinitions: {} }, {}, catalog);
    const ports = (bindings: Record<string, WorkflowEndpoint> | undefined, direction: "inputs" | "outputs") =>
      Object.entries(bindings ?? {}).flatMap(([name, { node, port }]) => {
        const value = childDefinitions[node]?.[direction].find((candidate) => candidate.name === port)?.value;
        return value ? [{ name, value }] : [];
      });
    // The public subflow input points at a Start output.
    return { ...base, title: child.name, inputs: ports(child.inputs, "outputs"), outputs: ports(child.outputs, "outputs") };
  }
  const definitions = editorDefinitions(state, discovered, catalog, subflowDefinition);
  const backlogBound = (id: string) => graph.nodes.some((node) => node.id === id && node.kind === "run_backlog") && Boolean(references[id]);
  function referencePicker(id: string) {
    const node = graph.nodes.find((candidate) => candidate.id === id);
    if (!node) return null;
    const kind = ["subflow", "repeat_until", "for_each"].includes(node.kind) ? "subflow"
      : node.kind === "run_backlog" ? "pipeline" : node.configuration.kind === "agent_role" ? "role" : null;
    if (!kind) return null;
    const reference = references[id];
    const choices: { label: string; reference: ReusableReference }[] = (bindings.data ?? [])
      .filter(({ version }) => version.kind === kind && (kind !== "pipeline" || version.destination === "project"))
      .map(({ version, name }) => ({ label: `${name} · ${version.destination} · ${version.revision.slice(0, 10)}`,
        reference: { source: "published", version } }));
    if (kind !== "role") choices.unshift(...graphs.filter((child) => child.key !== memberKey && child.kind === kind)
      .map(({ key, name }) => ({ label: `${name} · This draft`, reference: { source: "draft" as const, key } })));
    if (reference && !choices.some((option) => JSON.stringify(option.reference) === JSON.stringify(reference))) {
      choices.unshift({ label: "Current pinned reference", reference });
    }
    return <label>{kind === "subflow" ? "Subflow revision" : kind === "pipeline" ? "Backlog child revision" : "Role revision"}
      <select disabled={disabled} value={reference ? JSON.stringify(reference) : ""} onChange={(event) => {
        const match = choices.find((choice) => JSON.stringify(choice.reference) === event.currentTarget.value);
        const next = { ...references };
        if (match) next[id] = match.reference;
        else delete next[id];
        edit(graph, next);
      }}>
        <option value="">Select {kind}</option>
        {choices.map((choice) => <option key={JSON.stringify(choice.reference)} value={JSON.stringify(choice.reference)}>{choice.label}</option>)}
      </select></label>;
  }
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
      problems={graphProblems(presented, definitions, backlogBound)} disabled={disabled} backlogBound={backlogBound}
      allowDefinitionUpdates={(id) => !references[id] || graph.nodes.find((node) => node.id === id)?.configuration.kind === "agent_role"}
      editablePresetFields={Object.fromEntries([...presets.entries()].map(([id, preset]) => [id, preset.fields]))}
      goalRoleForNode={(id) => {
        const reference = references[id];
        if (reference?.source === "published" && reference.version.kind === "role") return { version: reference.version };
        if ((reference?.source === "draft" && !Object.hasOwn(draftPresets, reference.key)) ||
          (reference?.source === "parameter" && reference.kind === "role")) return { pending: true };
        return undefined;
      }}
      historyScope={JSON.stringify(references)} subflowDefinitionForNode={subflowDefinition}
      inspectReference={(id) => {
        const reference = references[id];
        return <>
          {referencePicker(id)}
          {reference?.source === "draft" && openDraftSubflow(reference.key)}
          {reference?.source === "published" && reference.version.kind === "subflow" &&
            <PublishedSubflow key={JSON.stringify(reference.version)} projectId={projectId} checkoutId={checkoutId} version={reference.version} />}
          {reference?.source === "published" && reference.version.kind === "preset" &&
            <p className="break-all">Pinned preset revision <code>{reference.version.revision}</code></p>}
        </>;
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

export function PackagePublicationPreview({ preview }: { preview: Preview }) {
  return <ul className="pipeline-stack" aria-label="Reviewed package changes">
    {preview.members.map((member) => <li key={member.key} className="pipeline-evidence pipeline-stack">
      <h3 className="font-semibold">{member.name}</h3>
      <MemberReview member={member} />
    </li>)}
  </ul>;
}

function Outputs({ values }: { values: TraceExports }) {
  return <ul aria-label="Forwarded outputs">{Object.entries(values).map(([port, { from, state }]) => <li key={port} className="break-all">
    {port} · {state} · <code>{from.node}.{from.port}</code>
  </li>)}</ul>;
}

function SimulationTrace({ trace }: { trace: Trace }) {
  const byNode = new Map(trace.steps.map((step) => [step.nodeId, step]));
  function renderStep({ inputs, outputs, reason }: Trace["steps"][number]) {
    return <div className="pipeline-stack">
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
    </div>;
  }
  const ledger = <ol className="pipeline-stack" aria-label="Simulated stages">{trace.steps.map((step) => <li key={step.nodeId} className="pipeline-evidence">
    <details>
      <summary><strong>{step.title}</strong> · <code>{step.nodeId}</code> · <span className="capitalize">{step.state}</span></summary>
      {renderStep(step)}
    </details>
  </li>)}</ol>;
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
    {trace.graph && <WorkflowSimulationCanvas graph={trace.graph} steps={trace.steps} renderStep={(nodeId) => {
      const step = byNode.get(nodeId);
      return step && renderStep(step);
    }} />}
    {trace.graph ? <details><summary>All simulated stages</summary>{ledger}</details> : ledger}
    {Object.keys(trace.outputs).length > 0 && <Outputs values={trace.outputs} />}
  </section>;
}

type SampleOutcome = {
  state: "completed" | "failed" | "waiting";
  outputs?: Record<string, unknown>;
  reason?: string;
  failurePort?: string;
};

function SimulateMember({ projectId, draftId, revision, member }: { projectId: string; draftId: string; revision: string; member: Member }) {
  const [inputs, setInputs] = useState<Record<string, unknown>>(() => Object.fromEntries(member.sampleInputs.map((name) =>
    [name, name === "document" && member.kind === "pipeline" ? { relativePath: "sample.md" }
      : name === "inputs" && member.startSchema ? initialWorkflowStartFields(member.startSchema)
      : name === "inputs" ? {} : ""])));
  const [outcomes, setOutcomes] = useState<Record<string, SampleOutcome>>({});
  const [source, setSource] = useState<"prompt" | "markdown">("prompt");
  const [sampleRequest, setSampleRequest] = useState("");
  const values = (inputs.inputs as Record<string, unknown> | undefined) ?? {};
  const requestMode = member.startSchema?.promptOrMarkdown === true;
  const run = useMutation({ mutationFn: () => invoke<Trace>("simulate_workflow_package_revision", {
    projectId, draftId, revision, memberKey: member.key, scenario: {
      inputs: requestMode ? {
        ...inputs,
        ...(source === "prompt"
          ? { inputs: { ...values, prompt: sampleRequest } }
          : { document: sampleRequest }),
      } : inputs,
      outcomes,
    },
  }) });
  const stale = run.data && (run.data.revision !== revision || run.data.memberKey !== member.key);
  const missingStartField = member.startSchema?.fields.some(({ name, required }) => required && !Object.hasOwn(values, name));
  function setStartValue(name: string, value: unknown) {
    const updated = { ...values };
    if (value === undefined) delete updated[name];
    else updated[name] = value;
    setInput("inputs", updated);
  }
  function setInput(name: string, value: unknown) {
    run.reset();
    setInputs((previous) => ({ ...previous, [name]: value }));
  }
  function setOutcome(id: string, outcome?: SampleOutcome) {
    run.reset();
    setOutcomes((previous) => {
      if (outcome) return { ...previous, [id]: outcome };
      const next = { ...previous };
      delete next[id];
      return next;
    });
  }
  return <section className="pipeline-stack" aria-label="Simulate selected artifact">
    {(member.sampleInputs.length > 0 || requestMode) && <fieldset className="pipeline-form grid gap-2" disabled={run.isPending}>
      <legend>Sample inputs</legend>
      {requestMode && <>
        <label>Sample request source<select value={source} onChange={(event) => {
          setSource(event.currentTarget.value === "markdown" ? "markdown" : "prompt");
          run.reset();
        }}>
          <option value="prompt">Prompt</option>
          <option value="markdown">Markdown</option>
        </select></label>
        <label>{source === "prompt" ? "Sample prompt" : "Sample Markdown"}<textarea value={sampleRequest}
          onChange={(event) => { setSampleRequest(event.currentTarget.value); run.reset(); }} /></label>
      </>}
      {member.sampleInputs.map((name) => name === "inputs" && member.startSchema
        ? member.startSchema.fields.map((field) => <div key={field.name} className="pipeline-stack">
          {!field.required && <label className="pipeline-inline-field">
            <input type="checkbox" checked={Object.hasOwn(values, field.name)} onChange={(event) =>
              setStartValue(field.name, event.currentTarget.checked ? initialWorkflowStartValue(field.valueType) : undefined)} />
            Include {field.name}
          </label>}
          {(field.required || Object.hasOwn(values, field.name)) && <WorkflowStartValueField field={field}
            value={values[field.name]} disabled={run.isPending} onChange={(value) => setStartValue(field.name, value)} />}
        </div>)
        : name === "document" && member.kind === "pipeline"
        ? <label key={name}>Sample Markdown path<input value={(inputs.document as { relativePath: string }).relativePath}
          onChange={(event) => setInput(name, { relativePath: event.currentTarget.value })} /></label>
        : <StructuredValueField key={name} label={name} value={inputs[name]} disabled={run.isPending}
          onChange={(value) => setInput(name, value)} />)}
    </fieldset>}
    {member.sampleNodes.length > 0 && <details className="pipeline-evidence"><summary>Sample outcomes</summary>
      <div className="pipeline-stack pt-2">
        {member.sampleNodes.map(({ nodeId, title, outputs, failurePorts = [] }) => {
          const outcome = outcomes[nodeId];
          return <fieldset key={nodeId} className="pipeline-form grid gap-2" disabled={run.isPending}>
            <legend>{title} · {nodeId}</legend>
            <label>Outcome for {nodeId}<select value={outcome?.state ?? ""}
              onChange={(event) => {
                const state = event.currentTarget.value as "" | SampleOutcome["state"];
                setOutcome(nodeId, state === "" ? undefined : { state, ...(state === "completed" && {
                  outputs: Object.fromEntries(outputs.map((name) => [name, ""])),
                }) });
              }}>
              <option value="">Use default wait</option>
              <option value="completed">Completed</option>
              <option value="failed">Failed</option>
              <option value="waiting">Waiting</option>
            </select></label>
            {outcome?.state === "failed" && failurePorts.length > 1 && <label>Failure branch
              <select value={outcome.failurePort ?? "failed"} onChange={(event) =>
                setOutcome(nodeId, { ...outcome, failurePort: event.currentTarget.value })}>
                {failurePorts.map((port) => <option key={port} value={port}>{kindName(port)}</option>)}
              </select>
            </label>}
            {outcome && <label>Reason for {nodeId}<input value={outcome.reason ?? ""}
              onChange={(event) => setOutcome(nodeId, { ...outcome, reason: event.currentTarget.value })} /></label>}
            {outcome?.state === "completed" && outputs.map((name) =>
              <StructuredValueField key={name} label={name} value={outcome.outputs?.[name]} disabled={run.isPending}
                onChange={(value) => setOutcome(nodeId, { ...outcome, outputs: { ...outcome.outputs, [name]: value } })} />)}
          </fieldset>;
        })}
      </div>
    </details>}
    <Button type="button" variant="outline" className="w-fit"
      disabled={run.isPending || missingStartField || (requestMode && !sampleRequest.trim())}
      onClick={() => run.mutate()}>Simulate</Button>
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
        <MemberReview member={member} />
        {member.simulatable && <SimulateMember key={`${revision}:${member.key}`} projectId={projectId} draftId={draftId} revision={revision} member={member} />}
      </>}
    </>}
  </section>;
}
