import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ArrowRight, FilePenLine, Sparkles } from "lucide-react";
import { Dialog } from "radix-ui";
import { useId, useState, type FormEvent } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { EffortSelect } from "@/components/effort-select";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { availableModelSelection, liveModelsQuery, sameModelSelection, useLiveModelOptions } from "@/live-models";
import { WorkflowGraphEditor, editorDefinitions, type WorkflowEditorState } from "@/workflow-graph-editor";
import { graphProblems, type WorkflowDefinitions, type WorkflowGraph, type WorkflowLayout } from "@/workflow-graph-model";
import { WorkflowPublicationControls, type WorkflowPublication } from "@/workflow-publication";
import { PipelineBuilderLanding, WorkflowPackageEditor, type PackageDraft, type PackageSummary } from "@/workflow-authoring-package";
import { WorkflowLaunchPicker, preferredWorkflowRevision } from "@/workflow-launch";
import { workflowGraphRegistryQuery } from "@/pipeline-catalog";
import { WorkflowEditorAssistance } from "@/workflow-editor-assistance";
import type { WorkspaceProps } from "@/workbench-contract";

type Version = { id: string; revision: string };
type AuthoringBase = { version: Version; name: string };
type Revision = { id: string; runId: string | null; graph: WorkflowGraph; layout?: WorkflowLayout; definitions?: WorkflowDefinitions };
type ProjectDraft = { id: string; name: string | null; revision: string | null };
type Draft = {
  id: string;
  conversationId: string;
  base: Version | null;
  head: Revision | null;
  request: { jobId: string; runId: string | null; state: string; diagnostic: string | null };
  publication?: WorkflowPublication | null;
};

const chatKey = (conversationId: string) => ["conversation", conversationId, "workflow-authoring"];
const editorKey = (projectId: string, draftId: string) => ["workflow-authoring", projectId, draftId];
const draftsKey = (projectId: string) => ["project-workflow-graph-drafts", projectId];
const pending = (draft: Draft) => ["queued", "starting", "running", "waiting_for_input", "paused"].includes(draft.request.state);
export const workflowAuthoringTarget = (draftId: string) => `authoring:${draftId}`;

function RequestState({ draft }: { draft: Draft }) {
  const { state, diagnostic } = draft.request;
  const label = state === "saved" ? "Draft saved"
    : state === "invalid" ? "The response could not be saved"
      : state === "superseded" ? "The draft was edited while this request ran"
        : `Authoring ${state.replaceAll("_", " ")}`;
  return <p className="text-sm text-muted-foreground" role="status">{label}{diagnostic && <>. {diagnostic}</>}</p>;
}

const authoringProviders = ["codex", "claude"] as const;

export function BuilderAuthoring({ checkout, openWorkspace }: Pick<WorkspaceProps, "checkout" | "openWorkspace">) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  return <Dialog.Root open={open} onOpenChange={(next) => { if (!busy) setOpen(next); }}>
    <Dialog.Trigger asChild>
      <Button type="button" variant="outline"><Sparkles size={15} aria-hidden /> Create or edit with AI</Button>
    </Dialog.Trigger>
    {open && <BuilderAuthoringDialog checkout={checkout} onBusy={setBusy}
      onOpenDraft={(draftId) => { setOpen(false); openWorkspace("pipelines", workflowAuthoringTarget(draftId)); }} />}
  </Dialog.Root>;
}

