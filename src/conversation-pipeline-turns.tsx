import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ArrowUpRight, CircleAlert, CircleCheck, Clock3, FolderOpen, SlidersHorizontal, Workflow, X } from "lucide-react";
import { Dialog } from "radix-ui";
import { useId, useRef, useState } from "react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { PipelineComposerAgent } from "@/pipeline-composer-customizations";
import { Button } from "@/components/ui/button";
import { initialWorkflowStartFields, initialWorkflowStartValue, WorkflowStartValueField, type WorkflowStartSchema } from "@/workflow-launch-fields";

type StartField = WorkflowStartSchema["fields"][number] & { conversationSource?: "message" | "history" };
type MarkdownRevision = { id: string; relativePath: string; sha256: string };
type Work = { task: { id: string; title: string }; placement: { kind: string } };
type Event = { id: string; workId: string; label: string };
type TurnArtifact = { id: string; nodeId: string; port: string; kind: string };

type ConversationPipelineSchema = Omit<WorkflowStartSchema, "fields"> & {
  fields: StartField[];
  requiresModel?: boolean;
  events?: Event[];
  agents?: PipelineComposerAgent[];
};

export type ConversationPipelineInputs = {
  documentRevisionId?: string | null;
  workId?: string | null;
  agileEventId?: string | null;
  values: Record<string, unknown>;
};

export type PipelineTurn = {
  requestId: string;
  text: string;
  submittedDraft?: string | null;
  inputs?: ConversationPipelineInputs;
  selection?: ModelSelection | null;
  effort?: string | null;
  state: "queued" | "running" | "succeeded" | "failed" | "cancelled" | "interrupted";
  runId?: string | null;
  messageRevisionId?: string | null;
  resultRevisionId?: string | null;
  waiting?: { nodeId: string; label: string; kind: string; runId?: string | null } | null;
  artifacts?: TurnArtifact[];
  actionOutcome?: { nodeId: string; label?: string; state: "skipped" | "not_executed" } | null;
  error?: string | null;
};

type TurnRequest = {
  conversationId: string;
  text: string;
  inputs?: ConversationPipelineInputs;
  selection?: ModelSelection;
  effort: string | null;
  referenceIds?: string[];
  submittedDraft?: string;
  action?: { nodeId: string; marker: string };
};

const emptyInputs: ConversationPipelineInputs = { values: {} };
const live = (turn: PipelineTurn) => turn.state === "queued" || turn.state === "running";

function supplied(schema: ConversationPipelineSchema, inputs: ConversationPipelineInputs | undefined) {
  const data = inputs ?? emptyInputs;
  const event = schema.events?.find(({ id, workId }) => id === data.agileEventId && workId === data.workId);
  return (!schema.requiresDocument || Boolean(data.documentRevisionId))
    && (!schema.requiresWork || Boolean(data.workId))
    && (!schema.requiresEvent || Boolean(event))
    && schema.fields.every((field) => field.conversationSource || !field.required || Object.hasOwn(data.values, field.name));
}

