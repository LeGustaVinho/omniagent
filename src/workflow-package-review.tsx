import { useEffect, useRef, useState, type Dispatch, type ReactNode, type SetStateAction } from "react";
import { ArrowLeft, FileDiff, Workflow } from "lucide-react";
import { useQueries, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { workflowNodeDefinitionsQuery } from "@/workflow-definition-query";
import { Button } from "@/components/ui/button";
import { ReadOnlyWorkflowRole, useRoleValidation, WorkflowRoleFields, type TemplateDraft } from "@/conversation-template-editor";
import { WorkflowResourceActions, type DraftResource } from "@/workflow-resource-actions";
import { WorkflowGraphEditor, editorDefinitions, type WorkflowEditorState, type WorkflowGraphView } from "@/workflow-graph-editor";
import { WorkflowBuilderDialog } from "@/workflow-builder-dialog";
import { PromptRevisionView } from "@/workflow-prompt";
import { WorkflowSchemaEditor } from "@/workflow-schema-editor";
import { objectSchema, type JsonSchema, type SchemaField } from "@/workflow-schema-model";
import type { InputField } from "@/workflow-decision-editor";
import { builderNodeKinds, WorkflowNodePicker, type WorkflowCatalogItem } from "@/workflow-node-picker";
import { TypedPromptInspector, WorkflowReferenceField, type PromptSlot, type RoleSnapshot } from "@/workflow-typed-prompt";
import { promptNodeDefinition, promptReferenceValue, type PromptMember, type PromptPortRename,
  type PromptRoleChoice } from "@/workflow-prompt-model";
import type { PackageDraft, WorkflowMember } from "@/workflow-authoring-package";
import { configuredBranches, graphProblems, isSupportedWorkflowNode, type WorkflowCanvasEdit, type WorkflowDefinition, type WorkflowDefinitions, type WorkflowNodeConfiguration,
  type WorkflowEndpoint, type WorkflowGraph, type WorkflowGraphNode, type WorkflowLayout, type WorkflowReuseSelection } from "@/workflow-graph-model";
import { publishedWorkflowArtifactQuery, reusableEditableFields, reusableNodeQuery, WorkflowReusableDerivationDialog,
  type ReusableBody, type ReusableReference } from "@/workflow-reusable";

type Version = { id: string; revision: string; destination: "project" | "library"; kind: string };
type Reference = { source: "draft"; key: string } | { source: "published"; version: Version }
  | { source: "parameter"; name: string; kind: string };
type Member = {
  key: string; name: string; kind: string; base: Version | null;
  changes: { path: string; change: "added" | "removed" | "changed"; before: unknown; after: unknown }[];
  changesTruncated: boolean; dependencies: Record<string, Reference>;
  affectedUses: { consumer: Version; name: string; slot: string; pinnedRevision: string }[];
  diagnostics: string[]; requirements: string[];
  graphDefinitions?: WorkflowDefinitions;
};
type Preview = { revision: string; revisions: string[]; members: Member[] };
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
export type WorkflowMemberLocation = { key: string; focus?: string };
export type SchemaMember = { key: string; name: string; body: { kind: "schema"; fields: SchemaField[] } };
type RoleChoice = PromptRoleChoice & { template?: RoleSnapshot };
function findDraftResource(schemas: SchemaMember[], roles: RoleChoice[]): DraftResource {
  return (key) => {
    const schema = schemas.find((item) => item.key === key);
    if (schema) return { kind: "schema", name: schema.name, fields: schema.body.fields };
    const template = roles.find(({ reference }) => reference.source === "draft" && reference.key === key)?.template;
    if (template) return { kind: "role", template };
    return undefined;
  };
}
const typedPromptSlots: readonly PromptSlot[] = ["role", "input_schema", "output_schema", "reviewer"];
const names = (items: readonly SchemaMember[]): PromptRoleChoice[] => items.map(({ key, name }) =>
  ({ name: name + " · This draft", reference: { source: "draft", key } }));

function useTypedResources(projectId: string, references: ReusableReference[], draftSchemas: SchemaMember[], draftRoles: RoleChoice[]) {
  const versions = [...new Map(references.flatMap((reference) =>
    reference.source === "published" && ["schema", "role"].includes(reference.version.kind)
      ? [[promptReferenceValue(reference), reference.version] as const] : [])).entries()];
  const loads = useQueries({ queries: versions.map(([, version]) => publishedWorkflowArtifactQuery(projectId, version)) });
  const loaded = new Map(versions.map(([key], index) => [key, loads[index]?.data]));
  const artifacts = [...loaded.values()].filter((artifact) => artifact != null);
  return {
    choices: (kind: string) => referenceChoices(kind, artifacts),
    schema: (reference?: ReusableReference) => {
      const fields = reference?.source === "draft" ? draftSchemas.find(({ key }) => key === reference.key)?.body.fields
        : loaded.get(promptReferenceValue(reference))?.body.fields;
      return fields && objectSchema(fields);
    },
    role: (reference?: ReusableReference) => reference?.source === "draft"
      ? draftRoles.find((item) => item.reference.source === "draft" && item.reference.key === reference.key)?.template
      : loaded.get(promptReferenceValue(reference))?.body.template,
  };
}

function referenceChoices(kind: string, options: WorkflowCatalogItem[] | undefined): PromptRoleChoice[] {
  return (options ?? []).filter(({ version }) => version.kind === kind).map(({ name, version }) => ({
    name: name + " · " + version.revision.slice(0, 10), reference: { source: "published", version },
  }));
}

export function WorkflowPackageGraphEditing({ projectId, checkoutId, draftId, savedRevision, graphs, draftPresets = {}, disabled, edit,
  layouts = {}, canvasByMember, changeCanvas, deriveDisabled, onDerived, selection, select, views, history, setHistory, externalBack,
  promptMembers = [], draftRoles = [], draftSchemas = [], editPrompt, editSchema, editRole, newPrompt, savePrefab }: {
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
  editRole?: (key: string, role: TemplateDraft) => void;
  newPrompt?: (source?: PromptMember) => ReusableReference;
  savePrefab?: (name: string, prompt: PromptMember["body"]["prompt"], refs: Record<string, ReusableReference>) => void;
  deriveDisabled: boolean;
  onDerived: (draft: PackageDraft) => void;
  selection: string;
  select: (key: string) => void;
  views: Map<string, WorkflowGraphView>;
  history: WorkflowMemberLocation[];
  setHistory: Dispatch<SetStateAction<WorkflowMemberLocation[]>>;
  externalBack?: ReactNode;
}) {
  const [derivation, setDerivation] = useState<WorkflowReuseSelection>();
  const [published, setPublished] = useState<{ version: Version; focus?: string }>();
  const workspace = useRef<HTMLElement>(null);
  const prompt = promptMembers.find(({ key }) => key === selection) ?? (!graphs.length ? promptMembers[0] : undefined);
  const schema = draftSchemas.find(({ key }) => key === selection) ?? (!graphs.length && !prompt ? draftSchemas[0] : undefined);
  const roleMembers = draftRoles.flatMap(({ reference, template }) =>
    reference.source === "draft" && template ? [{ key: reference.key, name: template.name, template }] : []);
  const role = roleMembers.find(({ key }) => key === selection) ?? (!graphs.length && !prompt && !schema ? roleMembers[0] : undefined);
  const graph = graphs.find(({ key }) => key === selection) ?? graphs[0];
  const show = Boolean(prompt || graph || schema || role);
  const preview = usePackagePreview(projectId, draftId, savedRevision, show);
  const nodeKinds = graphs.flatMap(({ graph }) => graph.nodes.map(({ kind }) => kind));
  if (promptMembers.length || nodeKinds.includes("subflow")) {
    nodeKinds.push("subflow", "agent");
  }
  if (graph && newPrompt) nodeKinds.push("end", ...builderNodeKinds);
  const registry = useQuery(workflowNodeDefinitionsQuery(nodeKinds));
  const roleCatalog = useQuery({
    queryKey: ["workflow-package-catalog", projectId], enabled: Boolean(prompt),
    queryFn: () => invoke<WorkflowCatalogItem[]>("workflow_package_catalog", { projectId }),
  });
  const resources = useTypedResources(projectId, prompt?.body.prompt.schemaContract ? Object.values(prompt.references ?? {}) : [], draftSchemas, draftRoles);
  if (!show) return <>{externalBack && <div className="pipeline-actions">{externalBack}</div>}
    <div className="workflow-builder-empty"><Workflow size={24} aria-hidden />
    <p>This draft has no editable graph. Open Draft artifacts to add or edit a graph.</p>
    </div></>;
  const authoredPromptKeys = new Set(graphs.flatMap((member) => Object.values(member.references ?? {}).flatMap((ref) =>
    ref.source === "draft" ? [ref.key] : [])));
  const options = [...graphs, ...draftSchemas, ...roleMembers, ...promptMembers.filter(({ key, destination }) =>
    key === prompt?.key || destination === "library" || !authoredPromptKeys.has(key))];
  const selectedMember = role ?? schema ?? prompt ?? graph;
  const focusKey = () => document.activeElement?.getAttribute("data-member-focus") ?? undefined;
  const restoreFocus = (focus?: string) => requestAnimationFrame(() => {
    const origin = workspace.current?.querySelector<HTMLElement>(`[data-member-focus="${focus ?? "picker"}"]`)
      ?? workspace.current?.querySelector<HTMLElement>(".workflow-graph-canvas");
    origin?.focus({ preventScroll: true });
  });
  function openMember(key: string) {
    if (key === selectedMember?.key) return;
    if (selectedMember) setHistory((previous) => [...previous, { key: selectedMember.key, focus: focusKey() }]);
    select(key);
    if (focusKey() !== "picker") focusWorkflowBack(workspace);
  }
  function goBack() {
    const previous = history.at(-1);
    if (!previous) return;
    select(previous.key);
    setHistory((entries) => entries.slice(0, -1));
    restoreFocus(previous.focus);
  }
  if (published) return <section ref={workspace} className="workflow-builder-graph" aria-label="Pinned artifact">
    <WorkflowArtifactViewer projectId={projectId} checkoutId={checkoutId} source={{ version: published.version }}
      onBack={() => { setPublished(undefined); restoreFocus(published.focus); }} />
  </section>;
  const memberPicker = options.length > 1 && selectedMember ? <label className="pipeline-inline-field workflow-builder-member">
    <span>Graph member</span>
    <select aria-label="Graph member" data-member-focus="picker" value={selectedMember.key}
      onChange={(event) => openMember(event.currentTarget.value)} title={selectedMember.name}>
      {options.map((member) => <option key={member.key} value={member.key}>{member.name}</option>)}
    </select>
  </label> : undefined;
  const mainPipeline = graphs.find(({ kind }) => kind === "pipeline");
  const memberNavigation = <>
    {history.length > 0 ? <WorkflowBack onClick={goBack} /> : externalBack ??
      (mainPipeline && selectedMember?.key !== mainPipeline.key && <WorkflowBack onClick={() => { select(mainPipeline.key); restoreFocus(); }} />)}
    {memberPicker}
  </>;
  const pinned = preview.data?.members.find(({ key }) => key === graph?.key)?.graphDefinitions ?? {};
  const error = preview.error ?? registry.error ?? (prompt ? roleCatalog.error : null);
  const view = graph && graphView(views, graph.key);
  return <section ref={workspace} className="workflow-builder-graph" aria-label="Build workflow">
      {error && <p role="alert">{String(error)}</p>}
      {show && (registry.isPending || preview.isPending) && <p role="status">Loading pipeline graph…</p>}
      {(!registry.data || !preview.isSuccess) && !role && !schema && <div className="pipeline-actions">{memberNavigation}</div>}
      {role && editRole && <>
        <div className="pipeline-actions">{memberNavigation}</div>
        <DraftRoleEditor projectId={projectId} checkoutId={checkoutId} role={role.template}
          disabled={disabled} onChange={(next) => editRole(role.key, next)} />
      </>}
      {!role && schema && editSchema && <>
        <div className="pipeline-actions">{memberNavigation}</div>
        <WorkflowSchemaEditor key={schema.key} name={schema.name} fields={schema.body.fields} disabled={disabled}
          onChange={(name, fields) => editSchema({ ...schema, name, body: { kind: "schema", fields } })} />
      </>}
      {!role && !schema && prompt && registry.data && preview.isSuccess && editPrompt && <>
        <div className="pipeline-actions">{memberNavigation}</div>
        <PromptRevisionView>
        {prompt.body.prompt.schemaContract ? <TypedPromptInspector key={prompt.key} projectId={projectId} checkoutId={checkoutId}
          title={prompt.name} onTitle={(name) => editPrompt({ ...prompt, name })}
          prompt={prompt.body.prompt.text} references={prompt.references ?? {}} defaults={{}}
          role={resources.role(prompt.references?.role)} defaultRoleOverrides={{}} roleOverrides={prompt.body.prompt.roleOverrides ?? {}}
          roleChoices={promptRoles(draftRoles, roleCatalog.data)} schemaChoices={[...names(draftSchemas), ...referenceChoices("schema", roleCatalog.data)]}
          draftResource={findDraftResource(draftSchemas, draftRoles)}
          inputSchema={resources.schema(prompt.references?.input_schema)} disabled={disabled}
          onPrompt={(text) => editPrompt({ ...prompt, body: { ...prompt.body, prompt: { ...prompt.body.prompt, text: text ?? "" } } })}
          onReference={(slot, reference) => editPrompt(updatePromptReference(prompt, slot, reference))}
          onRoleOverrides={(roleOverrides) => editPrompt({ ...prompt, body: { ...prompt.body, prompt: { ...prompt.body.prompt, roleOverrides } } })} />
          : <p role="alert">This Prompt uses a retired executor contract. Create a typed AI Node Agent to replace it.</p>}
        </PromptRevisionView>
      </>}
      {!role && !schema && !prompt && graph && registry.data && preview.isSuccess && <PackageMemberGraph key={graph.key} projectId={projectId} checkoutId={checkoutId}
        graph={graph.graph} references={graph.references ?? {}} draftPresets={draftPresets}
        canvas={canvasByMember[graph.key] ?? { layout: layouts[graph.key] ?? {}, updateDefinitions: {} }}
        setCanvas={(canvas) => changeCanvas(graph.key, canvas)}
        pinned={pinned} catalog={registry.data} disabled={disabled} graphs={graphs} memberKey={graph.key} isFunction={graph.kind === "subflow"}
        edit={(next, references) => edit(graph.key, next, references)}
        promptMembers={promptMembers} draftRoles={draftRoles} draftSchemas={draftSchemas} editPrompt={editPrompt} newPrompt={newPrompt} savePrefab={savePrefab}
        previewMembers={preview.data.members}
        onDerive={setDerivation} deriveDisabled={deriveDisabled} toolbarStart={memberNavigation}
        view={view}
        openPublished={(version) => setPublished({ version, focus: focusKey() })}
        openDraftMember={(key) => {
          const child = graphs.find((member) => member.key === key && ["subflow", "pipeline"].includes(member.kind ?? ""));
          const prefab = promptMembers.find((member) => member.key === key && member.body.prompt.nodePrefab);
          const label = child?.kind === "subflow" ? "Open draft Function"
            : child ? "Open draft child workflow" : prefab ? "Open draft Prefab" : null;
          return label && <Button type="button" variant="outline" data-member-focus="draft" onClick={() => openMember(key)}>{label}</Button>;
        }} />}
      {derivation && graph && !prompt && <WorkflowReusableDerivationDialog projectId={projectId} graph={graph.graph}
        source={{ type: "package_revision", draftId, revision: savedRevision, memberKey: graph.key }} selection={derivation}
        disabled={deriveDisabled} onClose={() => setDerivation(undefined)} onSaved={onDerived} />}
  </section>;
}

function DraftRoleEditor({ projectId, checkoutId, role, disabled, onChange }: {
  projectId: string; checkoutId: string; role: TemplateDraft; disabled: boolean; onChange: (role: TemplateDraft) => void;
}) {
  const validation = useRoleValidation(projectId, checkoutId, role, !disabled);
  return <div className="workflow-artifact-form pipeline-stack pipeline-form workflow-role-form" aria-label="Role editor">
    <WorkflowRoleFields projectId={projectId} checkoutId={checkoutId} draft={role}
      disabled={disabled} validation={validation} onChange={onChange} />
  </div>;
}

function graphView(views: Map<string, WorkflowGraphView>, key: string) {
  const view = views.get(key) ?? { inspector: {} };
  views.set(key, view);
  return view;
}

function focusWorkflowBack(workspace: { current: HTMLElement | null }) {
  const frame = requestAnimationFrame(() => workspace.current
    ?.querySelector<HTMLElement>("[data-workflow-back]")?.focus({ preventScroll: true }));
  return () => cancelAnimationFrame(frame);
}

export function WorkflowBack({ onClick, disabled }: { onClick: () => void; disabled?: boolean }) {
  return <Button type="button" variant="outline" data-workflow-back="" disabled={disabled} onClick={onClick}>
    <ArrowLeft size={15} aria-hidden /> <span lang="pt-BR">Voltar</span>
  </Button>;
}

type ArtifactSource = { version: Version } | { key: string };
const artifactKey = (source: ArtifactSource) => "key" in source ? source.key : JSON.stringify(source.version);
const ignoreEdit = () => undefined;

export function WorkflowArtifactViewer({ projectId, checkoutId, source, members = [], onBack, actions, busy }: {
  projectId: string; checkoutId: string; source: ArtifactSource; members?: WorkflowMember[];
  onBack: () => void; actions?: ReactNode; busy?: boolean;
}) {
  const [history, setHistory] = useState<{ source: ArtifactSource; focus?: string }[]>([]);
  const [current, setCurrent] = useState(source);
  const views = useRef(new Map<string, WorkflowGraphView>());
  const workspace = useRef<HTMLElement>(null);
  useEffect(() => focusWorkflowBack(workspace), []);
  const key = artifactKey(current);
  function open(next: ArtifactSource) {
    if (artifactKey(next) === key) return;
    const focus = document.activeElement?.getAttribute("data-member-focus") ?? undefined;
    setHistory((previous) => [...previous, { source: current, focus }]);
    setCurrent(next);
    focusWorkflowBack(workspace);
  }
  function back() {
    const previous = history.at(-1);
    if (!previous) return onBack();
    setHistory((entries) => entries.slice(0, -1));
    setCurrent(previous.source);
    requestAnimationFrame(() => {
      const target = workspace.current?.querySelector<HTMLElement>(`[data-member-focus="${previous.focus ?? ""}"]`)
        ?? workspace.current?.querySelector<HTMLElement>(".workflow-graph-canvas");
      target?.focus({ preventScroll: true });
    });
  }
  return <section ref={workspace} className="workflow-artifact-viewer" aria-label="Artifact preview">
    <div className="pipeline-actions workflow-artifact-toolbar">
      <WorkflowBack onClick={back} disabled={busy} />
      <span className="workflow-artifact-mode">Read-only</span>
      {"version" in current && <code className="workflow-artifact-mode" title={current.version.revision}>{current.version.revision.slice(0, 12)}</code>}
      {history.length === 0 && actions}
    </div>
    {"version" in current ? <PublishedArtifactView key={key} projectId={projectId} checkoutId={checkoutId}
      version={current.version} view={graphView(views.current, key)} openPublished={(version) => open({ version })} />
      : <ArtifactView key={key} projectId={projectId} checkoutId={checkoutId} artifact={members.find((member) => member.key === current.key)}
        members={members} view={graphView(views.current, key)}
        openDraftMember={(key) => <Button type="button" variant="outline" data-member-focus="draft"
          onClick={() => open({ key })}>Open {members.find((member) => member.key === key)?.name ?? key}</Button>}
        openPublished={(version) => open({ version })} />}
  </section>;
}

function PublishedArtifactView({ projectId, version, ...props }: {
  projectId: string; version: Version; checkoutId: string; view: WorkflowGraphView;
  openPublished: (version: Version) => void;
}) {
  const published = useQuery(publishedWorkflowArtifactQuery(projectId, version));
  if (!published.data) return <div className="workflow-artifact-form">
    {published.isPending ? <p role="status">Loading revision…</p>
      : <p role="alert">{String(published.error ?? "This revision is unavailable.")}</p>}</div>;
  const artifact = published.data;
  return <ArtifactView {...props} projectId={projectId} artifact={{ ...artifact, key: version.id }}
    layout={artifact.layout} definitions={artifact.definitions} />;
}

function ArtifactView({ projectId, checkoutId, artifact, view, members = [], layout = {}, definitions = {},
  openDraftMember = () => null, openPublished }: {
  projectId: string; checkoutId: string; artifact?: WorkflowMember; view: WorkflowGraphView;
  members?: WorkflowMember[]; layout?: WorkflowLayout; definitions?: WorkflowDefinitions;
  openDraftMember?: (key: string) => ReactNode; openPublished: (version: Version) => void;
}) {
  const body = artifact?.body;
  const graphs = members.flatMap(({ body, ...member }) => body.graph ? [{ ...member, ...body, graph: body.graph }] : []);
  const registry = useQuery({
    ...workflowNodeDefinitionsQuery([...(body?.graph?.nodes.map(({ kind }) => kind) ?? []),
      ...graphs.flatMap(({ graph }) => graph.nodes.map(({ kind }) => kind))]),
    enabled: Boolean(body?.graph),
  });
  const schemas = members.flatMap(({ key, name, body }) => body.kind === "schema" && body.fields
    ? [{ key, name, body: { kind: "schema" as const, fields: body.fields } }] : []);
  const roles = members.flatMap(({ key, name, body }) => body.template
    ? [{ name, reference: { source: "draft" as const, key }, template: body.template }] : []);
  const prompts = members.flatMap(({ body, ...member }) => body.prompt
    ? [{ ...member, body: { kind: "prompt" as const, prompt: body.prompt } }] : []);
  const references = artifact?.references ?? {};
  const resources = useTypedResources(projectId, Object.values(references), schemas, roles);
  if (artifact && body?.graph && registry.data) return <PackageMemberGraph projectId={projectId} checkoutId={checkoutId}
    memberKey={artifact.key} graph={body.graph} references={references} catalog={registry.data} pinned={definitions}
    canvas={{ layout, updateDefinitions: {} }} setCanvas={ignoreEdit} graphs={graphs} draftPresets={{}}
    disabled readOnly edit={ignoreEdit} view={view} isFunction={body.kind === "subflow"}
    promptMembers={prompts} draftRoles={roles} draftSchemas={schemas} previewMembers={[]}
    openDraftMember={openDraftMember} openPublished={openPublished} />;
  return <div className="workflow-artifact-form pipeline-stack">
    {body?.graph ? registry.error ? <p role="alert">{String(registry.error)}</p> : <p role="status">Loading graph…</p>
      : body?.fields && artifact ? <WorkflowSchemaEditor name={artifact.name} fields={body.fields} disabled onChange={ignoreEdit} />
      : body?.template ? <ReadOnlyWorkflowRole projectId={projectId} checkoutId={checkoutId} draft={body.template} />
      : body?.prompt && artifact ? <PromptRevisionView>
        <TypedPromptInspector projectId={projectId} checkoutId={checkoutId} title={artifact.name} onTitle={ignoreEdit}
          prompt={body.prompt.text} references={references} defaults={{}} role={resources.role(references.role)}
          defaultRoleOverrides={{}} roleOverrides={body.prompt.roleOverrides ?? {}}
          roleChoices={[...roles, ...resources.choices("role")]} schemaChoices={[...names(schemas), ...resources.choices("schema")]}
          inputSchema={resources.schema(references.input_schema)} disabled onPrompt={ignoreEdit} onReference={ignoreEdit} onRoleOverrides={ignoreEdit} />
      </PromptRevisionView>
      : <p role="alert">This artifact is unavailable in the visual editor.</p>}
  </div>;
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

function acceptsStartSchema(node: WorkflowGraphNode, definition?: WorkflowDefinition) {
  return node.kind === "typed_start" || node.configuration.kind === "subflow_entry" && definition?.outputs.some(({ name, value }) =>
    name === "inputs" && value.kind === "data" && value.artifact === "json_object") === true;
}

function PackageMemberGraph({ projectId, checkoutId, graph, references, draftPresets, pinned, catalog, disabled, readOnly, edit, openDraftMember, openPublished,
  canvas, setCanvas, graphs, memberKey, isFunction, onDerive, deriveDisabled, toolbarStart, view,
  promptMembers, draftRoles, draftSchemas, editPrompt, previewMembers, newPrompt, savePrefab }: {
  projectId: string; checkoutId: string; graph: WorkflowGraph; pinned: WorkflowDefinitions; catalog: WorkflowDefinition[];
  disabled: boolean; readOnly?: boolean;
  references: Record<string, ReusableReference>; draftPresets: Record<string, ReusableBody>;
  edit: (graph: WorkflowGraph, references?: Record<string, ReusableReference>) => void;
  promptMembers: PromptMember[];
  draftRoles: RoleChoice[];
  draftSchemas: SchemaMember[];
  editPrompt?: (prompt: PromptMember, rename?: PromptPortRename) => void;
  newPrompt?: (source?: PromptMember) => ReusableReference;
  savePrefab?: (name: string, prompt: PromptMember["body"]["prompt"], refs: Record<string, ReusableReference>) => void;
  previewMembers: Member[];
  openDraftMember: (key: string) => React.ReactNode;
  openPublished: (version: Version) => void;
  canvas: WorkflowCanvasEdit;
  setCanvas: (canvas: WorkflowCanvasEdit) => void;
  graphs: GraphMember[];
  memberKey: string;
  isFunction: boolean;
  onDerive?: (selection: WorkflowReuseSelection) => void;
  deriveDisabled?: boolean;
  toolbarStart?: ReactNode;
  view?: WorkflowGraphView;
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
    enabled: graph.nodes.some(({ kind, configuration }) => ["end", "subflow", "loop"].includes(kind)
      || configuration.kind === "agent_role"),
  });
  const error = queries.find((query) => query.error)?.error;
  const choices = promptRoles(draftRoles, bindings.data);
  const schemas = [...names(draftSchemas), ...referenceChoices("schema", bindings.data)];
  const draftResource = findDraftResource(draftSchemas, draftRoles);
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
      input: resources.schema(effective.input_schema), output: resources.schema(effective.output_schema) };
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
  const savedReferences = previewMembers.find(({ key }) => key === memberKey)?.dependencies;
  for (const node of graph.nodes) {
    const slot = node.id + ":schema";
    const schema = resources.schema(references[slot]);
    const definition = definitions[node.id];
    if (!definition) continue;
    if (acceptsStartSchema(node, definition) && (schema || savedReferences?.[slot])) {
      const outputSchemas = { ...definition.outputSchemas };
      if (schema) outputSchemas.inputs = schema;
      else delete outputSchemas.inputs;
      definitions[node.id] = { ...definition, outputSchemas };
    }
    if (node.kind === "end") {
      const terminal = isFunction ? { ...definition, title: "Return" } : definition;
      definitions[node.id] = schema ? { ...terminal, inputSchemas: { result: schema }, outputSchemas: { result: schema } }
        : { ...terminal, inputSchemas: undefined, outputSchemas: undefined };
    }
  }
  for (const [id, member] of promptsByNode) {
    const base = definitions[id];
    if (!base || !member.body.prompt.schemaContract) continue;
    const typed = typedPrompt(id, member);
    definitions[id] = promptNodeDefinition(member.name, base,
      typed ? { input: typed.input, output: typed.output } : undefined);
  }
  propagateSchemas(graph, definitions);
  const rolesByNode = Object.fromEntries([...promptsByNode.entries()].map(([id, member]) => {
    const role = references[id + ":role"] ?? member.references?.role;
    return [id, choices.find(({ reference }) => promptReferenceValue(reference) === promptReferenceValue(role))?.name
      ?? (role ? "Pinned Role" : "Choose a Role")];
  }));
  function referencePicker(id: string) {
    const node = graph.nodes.find((candidate) => candidate.id === id);
    if (!node || !isSupportedWorkflowNode(node.kind)) return null;
    const startSchema = acceptsStartSchema(node, definitions[id]);
    if ((startSchema || node.kind === "end") && graph.nodes.some(({ kind }) => kind === "end")) {
      const slot = id + ":schema";
      const label = startSchema ? "Input Schema" : "Output Schema";
      const onChange = (reference?: ReusableReference) => edit(graph, replaceReference(references, slot, reference));
      return <WorkflowReferenceField label={label} selected={references[slot]} choices={schemas} disabled={disabled} onChange={onChange}
        actions={!disabled && <WorkflowResourceActions projectId={projectId} checkoutId={checkoutId} kind="schema"
          label={label} selected={references[slot]} draftResource={draftResource} onSaved={onChange} />} />;
    }
    const kind = node.kind === "subflow" || node.kind === "loop" ? "subflow"
      : node.configuration.kind === "agent_role" ? "role" : null;
    if (!kind) return null;
    const reference = references[id];
    const choices: { label: string; reference: ReusableReference }[] = (bindings.data ?? [])
      .filter(({ version, prompt, blockers }) => version.kind === kind && !blockers.length
        && (node.kind !== "loop" || !prompt))
      .map(({ version, name }) => ({ label: `${name} · ${version.destination} · ${version.revision.slice(0, 10)}`,
        reference: { source: "published", version } }));
    if (kind !== "role") choices.unshift(...graphs.filter((child) => child.key !== memberKey && child.kind === kind)
      .map(({ key, name }) => ({ label: `${name} · This draft`, reference: { source: "draft" as const, key } })));
    if (reference && !choices.some((option) => JSON.stringify(option.reference) === JSON.stringify(reference))) {
      choices.unshift({ label: "Current pinned reference", reference });
    }
    const picker = <label>{kind === "subflow" ? "Function revision" : "Role revision"}
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
    return kind === "role" && !disabled ? <div className="workflow-reference-field">{picker}
      <WorkflowResourceActions projectId={projectId} checkoutId={checkoutId} kind="role" label="Role"
        selected={reference} draftResource={draftResource} onSaved={(reference) => edit(graph, replaceReference(references, id, reference))} />
    </div> : picker;
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
      fullHeight toolbarStart={toolbarStart} view={view} readOnly={readOnly}
      nameLabel={isFunction ? "Function name" : undefined}
      insertTools={newPrompt ? (insert, options) =>
        <WorkflowNodePicker {...options} projectId={projectId} disabled={disabled}
          terminal={graph.nodes.some(({ kind }) => kind === "end") ? undefined : isFunction ? "Return" : "End"} onAdd={(kind) => {
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
          const bindResource = (slot: PromptSlot, reference?: ReusableReference) => {
            const next = replaceReference(references, id + ":" + slot, reference);
            const current = promptReferenceValue(typed.effective[slot]);
            const resolved = promptReferenceValue(reference ?? member.references?.[slot]);
            const configuration: WorkflowNodeConfiguration = {
              kind: "prompt_overrides", ...typed.overrides,
              ...(slot === "role" && current !== resolved && { roleOverrides: {} }),
            };
            edit({ ...graph, nodes: graph.nodes.map((node) => node.id === id ? { ...node, configuration } : node) }, next);
          };
          if (local) return <TypedPromptInspector key={id} projectId={projectId} checkoutId={checkoutId}
            title={member.name} onTitle={(name) => local({ ...member, name })}
            prompt={member.body.prompt.text} references={member.references ?? {}} defaults={{}} role={typed.role}
            defaultRoleOverrides={{}} roleOverrides={member.body.prompt.roleOverrides ?? {}} roleChoices={choices}
            schemaChoices={schemas} inputSchema={typed.input} disabled={disabled}
            draftResource={draftResource} onResource={(slot, reference) => {
              const consumers = graphs.flatMap((graph) => Object.values(graph.references ?? {}))
                .filter((ref) => ref.source === "draft" && ref.key === member.key);
              if (consumers.length > 1) bindResource(slot, reference);
              else local(updatePromptReference(member, slot, reference));
            }}
            onPrompt={(text) => local({ ...member, body: { ...member.body, prompt: { ...member.body.prompt, text: text ?? "" } } })}
            onReference={(slot, reference) => local(updatePromptReference(member, slot, reference))}
            onRoleOverrides={(roleOverrides) => local({ ...member, body: { ...member.body, prompt: { ...member.body.prompt, roleOverrides } } })}
            onSavePrefab={savePrefab && persistPrefab} />;
          const replaceConfiguration = (patch?: Partial<WorkflowNodeConfiguration>, nextReferences = references) => {
            const configuration: WorkflowNodeConfiguration = { kind: "prompt_overrides", ...typed.overrides, ...patch };
            edit({ ...graph, nodes: graph.nodes.map((node) => node.id === id ? { ...node, configuration } : node) }, nextReferences);
          };
          const reference = references[id];
          return <>{reference?.source === "draft" && openDraftMember(reference.key)}
            {reference?.source === "published" && member.body.prompt.nodePrefab &&
              <Button type="button" variant="outline" data-member-focus="published"
                onClick={() => openPublished(reference.version)}>Open pinned Prefab</Button>}
            <TypedPromptInspector key={id} projectId={projectId} checkoutId={checkoutId}
            prompt={typeof typed.overrides?.prompt === "string" ? typed.overrides.prompt : undefined}
            defaultPrompt={member.body.prompt.text} references={typed.local} defaults={member.references ?? {}} role={typed.role}
            defaultRoleOverrides={typed.defaults} roleOverrides={typed.fields} roleChoices={choices}
            schemaChoices={schemas} inputSchema={typed.input} disabled={disabled}
            draftResource={draftResource}
            onPrompt={(prompt) => replaceConfiguration({ prompt })}
            onReference={bindResource}
            onRoleOverrides={(roleOverrides) => replaceConfiguration({ roleOverrides })}
            onSavePrefab={savePrefab && persistPrefab} />
          </>;
        }
        return <p role="alert">This Prompt uses a retired executor contract. Replace it with a typed AI Node Agent.</p>;
      }}
      problems={graphProblems(presented, definitions)} disabled={disabled}
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
          {reference?.source === "draft" && openDraftMember(reference.key)}
          {reference?.source === "published" && reference.version.kind === "subflow" &&
            <Button type="button" variant="outline" data-member-focus="published"
              onClick={() => openPublished(reference.version)}>Open pinned Function</Button>}
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

export function WorkflowPackageReview({ projectId, draftId, savedRevision }: { projectId: string; draftId: string; savedRevision: string }) {
  const [open, setOpen] = useState(false);
  const [revision, setRevision] = useState(savedRevision);
  const [selection, select] = useState("");
  const preview = usePackagePreview(projectId, draftId, revision, open);
  const member = preview.data?.members.find(({ key }) => key === selection) ?? preview.data?.members[0];
  return <WorkflowBuilderDialog title="Review workflow package" description="Inspect the changes, dependencies, and requirements of a saved revision."
    wide open={open} onOpenChange={(next) => {
      if (next) setRevision(savedRevision);
      setOpen(next);
    }} trigger={<Button type="button" variant="outline">
      <FileDiff size={15} aria-hidden /> Review changes
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
      {member && (
        <details className="workflow-review-card" key={member.key} open>
          <summary><FileDiff size={16} aria-hidden /> Changes and dependencies</summary>
          <MemberReview member={member} />
        </details>
      )}
    </section>}
  </WorkflowBuilderDialog>;
}
