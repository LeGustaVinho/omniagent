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
import { WorkflowSchemaEditor } from "@/workflow-schema-editor";
import { objectSchema, schemaValueProblem, type JsonSchema, type SchemaField } from "@/workflow-schema-model";
import type { InputField } from "@/workflow-decision-editor";
import { builderNodeKinds, WorkflowNodePicker, type WorkflowCatalogItem } from "@/workflow-node-picker";
import { TypedPromptInspector, WorkflowReferenceField, type PromptSlot, type RoleSnapshot } from "@/workflow-typed-prompt";
import { PROMPT_EXECUTORS, promptNodeDefinition, promptReferenceValue, type PromptMember, type PromptPortRename, type PromptRoleChoice,
  initialPromptSample, type PromptSample } from "@/workflow-prompt-model";
import type { PackageDraft } from "@/workflow-authoring-package";
import { configuredBranches, graphProblems, type WorkflowCanvasEdit, type WorkflowDefinition, type WorkflowDefinitions, type WorkflowNodeConfiguration,
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
export type SchemaMember = { key: string; name: string; body: { kind: "schema"; fields: SchemaField[] } };
type RoleChoice = PromptRoleChoice & { template?: RoleSnapshot };
const typedPromptSlots: readonly PromptSlot[] = ["role", "input_schema", "output_schema", "reviewer"];
const names = (items: readonly SchemaMember[]): PromptRoleChoice[] => items.map(({ key, name }) =>
  ({ name: name + " · This draft", reference: { source: "draft", key } }));

function useTypedResources(projectId: string, references: ReusableReference[], draftSchemas: SchemaMember[], draftRoles: RoleChoice[]) {
  const versions = [...new Map(references.flatMap((reference) =>
    reference.source === "published" && ["schema", "role"].includes(reference.version.kind)
      ? [[promptReferenceValue(reference), reference.version] as const] : [])).entries()];
  const loads = useQueries({ queries: versions.map(([key, version]) => ({
    queryKey: ["workflow-typed-resource", projectId, key],
    queryFn: () => invoke<{ body: ReusableBody }>("read_published_workflow_package_artifact", { projectId, version }),
  })) });
  const loaded = new Map(versions.map(([key], index) => [key, loads[index]?.data?.body]));
  return {
    schema: (reference?: ReusableReference) => {
      const fields = reference?.source === "draft" ? draftSchemas.find(({ key }) => key === reference.key)?.body.fields
        : loaded.get(promptReferenceValue(reference))?.fields;
      return fields && objectSchema(fields);
    },
    role: (reference?: ReusableReference) => reference?.source === "draft"
      ? draftRoles.find((item) => item.reference.source === "draft" && item.reference.key === reference.key)?.template
      : loaded.get(promptReferenceValue(reference))?.template,
  };
}

function referenceChoices(kind: string, options: WorkflowCatalogItem[] | undefined): PromptRoleChoice[] {
  return (options ?? []).filter(({ version }) => version.kind === kind).map(({ name, version }) => ({
    name: name + " · " + version.revision.slice(0, 10), reference: { source: "published", version },
  }));
}

export function WorkflowPackageGraphEditing({ projectId, checkoutId, draftId, savedRevision, graphs, draftPresets = {}, disabled, edit,
  layouts = {}, canvasByMember, changeCanvas, deriveDisabled, onDerived, selection, select, viewports,
  promptMembers = [], draftRoles = [], draftSchemas = [], editPrompt, editSchema, newPrompt, savePrefab }: {
  projectId: string; checkoutId: string; draftId: string; savedRevision: string; graphs: GraphMember[]; disabled: boolean;
  draftPresets?: Record<string, ReusableBody>;
  layouts?: Record<string, WorkflowLayout>;
  canvasByMember: Record<string, WorkflowCanvasEdit>;
  changeCanvas: (key: string, canvas: WorkflowCanvasEdit) => void;
  edit: (key: string, graph: WorkflowGraph, references?: Record<string, ReusableReference>) => void;
  promptMembers?: PromptMember[];
  draftRoles?: RoleChoice[];
  draftSchemas?: SchemaMember[];
  editPrompt?: (prompt: PromptMember, rename?: PromptPortRename) => void;
  editSchema?: (schema: SchemaMember) => void;
  newPrompt?: (source?: PromptMember) => ReusableReference;
  savePrefab?: (name: string, prompt: PromptMember["body"]["prompt"], refs: Record<string, ReusableReference>) => void;
  deriveDisabled: boolean;
  onDerived: (draft: PackageDraft) => void;
  selection: string;
  select: (key: string) => void;
  viewports: Map<string, Viewport>;
}) {
  const [derivation, setDerivation] = useState<WorkflowReuseSelection>();
  const prompt = promptMembers.find(({ key }) => key === selection) ?? (!graphs.length ? promptMembers[0] : undefined);
  const schema = draftSchemas.find(({ key }) => key === selection) ?? (!graphs.length && !prompt ? draftSchemas[0] : undefined);
  const graph = graphs.find(({ key }) => key === selection) ?? graphs[0];
  const show = Boolean(prompt || graph || schema);
  const preview = usePackagePreview(projectId, draftId, savedRevision, show);
  const nodeKinds = graphs.flatMap(({ graph }) => graph.nodes.map(({ kind }) => kind));
  if (promptMembers.length || nodeKinds.includes("subflow")) {
    nodeKinds.push("subflow", ...Object.keys(PROMPT_EXECUTORS));
  }
  if (graph?.graph.nodes.some(({ kind }) => kind === "end")) nodeKinds.push(...builderNodeKinds);
  const registry = useQuery(workflowNodeDefinitionsQuery(nodeKinds));
  const roleCatalog = useQuery({
    queryKey: ["workflow-package-catalog", projectId], enabled: Boolean(prompt),
    queryFn: () => invoke<WorkflowCatalogItem[]>("workflow_package_catalog", { projectId }),
  });
  const resources = useTypedResources(projectId, prompt?.body.prompt.schemaContract ? Object.values(prompt.references ?? {}) : [], draftSchemas, draftRoles);
  if (!show) return <div className="workflow-builder-empty"><Workflow size={24} aria-hidden />
    <p>This draft has no editable graph. Open Draft artifacts to add or edit a graph.</p>
  </div>;
  const authoredPromptKeys = new Set(graphs.flatMap((member) => Object.values(member.references ?? {}).flatMap((ref) =>
    ref.source === "draft" ? [ref.key] : [])));
  const options = [...graphs, ...draftSchemas, ...promptMembers.filter(({ key, destination }) =>
    key === prompt?.key || destination === "library" || !authoredPromptKeys.has(key))];
  const selectedMember = schema ?? prompt ?? graph;
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
      {schema && editSchema && <>
        {memberPicker}
        <WorkflowSchemaEditor key={schema.key} name={schema.name} fields={schema.body.fields} disabled={disabled}
          onChange={(name, fields) => editSchema({ ...schema, name, body: { kind: "schema", fields } })} />
      </>}
      {!schema && prompt && registry.data && preview.isSuccess && editPrompt && <PromptRevisionView>
        {memberPicker}
        {prompt.body.prompt.schemaContract ? <TypedPromptInspector key={prompt.key} projectId={projectId} checkoutId={checkoutId}
          title={prompt.name} onTitle={(name) => editPrompt({ ...prompt, name })}
          prompt={prompt.body.prompt.text} references={prompt.references ?? {}} defaults={{}}
          role={resources.role(prompt.references?.role)} defaultRoleOverrides={{}} roleOverrides={prompt.body.prompt.roleOverrides ?? {}}
          roleChoices={promptRoles(draftRoles, roleCatalog.data)} schemaChoices={[...names(draftSchemas), ...referenceChoices("schema", roleCatalog.data)]}
          inputSchema={resources.schema(prompt.references?.input_schema)} disabled={disabled}
          onPrompt={(text) => editPrompt({ ...prompt, body: { ...prompt.body, prompt: { ...prompt.body.prompt, text: text ?? "" } } })}
          onReference={(slot, reference) => editPrompt(updatePromptReference(prompt, slot, reference))}
          onRoleOverrides={(roleOverrides) => editPrompt({ ...prompt, body: { ...prompt.body, prompt: { ...prompt.body.prompt, roleOverrides } } })} />
          : <PromptInspector key={prompt.key} member={prompt} choices={promptRoles(draftRoles, roleCatalog.data)}
          catalog={registry.data} disabled={disabled} onChange={editPrompt}
          technical={preview.data.members.find(({ key }) => key === prompt.key)?.graphDefinitions}
          suggestion={(onApply) => <PromptContractSuggestion projectId={projectId} checkoutId={checkoutId}
            prompt={prompt.body.prompt} cardFingerprint={JSON.stringify(prompt)} disabled={disabled} onApply={onApply} />} />}
      </PromptRevisionView>}
      {!schema && !prompt && graph && registry.data && preview.isSuccess && <PackageMemberGraph key={graph.key} projectId={projectId} checkoutId={checkoutId}
        graph={graph.graph} references={graph.references ?? {}} draftPresets={draftPresets}
        canvas={canvasByMember[graph.key] ?? { layout: layouts[graph.key] ?? {}, updateDefinitions: {} }}
        setCanvas={(canvas) => changeCanvas(graph.key, canvas)}
        pinned={pinned} catalog={registry.data} disabled={disabled} graphs={graphs} memberKey={graph.key}
        edit={(next, references) => edit(graph.key, next, references)}
        promptMembers={promptMembers} draftRoles={draftRoles} draftSchemas={draftSchemas} editPrompt={editPrompt} newPrompt={newPrompt} savePrefab={savePrefab}
        previewMembers={preview.data.members}
        onDerive={setDerivation} deriveDisabled={deriveDisabled} toolbarStart={memberPicker}
        initialViewport={viewports.get(graph.key)} onViewportChange={(viewport) => viewports.set(graph.key, viewport)}
        openDraftSubflow={(key) => {
          const child = graphs.find((member) => member.key === key && ["subflow", "pipeline"].includes(member.kind ?? ""));
          return child ? <Button type="button" variant="outline" onClick={() => select(child.key)}>
            {child.kind === "subflow" ? "Open draft Function" : "Open draft child workflow"}
          </Button> : null;
        }} />}
      {derivation && graph && !prompt && <WorkflowReusableDerivationDialog projectId={projectId} graph={graph.graph}
        source={{ type: "package_revision", draftId, revision: savedRevision, memberKey: graph.key }} selection={derivation}
        disabled={deriveDisabled} onClose={() => setDerivation(undefined)} onSaved={onDerived} />}
  </section>;
}

function replaceReference(references: Record<string, ReusableReference>, slot: string, reference?: ReusableReference) {
  const next = { ...references };
  if (reference) next[slot] = reference;
  else delete next[slot];
  return next;
}

function updatePromptReference(member: PromptMember, slot: PromptSlot, reference?: ReusableReference): PromptMember {
  const reset = slot === "role" && promptReferenceValue(member.references?.role) !== promptReferenceValue(reference);
  return { ...member, references: replaceReference(member.references ?? {}, slot, reference),
    body: reset ? { ...member.body, prompt: { ...member.body.prompt, roleOverrides: {} } } : member.body };
}

function promptRoles(drafts: PromptRoleChoice[], published?: { name: string; version: Version }[]): PromptRoleChoice[] {
  return [...drafts, ...(published ?? []).filter(({ version }) => version.kind === "role").map(({ name, version }) => ({
    name: name + " · " + version.revision.slice(0, 10), reference: { source: "published" as const, version },
  }))];
}

function PackageMemberGraph({ projectId, checkoutId, graph, references, draftPresets, pinned, catalog, disabled, edit, openDraftSubflow,
  canvas, setCanvas, graphs, memberKey, onDerive, deriveDisabled, toolbarStart, initialViewport, onViewportChange,
  promptMembers, draftRoles, draftSchemas, editPrompt, previewMembers, newPrompt, savePrefab }: {
  projectId: string; checkoutId: string; graph: WorkflowGraph; pinned: WorkflowDefinitions; catalog: WorkflowDefinition[]; disabled: boolean;
  references: Record<string, ReusableReference>; draftPresets: Record<string, ReusableBody>;
  edit: (graph: WorkflowGraph, references?: Record<string, ReusableReference>) => void;
  promptMembers: PromptMember[];
  draftRoles: RoleChoice[];
  draftSchemas: SchemaMember[];
  editPrompt?: (prompt: PromptMember, rename?: PromptPortRename) => void;
  newPrompt?: (source?: PromptMember) => ReusableReference;
  savePrefab?: (name: string, prompt: PromptMember["body"]["prompt"], refs: Record<string, ReusableReference>) => void;
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
    queryFn: () => invoke<WorkflowCatalogItem[]>("workflow_package_catalog", { projectId }),
    enabled: graph.nodes.some(({ kind, configuration }) => ["end", "subflow", "repeat_until", "for_each", "run_backlog", "loop"].includes(kind)
      || configuration.kind === "agent_role"),
  });
  const error = queries.find((query) => query.error)?.error;
  const choices = promptRoles(draftRoles, bindings.data);
  const schemas = [...names(draftSchemas), ...referenceChoices("schema", bindings.data)];
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
  const resources = useTypedResources(projectId, [...Object.entries(references).flatMap(([slot, value]) => slot.includes(":") ? [value] : []),
    ...[...promptsByNode.values()].flatMap((member) => member.body.prompt.schemaContract ? Object.values(member.references ?? {}) : [])],
    draftSchemas, draftRoles);
  function typedPrompt(id: string, member: PromptMember) {
    const local: Record<string, ReusableReference> = {};
    for (const slot of typedPromptSlots) {
      const reference = references[id + ":" + slot];
      if (reference) local[slot] = reference;
    }
    const effective = { ...member.references, ...local };
    const node = graph.nodes.find((item) => item.id === id);
    const overrides = node?.configuration.kind === "prompt_overrides" ? node.configuration : undefined;
    const roleChanged = !!local.role && promptReferenceValue(local.role) !== promptReferenceValue(member.references?.role);
    const defaults = roleChanged ? {} : member.body.prompt.roleOverrides ?? {};
    const fields = (overrides?.roleOverrides ?? {}) as Record<string, unknown>;
    const role = resources.role(effective.role);
    return { local, effective, overrides, defaults, fields, role,
      input: resources.schema(effective.input_schema), output: resources.schema(effective.output_schema),
      write: (fields.toolScope ?? defaults.toolScope ?? role?.toolScope) === "approved_write" };
  }
  const discovered: WorkflowDefinitions = { ...pinned };
  const nodeBindings: NonNullable<WorkflowEditorState["nodeBindings"]> = Object.create(null);
  const nodes = graph.nodes.map((node) => {
    const known = reusable.get(node.id);
    if (known?.definition.kind === node.kind) discovered[node.id] = known.definition;
    const reference = Object.hasOwn(references, node.id) ? references[node.id] : undefined;
    const local = reference?.source === "draft" ? draftPresets[reference.key] : undefined;
    const base = known?.artifact.body.kind === "preset" ? known.artifact.body : local;
    const slots = Object.fromEntries(Object.entries(references).flatMap(([key, value]) =>
      key.startsWith(idPrefix(node.id)) ? [[key.slice(idPrefix(node.id).length), value]] : []));
    if (!reference) {
      if (Object.keys(slots).length) nodeBindings[node.id] = { slots };
      return node;
    }
    if (!base?.configuration) {
      nodeBindings[node.id] = { reference, slots };
      return node;
    }
    const fields = known?.editableOverrides ?? reusableEditableFields(base.configuration)
      .filter((field) => base.editableFields?.includes(field));
    const stored = node.configuration.kind === "preset_overrides" && node.configuration.fields &&
      typeof node.configuration.fields === "object" ? node.configuration.fields as Record<string, unknown> : {};
    const shown = { ...base.configuration, ...stored };
    nodeBindings[node.id] = { reference, slots, preset: { base: base.configuration, fields, stored: node.configuration, shown } };
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
    const childPins = previewMembers.find(({ key }) => key === child.key)?.graphDefinitions ?? {};
    const childDefinitions = editorDefinitions({ graph: child.graph, layout: {}, updateDefinitions: {} }, childPins, catalog);
    const ports = (bindings: Record<string, WorkflowEndpoint> | undefined, direction: "inputs" | "outputs") =>
      Object.entries(bindings ?? {}).flatMap(([name, { node, port }]) => {
        const value = childDefinitions[node]?.[direction].find((candidate) => candidate.name === port)?.value;
        return value ? [{ name, value }] : [];
      });
    // The public subflow input points at a Start output.
    const schemaPorts = (bindings?: Record<string, WorkflowEndpoint>) =>
      Object.fromEntries(Object.entries(bindings ?? {}).flatMap(([name, { node, port }]) => {
        const schema = childDefinitions[node]?.outputSchemas?.[port];
        return schema ? [[name, schema]] : [];
      }));
    return { ...base, title: child.name, inputs: ports(child.inputs, "outputs"), outputs: ports(child.outputs, "outputs"),
      inputSchemas: schemaPorts(child.inputs), outputSchemas: schemaPorts(child.outputs) };
  }
  const definitions = editorDefinitions(state, discovered, catalog, subflowDefinition);
  for (const node of graph.nodes) {
    const schema = resources.schema(references[node.id + ":schema"]);
    const definition = definitions[node.id];
    if (!definition) continue;
    if (schema && node.kind === "typed_start") definitions[node.id] = { ...definition, outputSchemas: { inputs: schema } };
    if (node.kind === "end") definitions[node.id] = schema ? { ...definition, inputSchemas: { result: schema }, outputSchemas: { result: schema } }
      : { ...definition, inputSchemas: undefined, outputSchemas: undefined };
    if (schema && node.kind === "validate_output") definitions[node.id] = { ...definition, outputSchemas: { object: schema } };
  }
  for (const [id, member] of promptsByNode) {
    const base = definitions[id];
    if (!base) continue;
    const typed = member.body.prompt.schemaContract ? typedPrompt(id, member) : undefined;
    definitions[id] = promptNodeDefinition(member.name, member.body.prompt, base,
      typed ? { input: typed.input, output: typed.output } : undefined, typed?.write);
  }
  propagateSchemas(graph, definitions);
  const rolesByNode = Object.fromEntries([...promptsByNode.entries()].map(([id, member]) => {
    const role = references[id + ":role"] ?? member.references?.role;
    return [id, choices.find(({ reference }) => promptReferenceValue(reference) === promptReferenceValue(role))?.name
      ?? (role ? "Pinned Role" : "Choose a Role")];
  }));
  const backlogBound = (id: string) => graph.nodes.some((node) => node.id === id && node.kind === "run_backlog") && Boolean(references[id]);
  function referencePicker(id: string) {
    const node = graph.nodes.find((candidate) => candidate.id === id);
    if (!node) return null;
    if (["end", "typed_start", "validate_output"].includes(node.kind) && graph.nodes.some(({ kind }) => kind === "end")) {
      const slot = id + ":schema";
      return <WorkflowReferenceField label={node.kind === "typed_start" ? "Input Schema" : "Output Schema"} selected={references[slot]}
        choices={schemas} disabled={disabled} onChange={(reference) => edit(graph, replaceReference(references, slot, reference))} />;
    }
    const kind = ["subflow", "repeat_until", "for_each", "loop"].includes(node.kind) ? "subflow"
      : node.kind === "run_backlog" ? "pipeline" : node.configuration.kind === "agent_role" ? "role" : null;
    if (!kind) return null;
    const reference = references[id];
    const choices: { label: string; reference: ReusableReference }[] = (bindings.data ?? [])
      .filter(({ version, prompt }) => version.kind === kind && (kind !== "pipeline" || version.destination === "project")
        && (node.kind !== "loop" || !prompt))
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
    const retained = new Map(Object.entries(references).filter(([slot]) => ![...before].some((id) => id === slot || slot.startsWith(idPrefix(id)))
      || (next.nodeBindings && Object.hasOwn(next.nodeBindings, slot))));
    const updated = next.graph.nodes.map((node) => {
      const binding = next.nodeBindings && Object.hasOwn(next.nodeBindings, node.id) ? next.nodeBindings[node.id] : undefined;
      if (binding?.reference) retained.set(node.id, binding.reference);
      for (const [slot, reference] of Object.entries(binding?.slots ?? {})) retained.set(idPrefix(node.id) + slot, reference);
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
      insertTools={newPrompt && graph.nodes.some(({ kind }) => kind === "end") ? (insert) =>
        <WorkflowNodePicker projectId={projectId} disabled={disabled} onAdd={(kind) => {
          if (kind === "subflow") insert(kind, { reference: newPrompt() });
          else insert(kind);
        }} onReuse={({ artifact }) => insert("subflow", { reference: { source: "published", version: artifact.version } })} /> : undefined}
      cloneBinding={newPrompt ? (binding) => {
        if (binding.reference?.source !== "draft") return binding;
        const key = binding.reference.key;
        const member = promptMembers.find((prompt) => prompt.key === key);
        return member?.body.prompt.schemaContract && !member.body.prompt.nodePrefab ? { ...binding, reference: newPrompt(member) } : binding;
      } : undefined}
      promptRoles={rolesByNode}
      inspectPrompt={(id) => {
        const member = promptsByNode.get(id);
        if (!member) return null;
        if (member.body.prompt.schemaContract) {
          const typed = typedPrompt(id, member);
          const local = references[id]?.source === "draft" && !member.body.prompt.nodePrefab && !typed.overrides
            && Object.keys(typed.local).length === 0 && editPrompt;
          const persistPrefab = (name: string) => savePrefab?.(name, { ...member.body.prompt,
            text: typeof typed.overrides?.prompt === "string" ? typed.overrides.prompt : member.body.prompt.text,
            roleOverrides: { ...typed.defaults, ...typed.fields },
          }, typed.effective);
          if (local) return <TypedPromptInspector key={id} projectId={projectId} checkoutId={checkoutId}
            title={member.name} onTitle={(name) => local({ ...member, name })}
            prompt={member.body.prompt.text} references={member.references ?? {}} defaults={{}} role={typed.role}
            defaultRoleOverrides={{}} roleOverrides={member.body.prompt.roleOverrides ?? {}} roleChoices={choices}
            schemaChoices={schemas} inputSchema={typed.input} disabled={disabled}
            onPrompt={(text) => local({ ...member, body: { ...member.body, prompt: { ...member.body.prompt, text: text ?? "" } } })}
            onReference={(slot, reference) => local(updatePromptReference(member, slot, reference))}
            onRoleOverrides={(roleOverrides) => local({ ...member, body: { ...member.body, prompt: { ...member.body.prompt, roleOverrides } } })}
            onSavePrefab={savePrefab && persistPrefab} />;
          const replaceConfiguration = (patch?: Partial<WorkflowNodeConfiguration>, nextReferences = references) => {
            const configuration: WorkflowNodeConfiguration = { kind: "prompt_overrides", ...typed.overrides, ...patch };
            edit({ ...graph, nodes: graph.nodes.map((node) => node.id === id ? { ...node, configuration } : node) }, nextReferences);
          };
          return <TypedPromptInspector key={id} projectId={projectId} checkoutId={checkoutId}
            prompt={typeof typed.overrides?.prompt === "string" ? typed.overrides.prompt : undefined}
            defaultPrompt={member.body.prompt.text} references={typed.local} defaults={member.references ?? {}} role={typed.role}
            defaultRoleOverrides={typed.defaults} roleOverrides={typed.fields} roleChoices={choices}
            schemaChoices={schemas} inputSchema={typed.input} disabled={disabled}
            onPrompt={(prompt) => replaceConfiguration({ prompt })}
            onReference={(slot, reference) => {
              const next = replaceReference(references, id + ":" + slot, reference);
              const current = promptReferenceValue(typed.effective[slot]);
              const resolved = promptReferenceValue(reference ?? member.references?.[slot]);
              replaceConfiguration(slot === "role" && current !== resolved ? { roleOverrides: {} } : undefined, next);
            }}
            onRoleOverrides={(roleOverrides) => replaceConfiguration({ roleOverrides })}
            onSavePrefab={savePrefab && persistPrefab} />;
        }
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

function idPrefix(id: string) { return id + ":"; }

function responseSchema(configuration: WorkflowNodeConfiguration, original: JsonSchema): JsonSchema {
  const fields = (configuration.fields ?? []) as InputField[];
  const approval = configuration.interaction === "approval";
  const expires = configuration.timeoutSeconds != null;
  const properties: Record<string, JsonSchema> = {
    interaction: { type: "string" }, status: { type: "string" }, original,
    values: configuration.interaction === "review" ? original : {
      type: "object", additionalProperties: false, required: fields.filter(({ required }) => required).map(({ id }) => id),
      properties: Object.fromEntries(fields.map(({ id, input }) => [id, input.kind === "boolean" ? { type: "boolean" }
        : input.kind === "number" ? { type: input.integer ? "integer" : "number" }
        : input.kind === "choice" && input.multiple ? { type: "array", items: { type: "string" } }
        : { type: "string" }])),
    },
  };
  if (expires) properties.expired = { type: "boolean" };
  if (approval) properties.approved = { type: "boolean" };
  return { type: "object", additionalProperties: false, properties, required: [
    "interaction", "status", ...(!expires ? ["values"] : []),
    ...(approval && (!expires || configuration.onExpiration === "reject") ? ["approved"] : []),
  ] };
}

function propagateSchemas(graph: WorkflowGraph, definitions: WorkflowDefinitions) {
  // Pass-through stages can inherit the contract of their selected producer.
  // Revisit downstream stages until forward references settle.
  let changed = true;
  for (let pass = 0; pass < graph.nodes.length && changed; pass += 1) {
    changed = false;
    for (const node of graph.nodes) {
      if (!["fork", "wait", "human", "join_results", "end"].includes(node.kind)) continue;
      const wires = graph.wires.filter(({ to }) => to.node === node.id);
      const source = (name: string) => {
        const endpoint = wires.find(({ to }) => to.port === name)?.from;
        return endpoint && definitions[endpoint.node]?.outputSchemas?.[endpoint.port];
      };
      const schema = source(node.kind === "end" ? "result" : "input");
      const definition = definitions[node.id];
      if (!definition) continue;
      const ports = definition.outputs.flatMap(({ name }) =>
        (node.kind === "fork" ? name.startsWith("context:") : name === (node.kind === "wait" ? "result" : "approved_result")) ? [name] : []);
      const outputSchemas: Record<string, JsonSchema> = schema ? Object.fromEntries(ports.map((name) => [name, schema])) : {};
      if (node.kind === "human") outputSchemas.response = responseSchema(node.configuration, schema ?? { type: "object" });
      if (node.kind === "end" && (definition.inputSchemas?.result ?? schema)) outputSchemas.result = (definition.inputSchemas?.result ?? schema)!;
      if (node.kind === "join_results") {
        const branches = Object.fromEntries(configuredBranches(node.configuration).map((branch) => {
          const result = `result:${branch}`;
          return [branch, { type: "object", properties: { status: { type: "string" }, error: { type: "string" },
            ...(wires.some(({ to }) => to.port === result) && { result: source(result) ?? { type: "object" } }),
          }, required: ["status"], additionalProperties: false } as JsonSchema];
        }));
        outputSchemas.results = { type: "object", properties: {
          branches: { type: "object", properties: branches, additionalProperties: false },
        }, required: ["branches"], additionalProperties: false };
      }
      if (JSON.stringify(outputSchemas) !== JSON.stringify(definition.outputSchemas ?? {})) {
        definitions[node.id] = { ...definition, outputSchemas };
        changed = true;
      }
    }
  }
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
  const missingStartField = member.startSchema?.fields.some(({ name, required }) => required && !Object.hasOwn(values, name))
    || !!(member.startSchema?.jsonSchema && schemaValueProblem(member.startSchema.jsonSchema, values, "Sample inputs"));
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
            schema={member.startSchema?.jsonSchema?.properties?.[field.name]}
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
    wide open={open} onOpenChange={(next) => {
      if (next) setRevision(savedRevision);
      setOpen(next);
    }} trigger={<Button type="button" variant="outline">
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
