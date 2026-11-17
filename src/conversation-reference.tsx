import { useRef, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { X } from "lucide-react";
import type { Conversation } from "@/bindings/Conversation";
import type { MessageRevision } from "@/bindings/MessageRevision";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";

type Source = {
  source: Conversation;
  projectId: string;
  projectName: string;
  contextId: string;
  messages: MessageRevision[];
  summaries: { artifactId: string; contextId: string; text: string }[];
};

type ReferenceSelection = { kind: "messages"; contextId: string; spans: { revisionId: string }[] }
  | { kind: "summary"; artifactId: string };

type ReferenceArtifact =
  | { kind: "excerpt"; id: string; sourceContextId: string; excerpts: {
    source: { origin: { revisionId: string }; byteRange: { start: number; end: number } };
    role: MessageRevision["role"]; text: string;
  }[] }
  | { kind: "summary"; id: string; sourceContextId: string; text: string };

export type ConversationReferencePreview = {
  id: string;
  destinationConversationId: string;
  projectId: string;
  projectName: string;
  sourceConversationId: string;
  sourceTitle: string;
  artifact: ReferenceArtifact;
};

const roleLabel = { user: "User", assistant: "Assistant" };

export function ReferencePreview({ reference }: { reference: ConversationReferencePreview }) {
  const { artifact, sourceTitle, projectName } = reference;
  const { copy, feedback } = useContextActionFeedback();
  return <section aria-label="Exact conversation reference" className="grid min-w-0 gap-2">
    <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 text-xs">
      <dt>Conversation</dt><dd className="break-words">{sourceTitle}</dd>
      <dt>Source project</dt><dd className="break-words">{projectName}</dd>
      <dt>Saved context</dt><dd><code className="break-all">{artifact.sourceContextId}</code></dd>
      <dt>Reference revision</dt><dd><ContextActions label="reference revision" groups={[[
        { label: "Copy reference revision ID", run: () => copy(artifact.id) },
        { label: "Copy source context ID", run: () => copy(artifact.sourceContextId) },
        { label: "Copy source conversation ID", run: () => copy(reference.sourceConversationId) },
      ]]}><code className="min-w-0 break-all">{artifact.id}</code></ContextActions></dd>
    </dl>
    {artifact.kind === "summary"
      ? <ContextActions label="saved summary" groups={[[{ label: "Copy saved summary", run: () => copy(artifact.text) }]]}>
        <pre tabIndex={0} className="max-h-48 min-w-0 flex-1 overflow-auto whitespace-pre-wrap break-words text-xs">{artifact.text}</pre>
      </ContextActions>
      : <ol className="grid max-h-48 gap-2 overflow-auto">
        {artifact.excerpts.map(({ role, text, source }) => <li key={source.origin.revisionId} className="min-w-0 border-t border-border pt-1">
          <ContextActions label={`${roleLabel[role]} excerpt`} groups={[[
            { label: "Copy saved excerpt", run: () => copy(text) },
            { label: "Copy source message revision ID", run: () => copy(source.origin.revisionId) },
          ]]}>
            <div className="min-w-0 flex-1">
              <p className="break-all text-xs">{roleLabel[role]} · <code>{source.origin.revisionId}</code></p>
              <pre tabIndex={0} className="whitespace-pre-wrap break-words text-xs">{text}</pre>
            </div>
          </ContextActions>
        </li>)}
      </ol>}
    {feedback && <p role={feedback.failed ? "alert" : "status"} className="text-xs">{feedback.text}</p>}
  </section>;
}

export function ConversationReferenceSelection({ destinationConversationId, source, onBack, onAttach }: {
  destinationConversationId: string;
  source: { conversation: Pick<Conversation, "id" | "title">; projectName: string };
  onBack: () => void;
  onAttach: (reference: ConversationReferencePreview) => void;
}) {
  const sourceConversationId = source.conversation.id;
  const scope = { destinationConversationId, sourceConversationId };
  const saved = useQuery({
    queryKey: ["conversation-reference-source", destinationConversationId, sourceConversationId],
    queryFn: () => invoke<Source>("conversation_reference_source", scope),
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
  const [kind, setKind] = useState<ReferenceSelection["kind"]>("messages");
  const [messages, setMessages] = useState<string[]>([]);
  const [summary, setSummary] = useState("");
  const preview = useMutation({
    mutationFn: (selection: ReferenceSelection) => invoke<ConversationReferencePreview>("preview_conversation_reference", {
      ...scope, selection,
    }),
  });
  const options = saved.data;
  const spans = options?.messages.filter(({ revisionId }) => messages.includes(revisionId))
    .map(({ revisionId }) => ({ revisionId })) ?? [];
  const summaryExists = options?.summaries.some(({ artifactId }) => artifactId === summary);
  const selection = kind === "messages" && options && spans.length
    ? { kind, contextId: options.contextId, spans }
    : kind === "summary" && summaryExists ? { kind, artifactId: summary } : undefined;
  const error = saved.error ?? preview.error;

  return <div className="grid min-w-0 gap-2">
    <Button type="button" variant="ghost" autoFocus onClick={onBack}>Back to conversations</Button>
    <div>
      <p className="break-words font-medium">{source.conversation.title}</p>
      <p className="break-words text-xs text-muted-foreground">{options?.projectName ?? source.projectName}</p>
    </div>
    {saved.isPending && <p role="status">Loading saved conversation…</p>}
    {error && <p role="alert" className="conv-alert">{String(error)}</p>}
    {options && <>
      <fieldset disabled={preview.isPending} className="grid min-w-0 gap-2">
        <legend className="sr-only">Conversation content to attach</legend>
        <div className="flex gap-1">
          <Button type="button" variant="outline" aria-pressed={kind === "messages"} onClick={() => {
            setKind("messages");
            preview.reset();
          }}>Messages</Button>
          <Button type="button" variant="outline" aria-pressed={kind === "summary"} disabled={options.summaries.length === 0} onClick={() => {
            setKind("summary");
            preview.reset();
          }}>Saved summary</Button>
        </div>
        {kind === "messages" && (options.messages.length ? <ol className="grid max-h-40 gap-1 overflow-auto">
          {options.messages.map(({ revisionId, role, text }) => <li key={revisionId}>
            <label className="flex min-w-0 items-start gap-2 rounded border border-border p-2 text-xs">
              <input className="mt-0.5" type="checkbox" checked={messages.includes(revisionId)} onChange={(event) => {
                const checked = event.currentTarget.checked;
                setMessages((current) => checked ? [...current, revisionId] : current.filter((id) => id !== revisionId));
                preview.reset();
              }} />
              <span className="min-w-0"><strong>{roleLabel[role]}</strong><span className="block line-clamp-2 break-words">{text}</span></span>
            </label>
          </li>)}
        </ol> : <p className="text-xs">This conversation has no saved messages.</p>)}
        {kind === "summary" && <label className="grid gap-1 text-xs">
          Summary revision
          <select value={summary} className="conv-select w-full" onChange={(event) => {
            setSummary(event.currentTarget.value);
            preview.reset();
          }}>
            <option value="">Choose a saved summary</option>
            {options.summaries.map(({ artifactId, text }) => <option key={artifactId} value={artifactId}>
              {text.trim().replace(/\s+/g, " ").slice(0, 72)} · {artifactId.slice(0, 8)}
            </option>)}
          </select>
        </label>}
      </fieldset>
      {preview.data ? <>
        <ReferencePreview reference={preview.data} />
        <Button type="button" onClick={() => { if (preview.data) onAttach(preview.data); }}>Attach reference</Button>
      </> : <Button type="button" variant="outline" disabled={!selection || preview.isPending}
        onClick={() => { if (selection) preview.mutate(selection); }}>
        {preview.isPending ? "Loading preview…" : "Preview selection"}
      </Button>}
    </>}
  </div>;
}

export function ConversationReferenceChips({ references, onRemove, disabled = false }: {
  references: readonly ConversationReferencePreview[];
  onRemove: (referenceId: string) => void;
  disabled?: boolean;
}) {
  if (!references.length) return null;
  return <ul className="grid min-w-0 gap-1" aria-label="Attached conversation references">
    {references.map((reference) => <ReferenceChip key={reference.id} reference={reference} disabled={disabled} remove={() => onRemove(reference.id)} />)}
  </ul>;
}

function ReferenceChip({ reference, disabled, remove }: { reference: ConversationReferencePreview; disabled: boolean; remove: () => void }) {
  const details = useRef<HTMLDetailsElement>(null);
  return <li className="min-w-0 rounded border border-border p-1">
    <ContextActions label={`${reference.sourceTitle} reference`} groups={[[
      { label: "Show reference preview", run: () => { if (details.current) details.current.open = true; } },
      { label: "Remove reference from draft", disabled, run: remove },
    ]]}>
      <details ref={details} className="min-w-0 flex-1 px-1">
        <summary className="cursor-pointer truncate text-xs" title={reference.sourceTitle}>
          {reference.sourceTitle} · {reference.artifact.kind === "summary" ? "Summary" : "Excerpt"}
        </summary>
        <ReferencePreview reference={reference} />
      </details>
      <button type="button" className="conv-icon-button" aria-label={`Remove reference to ${reference.sourceTitle}`}
        disabled={disabled} onClick={remove}><X size={14} aria-hidden /></button>
    </ContextActions>
  </li>;
}
