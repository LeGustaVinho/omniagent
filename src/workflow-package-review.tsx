import { useState, type Dispatch, type ReactNode, type SetStateAction } from "react";
import type { Viewport } from "@xyflow/react";
import { FileDiff, FlaskConical, Workflow } from "lucide-react";
import { useMutation, useQueries, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { workflowNodeDefinitionsQuery } from "@/workflow-definition-query";
import { Button } from "@/components/ui/button";
import { WorkflowSimulationCanvas, type WorkflowSimulationGraph } from "@/workflow-simulation-canvas";
import { WorkflowStartValueField, initialWorkflowStartFields, initialWorkflowStartValue,
  type WorkflowStartSchema } from "@/workflow-launch-fields";
import { StructuredValueField } from "@/workflow-structured-fields";
import { WorkflowGraphEditor, editorDefinitions, type WorkflowEditorState } from "@/workflow-graph-editor";
import { WorkflowBuilderDialog } from "@/workflow-builder-dialog";
import { PromptInspector, PromptRevisionView } from "@/workflow-prompt";
import { PromptContractSuggestion } from "@/workflow-prompt-suggestion";
import { PROMPT_EXECUTORS, promptNodeDefinition, promptReferenceValue, type PromptMember, type PromptPortRename, type PromptRoleChoice,
  initialPromptSample, type PromptSample } from "@/workflow-prompt-model";
import type { PackageDraft } from "@/workflow-authoring-package";
import { graphProblems, type WorkflowCanvasEdit, type WorkflowDefinition, type WorkflowDefinitions,
  type WorkflowEndpoint, type WorkflowGraph, type WorkflowLayout, type WorkflowPortType, type WorkflowReuseSelection } from "@/workflow-graph-model";
import { PublishedSubflow, reusableEditableFields, reusableNodeQuery, WorkflowReusableDerivationDialog,
  type ReusableBody, type ReusableReference } from "@/workflow-reusable";

type Version = { id: string; revision: string; destination: "project" | "library"; kind: string };
type Reference = { source: "draft"; key: string } | { source: "published"; version: Version }
  | { source: "parameter"; name: string; kind: string };
type Member = {
  key: string; name: string; kind: string; base: Version | null;
  changes: { path: string; change: "added" | "removed" | "changed"; before: unknown; after: unknown }[];
  changesTruncated: boolean; dependencies: Record<string, Reference>;
  affectedUses: { consumer: Version; name: string; slot: string; pinnedRevision: string }[];
  diagnostics: string[]; requirements: string[]; simulatable: boolean;
  sampleInputs: string[]; startSchema?: WorkflowStartSchema;
  sampleNodes: { nodeId: string; title: string; outputs: string[]; failurePorts?: string[]; prompt?: PromptSample }[];
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
  layouts = {}, canvasByMember, changeCanvas, deriveDisabled, onDerived, selection, select, viewports,
  promptMembers = [], draftRoles = [], editPrompt }: {
  projectId: string; checkoutId: string; draftId: string; savedRevision: string; graphs: GraphMember[]; disabled: boolean;
  draftPresets?: Record<string, ReusableBody>;
  layouts?: Record<string, WorkflowLayout>;
  canvasByMember: Record<string, WorkflowCanvasEdit>;
  changeCanvas: (key: string, canvas: WorkflowCanvasEdit) => void;
  edit: (key: string, graph: WorkflowGraph, references?: Record<string, ReusableReference>) => void;
  promptMembers?: PromptMember[];
  draftRoles?: PromptRoleChoice[];
  editPrompt?: (prompt: PromptMember, rename?: PromptPortRename) => void;
  deriveDisabled: boolean;
  onDerived: (draft: PackageDraft) => void;
  selection: string;
  select: (key: string) => void;
  viewports: Map<string, Viewport>;
}) {
  const [derivation, setDerivation] = useState<WorkflowReuseSelection>();
  const prompt = promptMembers.find(({ key }) => key === selection) ?? (!graphs.length ? promptMembers[0] : undefined);
  const graph = graphs.find(({ key }) => key === selection) ?? graphs[0];
  const show = Boolean(prompt || graph);
  const preview = usePackagePreview(projectId, draftId, savedRevision, show);
  const nodeKinds = graphs.flatMap(({ graph }) => graph.nodes.map(({ kind }) => kind));
  if (promptMembers.length || nodeKinds.includes("subflow")) {
    nodeKinds.push("subflow", ...Object.keys(PROMPT_EXECUTORS));
  }
  const registry = useQuery(workflowNodeDefinitionsQuery(nodeKinds));
  const roleCatalog = useQuery({
    queryKey: ["workflow-package-catalog", projectId], enabled: Boolean(prompt),
    queryFn: () => invoke<{ name: string; version: Version }[]>("workflow_package_catalog", { projectId }),
  });
  if (!show) return <div className="workflow-builder-empty"><Workflow size={24} aria-hidden />
    <p>This draft has no editable graph. Open Draft artifacts to add or edit a graph.</p>
  </div>;
  const authoredPromptKeys = new Set(graphs.flatMap((member) => Object.values(member.references ?? {}).flatMap((ref) =>
    ref.source === "draft" ? [ref.key] : [])));
  const options = [...graphs, ...promptMembers.filter(({ key, destination }) =>
    key === prompt?.key || destination === "library" || !authoredPromptKeys.has(key))];
  const selectedMember = prompt ?? graph;
  const memberPicker = options.length > 1 && selectedMember ? <label className="pipeline-inline-field workflow-builder-member">
    <span>Graph member</span>
    <select aria-label="Graph member" value={selectedMember.key} onChange={(event) => select(event.currentTarget.value)} title={selectedMember.name}>
      {options.map((member) => <option key={member.key} value={member.key}>{member.name}</option>)}
    </select>
  </label> : undefined;
  const pinned = preview.data?.members.find(({ key }) => key === graph?.key)?.graphDefinitions ?? {};
  const error = preview.error ?? registry.error ?? (prompt ? roleCatalog.error : null);
  return <section className="workflow-builder-graph" aria-label="Build workflow">
      {error && <p role="alert">{String(error)}</p>}
      {show && (registry.isPending || preview.isPending) && <p role="status">Loading pipeline graph…</p>}
      {prompt && registry.data && preview.isSuccess && editPrompt && <PromptRevisionView>
        {memberPicker}
        <PromptInspector key={prompt.key} member={prompt} choices={promptRoles(draftRoles, roleCatalog.data)}
          catalog={registry.data} disabled={disabled} onChange={editPrompt}
          technical={preview.data.members.find(({ key }) => key === prompt.key)?.graphDefinitions}
          suggestion={(onApply) => <PromptContractSuggestion projectId={projectId} checkoutId={checkoutId}
            prompt={prompt.body.prompt} cardFingerprint={JSON.stringify(prompt)} disabled={disabled} onApply={onApply} />} />
      </PromptRevisionView>}
      {!prompt && graph && registry.data && preview.isSuccess && <PackageMemberGraph key={graph.key} projectId={projectId} checkoutId={checkoutId}
        graph={graph.graph} references={graph.references ?? {}} draftPresets={draftPresets}
        canvas={canvasByMember[graph.key] ?? { layout: layouts[graph.key] ?? {}, updateDefinitions: {} }}
        setCanvas={(canvas) => changeCanvas(graph.key, canvas)}
        pinned={pinned} catalog={registry.data} disabled={disabled} graphs={graphs} memberKey={graph.key}
        edit={(next, references) => edit(graph.key, next, references)}
        promptMembers={promptMembers} draftRoles={draftRoles} editPrompt={editPrompt}
        previewMembers={preview.data.members}
        onDerive={setDerivation} deriveDisabled={deriveDisabled} toolbarStart={memberPicker}
        initialViewport={viewports.get(graph.key)} onViewportChange={(viewport) => viewports.set(graph.key, viewport)}
        openDraftSubflow={(key) => {
          const child = graphs.find((member) => member.key === key && ["subflow", "pipeline"].includes(member.kind ?? ""));
          return child ? <Button type="button" variant="outline" onClick={() => select(child.key)}>
            {child.kind === "subflow" ? "Open draft subflow" : "Open draft child workflow"}
          </Button> : null;
        }} />}
      {derivation && graph && !prompt && <WorkflowReusableDerivationDialog projectId={projectId} graph={graph.graph}
        source={{ type: "package_revision", draftId, revision: savedRevision, memberKey: graph.key }} selection={derivation}
        disabled={deriveDisabled} onClose={() => setDerivation(undefined)} onSaved={onDerived} />}
  </section>;
}