async function fingerprint(json: string) {
  if (!crypto.subtle) return "";
  const bytes = new TextEncoder().encode(json);
  return Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function rememberedRequest(storageKey: string, hash: string) {
  if (!hash) return null;
  try {
    const entry = JSON.parse(localStorage.getItem(storageKey) ?? "null");
    return typeof entry?.id === "string" && entry?.hash === hash ? entry.id : null;
  } catch { return null; }
}

export function useConversationPipelineTurns(conversationId: string, enabled: boolean, saveSubmittedDraft: (text: string) => Promise<void>) {
  const client = useQueryClient();
  const key = ["conversation", conversationId, "pipeline-turns"] as const;
  const [configured, setConfigured] = useState<ConversationPipelineInputs>();
  const pendingRequest = useRef<{ key: string; id: string } | undefined>(undefined);
  const storageKey = "omniagent:pipeline-message:" + conversationId;
  const schema = useQuery({
    queryKey: ["conversation", conversationId, "pipeline-schema"],
    queryFn: () => invoke<ConversationPipelineSchema>("conversation_pipeline_schema", { conversationId }),
    enabled,
    retry: false,
  });
  const turns = useQuery({
    queryKey: key,
    queryFn: async () => {
      const rows = await invoke<PipelineTurn[]>("conversation_pipeline_turns", { conversationId });
      const previous = client.getQueryData<PipelineTurn[]>(key);
      if (rows.some((row) => row.messageRevisionId !== previous?.find(({ requestId }) => requestId === row.requestId)?.messageRevisionId
        || row.resultRevisionId !== previous?.find(({ requestId }) => requestId === row.requestId)?.resultRevisionId)) {
        void client.invalidateQueries({ queryKey: ["conversation", conversationId, "messages"], exact: true });
      }
      return rows;
    },
    enabled,
    retry: false,
    refetchInterval: (query) => query.state.data?.some(live) ? 1_100 : false,
  });
  const request = useMutation({
    scope: { id: "conversation-pipeline:" + conversationId },
    mutationFn: async (input: Omit<TurnRequest, "conversationId">) => {
      await saveSubmittedDraft(input.submittedDraft ?? input.text);
      const payload = { conversationId, ...input, inputs: {
        documentRevisionId: input.inputs?.documentRevisionId ?? null, workId: input.inputs?.workId ?? null,
        agileEventId: input.inputs?.agileEventId ?? null, values: input.inputs?.values ?? {},
      } };
      const json = JSON.stringify(payload, (_, value: unknown) => value && typeof value === "object" && !Array.isArray(value)
        ? Object.fromEntries(Object.keys(value).sort().map((key) => [key, (value as Record<string, unknown>)[key]])) : value);
      const hash = await fingerprint(json);
      const cacheKey = hash || json;
      const id = (pendingRequest.current?.key === cacheKey ? pendingRequest.current.id : undefined)
        ?? rememberedRequest(storageKey, hash) ?? crypto.randomUUID();
      pendingRequest.current = { key: cacheKey, id };
      if (hash) {
        try { localStorage.setItem(storageKey, JSON.stringify({ id, hash })); } catch { /* The current request is still retryable. */ }
      }
      const row = await invoke<PipelineTurn>("enqueue_conversation_pipeline_turn", { request: { ...payload, requestId: id } });
      pendingRequest.current = undefined;
      try { localStorage.removeItem(storageKey); } catch { /* The accepted turn is already stored. */ }
      return row;
    },
    onSuccess: (row) => {
      client.setQueryData<PipelineTurn[]>(key, (previous = []) =>
        previous.some(({ requestId }) => requestId === row.requestId) ? previous : [...previous, row]);
      void client.invalidateQueries({ queryKey: key, exact: true });
      void client.invalidateQueries({ queryKey: ["conversation", conversationId, "messages"], exact: true });
    },
  });
  const accepted = turns.data?.filter(({ state }) => state !== "failed");
  const inputs = configured ?? accepted?.at(-1)?.inputs;
  const defaultModel = accepted?.findLast(({ selection }) => Boolean(selection));
  return { schema, turns, request, inputs, setConfigured, defaultModel,
    setupNeeded: Boolean(schema.data && !supplied(schema.data, inputs)),
    active: turns.data?.some(live) ?? false };
}

const turnLabels: Record<PipelineTurn["state"], string> = {
  queued: "Queued pipeline turn", running: "Pipeline running", succeeded: "Pipeline completed",
  failed: "Pipeline failed", cancelled: "Pipeline cancelled", interrupted: "Pipeline interrupted",
};

export function ConversationPipelineTurnStatus({ turn, attached, onRun, onRestore }: {
  turn: PipelineTurn;
  attached?: boolean;
  onRun?: (turn: PipelineTurn, artifact?: TurnArtifact) => void;
  onRestore?: (text: string) => void;
}) {
  const Icon = turn.waiting || turn.state === "failed" || turn.state === "interrupted" ? CircleAlert
    : turn.state === "succeeded" ? CircleCheck : turn.state === "queued" ? Clock3 : Workflow;
  return <article className="conversation-pipeline-turn" data-state={turn.state}>
    <header>
      <span role="status"><Icon size={15} aria-hidden /> {turn.waiting ? "Waiting for action" : turnLabels[turn.state]}</span>
      {turn.runId && onRun && <Button type="button" variant="ghost" onClick={() => onRun(turn)}>
        {turn.waiting ? "Open waiting step" : "Open pipeline run"} <ArrowUpRight size={14} aria-hidden />
      </Button>}
    </header>
    {!attached && <p>{turn.text}</p>}
    {turn.waiting && <p>{turn.waiting.label || turn.waiting.nodeId} · {turn.waiting.kind.replaceAll("_", " ")}</p>}
    {turn.actionOutcome && <p className="conversation-pipeline-action-outcome" role="status">
      <CircleAlert size={14} aria-hidden />
      {turn.actionOutcome.state === "skipped"
        ? `The pipeline skipped ${turn.actionOutcome.label || turn.actionOutcome.nodeId}. Its slash action was not run.`
        : `The slash action for ${turn.actionOutcome.label || turn.actionOutcome.nodeId} did not run.`}
    </p>}
    {Boolean(turn.artifacts?.length) && <ul className="conversation-pipeline-artifacts" aria-label="Pipeline artifacts">
      {turn.artifacts?.map((artifact) => {
        const label = `${artifact.port} · ${artifact.kind.replaceAll("_", " ")}`;
        return <li key={artifact.id} title={artifact.nodeId}>
          {turn.runId && onRun ? <Button type="button" variant="ghost" onClick={() => onRun(turn, artifact)}>
            {label} <ArrowUpRight size={12} aria-hidden />
          </Button> : <span>{label}</span>}
        </li>;
      })}
    </ul>}
    {turn.error && <p role="alert">{turn.error}</p>}
    {onRestore && turn.text && !live(turn) && turn.state !== "succeeded" && <Button type="button" variant="outline"
      className="justify-self-start" onClick={() => onRestore(turn.submittedDraft ?? turn.text)}>Restore to draft</Button>}
  </article>;
}

export function ConversationPipelineInputsDialog({ schema, previous, projectId, checkoutId, pending, sendAfterSave, opener, onSave, onClose }: {
  schema: ConversationPipelineSchema;
  previous?: ConversationPipelineInputs;
  projectId: string;
  checkoutId: string;
  pending: boolean;
  sendAfterSave: boolean;
  opener?: HTMLElement | null;
  onSave: (inputs: ConversationPipelineInputs) => void;
  onClose: () => void;
}) {
  const client = useQueryClient();
  const formId = useId();
  const fields = schema.fields.filter(({ conversationSource }) => !conversationSource);
  const [values, setValues] = useState(() => ({ ...initialWorkflowStartFields({ ...schema, fields }), ...previous?.values }));
  const [documentRevisionId, setDocument] = useState(previous?.documentRevisionId ?? "");
  const [workId, setWork] = useState(previous?.workId ?? "");
  const [agileEventId, setEvent] = useState(previous?.agileEventId ?? "");
  const revisionKey = ["repository-markdown-revisions", checkoutId];
  const revisions = useQuery({
    queryKey: revisionKey,
    queryFn: () => invoke<MarkdownRevision[]>("list_repository_markdown_revisions", { checkoutId }),
  });
  const hierarchy = useQuery({
    queryKey: ["project-tasks", projectId, "hierarchy"],
    queryFn: () => invoke<{ work: Work[] }>("project_work_hierarchy", { projectId }),
    enabled: Boolean(projectId) && !schema.requiresEvent,
  });
  const pick = useMutation({
    mutationFn: () => invoke<{ revision: MarkdownRevision; markdown: string } | null>("choose_repository_markdown", { checkoutId }),
    onSuccess: (file) => {
      if (!file) return;
      client.setQueryData<MarkdownRevision[]>(revisionKey, (current = []) =>
        [file.revision, ...current.filter(({ id }) => id !== file.revision.id)]);
      setDocument(file.revision.id);
    },
  });
  const documentValid = !documentRevisionId || revisions.data?.some(({ id }) => id === documentRevisionId);
  const workValid = !workId || schema.requiresEvent || hierarchy.data?.work.some(({ task }) => task.id === workId);
  const inputs = { documentRevisionId: documentRevisionId || null, workId: workId || null, agileEventId: agileEventId || null,
    values: Object.fromEntries(fields.filter(({ name }) => Object.hasOwn(values, name)).map(({ name }) => [name, values[name]])) };
  const ready = Boolean(supplied(schema, inputs) && documentValid && workValid && !pending && !pick.isPending);
  const error = revisions.error ?? pick.error ?? hierarchy.error;

  function setValue(name: string, value: unknown) {
    setValues((current) => {
      const next = { ...current };
      if (value === undefined) delete next[name];
      else next[name] = value;
      return next;
    });
  }

  return <Dialog.Root open onOpenChange={(open) => { if (!open && !pending) onClose(); }}>
    <Dialog.Portal>
      <Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog conv-dialog-layout conversation-pipeline-dialog"
        onCloseAutoFocus={(event) => {
          if (opener?.isConnected) { event.preventDefault(); opener.focus({ preventScroll: true }); }
        }}>
        <header className="conv-dialog-header">
          <span className="conv-dialog-mark"><SlidersHorizontal size={20} aria-hidden /></span>
          <div><Dialog.Title>Pipeline inputs</Dialog.Title>
            <Dialog.Description>The conversation reuses these inputs on later turns.</Dialog.Description></div>
          <Dialog.Close asChild><Button type="button" variant="ghost" className="ui-icon-button" aria-label="Close pipeline inputs" disabled={pending}><X size={17} aria-hidden /></Button></Dialog.Close>
        </header>
        <div className="conv-dialog-body">
          <form className="conversation-pipeline-inputs" id={formId} onSubmit={(event) => { event.preventDefault(); if (ready) onSave(inputs); }}>
            <fieldset>
              <legend>Context</legend>
              {schema.requiresEvent ? <label>Agile event
                <select value={agileEventId} onChange={(event) => {
                  const next = schema.events?.find(({ id }) => id === event.currentTarget.value);
                  setEvent(next?.id ?? "");
                  setWork(next?.workId ?? "");
                }} required>
                  <option value="">Choose a recorded event</option>
                  {agileEventId && !schema.events?.some(({ id }) => id === agileEventId) && <option value={agileEventId} disabled>Previous event unavailable</option>}
                  {schema.events?.map((event) => <option key={event.id} value={event.id}>{event.label}</option>)}
                </select>
                {!schema.events?.length && <p className="conversation-pipeline-note">There are no eligible events recorded for this project.</p>}
              </label> : <label>Task or Sub-Task
                <select value={workId} onChange={(event) => setWork(event.currentTarget.value)} disabled={hierarchy.isFetching} required={schema.requiresWork}>
                  <option value="">{schema.requiresWork ? "Choose work" : "No linked work"}</option>
                  {workId && !workValid && <option value={workId} disabled>Previous selection unavailable</option>}
                  {hierarchy.data?.work.map(({ task, placement }) => <option value={task.id} key={task.id}>
                    {placement.kind === "subtask" ? "Sub-Task" : "Task"} · {task.title}
                  </option>)}
                </select>
              </label>}
              <label>Saved Markdown revision
                <select value={documentRevisionId} onChange={(event) => setDocument(event.currentTarget.value)} disabled={revisions.isFetching || pick.isPending} required={schema.requiresDocument}>
                  <option value="">{schema.requiresDocument ? "Choose a revision" : "No Markdown revision"}</option>
                  {documentRevisionId && !documentValid && <option value={documentRevisionId} disabled>Previous revision unavailable</option>}
                  {revisions.data?.map((revision) => <option value={revision.id} key={revision.id}>
                    {revision.relativePath} · {revision.sha256.slice(0, 10)}
                  </option>)}
                </select>
              </label>
              <Button type="button" variant="outline" className="justify-self-start" onClick={() => pick.mutate()} disabled={pick.isPending}>
                <FolderOpen size={15} aria-hidden /> {pick.isPending ? "Selecting…" : "Select Markdown file"}
              </Button>
            </fieldset>
            {fields.length > 0 && <fieldset>
              <legend>Workflow inputs</legend>
              {fields.map((field) => {
                const included = field.required || Object.hasOwn(values, field.name);
                return <div key={field.name}>
                  {!field.required && <label className="pipeline-checkline"><input type="checkbox" checked={included}
                    onChange={(event) => setValue(field.name, event.currentTarget.checked ? initialWorkflowStartValue(field.valueType) : undefined)} /> Provide {field.name}</label>}
                  {included && <WorkflowStartValueField field={field} value={values[field.name]} disabled={pending}
                    onChange={(value) => setValue(field.name, value)} />}
                </div>;
              })}
            </fieldset>}
            {error && <p role="alert" className="conv-alert">{String(error)}</p>}
          </form>
        </div>
        <footer className="conv-dialog-actions">
          <Dialog.Close asChild><Button type="button" variant="outline" disabled={pending}>Cancel</Button></Dialog.Close>
          <Button type="submit" form={formId} disabled={!ready}>{pending ? "Queueing…" : sendAfterSave ? "Send with these inputs" : "Use these inputs"}</Button>
        </footer>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
