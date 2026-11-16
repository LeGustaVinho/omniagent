import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Dialog } from "radix-ui";
import { useState, type FormEvent } from "react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { liveModelsQuery, sameModelSelection } from "@/live-models";
import type { WorkspaceProps } from "@/workbench-contract";

type Version = { id: string; revision: string };
type AuthoringBase = { version: Version; name: string };
type Endpoint = { node: string; port: string };
type Graph = {
  name: string;
  nodes: { id: string; kind: string; configuration: { kind: string; role?: string; names?: string[] } }[];
  wires: { from: Endpoint; to: Endpoint }[];
};
type Revision = { id: string; runId: string | null; graph: Graph };
type Draft = {
  id: string;
  conversationId: string;
  base: Version | null;
  head: Revision | null;
  request: { jobId: string; runId: string | null; state: string; diagnostic: string | null };
};

const chatKey = (conversationId: string) => ["conversation", conversationId, "workflow-authoring"];
const editorKey = (projectId: string, draftId: string) => ["workflow-authoring", projectId, draftId];
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
  return <Dialog.Portal>
    <Dialog.Overlay className="conv-dialog-scrim" />
    <Dialog.Content className="conv-dialog">
      <Dialog.Title>Create or edit with AI</Dialog.Title>
      <Dialog.Description>Ask your selected Codex or Claude model for a pipeline draft.</Dialog.Description>
      <form className="conv-dialog-form" onSubmit={(event) => {
        event.preventDefault();
        if (ready && !start.isPending) start.mutate();
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
        <div className="flex justify-end gap-2">
          <Dialog.Close asChild><Button variant="outline" type="button">Close</Button></Dialog.Close>
          <Button type="submit" disabled={!ready || start.isPending}>
            {start.isPending ? "Starting…" : "Start authoring request"}
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
    </Dialog.Content>
  </Dialog.Portal>;
}

function baseKey(version: Version) { return JSON.stringify([version.id, version.revision]); }

export function PipelineAuthoringEditor({ draftId, checkout, openWorkspace, openActivity, openQueueJob }: WorkspaceProps & { draftId: string }) {
  const projectId = checkout.projectId;
  const query = useQuery({
    queryKey: editorKey(projectId, draftId),
    queryFn: () => invoke<Draft>("inspect_workflow_authoring", { projectId, draftId }),
    refetchInterval: (record) => record.state.data && pending(record.state.data) ? 1200 : false,
  });
  const draft = query.data;
  const runId = draft?.request.runId;
  return <section aria-label="Pipeline draft editor" className="pipeline-stack h-full overflow-auto p-4">
    <header className="flex flex-wrap items-center gap-2">
      <h1 className="mr-auto text-lg font-semibold">{draft?.head?.graph.name ?? "Pipeline authoring"}</h1>
      {draft && <Button type="button" variant="outline" onClick={() => openWorkspace("conversations", draft.conversationId)}>Open source conversation</Button>}
      {runId && <Button type="button" variant="outline" onClick={() => openActivity(runId)}>Open authoring run</Button>}
      {draft && !draft.request.runId && openQueueJob && <Button type="button" variant="outline" onClick={() => openQueueJob(draft.request.jobId)}>Open queued job</Button>}
    </header>
    {query.isPending && <p role="status">Loading pipeline draft…</p>}
    {query.error && <p role="alert">{String(query.error)}</p>}
    {draft && <>
      <RequestState draft={draft} />
      {draft.base && <p className="text-xs text-muted-foreground">Published base <code>{draft.base.revision}</code></p>}
      {draft.head && <AuthoringRevisionEditor key={draft.head.id} projectId={projectId} draft={draft} revision={draft.head} />}
    </>}
  </section>;
}

function AuthoringRevisionEditor({ projectId, draft, revision }: { projectId: string; draft: Draft; revision: Revision }) {
  const client = useQueryClient();
  const original = JSON.stringify(revision.graph, null, 2);
  const [text, setText] = useState(original);
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
  }
  const save = useMutation({
    mutationFn: () => invoke<Draft>("save_workflow_authoring_draft", { ...bound, graph: JSON.parse(text) }),
    onSuccess: updated,
  });
  const request = useMutation({
    mutationFn: () => invoke<Draft>("request_workflow_authoring_changes", { ...bound, selection, effort: null, feedback: feedback.trim() }),
    onSuccess: (next) => { setFeedback(""); updated(next); },
  });
  const dirty = text !== original;
  const editingDisabled = pending(draft) || save.isPending || request.isPending;
  const ready = Boolean(readModel && feedback.trim() && !dirty && !pending(draft) && !save.isPending && !request.isPending);
  function requestChanges(event: FormEvent) {
    event.preventDefault();
    if (ready) request.mutate();
  }
  return <div className="grid items-start gap-6 lg:grid-cols-[2fr_1fr]">
    <form className="pipeline-stack pipeline-form" onSubmit={(event) => { event.preventDefault(); if (dirty && !editingDisabled) save.mutate(); }}>
      <label>Pipeline definition<textarea className="min-h-80 font-mono" spellCheck={false} value={text}
        disabled={editingDisabled} onChange={(event) => setText(event.currentTarget.value)} /></label>
      <p className="break-all text-xs text-muted-foreground">Draft revision <code>{revision.id}</code></p>
      {save.error && <p role="alert">{String(save.error)}</p>}
      <Button type="submit" disabled={!dirty || editingDisabled}>Save draft revision</Button>
    </form>
    <form className="pipeline-stack pipeline-form" onSubmit={requestChanges}>
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
  </div>;
}