function promptRoles(drafts: PromptRoleChoice[], published?: { name: string; version: Version }[]): PromptRoleChoice[] {
  return [...drafts, ...(published ?? []).filter(({ version }) => version.kind === "role").map(({ name, version }) => ({
    name: name + " · " + version.revision.slice(0, 10), reference: { source: "published" as const, version },
  }))];
}

function PackageMemberGraph({ projectId, checkoutId, graph, references, draftPresets, pinned, catalog, disabled, edit, openDraftSubflow,
  canvas, setCanvas, graphs, memberKey, onDerive, deriveDisabled, toolbarStart, initialViewport, onViewportChange,
  promptMembers, draftRoles, editPrompt, previewMembers }: {
  projectId: string; checkoutId: string; graph: WorkflowGraph; pinned: WorkflowDefinitions; catalog: WorkflowDefinition[]; disabled: boolean;
  references: Record<string, ReusableReference>; draftPresets: Record<string, ReusableBody>;
  edit: (graph: WorkflowGraph, references?: Record<string, ReusableReference>) => void;
  promptMembers: PromptMember[];
  draftRoles: PromptRoleChoice[];
  editPrompt?: (prompt: PromptMember, rename?: PromptPortRename) => void;
  previewMembers: Member[];
  openDraftSubflow: (key: string) => React.ReactNode;
  canvas: WorkflowCanvasEdit;
  setCanvas: (canvas: WorkflowCanvasEdit) => void;
  graphs: GraphMember[];
  memberKey: string;
  onDerive: (selection: WorkflowReuseSelection) => void;
  deriveDisabled: boolean;
  toolbarStart?: ReactNode;
  initialViewport?: Viewport;
  onViewportChange: (viewport: Viewport) => void;
}) {
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
  const choices = promptRoles(draftRoles, bindings.data);
  const promptsByNode = new Map(graph.nodes.flatMap(({ id, kind }) => {
    if (kind !== "subflow") return [];
    const ref = references[id];
    if (ref?.source === "draft") {
      const prompt = promptMembers.find(({ key }) => key === ref.key);
      return prompt ? [[id, prompt] as const] : [];
    }
    const artifact = reusable.get(id)?.artifact;
    const prompt = artifact?.body.prompt;
    if (ref?.source !== "published" || artifact?.body.kind !== "prompt" || !prompt) return [];
    const member: PromptMember = { key: artifact.version.id, name: artifact.name, destination: artifact.version.destination,
      body: { kind: "prompt", prompt }, references: artifact.references };
    return [[id, member] as const];
  }));
  const discovered: WorkflowDefinitions = { ...pinned };
  const nodeBindings: NonNullable<WorkflowEditorState["nodeBindings"]> = Object.create(null);
  const nodes = graph.nodes.map((node) => {
    const known = reusable.get(node.id);
    if (known?.definition.kind === node.kind) discovered[node.id] = known.definition;
    const reference = Object.hasOwn(references, node.id) ? references[node.id] : undefined;
    const local = reference?.source === "draft" ? draftPresets[reference.key] : undefined;
    const base = known?.artifact.body.kind === "preset" ? known.artifact.body : local;
    if (!reference) return node;
    if (!base?.configuration) {
      nodeBindings[node.id] = { reference };
      return node;
    }
    const fields = known?.editableOverrides ?? reusableEditableFields(base.configuration)
      .filter((field) => base.editableFields?.includes(field));
    const stored = node.configuration.kind === "preset_overrides" && node.configuration.fields &&
      typeof node.configuration.fields === "object" ? node.configuration.fields as Record<string, unknown> : {};
    const shown = { ...base.configuration, ...stored };
    nodeBindings[node.id] = { reference, preset: { base: base.configuration, fields, stored: node.configuration, shown } };
    return { ...node, configuration: shown };
  });
  const presented = { ...graph, nodes };
  const state: WorkflowEditorState = { ...canvas, graph: presented, nodeBindings };
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
  for (const [id, member] of promptsByNode) {
    const base = definitions[id];
    if (base) definitions[id] = promptNodeDefinition(member.name, member.body.prompt, base);
  }
  const rolesByNode = Object.fromEntries([...promptsByNode.entries()].map(([id, member]) => {
    const role = member.references?.role;
    return [id, choices.find(({ reference }) => promptReferenceValue(reference) === promptReferenceValue(role))?.name
      ?? (role ? "Pinned Role" : "Choose a Role")];
  }));
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
    setCanvas({ layout: next.layout, updateDefinitions: next.updateDefinitions,
      ...(next.definitionCopies && { definitionCopies: next.definitionCopies }) });
    if (next.graph === state.graph) return;
    const before = new Set(graph.nodes.map(({ id }) => id));
    const retained = new Map(Object.entries(references).filter(([slot]) => !before.has(slot)
      || (next.nodeBindings && Object.hasOwn(next.nodeBindings, slot))));
    const updated = next.graph.nodes.map((node) => {
      const binding = next.nodeBindings && Object.hasOwn(next.nodeBindings, node.id) ? next.nodeBindings[node.id] : undefined;
      if (binding) retained.set(node.id, binding.reference);
      const preset = binding?.preset;
      if (!preset) return node;
      if (preset.shown === node.configuration) return { ...node, configuration: preset.stored };
      const fields = Object.fromEntries(preset.fields.filter((field) =>
        JSON.stringify(preset.base[field]) !== JSON.stringify(node.configuration[field]))
        .map((field) => [field, node.configuration[field]]));
      return { ...node, configuration: { kind: "preset_overrides", fields } };
    });
    edit({ ...next.graph, nodes: updated }, Object.fromEntries(retained));
  };
  return <>
    {error && <p role="alert">{String(error)}</p>}
    <WorkflowGraphEditor projectId={projectId} checkoutId={checkoutId} state={state} setState={update} definitions={definitions} catalog={catalog}
      fullHeight toolbarStart={toolbarStart} initialViewport={initialViewport} onViewportChange={onViewportChange}
      promptRoles={rolesByNode}
      inspectPrompt={(id) => {
        const member = promptsByNode.get(id);
        if (!member) return null;
        const editable = references[id]?.source === "draft" && !!editPrompt;
        return <PromptInspector key={id} member={member} choices={choices} catalog={catalog} disabled={disabled || !editable}
          onChange={(next, rename) => editPrompt?.(next, rename)}
          technical={editable ? previewMembers.find(({ key }) => key === member.key)?.graphDefinitions : reusable.get(id)?.graphDefinitions}
          suggestion={(onApply) => <PromptContractSuggestion projectId={projectId} checkoutId={checkoutId}
            prompt={member.body.prompt} cardFingerprint={JSON.stringify(member)} disabled={disabled} onApply={onApply} />} />;
      }}
      problems={graphProblems(presented, definitions, backlogBound)} disabled={disabled} backlogBound={backlogBound}
      onDerive={onDerive} deriveDisabled={deriveDisabled}
      allowDefinitionUpdates={(id) => !promptsByNode.has(id) &&
        (!references[id] || graph.nodes.find((node) => node.id === id)?.configuration.kind === "agent_role")}
      editablePresetFields={Object.fromEntries(Object.entries(nodeBindings).flatMap(([id, { preset }]) => preset ? [[id, preset.fields]] : []))}
      goalRoleForNode={(id) => {
        const reference = references[id];
        if (reference?.source === "published" && reference.version.kind === "role") return { version: reference.version };
        if ((reference?.source === "draft" && !Object.hasOwn(draftPresets, reference.key)) ||
          (reference?.source === "parameter" && reference.kind === "role")) return { pending: true };
        return undefined;
      }}
      subflowDefinitionForNode={subflowDefinition}
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
    {!!trace.subflows.length && <details aria-label="Simulated subflows" className="workflow-trace-subflows">
      <summary>Simulated subflows ({trace.subflows.length})</summary>
      <div className="pipeline-stack">
      {trace.subflows.map(({ path, name, revision, nodes, outputs }) => <details key={path}>
        <summary>{name} · <code>{path}</code></summary>
        <p className="break-all">Pinned revision <code>{revision}</code></p>
        <ul>{nodes.map((node) => <li key={node}><code>{node}</code> · {byNode.get(node)?.state}</li>)}</ul>
        {Object.keys(outputs).length > 0 && <Outputs values={outputs} />}
      </details>)}
      </div>
    </details>}
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

