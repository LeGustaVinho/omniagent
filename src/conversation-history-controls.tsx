import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { GitFork, History, Pencil, RotateCcw } from "lucide-react";
import { Dialog } from "radix-ui";
import { useState } from "react";
import type { ConversationHistory } from "@/bindings/ConversationHistory";
import type { Conversation } from "@/bindings/Conversation";
import type { MessageRevision } from "@/bindings/MessageRevision";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { QueueHistoryPolicy } from "@/bindings/QueueHistoryPolicy";
import type { QueueHistorySettings } from "@/bindings/QueueHistorySettings";
import { Button } from "@/components/ui/button";
import { SettingRow } from "@/settings-controls";
import type { ConversationMode } from "@/conversation-mode";

const policies = {
  review: "Pause for review",
  recorded: "Use recorded context",
  cancel: "Cancel pending jobs",
} satisfies Record<QueueHistoryPolicy, string>;

function policyOptions() {
  return Object.entries(policies).map(([value, label]) => <option key={value} value={value}>{label}</option>);
}

export function ProjectQueueHistoryPolicy({ projectId }: { projectId: string }) {
  const client = useQueryClient();
  const key = ["project-queue-history-policy", projectId];
  const saved = useQuery({
    queryKey: key,
    queryFn: () => invoke<QueueHistoryPolicy>("project_queue_history_policy", { projectId }),
  });
  const save = useMutation({
    mutationFn: (policy: QueueHistoryPolicy) => invoke<void>("set_project_queue_history_policy", { projectId, policy }),
    onSuccess: (_, policy) => {
      client.setQueryData(key, policy);
      return client.invalidateQueries({ queryKey: ["conversation-queue-history-policy"] });
    },
  });

  return <>
    <SettingRow id="queued-context" label="Queued work after context changes"
      description="Applies when you rewind or clear a conversation that uses the project default.">
      <select id="queued-context" aria-describedby="queued-context-description"
        value={save.isPending ? save.variables : saved.data ?? ""}
        disabled={!saved.data || save.isPending}
        onChange={(event) => save.mutate(event.currentTarget.value as QueueHistoryPolicy)}>
        {!saved.data && <option value="">Loading policy…</option>}
        {policyOptions()}
      </select>
    </SettingRow>
    {(saved.error || save.error) && <p role="alert" className="settings-result settings-result-error">{String(saved.error || save.error)}</p>}
  </>;
}

export type ConversationMessageHistoryAction = { kind: "rewind" | "fork" | "edit"; revisionId: string; sourceContextId?: string };

export function ConversationMessageHistoryActions({ message, disabled, onAction }: {
  message: MessageRevision; disabled: boolean; onAction: (action: ConversationMessageHistoryAction) => void;
}) {
  const run = (kind: ConversationMessageHistoryAction["kind"]) => onAction({ kind, revisionId: message.revisionId });
  return <div className="mt-2 flex flex-wrap gap-2" aria-label="Message history actions">
    <button type="button" className="conv-quiet-button" disabled={disabled} onClick={() => run("rewind")}><RotateCcw aria-hidden /> Rewind here</button>
    <button type="button" className="conv-quiet-button" disabled={disabled} onClick={() => run("fork")}><GitFork aria-hidden /> Fork from message</button>
    {message.role === "user" && <button type="button" className="conv-quiet-button" disabled={disabled} onClick={() => run("edit")}><Pencil aria-hidden /> Edit in a fork</button>}
  </div>;
}

type HistoryInspection = {
  previous: { contextId: string; supersededBy: string; replacedAt: string; messageCount: number; action: "rewind" | "clear" | "select" }[];
  fork: { source: Conversation | null; sourceContextId: string | null; sourceRevisionId: string | null } | null;
  retainedQuestions: { runId: string; requestId: string | number }[];
};

function useHistoryInspection(conversationId: string) {
  return useQuery({ queryKey: ["conversation", conversationId, "history-inspection"],
    queryFn: () => invoke<HistoryInspection>("conversation_history_inspection", { conversationId }),
  });
}