function BuilderAuthoringDialog({ checkout, onBusy, onOpenDraft }: {
  checkout: WorkspaceProps["checkout"]; onBusy: (busy: boolean) => void; onOpenDraft: (draftId: string) => void;
}) {
  const { projectId, checkoutId } = checkout;
  const client = useQueryClient();
  const formId = useId();
  const [output, setOutput] = useState<"pipeline" | "package">("package");
  const [conversationId, setConversationId] = useState("");
  const [selected, setSelected] = useState<ModelSelection | null>(null);
  const [effort, setEffort] = useState("");
  const [brief, setBrief] = useState("");
  const [base, setBase] = useState("");
  const conversations = useQuery({
    queryKey: ["conversations", checkoutId],
    queryFn: () => invoke<Conversation[]>("list_conversations", { checkoutId }),
    select: (items) => items.filter((conversation) => conversation.checkoutId === checkoutId),
  });
  const source = conversations.data?.find(({ id }) => id === conversationId);
  const models = useLiveModelOptions(authoringProviders);
  const selection = availableModelSelection(models.options, selected);
  const model = models.models.find(({ option }) => sameModelSelection(option.selection, selection));
  const bases = useQuery({
    queryKey: ["workflow-authoring-bases", projectId],
    queryFn: () => invoke<AuthoringBase[]>("workflow_authoring_bases", { projectId }),
  });
  const drafts = useQuery({
    queryKey: chatKey(conversationId),
    queryFn: () => invoke<Draft[]>("conversation_workflow_authoring", { conversationId }),
    enabled: Boolean(source),
    refetchInterval: (query) => query.state.data?.some(pending) ? 1200 : false,
  });
  const packages = useQuery({
    queryKey: ["workflow-packages", projectId, "conversation", conversationId],
    queryFn: () => invoke<PackageSummary[]>("list_workflow_packages", { projectId, conversationId }),
    enabled: Boolean(source),
  });
  const selectedBase = bases.data?.find((item) => baseKey(item.version) === base)?.version ?? null;
  const sourceReady = Boolean(source || (!conversationId && output === "package"));
  const ready = Boolean(selection && sourceReady && (!base || selectedBase) && brief.trim());
  const request = useMutation({
    mutationFn: async () => {
      const settings = { selection, effort: model?.efforts.includes(effort) ? effort : null, brief: brief.trim() };
      if (!ready) throw new Error("Review the authoring request before starting.");
      if (output === "pipeline" && source) {
        const draft = await invoke<Draft>("start_workflow_authoring", { ...settings, conversationId: source.id, base: selectedBase });
        return { output, draft };
      }
      const draft = source
        ? await invoke<PackageDraft>("start_workflow_package_authoring", { ...settings, projectId, conversationId: source.id, base: selectedBase })
        : await invoke<PackageDraft>("start_workflow_editor_authoring", { ...settings, projectId, checkoutId,
          base: selectedBase && { ...selectedBase, kind: "pipeline", destination: "project" } });
      return { output: "package" as const, draft };
    },
    onMutate: () => onBusy(true),
    onSettled: () => onBusy(false),
    onSuccess: (result) => {
      if (result.output === "pipeline") {
        void client.invalidateQueries({ queryKey: chatKey(result.draft.conversationId) });
        void client.invalidateQueries({ queryKey: draftsKey(projectId) });
        onOpenDraft(result.draft.id);
      } else {
        void client.invalidateQueries({ queryKey: ["conversations"] });
        void client.invalidateQueries({ queryKey: ["conversation-library"] });
        void client.invalidateQueries({ queryKey: ["workflow-packages", projectId] });
        onOpenDraft(`package:${result.draft.id}`);
      }
    },
  });
  const busy = request.isPending;
  return <Dialog.Portal>
    <Dialog.Overlay className="conv-dialog-scrim" />
    <Dialog.Content className="conv-dialog conv-dialog-layout">
      <header className="conv-dialog-header">
        <span className="conv-dialog-mark"><Sparkles size={19} aria-hidden /></span>
        <div><Dialog.Title>Create or edit with AI</Dialog.Title>
          <Dialog.Description>Ask a Codex or Claude model to create a draft for your review.</Dialog.Description>
        </div>
      </header>
      <div className="conv-dialog-body grid gap-4">
        <form id={formId} className="conv-dialog-form" onSubmit={(event) => {
          event.preventDefault();
          if (ready && !busy) request.mutate();
        }}>
          <label>Output<select value={output} onChange={(event) => setOutput(event.currentTarget.value === "pipeline" ? "pipeline" : "package")} disabled={busy}>
            <option value="package">Workflow package</option>
            <option value="pipeline">Pipeline draft</option>
          </select></label>
          <label>Source conversation<select value={conversationId} onChange={(event) => setConversationId(event.currentTarget.value)} disabled={busy}>
            <option value="" disabled={output === "pipeline"}>{output === "pipeline" ? "Choose a conversation" : "New linked conversation"}</option>
            {conversationId && !source && <option value={conversationId} disabled>Conversation unavailable</option>}
            {conversations.data?.map(({ id, title }) => <option key={id} value={id}>{title}</option>)}
          </select></label>
          {conversations.isPending && <p role="status">Loading conversations in this worktree…</p>}
          {output === "pipeline" && conversations.data?.length === 0 && <p>Open a conversation in this worktree to start a pipeline draft.</p>}
          {conversations.error && <p role="alert">{String(conversations.error)}</p>}
          <label>Starting point<select value={base} title={selectedBase?.revision} onChange={(event) => setBase(event.currentTarget.value)} disabled={busy}>
            <option value="">No base revision</option>
            {bases.data?.map(({ version, name }) => <option key={baseKey(version)} value={baseKey(version)}>
              {name} · {version.revision.slice(0, 12)}
            </option>)}
          </select></label>
          <ModelPicker label="Authoring model" options={models.options} loading={models.loading} disabled={busy} combinedCatalog
            selection={{ mode: "single", selected, onChange: (next) => { setSelected(next); setEffort(""); } }} />
          {model && Boolean(model.efforts.length) && <fieldset disabled={busy}><label>Authoring effort
            <EffortSelect capabilities={model} label="Authoring effort" value={effort} onChange={setEffort} />
          </label></fieldset>}
          <label>Authoring request<textarea rows={4} value={brief} onChange={(event) => setBrief(event.currentTarget.value)} disabled={busy} /></label>
          {models.errors.map((error, index) => <p key={index} role="alert">{String(error)}</p>)}
          {bases.error && <p role="alert">{String(bases.error)}</p>}
          {request.error && <p role="alert">{String(request.error)}</p>}
        </form>
        {drafts.error && <p role="alert">{String(drafts.error)}</p>}
        {!!drafts.data?.length && <section aria-label="Authoring drafts" className="grid gap-2">
          {drafts.data.map((draft) => <div key={draft.id} className="flex flex-wrap items-center justify-between gap-2 border-t border-border pt-2">
            <div><strong>{draft.head?.graph.name ?? "New pipeline"}</strong><RequestState draft={draft} /></div>
            <Button type="button" variant="outline" disabled={busy} onClick={() => onOpenDraft(draft.id)}>
              {draft.head ? "Open draft" : "Open authoring request"}
            </Button>
          </div>)}
        </section>}
        {packages.error && <p role="alert">{String(packages.error)}</p>}
        {!!packages.data?.length && <section aria-label="Linked workflow packages" className="grid gap-2">
          {packages.data.map((draft) => <div key={draft.id} className="flex flex-wrap items-center justify-between gap-2 border-t border-border pt-2">
            <div><strong>{draft.name}</strong><p>{draft.published ? "Published" : "Draft"} · {draft.memberCount} artifacts</p></div>
            <Button type="button" variant="outline" disabled={busy} onClick={() => onOpenDraft(`package:${draft.id}`)}>Open linked package</Button>
          </div>)}
        </section>}
      </div>
      <footer className="conv-dialog-actions">
        <Dialog.Close asChild><Button variant="outline" type="button" disabled={busy}>Close</Button></Dialog.Close>
        <Button type="submit" form={formId} disabled={!ready || busy}>
          {busy ? "Starting…" : "Start authoring request"}
        </Button>
      </footer>
    </Dialog.Content>
  </Dialog.Portal>;
}

