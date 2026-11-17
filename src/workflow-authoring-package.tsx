import { useEffect, useRef, useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ArrowRight, Check, CircleAlert, FilePlus2, LayoutTemplate, Package as PackageIcon, Plus, Save, Search, SlidersHorizontal, Sparkles, Upload, Workflow, X } from "lucide-react";
import type { Viewport } from "@xyflow/react";
import { Dialog } from "radix-ui";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import { ModelPicker } from "@/components/model-picker";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { availableModelSelection, LIVE_MODEL_PROVIDERS, useLiveModelOptions } from "@/live-models";
import type { WorkspaceProps } from "@/workbench-contract";
import { PackagePublicationPreview, WorkflowPackageGraphEditing, WorkflowPackageReview, usePackagePreview } from "@/workflow-package-review";
import { WorkflowEditorAssistance } from "@/workflow-editor-assistance";
import { WorkflowBuilderDialog } from "@/workflow-builder-dialog";
import { WorkflowExchangeDownload, WorkflowExchangePanel } from "@/workflow-exchange";
import { WorkflowLaunchPicker } from "@/workflow-launch";
import { WorkflowReusableDerivation, type ReusableBody, type ReusableReference } from "@/workflow-reusable";
import type { WorkflowCanvasEdit, WorkflowGraph, WorkflowLayout, WorkflowNodeConfiguration } from "@/workflow-graph-model";
import { defaultPromptContract, promptEndpointForRename, type PromptMember, type PromptPortRename } from "@/workflow-prompt-model";
import "./pipeline-builder-landing.css";

type Version = { kind: string; id: string; revision: string; destination: "project" | "library" };
type Member = { key: string; name: string; body: ReusableBody; destination?: string; base?: Version | null; references?: Record<string, ReusableReference> };
type Package = { name: string; members: Member[] };
type Artifact = { version: Version; name: string; blockers: string[]; body: Member["body"]; references: Record<string, ReusableReference> };
type CatalogArtifact = Pick<Artifact, "version" | "name" | "blockers">;
type Publication = {
  id: string;
  reviewedRevision: string;
  originatingRunId: string | null;
  members: { key: string; artifact: Artifact; previous: Version | null }[];
};
export type PackageDraft = {
  id: string; revision: string; conversationId: string | null; package: Package;
  publication: Publication | null;
  layouts?: Record<string, WorkflowLayout>;
  request: { state: string; runId: string | null; jobId?: string; diagnostic: string | null } | null;
};
export type PackageSummary = { id: string; revision: string; name: string; memberCount: number; published: boolean };
type FactoryTemplate = { id: string; name: string; revision: number; members: string[] };
type PublicationResult =
  | { status: "published"; publication: Publication }
  | { status: "stale_base"; member: string; current: Artifact };

const providers = ["codex", "claude"] as const;
const packagesKey = (projectId: string) => ["workflow-packages", projectId];
const packageKey = (projectId: string, draftId: string) => [...packagesKey(projectId), draftId];
const pending = (draft?: PackageDraft) => !!draft?.request
  && ["queued", "starting", "running", "waiting_for_input", "paused"].includes(draft.request.state);
export const packageDraftTarget = (draftId: string) => `authoring:package:${draftId}`;

function packageGraphs(text: string) {
  try {
    const parsed = JSON.parse(text) as Package;
    if (!Array.isArray(parsed.members)) return [];
    return parsed.members.flatMap((member) => {
      const graph = member?.body?.graph;
      if (!graph || typeof member.key !== "string" || typeof member.name !== "string" || typeof graph.name !== "string" || !Array.isArray(graph.nodes) || !Array.isArray(graph.wires)
        || !graph.nodes.every((node) => typeof node?.id === "string" && typeof node.kind === "string" && typeof node.configuration?.kind === "string")
        || !graph.wires.every((wire) => typeof wire?.from?.node === "string" && typeof wire.from.port === "string"
          && typeof wire?.to?.node === "string" && typeof wire.to.port === "string")) return [];
      return [{ key: member.key, name: member.name, graph, kind: member.body.kind, references: member.references ?? {},
        inputs: member.body.inputs, outputs: member.body.outputs }];
    });
  } catch {
    return [];
  }
}

function packagePromptMembers(members: Member[]): PromptMember[] {
  return members.flatMap((member) => {
    const prompt = member?.body?.prompt;
    if (member?.body?.kind !== "prompt" || typeof member.key !== "string" || typeof member.name !== "string" || !prompt
      || typeof prompt.text !== "string" || typeof prompt.executor !== "string" || !prompt.inputs || !prompt.outputs
      || !Array.isArray(prompt.routes) || !Array.isArray(prompt.delegates)) return [];
    return [{ ...member, body: { kind: "prompt", prompt } }];
  });
}

function unconfiguredStageModels(text: string) {
  try {
    const draft = JSON.parse(text) as Package;
    if (!Array.isArray(draft?.members)) return null;
    const stages: WorkflowNodeConfiguration[] = [];
    function add(configuration?: WorkflowNodeConfiguration) {
      if (configuration?.kind !== "agent_role" || configuration.model != null) return;
      const role = configuration.requirements as { boundRole?: unknown; roleTemplate?: unknown } | undefined;
      if (!role?.boundRole && !role?.roleTemplate) stages.push(configuration);
    }
    for (const member of draft.members) {
      if (member?.body?.kind === "preset" && !member.references?.["$preset"]) {
        add(member.body.configuration);
      }
      const graph = member?.body?.graph;
      if (Array.isArray(graph?.nodes)) {
        for (const node of graph.nodes) {
          if (node?.id && !Object.hasOwn(member.references ?? {}, node.id)) add(node.configuration);
        }
      }
    }
    return { draft, stages };
  } catch {
    return null;
  }
}