function HistoryMessages({ messages, forkMessage }: { messages: readonly MessageRevision[]; forkMessage?: (revisionId: string) => void }) {
  return <ol className="space-y-2">{messages.map((message) => <li key={message.revisionId}>
    <strong>{message.role === "user" ? "You" : "Assistant"}</strong>
    <p className="whitespace-pre-wrap break-words">{message.text}</p>
    {forkMessage && <button type="button" className="conv-quiet-button" onClick={() => forkMessage(message.revisionId)}>
      <GitFork aria-hidden /> Fork from saved message
    </button>}
  </li>)}</ol>;
}

type SnapshotTarget = { conversationId: string; contextId: string; throughRevision?: string; archived?: boolean };

function snapshotQuery(conversationId: string, contextId?: string) {
  return { queryKey: ["conversation", conversationId, "history-revision", contextId],
    queryFn: () => invoke<ConversationHistory>("conversation_history_revision", { conversationId, contextId }),
    enabled: Boolean(contextId), staleTime: Infinity };
}

function HistoricalSnapshot({ conversationId, contextId, throughRevision, forkMessage }: SnapshotTarget & { forkMessage?: (revisionId: string) => void }) {
  const history = useQuery(snapshotQuery(conversationId, contextId));
  if (history.error) return <p role="alert">{String(history.error)}</p>;
  if (!history.data) return <p role="status">Loading saved dialogue…</p>;
  const { messages } = history.data;
  const end = throughRevision ? messages.findIndex(({ revisionId }) => revisionId === throughRevision) + 1 : messages.length;
  if (throughRevision && end === 0) return <p>The selected source message is unavailable.</p>;
  return end > 0 ? <HistoryMessages messages={messages.slice(0, end)} forkMessage={forkMessage} /> : <p>This revision has no dialogue.</p>;
}

export function ConversationPreviousRevisions({ conversationId, openConversation, onArchivedAction }: {
  conversationId: string; openConversation?: (conversation: Conversation) => void;
  onArchivedAction?: (action: ConversationMessageHistoryAction) => void;
}) {
  const inspection = useHistoryInspection(conversationId);
  const [selected, setSelected] = useState<SnapshotTarget>();
  const fork = inspection.data?.fork;
  const source = fork?.source;
  const sourceContextId = fork?.sourceContextId;
  return <section className="grid gap-2" aria-label="Previous context revisions">
    <h3 className="font-medium">Previous context revisions</h3>
    {inspection.error && <p role="alert">{String(inspection.error)}</p>}
    {inspection.isPending && <p role="status">Loading previous revisions…</p>}
    {inspection.data?.previous.length === 0 && <p>No superseded context has been recorded.</p>}
    {inspection.data?.previous.map(({ contextId, supersededBy, messageCount, replacedAt, action }) => <button key={supersededBy}
      type="button" className="conv-quiet-button justify-self-start" aria-pressed={selected?.contextId === contextId}
      onClick={() => setSelected({ conversationId, contextId, archived: true })}>
      {action === "clear" ? "Before clear" : action === "rewind" ? "Before rewind" : "Before context selection"} · {messageCount} messages · {new Date(replacedAt).toLocaleString()}
    </button>)}
    {fork && <div className="grid gap-1">
      <p>{source ? `Forked from ${source.title}` : "Fork source unavailable"}</p>
      {source && openConversation && <button type="button" className="conv-quiet-button justify-self-start"
        onClick={() => openConversation(source)}>Open source conversation</button>}
      {source && sourceContextId && <button type="button" className="conv-quiet-button justify-self-start"
        onClick={() => setSelected({ conversationId: source.id, contextId: sourceContextId, throughRevision: fork.sourceRevisionId ?? undefined })}>
        Inspect retained source messages
      </button>}
    </div>}
    {selected && <div className="rounded border border-border p-2" aria-label="Saved dialogue"><HistoricalSnapshot {...selected}
      forkMessage={selected.archived && onArchivedAction ? (revisionId) => onArchivedAction({ kind: "fork", revisionId, sourceContextId: selected.contextId }) : undefined} />
    </div>}
  </section>;
}