function baseKey(version: Version) { return JSON.stringify([version.id, version.revision]); }

export function ProjectPipelineDrafts({ checkout, openWorkspace }: Pick<WorkspaceProps, "checkout" | "openWorkspace">) {
  const projectId = checkout.projectId;
  const drafts = useQuery({ queryKey: draftsKey(projectId), queryFn: () => invoke<ProjectDraft[]>("project_workflow_graph_drafts", { projectId }) });
  return <PipelineBuilderLanding checkout={checkout} openWorkspace={openWorkspace}
    authoring={<BuilderAuthoring checkout={checkout} openWorkspace={openWorkspace} />}
    drafts={<section aria-label="Pipeline drafts" className="pipeline-builder-card">
      <header className="pipeline-builder-card-heading">
        <span className="pipeline-builder-icon"><FilePenLine size={17} aria-hidden /></span>
        <div><h3>Pipeline drafts</h3><p>Resume a draft or check an authoring request.</p></div>
      </header>
      <div className="pipeline-builder-card-body">
        {drafts.isPending && <p role="status">Loading pipeline drafts…</p>}
        {drafts.error && <p role="alert">{String(drafts.error)}</p>}
        {drafts.data?.length === 0 && <p className="pipeline-builder-empty">No pipeline drafts yet.</p>}
        {!!drafts.data?.length && <ul className="workflow-draft-list">
          {drafts.data.map((draft) => <li key={draft.id}>
            <div><strong>{draft.name ?? "Pipeline authoring request"}</strong>
              {draft.revision && <p>Revision <code>{draft.revision.slice(0, 12)}</code></p>}
            </div>
            <Button type="button" variant="outline" onClick={() => openWorkspace("pipelines", workflowAuthoringTarget(draft.id))}>
              {draft.revision ? "Open draft" : "Open request"} <ArrowRight size={15} aria-hidden />
            </Button>
          </li>)}
        </ul>}
      </div>
    </section>}
  />;
}