export function PipelineBuilderLanding({ checkout, openWorkspace, drafts, authoring }: Pick<WorkspaceProps, "checkout" | "openWorkspace"> & {
  drafts?: ReactNode;
  authoring?: ReactNode;
}) {
  const { projectId } = checkout;
  const client = useQueryClient();
  const packages = useQuery({
    queryKey: packagesKey(projectId),
    queryFn: () => invoke<PackageSummary[]>("list_workflow_packages", { projectId }),
  });
  const { copy, feedback } = useContextActionFeedback();
  function openDraft(draft: PackageDraft) {
    client.setQueryData(packageKey(projectId, draft.id), draft);
    void client.invalidateQueries({ queryKey: packagesKey(projectId), exact: true });
    openWorkspace("pipelines", packageDraftTarget(draft.id));
  }
  const [creatingPipeline, showCreatePipeline] = useState(false);
  const [pipelineName, setPipelineName] = useState("");
  const create = useMutation({
    mutationFn: (name: string | null) => {
      const packageDocument: Package = name === null ? { name: "New workflow package", members: [] } : {
        name,
        members: [{
          key: "pipeline", name, destination: "project",
          body: { kind: "pipeline", graph: { name, nodes: [{ id: "start", kind: "typed_start", configuration: { kind: "typed_start", fields: [] } }], wires: [] } },
        }],
      };
      return invoke<PackageDraft>("create_workflow_package", { projectId, conversationId: null, package: packageDocument });
    },
    onSuccess: (draft) => { showCreatePipeline(false); openDraft(draft); },
  });
  return <section aria-label="Pipeline builder" className="pipeline-builder-landing">
    <div className="pipeline-builder-content">
      <header className="pipeline-builder-intro">
        <span className="pipeline-builder-icon"><Workflow size={23} aria-hidden /></span>
        <div>
          <h2>Build a pipeline</h2>
          <p>Connect steps on the canvas, test the workflow, and publish a reviewed revision.</p>
        </div>
      </header>
      <div className="pipeline-builder-starts">
        <article className="pipeline-builder-start" data-primary="true">
          <span className="pipeline-builder-icon"><FilePlus2 size={18} aria-hidden /></span>
          <div><h3>From scratch</h3><p>Open the canvas with a start node and add the steps you need.</p></div>
          <Dialog.Root open={creatingPipeline} onOpenChange={(open) => {
            if (create.isPending) return;
            if (open) { setPipelineName(""); create.reset(); }
            showCreatePipeline(open);
          }}>
            <Dialog.Trigger asChild><Button type="button"><Plus size={16} aria-hidden /> New pipeline</Button></Dialog.Trigger>
            <BuilderLandingDialog title="New pipeline" icon={<FilePlus2 size={20} aria-hidden />}
              description="Name your pipeline to open it on the canvas." busy={create.isPending}>
              <form className="pipeline-form pipeline-builder-dialog-form" onSubmit={(event) => {
                event.preventDefault();
                if (pipelineName.trim() && !create.isPending) create.mutate(pipelineName.trim());
              }}>
                <label>Pipeline name<input autoFocus required maxLength={120} value={pipelineName} disabled={create.isPending}
                  onChange={(event) => setPipelineName(event.currentTarget.value)} /></label>
                {create.error && <p role="alert">{String(create.error)}</p>}
                <div className="pipeline-actions justify-end">
                  <Dialog.Close asChild><Button variant="outline" type="button" disabled={create.isPending}>Cancel</Button></Dialog.Close>
                  <Button type="submit" disabled={!pipelineName.trim() || create.isPending}>Create pipeline</Button>
                </div>
              </form>
            </BuilderLandingDialog>
          </Dialog.Root>
        </article>
        <article className="pipeline-builder-start">
          <span className="pipeline-builder-icon"><LayoutTemplate size={18} aria-hidden /></span>
          <div><h3>From a template</h3><p>Choose a workflow with steps and connections already set up.</p></div>
          <WorkflowFactoryTemplates key={projectId} projectId={projectId} openDraft={openDraft} />
        </article>
        {authoring && <article className="pipeline-builder-start">
          <span className="pipeline-builder-icon"><Sparkles size={18} aria-hidden /></span>
          <div><h3>AI-assisted</h3><p>Describe a pipeline or changes to an existing revision.</p></div>
          {authoring}
        </article>}
      </div>
      <div className="pipeline-builder-saved">
        {drafts}
        <section aria-label="Workflow packages" className="pipeline-builder-card">
          <header className="pipeline-builder-card-heading">
            <span className="pipeline-builder-icon"><PackageIcon size={17} aria-hidden /></span>
            <div><h3>Workflow packages</h3><p>Group pipelines and reusable artifacts.</p></div>
            <Button type="button" variant="outline" disabled={create.isPending} onClick={() => create.mutate(null)}>
              <Plus size={15} aria-hidden /> New workflow package
            </Button>
          </header>
          <div className="pipeline-builder-card-body">
            {packages.isPending && <p role="status">Loading workflow packages…</p>}
            {(packages.error ?? (!creatingPipeline && create.error)) && <p role="alert">{String(packages.error ?? create.error)}</p>}
            {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
            {packages.data?.length === 0 && <p className="pipeline-builder-empty">No workflow packages yet.</p>}
            {!!packages.data?.length && <ul className="workflow-draft-list">
              {packages.data.map((draft) => <li key={draft.id}>
                <ContextActions label={draft.name} className="workflow-draft-menu" groups={[[
                  { label: "Open package", run: () => openWorkspace("pipelines", packageDraftTarget(draft.id)) },
                  { label: "Copy package ID", run: () => copy(draft.id) },
                  { label: "Copy draft revision", run: () => copy(draft.revision) },
                ]]}><div><strong>{draft.name}</strong><p>{draft.memberCount} {draft.memberCount === 1 ? "artifact" : "artifacts"} · {draft.published ? "Published" : "Draft"}</p></div></ContextActions>
                <Button type="button" variant="outline" onClick={() => openWorkspace("pipelines", packageDraftTarget(draft.id))}>
                  Open package <ArrowRight size={15} aria-hidden />
                </Button>
              </li>)}
            </ul>}
            <WorkflowExchangePanel key={projectId} projectId={projectId} openWorkspace={openWorkspace} />
          </div>
        </section>
      </div>
    </div>
  </section>;
}

function BuilderLandingDialog({ title, description, icon, busy = false, children }: {
  title: string; description: string; icon: ReactNode; busy?: boolean; children: ReactNode;
}) {
  return <Dialog.Portal>
    <Dialog.Overlay className="pipeline-builder-overlay" />
    <Dialog.Content className="pipeline-builder-dialog">
      <header className="pipeline-builder-dialog-heading">
        <span className="pipeline-builder-icon">{icon}</span>
        <div><Dialog.Title>{title}</Dialog.Title><Dialog.Description>{description}</Dialog.Description></div>
        <Dialog.Close asChild><Button type="button" variant="ghost" className="ui-icon-button"
          aria-label={"Close " + title.toLowerCase()} disabled={busy}><X size={17} aria-hidden /></Button></Dialog.Close>
      </header>
      <div className="pipeline-builder-dialog-body">{children}</div>
    </Dialog.Content>
  </Dialog.Portal>;
}

function WorkflowFactoryTemplates({ projectId, openDraft }: { projectId: string; openDraft: (draft: PackageDraft) => void }) {
  const [open, setOpen] = useState(false);
  const templates = useQuery({
    queryKey: ["workflow-factory-templates"],
    queryFn: () => invoke<FactoryTemplate[]>("workflow_factory_templates"),
    enabled: open,
  });
  const { copy, feedback } = useContextActionFeedback();
  const create = useMutation({
    mutationFn: ({ id, revision }: FactoryTemplate) => invoke<PackageDraft>("create_workflow_factory_draft", {
      projectId, template: id, revision,
    }),
    onSuccess: (draft) => { setOpen(false); openDraft(draft); },
    onError: () => templates.refetch(),
  });
  const error = templates.error ?? create.error;
  return <Dialog.Root open={open} onOpenChange={(next) => { if (!create.isPending) setOpen(next); }}>
    <Dialog.Trigger asChild><Button type="button" variant="outline"><LayoutTemplate size={16} aria-hidden /> Start from a template</Button></Dialog.Trigger>
    <BuilderLandingDialog title="Workflow templates" icon={<LayoutTemplate size={20} aria-hidden />}
      description="Choose a template to open it as an editable draft in this project." busy={create.isPending}>
      {error && <p role="alert">{String(error)}</p>}
      {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
      {!error && templates.isPending && <p role="status">Loading workflow templates…</p>}
      {templates.data?.length === 0 && <p className="pipeline-builder-empty">No templates are available.</p>}
      {!!templates.data?.length && <ul className="workflow-draft-list">
        {templates.data.map((entry) => <li key={entry.id}>
          <ContextActions label={entry.name} className="workflow-draft-menu" groups={[[
            { label: "Create draft…", disabled: create.isPending, run: () => create.mutate(entry) },
            { label: "Copy template ID", run: () => copy(entry.id) },
            { label: "Copy template revision", run: () => copy(String(entry.revision)) },
          ]]}><div><strong>{entry.name}</strong>
            {entry.members.length > 1 && <p>{entry.members.join(" · ")}</p>}
          </div></ContextActions>
          <Button type="button" variant="outline" disabled={create.isPending} aria-label={`Create ${entry.name} draft`}
            onClick={() => create.mutate(entry)}>Create draft</Button>
        </li>)}
      </ul>}
    </BuilderLandingDialog>
  </Dialog.Root>;
}

export function WorkflowPackageEditor({ draftId, checkout, openActivity, openWorkspace, openQueueJob }: WorkspaceProps & { draftId: string }) {
  const { projectId, checkoutId } = checkout;
  const client = useQueryClient();
  const query = useQuery({
    queryKey: packageKey(projectId, draftId),
    queryFn: () => invoke<PackageDraft>("inspect_workflow_package", { projectId, draftId }),
    refetchInterval: ({ state }) => pending(state.data) ? 1_000 : false,
  });
  const [editBase, holdEdits] = useState<PackageDraft>();
  const [graphMember, selectGraphMember] = useState("");
  const viewports = useRef(new Map<string, Viewport>());
  const draft = editBase ?? query.data;
  function update(next: PackageDraft) {
    holdEdits(undefined);
    client.setQueryData(packageKey(projectId, draftId), next);
    void client.invalidateQueries({ queryKey: packagesKey(projectId), exact: true });
    if (next.conversationId && next.conversationId !== draft?.conversationId) {
      void client.invalidateQueries({ queryKey: ["conversations"] });
    }
    if (next.publication) {
      void client.invalidateQueries({ queryKey: ["workflow-authoring-bases", projectId] });
      void client.invalidateQueries({ queryKey: ["workflow-package-catalog", projectId] });
    }
  }
  const sourceActions = <>
      {draft?.conversationId && <Button variant="outline" onClick={() => openWorkspace("conversations", draft.conversationId!)}>Open source conversation</Button>}
      {draft?.request?.runId && <Button variant="outline" onClick={() => openActivity(draft.request!.runId!)}>Open authoring run</Button>}
      {openQueueJob && draft?.request?.jobId && !draft.request.runId &&
        <Button variant="outline" onClick={() => openQueueJob(draft.request!.jobId!)}>Open queued authoring job</Button>}
  </>;
  return <section aria-label="Workflow package editor" className="workflow-builder">
    {!draft && <header className="workflow-builder-header"><h1>Workflow package</h1></header>}
    {query.isPending && <p role="status">Loading package revision…</p>}
    {query.error && <p role="alert">{String(query.error)}</p>}
    {draft && <PackageRevision key={draft.revision} projectId={projectId} checkoutId={checkoutId} draft={draft} update={update} holdEdits={holdEdits}
      revisionConflict={!!editBase && !!query.data && editBase.revision !== query.data.revision} openWorkspace={openWorkspace}
      graphMember={graphMember} selectGraphMember={selectGraphMember} viewports={viewports.current} sourceActions={sourceActions} />}
  </section>;
}

function PackageRevision({ draft, projectId, checkoutId, update, holdEdits, revisionConflict, openWorkspace, graphMember, selectGraphMember, viewports, sourceActions }: {
  draft: PackageDraft; projectId: string; checkoutId: string; update: (draft: PackageDraft) => void;
  holdEdits: (base: PackageDraft | undefined) => void;
  revisionConflict: boolean;
  openWorkspace: WorkspaceProps["openWorkspace"];
  graphMember: string;
  selectGraphMember: (key: string) => void;
  viewports: Map<string, Viewport>;
  sourceActions: ReactNode;
}) {
  const client = useQueryClient();
  const [text, setText] = useState(() => JSON.stringify(draft.package, null, 2));
  const [canvasByMember, setCanvases] = useState<Record<string, WorkflowCanvasEdit>>({});
  const promptArchive = useRef(new Map<string, Member>());
  const currentText = useRef(text);
  currentText.current = text;
  const [confirmation, setConfirmation] = useState(false);
  const [packageOpen, setPackageOpen] = useState(false);
  const [authoringOpen, setAuthoringOpen] = useState(false);
  const [reusableMember, setReusableMember] = useState("");
  const [feedback, setFeedback] = useState("");
  const [selection, setSelection] = useState<ModelSelection | null>(null);
  const [showStageModels, setShowStageModels] = useState(false);
  const [stageModelSelection, setStageModelSelection] = useState<ModelSelection | null>(null);
  const unconfigured = unconfiguredStageModels(text)?.stages.length ?? 0;
  const stageModels = useLiveModelOptions(LIVE_MODEL_PROVIDERS, showStageModels && unconfigured > 0 && !draft.publication);
  const stageModel = availableModelSelection(stageModels.options, stageModelSelection);
  const models = useLiveModelOptions(providers, authoringOpen && !draft.publication);
  const model = availableModelSelection(models.options, selection);
  const scoped = { projectId, draftId: draft.id };
  const save = useMutation({
    mutationFn: () => invoke<PackageDraft>("save_workflow_package", {
      ...scoped, expectedRevision: draft.revision, package: JSON.parse(text) as unknown,
      ...(Object.keys(canvasByMember).length > 0 && { canvasByMember }),
    }),
    onSuccess: (next) => {
      setText(JSON.stringify(next.package, null, 2));
      setCanvases({});
      update(next);
    },
  });
  const request = useMutation({
    mutationFn: () => invoke<PackageDraft>("request_workflow_package_changes", {
      ...scoped, checkoutId, expectedRevision: draft.revision, selection: model, effort: null, request: feedback.trim(),
    }),
    onSuccess: (next) => { setFeedback(""); update(next); },
  });
  const publish = useMutation({
    mutationFn: () => invoke<PublicationResult>("publish_reviewed_workflow_package", {
      ...scoped, reviewedRevision: draft.revision,
    }),
    onSuccess: (result) => {
      setConfirmation(false);
      if (result.status === "published") update({ ...draft, publication: result.publication });
    },
  });
  const dirty = text !== JSON.stringify(draft.package, null, 2) || Object.keys(canvasByMember).length > 0;
  const review = usePackagePreview(projectId, draft.id, draft.revision, confirmation);
  useEffect(() => holdEdits(dirty ? draft : undefined), [holdEdits, dirty, draft]);
  const busy = pending(draft) || save.isPending || publish.isPending || request.isPending;
  const stale = publish.data?.status === "stale_base" ? publish.data : null;
  const graphs = packageGraphs(text);
  const draftMembers = (() => {
    try {
      const members: unknown = JSON.parse(text)?.members;
      return Array.isArray(members) ? members as Member[] : [];
    } catch {
      return [];
    }
  })();
  const promptMembers = packagePromptMembers(draftMembers);
  const savedGraphs = packageGraphs(JSON.stringify(draft.package));
  const savedReuseGraph = savedGraphs.find(({ key }) => key === (reusableMember || graphMember)) ?? savedGraphs[0];
  const draftPresets = Object.fromEntries(draft.package.members.filter(({ body }) => body.kind === "preset")
    .map(({ key, body }) => [key, body]));
  function openDerived(next: PackageDraft) {
    client.setQueryData(packageKey(projectId, next.id), next);
    void client.invalidateQueries({ queryKey: packagesKey(projectId), exact: true });
    openWorkspace("pipelines", packageDraftTarget(next.id));
  }
  function editGraph(key: string, graph: WorkflowGraph, references?: Record<string, ReusableReference>, promptKeys: string[] = []) {
    const current = JSON.parse(currentText.current) as Package;
    const previous = current.members.find((member) => member.key === key);
    if (!previous) return;
    const removedRefs = new Set(Object.values(previous.references ?? {}).flatMap((ref) => ref.source === "draft" ? [ref.key] : []));
    const members = current.members.map((item) => item.key === key
      ? { ...item, ...(references && { references }), body: { ...item.body, graph } } : item);
    const used = new Set(members.flatMap((member) =>
      Object.values(member.references ?? {}).flatMap((ref) => ref.source === "draft" ? [ref.key] : [])));
    for (const promptKey of used) {
      if (members.some((item) => item.key === promptKey)) continue;
      const archived = promptArchive.current.get(promptKey);
      if (archived) members.push(archived);
      else if (promptKeys.includes(promptKey)) members.push({
        key: promptKey, name: "Prompt", destination: "project", body: { kind: "prompt", prompt: defaultPromptContract() }, references: {},
      });
    }
    const retained = members.filter((member) => {
      if (member.body.kind !== "prompt" || member.destination === "library" || member.base
        || used.has(member.key) || !removedRefs.has(member.key)) return true;
      promptArchive.current.set(member.key, member);
      return false;
    });
    setText(JSON.stringify({ ...current, members: retained }, null, 2));
  }
  function editPrompt(prompt: PromptMember, rename?: PromptPortRename) {
    const current = JSON.parse(currentText.current) as Package;
    const members = current.members.map((member) => {
      if (member.key === prompt.key) return { ...member, ...prompt };
      const graph = member.body.graph;
      if (!graph || !rename) return member;
      const affected = new Set(Object.entries(member.references ?? {}).flatMap(([id, ref]) =>
        ref.source === "draft" && ref.key === prompt.key ? [id] : []));
      if (!affected.size) return member;
      return { ...member, body: { ...member.body, graph: { ...graph, wires: graph.wires.map((wire) => rename.direction === "inputs"
        ? { ...wire, to: promptEndpointForRename(wire.to, affected, rename) }
        : { ...wire, from: promptEndpointForRename(wire.from, affected, rename) }),
      } } };
    });
    setText(JSON.stringify({ ...current, members }, null, 2));
  }
  function applyStageModel() {
    if (!stageModel) return;
    const missing = unconfiguredStageModels(currentText.current);
    if (!missing?.stages.length) return;
    for (const stage of missing.stages) stage.model = stageModel;
    setText(JSON.stringify(missing.draft, null, 2));
  }
  const title = <div className="workflow-builder-title">
    <span className="workflow-builder-icon"><Workflow size={18} aria-hidden /></span>
    <div><h1 title={draft.package.name}>{draft.package.name}</h1>
      <div className="workflow-builder-state" data-dirty={dirty || undefined}>
        {dirty ? <span className="workflow-builder-dot" /> : <Check size={13} aria-hidden />}
        <span>{draft.publication ? "Published" : dirty ? "Unsaved changes" : "Saved draft"}</span>
        <code title={draft.revision}>{draft.revision.slice(0, 12)}</code>
      </div>
    </div>
  </div>;
  if (draft.publication) {
    return <>
      <header className="workflow-builder-header">{title}<div className="pipeline-actions">{sourceActions}</div></header>
      <section aria-label="Package publication receipt" className="workflow-builder-results pipeline-stack">
      <div className="workflow-published-summary">
      <p role="status">Published package revision <code>{draft.publication.reviewedRevision}</code></p>
      <p>Publication receipt <code>{draft.publication.id}</code></p>
      {draft.publication.originatingRunId && <p>Originating run <code>{draft.publication.originatingRunId}</code></p>}
      <div className="pipeline-actions">
        <WorkflowExchangeDownload projectId={projectId} versions={draft.publication.members.map(({ artifact }) => artifact.version)} />
        <WorkflowPackageReview projectId={projectId} draftId={draft.id} savedRevision={draft.publication.reviewedRevision} />
      </div>
      </div>
      <ul className="pipeline-stack">
        {draft.publication.members.map(({ key, artifact }) => <li key={key} className="pipeline-evidence">
          <strong>{artifact.name}</strong> · {artifact.version.kind.replaceAll("_", " ")} · {artifact.version.destination}
          <p className="break-all"><code>{artifact.version.id}</code> · <code>{artifact.version.revision}</code></p>
          <WorkflowEditorAssistance projectId={projectId} checkoutId={checkoutId} artifact={artifact} openWorkspace={openWorkspace} />
          {artifact.version.kind === "pipeline" && artifact.version.destination === "project" && !artifact.blockers.length &&
            <WorkflowLaunchPicker projectId={projectId} checkoutId={checkoutId} preferred={artifact.version}
              openWorkspace={openWorkspace} />}
          {artifact.body.graph && <WorkflowReusableDerivation projectId={projectId} graph={artifact.body.graph}
            source={{ type: "published", version: artifact.version }} disabled={false} onSaved={openDerived} />}
          {artifact.blockers.map((blocker) => <p key={blocker}>{blocker}</p>)}
          {!!Object.keys(artifact.references).length && <details><summary>Pinned references</summary>
            <pre className="max-h-48 overflow-auto whitespace-pre-wrap text-xs">{JSON.stringify(artifact.references, null, 2)}</pre>
          </details>}
        </li>)}
      </ul>
      </section>
    </>;
  }
  return <>
    <header className="workflow-builder-header">
      {title}
      <div className="pipeline-actions">
        <Button type="button" aria-label="Save package revision" title="Save package revision" disabled={!dirty || busy} onClick={() => save.mutate()}>
          <Save size={15} aria-hidden /> <span className="workflow-builder-action-label">Save package revision</span>
        </Button>
        <Dialog.Root open={confirmation} onOpenChange={(open) => { if (!publish.isPending) setConfirmation(open); }}>
          <Dialog.Trigger asChild><Button type="button" variant="outline" aria-label="Review package publication" title="Review package publication"
            disabled={dirty || busy || draft.package.members.length === 0}>
            <Upload size={15} aria-hidden /> <span className="workflow-builder-action-label">Review package publication</span>
          </Button></Dialog.Trigger>
          <Dialog.Portal>
            <Dialog.Overlay className="fixed inset-0 z-50 bg-background/75 backdrop-blur-sm" />
            <Dialog.Content className="fixed left-1/2 top-1/2 z-50 grid max-h-[calc(100dvh-2rem)] w-[min(45rem,calc(100%-2rem))] -translate-x-1/2 -translate-y-1/2 gap-4 overflow-y-auto rounded-lg border border-border bg-card p-5 shadow-lg">
              <Dialog.Title className="text-lg font-semibold">Publish this package revision</Dialog.Title>
              <Dialog.Description>Publish all {draft.package.members.length} artifacts from the saved revision in one transaction.</Dialog.Description>
              <code className="break-all">{draft.revision}</code>
              <details className="pipeline-evidence"><summary>Artifacts in this publication ({draft.package.members.length})</summary>
                <ul className="workflow-publication-members">{draft.package.members.map((member) => <li key={member.key}>{member.name} · {member.destination ?? "project"}</li>)}</ul>
              </details>
              {review.isPending && <p role="status">Loading changes and dependencies…</p>}
              {review.error && <p role="alert">{String(review.error)}</p>}
              {review.data?.revision === draft.revision && <PackagePublicationPreview preview={review.data} />}
              {publish.error && <p role="alert">{String(publish.error)}</p>}
              <div className="flex justify-end gap-2">
                <Dialog.Close asChild><Button type="button" variant="outline" disabled={publish.isPending}>Cancel</Button></Dialog.Close>
                <Button type="button" disabled={busy || dirty || review.data?.revision !== draft.revision} onClick={() => publish.mutate()}>Confirm package publication</Button>
              </div>
            </Dialog.Content>
          </Dialog.Portal>
        </Dialog.Root>
      </div>
    </header>
    {revisionConflict && <p role="alert" className="workflow-builder-warning">A newer revision is available. Your edits remain here.</p>}
    {stale && <p role="alert" className="workflow-builder-warning">The published base changed. Open Compare published base to review it.</p>}
    {save.error && <p role="alert" className="workflow-builder-warning">{String(save.error)}</p>}
    <WorkflowPackageGraphEditing projectId={projectId} checkoutId={checkoutId} draftId={draft.id} savedRevision={draft.revision}
      graphs={graphs} draftPresets={draftPresets} layouts={draft.layouts} canvasByMember={canvasByMember}
      promptMembers={promptMembers} memberKeys={draftMembers.flatMap((member) => typeof member?.key === "string" ? [member.key] : [])}
      draftRoles={draftMembers.flatMap((member) => member?.body?.kind === "role" && typeof member?.key === "string" && typeof member?.name === "string"
        ? [{ name: member.name + " · This draft", reference: { source: "draft" as const, key: member.key } }] : [])}
      editPrompt={editPrompt}
      selection={graphMember} select={selectGraphMember} viewports={viewports}
      deriveDisabled={busy || dirty || revisionConflict} onDerived={update}
      changeCanvas={(key, canvas) => setCanvases((previous) => {
        const next = { ...previous };
        if (JSON.stringify(canvas.layout) === JSON.stringify(draft.layouts?.[key] ?? {})
          && Object.keys(canvas.updateDefinitions).length === 0 && !canvas.definitionCopies) delete next[key];
        else next[key] = canvas;
        return next;
      })}
      disabled={busy} edit={editGraph} />
    <footer className="workflow-builder-tools" aria-label="Package tools">
      <WorkflowPackageReview projectId={projectId} draftId={draft.id} savedRevision={draft.revision} />
      {unconfigured > 0 && <WorkflowBuilderDialog title="Configure stage models"
        description="Apply one model to stages that have no model or role assigned."
        open={showStageModels} onOpenChange={setShowStageModels}
        trigger={<Button variant="outline"><SlidersHorizontal size={15} aria-hidden /> Configure stage models</Button>}>
        <ModelPicker label="Pipeline stage model" options={stageModels.options} loading={stageModels.loading} disabled={busy}
          selection={{ mode: "single", selected: stageModelSelection, onChange: setStageModelSelection }} />
        {stageModels.errors[0] && <p role="alert">{String(stageModels.errors[0])}</p>}
        <Button type="button" variant="outline" className="w-fit" disabled={!stageModel || busy} onClick={applyStageModel}>
          Apply to {unconfigured} unconfigured {unconfigured === 1 ? "stage" : "stages"}
        </Button>
      </WorkflowBuilderDialog>}
      <WorkflowBuilderDialog title="Package details" description="Browse saved artifacts, reuse a graph, or edit the package document."
        wide open={packageOpen} onOpenChange={setPackageOpen}
        trigger={<Button variant="ghost"><PackageIcon size={15} aria-hidden /> Package details</Button>}>
        <div className="workflow-package-source pipeline-actions"><code>{draft.revision}</code>{sourceActions}</div>
        <PackageMembers members={draft.package.members} graphs={[...savedGraphs, ...packagePromptMembers(draft.package.members)].map(({ key }) => key)}
          onOpen={(key) => { selectGraphMember(key); setPackageOpen(false); }} />
        {savedReuseGraph && <section className="workflow-package-reuse pipeline-form pipeline-stack" aria-label={`Reuse ${savedReuseGraph.name}`}>
          {savedGraphs.length > 1 && <label>Saved graph<select value={savedReuseGraph.key} onChange={(event) => setReusableMember(event.currentTarget.value)}>
            {savedGraphs.map(({ key, name }) => <option key={key} value={key}>{name}</option>)}
          </select></label>}
          <WorkflowReusableDerivation key={savedReuseGraph.key} projectId={projectId} graph={savedReuseGraph.graph}
            source={{ type: "package_revision", draftId: draft.id, revision: draft.revision, memberKey: savedReuseGraph.key }}
            disabled={busy || dirty} onSaved={update} />
        </section>}
        <details className="pipeline-evidence">
          <summary>Edit package</summary>
          <PublishedArtifactPicker projectId={projectId} checkoutId={checkoutId} openWorkspace={openWorkspace} disabled={busy} add={(artifact) => {
            const parsed = JSON.parse(currentText.current) as Package;
            if (!Array.isArray(parsed.members)) throw new Error("The package needs a members array.");
            const version = artifact.version;
            if (parsed.members.some((member) => member.base?.id === version.id && member.base.kind === version.kind
              && member.base.destination === version.destination)) throw new Error("This artifact is already in the package.");
            const key = `${version.kind}-${version.id}`;
            if (parsed.members.some((member) => member.key === key)) throw new Error("Choose a distinct key for the existing package member.");
            setText(JSON.stringify({ ...parsed, members: [...parsed.members, {
              key, name: artifact.name, destination: version.destination, base: version, body: artifact.body, references: artifact.references,
            }] }, null, 2));
          }} />
          <textarea aria-label="Package document" autoCapitalize="off" spellCheck={false} className="mt-2 min-h-64 w-full rounded border border-border bg-background p-2 font-mono text-xs"
            value={text} disabled={busy} onChange={(event) => setText(event.currentTarget.value)} />
        </details>
        {save.error && <p role="alert">{String(save.error)}</p>}
        <Button className="w-fit" type="button" disabled={!dirty || busy} onClick={() => save.mutate()}><Save size={15} aria-hidden /> Save package revision</Button>
      </WorkflowBuilderDialog>
      <WorkflowBuilderDialog title="Edit with AI" description="Request changes to the saved package revision."
        open={authoringOpen} onOpenChange={setAuthoringOpen} trigger={<Button variant="ghost"><Sparkles size={15} aria-hidden /> Edit with AI</Button>}>
        {draft.request && <p role="status">Authoring {draft.request.state.replaceAll("_", " ")}{draft.request.diagnostic && ` · ${draft.request.diagnostic}`}</p>}
        {dirty && <p>Save the current edits before requesting AI changes.</p>}
        <ModelPicker label="Package authoring model" options={models.options} loading={models.loading} disabled={busy}
          selection={{ mode: "single", selected: selection, onChange: setSelection }} />
        <label>Package authoring request<textarea rows={3} className="w-full rounded border border-border bg-background p-2"
          value={feedback} disabled={busy} onChange={(event) => setFeedback(event.currentTarget.value)} /></label>
        <Button type="button" variant="outline" disabled={!model || !feedback.trim() || dirty || busy}
          onClick={() => request.mutate()}>Request package changes with AI</Button>
        {request.error && <p role="alert">{String(request.error)}</p>}
      </WorkflowBuilderDialog>
      {stale && <WorkflowBuilderDialog title="Published version changed" description={`Review the current published base for ${stale.member}.`}
        wide trigger={<Button variant="outline"><CircleAlert size={15} aria-hidden /> Compare published base</Button>}>
        <section aria-label="Current published package artifact" className="pipeline-stack">
          <p role="alert">The published base for {stale.member} changed. The package draft is preserved.</p>
          <p>{stale.current.name} · <code>{stale.current.version.revision}</code></p>
          <details><summary>Compare the current artifact</summary>
            <pre className="max-h-64 overflow-auto whitespace-pre-wrap text-xs">{JSON.stringify(stale.current.body, null, 2)}</pre>
          </details>
        </section>
      </WorkflowBuilderDialog>}
    </footer>
  </>;
}

function PackageMembers({ members, graphs, onOpen }: { members: Member[]; graphs: string[]; onOpen: (key: string) => void }) {
  const [filter, setFilter] = useState("");
  const search = filter.trim().toLowerCase();
  const visible = members.filter((member) => [member.name, member.body.kind, member.destination ?? "project"]
    .some((value) => value.toLowerCase().includes(search)));
  return <section className="workflow-package-members">
    <header>
      <h3>Saved artifacts <span>{members.length}</span></h3>
      {members.length > 5 && <label className="work-search"><Search size={15} aria-hidden />
        <input type="search" aria-label="Filter package artifacts" placeholder="Filter artifacts…" value={filter}
          onChange={(event) => setFilter(event.currentTarget.value)} /></label>}
    </header>
    {visible.length === 0 ? <p role="status">{search ? "No matching artifacts." : "No saved artifacts."}</p> :
      <ul aria-label="Saved package members">{visible.map((member) => <li key={member.key}>
        <span className="workflow-package-member-icon"><PackageIcon size={15} aria-hidden /></span>
        <div><strong>{member.name}</strong><small>{member.body.kind.replaceAll("_", " ")} · {member.destination ?? "project"}</small>
          {member.base && <code title={member.base.revision}>Base {member.base.revision}</code>}</div>
        {graphs.includes(member.key) && <Button variant="ghost" onClick={() => onOpen(member.key)} aria-label={member.body.kind === "prompt" ? "Open Prompt " + member.name : "Open graph " + member.name}>
          <ArrowRight size={15} aria-hidden />
        </Button>}
      </li>)}</ul>}
  </section>;
}

function PublishedArtifactPicker({ projectId, checkoutId, openWorkspace, disabled, add }: {
  projectId: string; checkoutId: string; openWorkspace: WorkspaceProps["openWorkspace"]; disabled: boolean; add: (artifact: Artifact) => void;
}) {
  const [enabled, setEnabled] = useState(false);
  const [selected, setSelected] = useState("");
  const catalog = useQuery({
    queryKey: ["workflow-package-catalog", projectId],
    queryFn: () => invoke<CatalogArtifact[]>("workflow_package_catalog", { projectId }),
    enabled,
  });
  const choice = catalog.data?.find(({ version }) => JSON.stringify(version) === selected);
  const load = useMutation({
    mutationFn: async (version: Version) => {
      const artifact = await invoke<Artifact | null>("read_published_workflow_package_artifact", { projectId, version });
      if (!artifact) throw new Error("The selected artifact revision is unavailable.");
      add(artifact);
    },
  });
  return <details className="mt-2" onToggle={(event) => setEnabled(event.currentTarget.open)}>
    <summary>Published artifacts</summary>
    {catalog.error && <p role="alert">{String(catalog.error)}</p>}
    {load.error && <p role="alert">{String(load.error)}</p>}
    {enabled && <div className="pipeline-stack pt-2">
      <label>Published artifact<select className="w-full rounded border border-border bg-background p-2" value={selected}
        disabled={disabled || load.isPending} onChange={(event) => setSelected(event.currentTarget.value)}>
        <option value="">Choose a published revision</option>
        {catalog.data?.map(({ name, version }) => <option key={JSON.stringify(version)} value={JSON.stringify(version)}>
          {name} · {version.kind.replaceAll("_", " ")} · {version.destination} · {version.revision.slice(0, 10)}
        </option>)}
      </select></label>
      {choice && <div className="pipeline-stack">
        {!!choice.blockers.length && <p>{choice.blockers.join(" ")}</p>}
        <Button type="button" variant="outline" disabled={disabled || load.isPending}
          onClick={() => load.mutate(choice.version)}>Edit this revision in the package</Button>
        <WorkflowEditorAssistance key={selected} projectId={projectId} checkoutId={checkoutId} artifact={choice} openWorkspace={openWorkspace} disabled={disabled || load.isPending} />
        <details><summary>Exact reference</summary>
          <pre className="overflow-auto whitespace-pre-wrap text-xs">{JSON.stringify({ source: "published", version: choice.version }, null, 2)}</pre>
        </details>
      </div>}
    </div>}
  </details>;
}
