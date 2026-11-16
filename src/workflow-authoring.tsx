import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Dialog } from "radix-ui";
import { useState, type FormEvent } from "react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { liveModelsQuery, sameModelSelection } from "@/live-models";
import { WorkflowGraphEditor, editorDefinitions, type WorkflowEditorState } from "@/workflow-graph-editor";
import { graphProblems, type WorkflowDefinition, type WorkflowDefinitions, type WorkflowGraph, type WorkflowLayout } from "@/workflow-graph-model";
import { WorkflowPublicationControls, type WorkflowPublication } from "@/workflow-publication";
import { WorkflowPackageEditor, WorkflowPackageList, type PackageDraft, type PackageSummary } from "@/workflow-authoring-package";
import { WorkflowLaunchPicker, preferredWorkflowRevision } from "@/workflow-launch";
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

export function ConversationAuthoring({ projectId, conversationId, selectedModel, effort, onOpenDraft }: {
  projectId: string;
  conversationId: string;
  selectedModel: ModelSelection | null;
  effort: string | null;
  onOpenDraft: (draftId: string) => void;
}) {
  const [open, setOpen] = useState(false);
  return <Dialog.Root open={open} onOpenChange={setOpen}>
    <Dialog.Trigger className="conv-quiet-button">Create or edit with AI</Dialog.Trigger>
    {open && <AuthoringDialog projectId={projectId} conversationId={conversationId} selectedModel={selectedModel} effort={effort}
      onOpenDraft={(draftId) => { setOpen(false); onOpenDraft(draftId); }} />}
  </Dialog.Root>;
}

function AuthoringDialog({ projectId, conversationId, selectedModel, effort, onOpenDraft }: Parameters<typeof ConversationAuthoring>[0]) {
  const client = useQueryClient();
  const [brief, setBrief] = useState("");
  const [base, setBase] = useState("");
  const bases = useQuery({
    queryKey: ["workflow-authoring-bases", projectId],
    queryFn: () => invoke<AuthoringBase[]>("workflow_authoring_bases", { projectId }),
  });
  const drafts = useQuery({
    queryKey: chatKey(conversationId),
    queryFn: () => invoke<Draft[]>("conversation_workflow_authoring", { conversationId }),
    refetchInterval: (query) => query.state.data?.some(pending) ? 1200 : false,
  });
  const packages = useQuery({
    queryKey: ["workflow-packages", projectId, "conversation", conversationId],
    queryFn: () => invoke<PackageSummary[]>("list_workflow_packages", { projectId, conversationId }),
  });
  const canAuthor = selectedModel && (selectedModel.provider === "codex" || selectedModel.provider === "claude");
  const selectedBase = bases.data?.find((item) => baseKey(item.version) === base)?.version;
  const ready = canAuthor && (!base || selectedBase) && brief.trim();
  const start = useMutation({
    mutationFn: () => invoke<Draft>("start_workflow_authoring", {
      conversationId, selection: selectedModel, effort, brief: brief.trim(),
      base: selectedBase ?? null,
    }),
    onSuccess: (draft) => {
      client.setQueryData<Draft[]>(chatKey(conversationId), (current) => [draft, ...(current ?? [])]);
      setBrief("");
    },
  });
  const startPackage = useMutation({
    mutationFn: () => invoke<PackageDraft>("start_workflow_package_authoring", {
      projectId, conversationId, selection: selectedModel, effort, base: selectedBase ?? null, brief: brief.trim(),
    }),
    onSuccess: (draft) => {
      void client.invalidateQueries({ queryKey: ["workflow-packages", projectId] });
      onOpenDraft(`package:${draft.id}`);
    },
  });
  const busy = start.isPending || startPackage.isPending;
  return <Dialog.Portal>
    <Dialog.Overlay className="conv-dialog-scrim" />
    <Dialog.Content className="conv-dialog">
      <Dialog.Title>Create or edit with AI</Dialog.Title>
      <Dialog.Description>Ask your selected Codex or Claude model for a pipeline draft or a package of related artifacts.</Dialog.Description>
      <form className="conv-dialog-form" onSubmit={(event) => {
        event.preventDefault();
        if (ready && !busy) start.mutate();
      }}>
        <label>Starting point<select value={base} onChange={(event) => setBase(event.currentTarget.value)}>
          <option value="">New pipeline</option>
          {bases.data?.map(({ version, name }) => <option key={baseKey(version)} value={baseKey(version)}>
            {name} · {version.revision.slice(0, 12)}
          </option>)}
        </select></label>
        <label>Authoring request<textarea rows={4} value={brief} onChange={(event) => setBrief(event.currentTarget.value)} /></label>
        {!canAuthor && <p>Choose a Codex or Claude model in the conversation to start a request.</p>}
        {bases.error && <p role="alert">{String(bases.error)}</p>}
        {start.error && <p role="alert">{String(start.error)}</p>}
        {startPackage.error && <p role="alert">{String(startPackage.error)}</p>}
        <div className="flex justify-end gap-2">
          <Dialog.Close asChild><Button variant="outline" type="button">Close</Button></Dialog.Close>
          <Button type="submit" disabled={!ready || busy}>
            {start.isPending ? "Starting…" : "Start authoring request"}
          </Button>
          <Button type="button" variant="outline" disabled={!ready || busy} onClick={() => startPackage.mutate()}>
            {startPackage.isPending ? "Starting package…" : "Start package authoring request"}
          </Button>
        </div>
      </form>
      {drafts.error && <p role="alert">{String(drafts.error)}</p>}
      {!!drafts.data?.length && <section aria-label="Authoring drafts" className="grid gap-2">
        {drafts.data.map((draft) => <div key={draft.id} className="flex items-center justify-between gap-2 border-t border-border pt-2">
          <div><strong>{draft.head?.graph.name ?? "New pipeline"}</strong><RequestState draft={draft} /></div>
          <Button type="button" variant="outline" onClick={() => onOpenDraft(draft.id)}>
            {draft.head ? "Open draft" : "Open authoring request"}
          </Button>
        </div>)}
      </section>}
      {packages.error && <p role="alert">{String(packages.error)}</p>}
      {!!packages.data?.length && <section aria-label="Linked workflow packages" className="grid gap-2">
        {packages.data.map((draft) => <div key={draft.id} className="flex items-center justify-between gap-2 border-t border-border pt-2">
          <div><strong>{draft.name}</strong><p>{draft.published ? "Published" : "Draft"} · {draft.memberCount} artifacts</p></div>
          <Button type="button" variant="outline" onClick={() => onOpenDraft(`package:${draft.id}`)}>Open linked package</Button>
        </div>)}
      </section>}
    </Dialog.Content>
  </Dialog.Portal>;
}