export function PipelineAuthoringEditor(props: WorkspaceProps & { draftId: string }) {
  return props.draftId.startsWith("package:")
    ? <WorkflowPackageEditor {...props} draftId={props.draftId.slice("package:".length)} />
    : <GraphAuthoringEditor {...props} />;
}

function GraphAuthoringEditor({ draftId, checkout, openWorkspace, openActivity, openQueueJob }: WorkspaceProps & { draftId: string }) {
  const projectId = checkout.projectId;
  const query = useQuery({
    queryKey: editorKey(projectId, draftId),
    queryFn: () => invoke<Draft>("inspect_workflow_authoring", { projectId, draftId }),
    refetchInterval: (record) => record.state.data && pending(record.state.data) ? 1200 : false,
  });
  const draft = query.data;
  const launchVersion = draft ? preferredWorkflowRevision(draft) : undefined;
  const runId = draft?.request.runId;
  return <section aria-label="Pipeline draft editor" className="pipeline-stack h-full overflow-auto p-4">
    <header className="flex flex-wrap items-center gap-2">
      <h1 className="mr-auto text-lg font-semibold">{draft?.head?.graph.name ?? "Pipeline authoring"}</h1>
      {draft?.publication && <WorkflowEditorAssistance projectId={projectId} checkoutId={checkout.checkoutId} openWorkspace={openWorkspace}
        artifact={{ name: draft.head?.graph.name ?? "Pipeline", version: { ...draft.publication.workflow, kind: "pipeline", destination: "project" } }} />}
      {draft && <Button type="button" variant="outline" onClick={() => openWorkspace("conversations", draft.conversationId)}>Open source conversation</Button>}
      {runId && <Button type="button" variant="outline" onClick={() => openActivity(runId)}>Open authoring run</Button>}
      {draft && !draft.request.runId && openQueueJob && <Button type="button" variant="outline" onClick={() => openQueueJob(draft.request.jobId)}>Open queued job</Button>}
    </header>
    {query.isPending && <p role="status">Loading pipeline draft…</p>}
    {query.error && <p role="alert">{String(query.error)}</p>}
    {draft && <>
      <RequestState draft={draft} />
      {draft.base && <p className="text-xs text-muted-foreground">Published base <code>{draft.base.revision}</code></p>}
      {draft.head && <AuthoringRevisionEditor key={draft.head.id} projectId={projectId} checkoutId={checkout.checkoutId} draft={draft} revision={draft.head} />}
      <WorkflowLaunchPicker key={launchVersion?.revision ?? draft.id} projectId={projectId}
        checkoutId={checkout.checkoutId} preferred={launchVersion} openWorkspace={openWorkspace} />
    </>}
  </section>;
}