type InstructionPreview = { sources: { path: string; selection: string }[]; unresolved: string[] };

function NextPromptScope({ conversationId, mode, selection, effort }: Pick<HistoryProps, "conversationId" | "mode" | "selection" | "effort">) {
  const inspection = useHistoryInspection(conversationId);
  const instructions = useQuery({ queryKey: ["conversation", conversationId, "next-prompt-instructions", selection, effort, mode],
    queryFn: () => invoke<InstructionPreview>("conversation_next_prompt_instructions", {
      conversationId, selection, effort: effort || null, mode: mode === "goal" ? "read_only" : mode,
    }), enabled: Boolean(selection),
  });
  return <section aria-label="Next prompt scope" className="grid gap-2">
    <h3 className="font-medium">Next prompt scope</h3>
    <dl className="grid gap-1">
      <div><dt className="inline font-medium">Role and mode </dt><dd className="inline">{mode === "goal" ? "Goal Mode" : mode === "plan" ? "Planning assistant · Plan Mode" : "Assistant · Read-only"}</dd></div>
      {selection && <div><dt className="inline font-medium">Selected model </dt><dd className="inline">{selection.modelId}</dd></div>}
    </dl>
    <div><h4 className="font-medium">Instruction files</h4>
      {!selection && <p>Select a model to inspect the active checkout and profile.</p>}
      {instructions.isFetching && <p role="status">Inspecting instruction files…</p>}
      {instructions.error && <p role="alert">{String(instructions.error)}</p>}
      {instructions.data && <>
        <ul className="break-all">{instructions.data.sources.map(({ path, selection: status }) => <li key={`${path}:${status}`}>{path} · {status.replaceAll("_", " ")}</li>)}</ul>
        {instructions.data.sources.length === 0 && <p>No instruction files detected for this profile.</p>}
        {instructions.data.unresolved.map((path) => <p key={path} className="break-all">Could not inspect {path}</p>)}
      </>}
    </div>
    {inspection.data && <div>
      <p>Pinned workflow questions retained after context changes: {inspection.data.retainedQuestions.length}.</p>
      <ul>{inspection.data.retainedQuestions.map(({ runId, requestId }) => <li key={`${runId}:${typeof requestId}:${requestId}`}>
        Question <code>{requestId}</code> · Run <code>{runId}</code>
      </li>)}</ul>
    </div>}
  </section>;
}

type HistoryProps = {
  conversationId: string;
  title: string;
  history?: ConversationHistory;
  disabled: boolean;
  mode: ConversationMode;
  selection?: ModelSelection;
  effort?: string;
  openFork: (conversation: Conversation) => void;
  requestedAction?: ConversationMessageHistoryAction;
  onRequestedActionClose?: () => void;
};

export function ConversationHistoryControls(props: HistoryProps) {
  const [open, setOpen] = useState(false);
  const [archivedAction, setArchivedAction] = useState<ConversationMessageHistoryAction>();
  const action = archivedAction ?? props.requestedAction;
  const showing = open || Boolean(action);
  const mutationKey = ["conversation-history-write", props.conversationId];
  const pending = useIsMutating({ mutationKey }) > 0;
  function close() {
    setOpen(false);
    setArchivedAction(undefined);
    props.onRequestedActionClose?.();
  }
  return <Dialog.Root open={showing} onOpenChange={(next) => {
    if (next) setOpen(true);
    else if (!pending) close();
  }}>
    <Dialog.Trigger type="button" className="conv-quiet-button" title="Conversation history controls" disabled={!props.history}>
      <History aria-hidden /> History
    </Dialog.Trigger>
    {showing && props.history && <Dialog.Portal>
      <Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog max-h-[85vh] overflow-y-auto">
        <Dialog.Title>Conversation history</Dialog.Title>
        <Dialog.Description>Choose a history boundary and review the resulting dialogue.</Dialog.Description>
        <HistoryActionForm key={action ? `${action.kind}:${action.revisionId}:${action.sourceContextId ?? "active"}` : "head"}
          {...props} history={props.history} close={close} requestedAction={action} onArchivedAction={setArchivedAction} />
      </Dialog.Content>
    </Dialog.Portal>}
  </Dialog.Root>;
}

