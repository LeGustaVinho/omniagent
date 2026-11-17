import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Dialog } from "radix-ui";
import { Button } from "@/components/ui/button";
import { ModelPicker } from "@/components/model-picker";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { availableModelSelection, LIVE_MODEL_PROVIDERS, useLiveModelOptions } from "@/live-models";
import type { WorkspaceProps } from "@/workbench-contract";
import { PackagePublicationPreview, WorkflowPackageGraphEditing, WorkflowPackageReview, usePackagePreview } from "@/workflow-package-review";
import { WorkflowEditorAssistance } from "@/workflow-editor-assistance";
import { WorkflowExchangeDownload, WorkflowExchangePanel } from "@/workflow-exchange";
import { WorkflowLaunchPicker } from "@/workflow-launch";
import { WorkflowReusableDerivation, type ReusableBody, type ReusableReference } from "@/workflow-reusable";
import type { WorkflowCanvasEdit, WorkflowGraph, WorkflowLayout, WorkflowNodeConfiguration } from "@/workflow-graph-model";

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

export function WorkflowPackageList({ checkout, openWorkspace }: Pick<WorkspaceProps, "checkout" | "openWorkspace">) {
  const { projectId } = checkout;
  const client = useQueryClient();
  const packages = useQuery({
    queryKey: packagesKey(projectId),
    queryFn: () => invoke<PackageSummary[]>("list_workflow_packages", { projectId }),
  });
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
  return <section aria-label="Workflow packages" className="pipeline-stack border-t border-border pt-4">
    <div className="flex flex-wrap items-center justify-between gap-2">
      <h2 className="font-semibold">Workflow packages</h2>
      <div className="pipeline-actions">
        <Button type="button" variant="outline" disabled={create.isPending} onClick={() => create.mutate(null)}>New workflow package</Button>
        <Dialog.Root open={creatingPipeline} onOpenChange={(open) => {
          if (create.isPending) return;
          if (open) { setPipelineName(""); create.reset(); }
          showCreatePipeline(open);
        }}>
          <Dialog.Trigger asChild><Button type="button">New pipeline</Button></Dialog.Trigger>
          <Dialog.Portal>
            <Dialog.Overlay className="fixed inset-0 z-50 bg-background/75 backdrop-blur-sm" />
            <Dialog.Content className="fixed left-1/2 top-1/2 z-50 grid w-[min(28rem,calc(100%-2rem))] -translate-x-1/2 -translate-y-1/2 gap-4 rounded-lg border border-border bg-card p-5 shadow-lg">
              <Dialog.Title className="text-lg font-semibold">New pipeline</Dialog.Title>
              <form className="pipeline-form grid gap-3" onSubmit={(event) => {
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
            </Dialog.Content>
          </Dialog.Portal>
        </Dialog.Root>
      </div>
    </div>
    {(packages.error ?? (!creatingPipeline && create.error)) && <p role="alert">{String(packages.error ?? create.error)}</p>}
    <WorkflowFactoryTemplates key={`factory:${projectId}`} projectId={projectId} openDraft={openDraft} />
    <WorkflowExchangePanel key={projectId} projectId={projectId} openWorkspace={openWorkspace} />
    {!!packages.data?.length && <ul className="workflow-draft-list">
      {packages.data.map((draft) => <li key={draft.id}>
        <div><strong>{draft.name}</strong><p>{draft.memberCount} artifacts · {draft.published ? "Published" : "Draft"}</p></div>
        <Button type="button" variant="outline" onClick={() => openWorkspace("pipelines", packageDraftTarget(draft.id))}>Open package</Button>
      </li>)}
    </ul>}
  </section>;
}

function WorkflowFactoryTemplates({ projectId, openDraft }: { projectId: string; openDraft: (draft: PackageDraft) => void }) {
  const [open, setOpen] = useState(false);
  const templates = useQuery({
    queryKey: ["workflow-factory-templates"],
    queryFn: () => invoke<FactoryTemplate[]>("workflow_factory_templates"),
    enabled: open,
  });
  const create = useMutation({
    mutationFn: ({ id, revision }: FactoryTemplate) => invoke<PackageDraft>("create_workflow_factory_draft", {
      projectId, template: id, revision,
    }),
    onSuccess: openDraft,
    onError: () => templates.refetch(),
  });
  const error = templates.error ?? create.error;
  return <details className="pipeline-evidence" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Start from a template</summary>
    {open && <>
      {error && <p role="alert">{String(error)}</p>}
      {!error && templates.isPending && <p role="status">Loading workflow templates…</p>}
      {!!templates.data?.length && <ul className="workflow-draft-list">
        {templates.data.map((entry) => <li key={entry.id}>
          <div><strong>{entry.name}</strong>
            {entry.members.length > 1 && <p>{entry.members.join(" · ")}</p>}
          </div>
          <Button type="button" variant="outline" disabled={create.isPending} aria-label={`Create ${entry.name} draft`}
            onClick={() => create.mutate(entry)}>Create draft</Button>
        </li>)}
      </ul>}
    </>}
  </details>;
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
  return <section aria-label="Workflow package editor" className="pipeline-stack h-full overflow-auto p-4">
    <header className="flex flex-wrap items-center justify-between gap-2">
      <h1 className="text-lg font-semibold">{draft?.package.name ?? "Workflow package"}</h1>
      {draft?.conversationId && <Button variant="outline" onClick={() => openWorkspace("conversations", draft.conversationId!)}>Open source conversation</Button>}
      {draft?.request?.runId && <Button variant="outline" onClick={() => openActivity(draft.request!.runId!)}>Open authoring run</Button>}
      {openQueueJob && draft?.request?.jobId && !draft.request.runId &&
        <Button variant="outline" onClick={() => openQueueJob(draft.request!.jobId!)}>Open queued authoring job</Button>}
    </header>
    {query.isPending && <p role="status">Loading package revision…</p>}
    {query.error && <p role="alert">{String(query.error)}</p>}
    {draft && <PackageRevision key={draft.revision} projectId={projectId} checkoutId={checkoutId} draft={draft} update={update} holdEdits={holdEdits}
      revisionConflict={!!editBase && !!query.data && editBase.revision !== query.data.revision} openWorkspace={openWorkspace} />}
  </section>;
}

function PackageRevision({ draft, projectId, checkoutId, update, holdEdits, revisionConflict, openWorkspace }: {
  draft: PackageDraft; projectId: string; checkoutId: string; update: (draft: PackageDraft) => void;
  holdEdits: (base: PackageDraft | undefined) => void;
  revisionConflict: boolean;
  openWorkspace: WorkspaceProps["openWorkspace"];
}) {
  const client = useQueryClient();
  const [text, setText] = useState(() => JSON.stringify(draft.package, null, 2));
  const [canvasByMember, setCanvases] = useState<Record<string, WorkflowCanvasEdit>>({});
  const currentText = useRef(text);
  currentText.current = text;
  const [confirmation, setConfirmation] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [selection, setSelection] = useState<ModelSelection | null>(null);
  const [showStageModels, setShowStageModels] = useState(false);
  const [stageModelSelection, setStageModelSelection] = useState<ModelSelection | null>(null);
  const unconfigured = unconfiguredStageModels(text)?.stages.length ?? 0;
  const stageModels = useLiveModelOptions(LIVE_MODEL_PROVIDERS, showStageModels && unconfigured > 0 && !draft.publication);
  const stageModel = availableModelSelection(stageModels.options, stageModelSelection);
  const models = useLiveModelOptions(providers, !draft.publication);
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
  const savedGraphs = packageGraphs(JSON.stringify(draft.package));
  const draftPresets = Object.fromEntries(draft.package.members.filter(({ body }) => body.kind === "preset")
    .map(({ key, body }) => [key, body]));
  function openDerived(next: PackageDraft) {
    client.setQueryData(packageKey(projectId, next.id), next);
    void client.invalidateQueries({ queryKey: packagesKey(projectId), exact: true });
    openWorkspace("pipelines", packageDraftTarget(next.id));
  }
  function applyStageModel() {
    if (!stageModel) return;
    const missing = unconfiguredStageModels(currentText.current);
    if (!missing?.stages.length) return;
    for (const stage of missing.stages) stage.model = stageModel;
    setText(JSON.stringify(missing.draft, null, 2));
  }
  if (draft.publication) {
    return <section aria-label="Package publication receipt" className="pipeline-stack">
      <p role="status">Published package revision <code>{draft.publication.reviewedRevision}</code></p>
      <p>Publication receipt <code>{draft.publication.id}</code></p>
      <WorkflowExchangeDownload projectId={projectId} versions={draft.publication.members.map(({ artifact }) => artifact.version)} />
      <WorkflowPackageReview projectId={projectId} draftId={draft.id} savedRevision={draft.publication.reviewedRevision} />
      {draft.publication.originatingRunId && <p>Originating run <code>{draft.publication.originatingRunId}</code></p>}
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
    </section>;
  }
  return <div className="pipeline-stack">
    <div className="pipeline-draft-toolbar">
      <div className="pipeline-draft-revision">
        <span>{dirty ? "Unsaved changes" : "Saved draft revision"}</span>
        <code>{draft.revision}</code>
      </div>
      <div className="pipeline-actions">
        <Button type="button" disabled={!dirty || busy} onClick={() => save.mutate()}>Save package revision</Button>
        <Dialog.Root open={confirmation} onOpenChange={(open) => { if (!publish.isPending) setConfirmation(open); }}>
          <Dialog.Trigger asChild><Button type="button" variant="outline" disabled={dirty || busy || draft.package.members.length === 0}>Review package publication</Button></Dialog.Trigger>
          <Dialog.Portal>
            <Dialog.Overlay className="fixed inset-0 z-50 bg-background/75 backdrop-blur-sm" />
            <Dialog.Content className="fixed left-1/2 top-1/2 z-50 grid max-h-[calc(100dvh-2rem)] w-[min(45rem,calc(100%-2rem))] -translate-x-1/2 -translate-y-1/2 gap-4 overflow-y-auto rounded-lg border border-border bg-card p-5 shadow-lg">
              <Dialog.Title className="text-lg font-semibold">Publish this package revision</Dialog.Title>
              <Dialog.Description>Publish all {draft.package.members.length} artifacts from the saved revision in one transaction.</Dialog.Description>
              <code className="break-all">{draft.revision}</code>
              <ul>{draft.package.members.map((member) => <li key={member.key}>{member.name} · {member.destination ?? "project"}</li>)}</ul>
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
    </div>
    {revisionConflict && <p role="alert">A newer revision is available. Your edits remain here.</p>}
    {save.error && <p role="alert">{String(save.error)}</p>}
    {unconfigured > 0 && <details className="pipeline-evidence" onToggle={(event) => setShowStageModels(event.currentTarget.open)}>
      <summary>Configure stage models</summary>
      {showStageModels && <div className="pipeline-stack pt-2">
        <ModelPicker label="Pipeline stage model" options={stageModels.options} loading={stageModels.loading} disabled={busy}
          selection={{ mode: "single", selected: stageModelSelection, onChange: setStageModelSelection }} />
        {stageModels.errors[0] && <p role="alert">{String(stageModels.errors[0])}</p>}
        <Button type="button" variant="outline" className="w-fit" disabled={!stageModel || busy} onClick={applyStageModel}>
          Apply to {unconfigured} unconfigured {unconfigured === 1 ? "stage" : "stages"}
        </Button>
      </div>}
    </details>}
    <WorkflowPackageGraphEditing projectId={projectId} checkoutId={checkoutId} draftId={draft.id} savedRevision={draft.revision}
      graphs={packageGraphs(text)} draftPresets={draftPresets} layouts={draft.layouts} canvasByMember={canvasByMember}
      changeCanvas={(key, canvas) => setCanvases((previous) => {
        const next = { ...previous };
        if (JSON.stringify(canvas.layout) === JSON.stringify(draft.layouts?.[key] ?? {})
          && Object.keys(canvas.updateDefinitions).length === 0) delete next[key];
        else next[key] = canvas;
        return next;
      })}
      disabled={busy} edit={(key, graph, references) => {
        const current = JSON.parse(currentText.current) as Package;
        setText(JSON.stringify({ ...current, members: current.members.map((member) => member.key === key
          ? { ...member, ...(references && { references }), body: { ...member.body, graph } } : member) }, null, 2));
      }} />
    <WorkflowPackageReview projectId={projectId} draftId={draft.id} savedRevision={draft.revision} />
    {draft.request && <p role="status">Authoring {draft.request.state.replaceAll("_", " ")}{draft.request.diagnostic && ` · ${draft.request.diagnostic}`}</p>}
    {draft.package.members.length > 1 && <ul className="pipeline-stack" aria-label="Saved package members">
      {draft.package.members.map((member) => <li key={member.key}>
        <strong>{member.name}</strong> · {member.body.kind.replaceAll("_", " ")} · {member.destination ?? "project"}
        {member.base && <p className="break-all">Base <code>{member.base.revision}</code></p>}
      </li>)}
    </ul>}
    {savedGraphs.map((member) => <section key={member.key} className="pipeline-stack" aria-label={`Reuse ${member.name}`}>
      {savedGraphs.length > 1 && <strong>{member.name}</strong>}
      <WorkflowReusableDerivation projectId={projectId} graph={member.graph}
        source={{ type: "package_revision", draftId: draft.id, revision: draft.revision, memberKey: member.key }}
        disabled={busy || dirty} onSaved={update} />
    </section>)}
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
    <div className="pipeline-stack max-w-lg">
      <ModelPicker label="Package authoring model" options={models.options} loading={models.loading} disabled={busy}
        selection={{ mode: "single", selected: selection, onChange: setSelection }} />
      <label>Package authoring request<textarea rows={3} className="w-full rounded border border-border bg-background p-2"
        value={feedback} disabled={busy} onChange={(event) => setFeedback(event.currentTarget.value)} /></label>
      <Button type="button" variant="outline" disabled={!model || !feedback.trim() || dirty || busy}
        onClick={() => request.mutate()}>Request package changes with AI</Button>
      {request.error && <p role="alert">{String(request.error)}</p>}
    </div>
    {stale && <section aria-label="Current published package artifact" className="pipeline-evidence">
      <p role="alert">The published base for {stale.member} changed. The package draft is preserved.</p>
      <p>{stale.current.name} · <code>{stale.current.version.revision}</code></p>
      <details><summary>Compare the current artifact</summary>
        <pre className="max-h-64 overflow-auto whitespace-pre-wrap text-xs">{JSON.stringify(stale.current.body, null, 2)}</pre>
      </details>
    </section>}
  </div>;
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