function AuthoringRevisionEditor({ projectId, checkoutId, draft, revision }: { projectId: string; checkoutId: string; draft: Draft; revision: Revision }) {
  const client = useQueryClient();
  const original: WorkflowEditorState = { graph: revision.graph, layout: revision.layout ?? {}, updateDefinitions: {} };
  const [editor, setEditor] = useState(original);
  const registry = useQuery(workflowGraphRegistryQuery);
  const catalog = registry.data ?? [];
  const definitions = editorDefinitions(editor, revision.definitions ?? {}, catalog);
  const problems = graphProblems(editor.graph, definitions);
  const [feedback, setFeedback] = useState("");
  const [provider, setProvider] = useState<"codex" | "claude">("codex");
  const [selection, setSelection] = useState<ModelSelection | null>(null);
  const models = useQuery(liveModelsQuery(provider));
  const options = models.data?.map(({ option }) => option) ?? [];
  const readModel = options.find(({ selection: candidate, selectable }) => selectable && sameModelSelection(candidate, selection));
  const bound = { projectId, draftId: draft.id, selectedRevision: revision.id };
  function updated(next: Draft) {
    client.setQueryData(editorKey(projectId, draft.id), next);
    void client.invalidateQueries({ queryKey: chatKey(draft.conversationId) });
    void client.invalidateQueries({ queryKey: draftsKey(projectId) });
  }
  const save = useMutation({
    mutationFn: () => invoke<Draft>("save_workflow_authoring_draft", { ...bound, graph: editor.graph,
      canvas: { layout: editor.layout, updateDefinitions: editor.updateDefinitions } }),
    onSuccess: updated,
  });
  const request = useMutation({
    mutationFn: () => invoke<Draft>("request_workflow_authoring_changes", { ...bound, selection, effort: null, feedback: feedback.trim() }),
    onSuccess: (next) => { setFeedback(""); updated(next); },
  });
  const dirty = JSON.stringify(editor) !== JSON.stringify(original);
  const editingDisabled = Boolean(draft.publication) || pending(draft) || save.isPending || request.isPending;
  const ready = Boolean(readModel && feedback.trim() && !dirty && !editingDisabled);
  function requestChanges(event: FormEvent) {
    event.preventDefault();
    if (ready) request.mutate();
  }
  return <div className="pipeline-stack">
    {registry.isPending && <p role="status">Loading node registry…</p>}
    {registry.error && <p role="alert">{String(registry.error)}</p>}
    {registry.data && <WorkflowGraphEditor projectId={projectId} checkoutId={checkoutId} state={editor} setState={setEditor} definitions={definitions}
      catalog={catalog} problems={problems} disabled={editingDisabled} />}
    <div className="flex flex-wrap items-center justify-between gap-2">
      <p className="break-all text-xs text-muted-foreground">Draft revision <code>{revision.id}</code></p>
      <div className="flex gap-2">
        {dirty && <Button type="button" variant="outline" disabled={editingDisabled} onClick={() => { setEditor(original); save.reset(); }}>Revert unsaved changes</Button>}
        {!draft.publication && <Button type="button" disabled={!dirty || editingDisabled || !registry.data || problems.length > 0}
          onClick={() => save.mutate()}>Save draft revision</Button>}
      </div>
    </div>
    {save.error && <p role="alert">{String(save.error)}</p>}
    <WorkflowPublicationControls projectId={projectId} draftId={draft.id} revisionId={revision.id} base={draft.base}
      graph={revision.graph} disabled={dirty || editingDisabled || !registry.data || problems.length > 0} publication={draft.publication}
      onPublished={(publication) => {
        updated({ ...draft, publication });
        void client.invalidateQueries({ queryKey: ["workflow-authoring-bases", projectId] });
      }} />
    {!draft.publication && <details open className="pipeline-evidence">
      <summary>Assist with AI</summary>
      <form className="pipeline-stack pipeline-form max-w-lg" onSubmit={requestChanges}>
      <label>Authoring provider<select value={provider} onChange={(event) => { setProvider(event.currentTarget.value === "claude" ? "claude" : "codex"); setSelection(null); }}>
        <option value="codex">Codex</option><option value="claude">Claude</option>
      </select></label>
      <ModelPicker label="Authoring model" options={options} loading={models.isPending} selection={{ mode: "single", selected: selection, onChange: setSelection }} />
      {models.error && <p role="alert">{String(models.error)}</p>}
      <label>Feedback<textarea rows={4} value={feedback} onChange={(event) => setFeedback(event.currentTarget.value)} /></label>
      {dirty && <p className="text-xs text-muted-foreground">Save your edits before requesting changes to them.</p>}
      {request.error && <p role="alert">{String(request.error)}</p>}
      <Button type="submit" disabled={!ready}>Request changes</Button>
      </form>
    </details>}
  </div>;
}