function HistoryActionForm({ conversationId, title, history, disabled, mode, selection, effort, openFork, requestedAction, close, onArchivedAction }:
  HistoryProps & { history: ConversationHistory; close: () => void; onArchivedAction: (action: ConversationMessageHistoryAction) => void }) {
  const [baseId] = useState(history.contextId);
  const [kind, setKind] = useState<ConversationMessageHistoryAction["kind"] | "clear">(requestedAction?.kind ?? "clear");
  const [selected, setSelected] = useState(requestedAction?.revisionId ?? "");
  const [forkTitle, setForkTitle] = useState(`${title} (fork)`);
  const [edit, setEdit] = useState(history.messages.find(({ revisionId }) => revisionId === requestedAction?.revisionId)?.text ?? "");
  const sourceContextId = requestedAction?.sourceContextId;
  const archive = useQuery(snapshotQuery(conversationId, sourceContextId));
  const messages = sourceContextId ? archive.data?.messages ?? [] : history.messages;
  const client = useQueryClient();
  const key = ["conversation-queue-history-policy", conversationId];
  const mutationKey = ["conversation-history-write", conversationId];
  const policy = useQuery({
    queryKey: key,
    queryFn: () => invoke<QueueHistorySettings>("conversation_queue_history_policy", { conversationId }),
  });
  const save = useMutation({
    mutationKey,
    mutationFn: (policy: QueueHistoryPolicy | null) =>
      invoke<QueueHistorySettings>("set_conversation_queue_history_policy", { conversationId, policy }),
    onSuccess: (settings) => client.setQueryData(key, settings),
  });
  const change = useMutation({
    mutationKey,
    mutationFn: async () => {
      const throughRevision = selected || null;
      if (sourceContextId) return invoke<Conversation>("fork_retained_conversation", {
        conversationId, expectedContextId: baseId, sourceContextId, throughRevision, title: forkTitle.trim(),
      });
      if (kind === "edit") return invoke<Conversation>("fork_conversation_with_message_edit", {
        conversationId, expectedContextId: baseId, throughRevision, title: forkTitle.trim(), text: edit,
      });
      if (kind === "fork") return invoke<Conversation>("fork_conversation", {
        conversationId, expectedContextId: baseId, throughRevision, title: forkTitle.trim(),
      });
      await invoke<string>("rewind_conversation_history", {
        conversationId, baseId, throughRevision: kind === "clear" ? null : throughRevision,
      });
    },
    onSuccess: async (fork) => {
      await Promise.all([
        client.invalidateQueries({ queryKey: ["conversation", conversationId] }),
        client.invalidateQueries({ queryKey: ["project-job-queue"] }),
        client.invalidateQueries({ queryKey: ["conversation-library"] }),
        client.invalidateQueries({ queryKey: ["conversations"] }),
      ]);
      close();
      if (fork) openFork(fork);
    },
    onError: () => client.invalidateQueries({ queryKey: ["conversation", conversationId, "messages"] }),
  });
  const lock = save.isPending || change.isPending;
  const fork = kind === "fork" || kind === "edit";
  const index = messages.findIndex((message) => message.revisionId === selected);
  const message = messages[index];
  const prefix = kind === "clear" ? [] : index >= 0 ? messages.slice(0, index + 1) : messages;
  const changed = history.contextId !== baseId;
  const invalid = (kind === "rewind" && !message) || (kind === "fork" && Boolean(selected) && !message)
    || (kind === "edit" && (message?.role !== "user" || !edit.trim())) || (fork && !forkTitle.trim()) || (Boolean(sourceContextId) && !message);

  return <div className="conv-dialog-form">
          {requestedAction?.kind !== "edit" && !sourceContextId && <label>Action
            <select value={kind} disabled={lock} onChange={(event) => {
              const next = event.currentTarget.value as "clear" | "rewind" | "fork";
              setKind(next);
              if (next === "rewind" && !selected) setSelected(history.messages.at(-1)?.revisionId ?? "");
              if (next === "fork" && !requestedAction) setSelected("");
            }}>
              <option value="clear">Clear context</option>
              <option value="rewind">Rewind here</option>
              <option value="fork">Fork conversation</option>
            </select>
          </label>}
          {!fork && <>
          <label>
            Queued work after context changes
            <select value={(save.isPending ? save.variables : policy.data?.conversationOverride) ?? ""}
              disabled={!policy.data || lock}
              onChange={(event) => save.mutate(event.currentTarget.value ? event.currentTarget.value as QueueHistoryPolicy : null)}>
              <option value="">{policy.data ? `Use project default · ${policies[policy.data.projectDefault]}` : "Loading policy…"}</option>
              {policyOptions()}
            </select>
          </label>
          <NextPromptScope conversationId={conversationId} mode={mode} selection={selection} effort={effort} />
          </>}
          {(kind === "rewind" || kind === "fork") && <label>
            {kind === "fork" ? "Fork through" : "Keep messages through"}
            <select value={selected} disabled={disabled || lock || !messages.length || changed}
              onChange={(event) => setSelected(event.currentTarget.value)}>
              {kind === "fork" && !sourceContextId && <option value="">Current conversation head</option>}
              {selected && !message && <option value={selected} disabled>Message no longer available</option>}
              {messages.map((message) => <option value={message.revisionId} key={message.revisionId}>
                {message.role === "user" ? "You" : "Assistant"} · {message.text.trim().replace(/\s+/g, " ").slice(0, 100)}
              </option>)}
            </select>
          </label>}
          {fork && <>
            <label>Fork title <input value={forkTitle} disabled={disabled || lock} onChange={(event) => setForkTitle(event.currentTarget.value)} /></label>
            <p>The fork opens with an independent provider session on the current checkout.</p>
          </>}
          {kind === "edit" && <label>Edit user message <textarea value={edit} rows={4} disabled={disabled || lock} onChange={(event) => setEdit(event.currentTarget.value)} /></label>}
          {kind !== "clear" && <details><summary>Retained dialogue preview · {prefix.length} messages</summary>
            {prefix.length ? <HistoryMessages messages={kind === "edit" ? prefix.map((entry) => entry.revisionId === selected ? { ...entry, text: edit } : entry) : prefix} /> : <p>The fork starts with empty dialogue.</p>}
          </details>}
          {kind === "clear" && <p>The next context starts with empty dialogue. Previous messages remain available below after clearing.</p>}
          <details><summary>Inspect earlier context</summary><ConversationPreviousRevisions conversationId={conversationId}
            openConversation={(source) => { close(); openFork(source); }} onArchivedAction={!disabled && !lock && !changed ? onArchivedAction : undefined} /></details>
          {disabled && <p role="status">Wait for the active turn to stop before changing history.</p>}
          {changed && <p role="alert">The conversation context changed. Reopen History to review the current messages.</p>}
          {(policy.error || save.error || change.error || archive.error) && <p className="conv-alert" role="alert">{String(policy.error || save.error || change.error || archive.error)}</p>}
          <footer>
            <Dialog.Close asChild><Button type="button" variant="outline" disabled={lock}>Close</Button></Dialog.Close>
            <Button type="button"
              disabled={disabled || lock || changed || invalid || (!fork && !policy.data)}
              onClick={() => change.mutate()}>
              {kind === "clear" ? "Confirm clear" : kind === "rewind" ? "Confirm rewind" : kind === "edit" ? "Create fork with edit" : "Create fork"}
            </Button>
          </footer>
        </div>;
}