function baseKey(version: Version) { return JSON.stringify([version.id, version.revision]); }

export function ProjectPipelineDrafts({ checkout, openWorkspace }: Pick<WorkspaceProps, "checkout" | "openWorkspace">) {
  const projectId = checkout.projectId;
  const drafts = useQuery({ queryKey: draftsKey(projectId), queryFn: () => invoke<ProjectDraft[]>("project_workflow_graph_drafts", { projectId }) });
  return <section aria-label="Pipeline drafts" className="pipeline-stack p-4">
    <header className="flex flex-wrap items-center justify-between gap-2">
      <h1 className="text-lg font-semibold">Pipeline drafts</h1>
      <WorkflowEditorAssistance projectId={projectId} checkoutId={checkout.checkoutId} openWorkspace={openWorkspace} />
    </header>
    {drafts.isPending && <p role="status">Loading pipeline drafts…</p>}
    {drafts.error && <p role="alert">{String(drafts.error)}</p>}
    {drafts.data?.length === 0 && <p>Start a pipeline draft with Create or edit with AI in a project conversation.</p>}
    {!!drafts.data?.length && <ul className="workflow-draft-list">
      {drafts.data.map((draft) => <li key={draft.id}>
        <div><strong>{draft.name ?? "Pipeline authoring request"}</strong>
          {draft.revision && <p className="text-xs text-muted-foreground">Revision <code>{draft.revision.slice(0, 12)}</code></p>}
        </div>
        <Button type="button" variant="outline" onClick={() => openWorkspace("pipelines", workflowAuthoringTarget(draft.id))}>
          {draft.revision ? "Open draft" : "Open request"}</Button>
      </li>)}
    </ul>}
    <WorkflowPackageList checkout={checkout} openWorkspace={openWorkspace} />
  </section>;
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
  const registry = useQuery({ queryKey: ["workflow-graph-registry"], queryFn: () => invoke<WorkflowDefinition[]>("workflow_graph_registry"), staleTime: Infinity });
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