type PromptSampleResponse = {
  route: string;
  reason: string;
  outputs: Record<string, unknown>;
  delegations: { role: string; focus: string }[];
};

function PromptSampleForm({ nodeId, prompt, outcome, disabled, change }: {
  nodeId: string; prompt: PromptSample; outcome?: SampleOutcome; disabled: boolean; change: (outcome?: SampleOutcome) => void;
}) {
  const response = outcome?.outputs?.response as PromptSampleResponse | undefined;
  function update(next: PromptSampleResponse) {
    change({ state: "completed", outputs: { response: next } });
  }
  return <fieldset className="pipeline-form pipeline-stack" disabled={disabled}>
    <legend>{prompt.title}</legend>
    <label>Sample route
      <select aria-label={`Sample route for ${nodeId}`} value={outcome?.state === "failed" ? "falhou" : response?.route ?? ""}
        onChange={(event) => {
          const route = event.currentTarget.value;
          if (!route) change();
          else if (route === "falhou") change({ state: "failed", reason: "Sample Prompt failure" });
          else update(response ? { ...response, route } : {
            route, reason: "Sample route selection",
            outputs: Object.fromEntries(Object.entries(prompt.outputs).map(([name, kind]) => [name, initialPromptSample(kind)])),
            delegations: [],
          });
        }}>
        <option value="">Use default wait</option>
        {prompt.routes.map((route) => <option key={route} value={route}>{route === "concluido" ? "Completed" : kindName(route)}</option>)}
        <option value="falhou">Failed</option>
      </select>
    </label>
    {response && <>
      <label>Justification<input value={response.reason} onChange={(event) => update({ ...response, reason: event.currentTarget.value })} /></label>
      {prompt.delegates.length > 0 && <h4>Invited Roles</h4>}
      {prompt.delegates.map(({ alias, required }) => {
        const call = response.delegations.find(({ role }) => role === alias);
        const drop = () => response.delegations.filter(({ role }) => role !== alias);
        return <div key={alias} className="pipeline-stack">
          {required ? <p>{alias} · Required</p> : <label className="workflow-check">
            <input type="checkbox" checked={!!call} onChange={(event) => update({ ...response, delegations: event.currentTarget.checked
              ? [...response.delegations, { role: alias, focus: "Review the saved subject." }] : drop() })} /> Call {alias}
          </label>}
          {(required || call) && <label>Focus for {alias}<input value={call?.focus ?? ""} placeholder={required ? "Use the Role's required focus" : ""}
            onChange={(event) => {
              const focus = event.currentTarget.value;
              update({ ...response, delegations: required && !focus.trim() ? drop() : [...drop(), { role: alias, focus }] });
            }} /></label>}
        </div>;
      })}
      {Object.keys(prompt.outputs).map((name) => <StructuredValueField key={name} label={name} value={response.outputs[name]} disabled={disabled}
        onChange={(value) => update({ ...response, outputs: { ...response.outputs, [name]: value } })} />)}
    </>}
    {outcome?.state === "failed" && <label>Failure diagnosis<input value={outcome.reason ?? ""}
      onChange={(event) => change({ ...outcome, reason: event.currentTarget.value })} /></label>}
  </fieldset>;
}

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
  const promptSamples = member.sampleNodes.filter(({ prompt }) => prompt);
  const nativeSamples = member.sampleNodes.filter(({ prompt }) => !prompt);
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
  return <section className="pipeline-stack workflow-review-card" aria-label="Simulate selected artifact">
    <h3><FlaskConical size={16} aria-hidden /> Simulation</h3>
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
    {promptSamples.length > 0 && <details className="pipeline-evidence"><summary>Prompt responses</summary>
      <div className="pipeline-stack pt-2">
        {promptSamples.map(({ nodeId, prompt }) => prompt && <PromptSampleForm key={nodeId} nodeId={nodeId} prompt={prompt}
          outcome={outcomes[nodeId]} disabled={run.isPending} change={(outcome) => setOutcome(nodeId, outcome)} />)}
      </div>
    </details>}
    {nativeSamples.length > 0 && <details className="pipeline-evidence"><summary>Sample outcomes</summary>
      <div className="pipeline-stack pt-2">
        {nativeSamples.map(({ nodeId, title, outputs, failurePorts = [] }) => {
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
  return <WorkflowBuilderDialog title="Review workflow package" description="Inspect a saved revision and try its stages with sample inputs."
    wide open={open} onOpenChange={setOpen} trigger={<Button type="button" variant="outline">
      <FlaskConical size={15} aria-hidden /> Review changes and simulate
    </Button>}>
    {open && <section className="pipeline-stack pipeline-form" aria-label="Review workflow package">
      <div className="workflow-review-filters">
        <label>Review saved revision<select value={revision} onChange={(event) => setRevision(event.currentTarget.value)}>
          {(preview.data?.revisions ?? [revision]).map((item) => <option key={item} value={item}>{item}{item === savedRevision && " · Current"}</option>)}
        </select></label>
        {member && <label>Review artifact<select value={member.key} onChange={(event) => select(event.currentTarget.value)}>
          {preview.data?.members.map((item) => <option key={item.key} value={item.key}>{item.name} · {kindName(item.kind)}</option>)}
        </select></label>}
      </div>
      {preview.isPending && <p role="status">Loading revision preview…</p>}
      {preview.error && <p role="alert">{String(preview.error)}</p>}
      {preview.data?.members.length === 0 && <p>This saved revision has no package members.</p>}
      {member && <>
        <details className="workflow-review-card" key={member.key} open={!member.simulatable || Boolean(member.diagnostics.length)}>
          <summary><FileDiff size={16} aria-hidden /> Changes and dependencies</summary>
          <MemberReview member={member} />
        </details>
        {member.simulatable && <SimulateMember key={`${revision}:${member.key}`} projectId={projectId} draftId={draftId} revision={revision} member={member} />}
      </>}
    </section>}
  </WorkflowBuilderDialog>;
}
