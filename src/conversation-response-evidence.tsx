import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { RunRecord } from "@/bindings/RunRecord";
import { useCheckoutConversationActions } from "@/checkout-conversation-actions";
import type { ChildLocation } from "@/conversation-children";
import { useContextActionFeedback } from "@/components/ui/context-actions";
import { downloadArtifact } from "@/download-artifact";
import { RecordedTextActions, type RecordedTextSelection } from "@/recorded-text-actions";

export type ResponseObservation = { type: "assistant_delta" | "assistant_message" | "assistant_final"; text: string };

type ResponseItem = {
  location: ChildLocation;
  status: "streamed" | "completed" | "final";
  text: string;
};

export type TurnResponseEvidence = { items: ResponseItem[] };

export function newTurnResponseEvidence(): TurnResponseEvidence {
  return { items: [] };
}

export function cloneTurnResponseEvidence(evidence: TurnResponseEvidence): TurnResponseEvidence {
  return { items: [...evidence.items] };
}

export function recordResponseEvidence(evidence: TurnResponseEvidence, event: ResponseObservation, location: ChildLocation) {
  if (location.parentItemId !== null || !event.text) return;
  const status = event.type === "assistant_delta" ? "streamed" : event.type === "assistant_message" ? "completed" : "final";
  const index = evidence.items.findLastIndex((item) => {
    if (item.location.sessionId !== location.sessionId || item.location.turnId !== location.turnId) return false;
    if (item.location.itemId !== location.itemId) return false;
    if (location.itemId !== null) return true;
    if (status === "final") return item.text === event.text;
    return item.status === "streamed";
  });
  const previous = evidence.items[index];
  if (!previous) {
    evidence.items.push({ location, status, text: event.text });
  } else if (status === "final" || previous.status !== "final" && (status !== "streamed" || previous.status === "streamed")) {
    evidence.items[index] = {
      location, status, text: status === "streamed" ? previous.text + event.text : event.text,
    };
  }
}

type ResponsePage = { cursor: string; more: boolean; events: { data: ResponseObservation | { type: string }; location: ChildLocation }[] };

function isResponseObservation(data: ResponseObservation | { type: string }): data is ResponseObservation {
  return data.type === "assistant_delta" || data.type === "assistant_message" || data.type === "assistant_final";
}

export async function readRecordedResponseEvidence(conversationId: string, runId: string, signal?: AbortSignal) {
  const responses = newTurnResponseEvidence();
  let cursor = "0";
  for (;;) {
    signal?.throwIfAborted();
    const page = await invoke<ResponsePage>("conversation_events", { conversationId, runId, after: cursor });
    if (page.more && page.cursor === cursor) throw new Error("The saved output cursor did not advance.");
    for (const event of page.events) if (isResponseObservation(event.data)) recordResponseEvidence(responses, event.data, event.location);
    cursor = page.cursor;
    if (!page.more) return { responses, cursor };
  }
}

type ResponseEvidence = {
  responses: TurnResponseEvidence;
  reasoning: { text: string; summary: boolean }[];
  usage?: { input: number | null; output: number | null; cached_input: number | null };
  contextWindow?: { used: number; size: number };
  reasoningTokens?: number;
};

type ApprovalDelivery = "pending" | "prepared" | "submitted" | "unconfirmed" | "closed";
type ApprovalDecision = "allow_once" | "deny" | "cancel";
type Approval = {
  eventSequence: string;
  operation: string;
  description: string | null;
  location: ChildLocation;
  decision: ApprovalDecision | null;
  delivery: ApprovalDelivery;
};

const deliveryLabels: Record<ApprovalDelivery, string> = {
  pending: "Waiting for decision",
  prepared: "Decision recorded",
  submitted: "Decision submitted",
  unconfirmed: "Delivery unconfirmed",
  closed: "Closed without a decision",
};

const decisionLabels: Record<ApprovalDecision, string> = {
  allow_once: "Allow once", deny: "Deny", cancel: "Cancel",
};

const responseLabels = {
  streamed: "Streamed provisional text",
  completed: "Recorded response text",
  final: "Provider-reported final response",
};

type IndexedResponse = { item: ResponseItem; ordinal: number };
type ResponseDraftSource = {
  conversationId: string;
  run: RunRecord;
  destination: { checkoutId: string; open: (conversationId: string) => void };
};
const endedStatuses = new Set<RunRecord["status"]>(["succeeded", "failed", "cancelled", "interrupted"]);

async function confirmResponseSource({ conversationId, run }: ResponseDraftSource, { item, ordinal }: IndexedResponse) {
  const unavailable = () => new Error("The recorded response is unavailable or changed. Reopen the attempt.");
  const [fresh, owner] = await invoke<[RunRecord, string]>("inspect_conversation_run", {
    projectId: run.projectId, conversationId, runId: run.id,
  });
  if (owner !== conversationId || fresh.id !== run.id || fresh.projectId !== run.projectId || fresh.checkoutId !== run.checkoutId
    || fresh.turnId !== run.turnId || fresh.status !== run.status || !endedStatuses.has(fresh.status)) throw unavailable();
  const { responses } = await readRecordedResponseEvidence(conversationId, run.id);
  const current = responses.items[ordinal - 1];
  if (!current || current.status !== item.status || current.text !== item.text || current.location.parentItemId !== item.location.parentItemId
    || current.location.sessionId !== item.location.sessionId || current.location.turnId !== item.location.turnId
    || current.location.itemId !== item.location.itemId) throw unavailable();
}

function Responses({ title, items, onExcerpt }: {
  title: string; items: IndexedResponse[]; onExcerpt?: (entry: IndexedResponse, excerpt: RecordedTextSelection) => void;
}) {
  const { copy, feedback, run } = useContextActionFeedback();
  if (!items.length) return null;
  return <details>
    <summary>{title}</summary>
    <ol aria-label={title} className="grid gap-2 text-xs">
      {items.map((entry, index) => {
        const { item, ordinal } = entry;
        const { itemId, turnId } = item.location;
        return <li key={JSON.stringify([ordinal, item.status, item.location])}>
          <RecordedTextActions label={`${responseLabels[item.status]} ${index + 1}`} text={item.text}
            bodyClassName="whitespace-pre-wrap break-words font-sans" excerptAction="Add response excerpt to conversation…"
            onExcerpt={onExcerpt && ((excerpt) => onExcerpt(entry, excerpt))} groups={[[
            { label: "Copy recorded response", run: () => copy(item.text) },
            { label: "Download response as text", run: () => void run(async () => {
              downloadArtifact(`${item.status}-response-${index + 1}.txt`, new Blob([item.text], { type: "text/plain;charset=utf-8" }));
            }) },
          ], [
            ...(itemId ? [{ label: "Copy provider item ID", run: () => copy(itemId) }] : []),
            ...(turnId ? [{ label: "Copy provider turn ID", run: () => copy(turnId) }] : []),
          ]]}>{(body) =>
            <div className="min-w-0 flex-1">
              <p className="text-muted-foreground">{responseLabels[item.status]}</p>
              {body}
            </div>
          }</RecordedTextActions>
        </li>;
      })}
    </ol>
    {feedback && <p role={feedback.failed ? "alert" : "status"} className="text-xs">{feedback.text}</p>}
  </details>;
}

function Usage({ evidence }: { evidence: ResponseEvidence }) {
  const { usage, contextWindow, reasoningTokens } = evidence;
  const details: [string, number | null | undefined][] = [
    ["Input tokens", usage?.input],
    ["Output tokens", usage?.output],
    ["Cached input tokens", usage?.cached_input],
    ["Reasoning tokens", reasoningTokens],
  ];
  const reported = details.filter((entry): entry is [string, number] => entry[1] !== null && entry[1] !== undefined);
  if (!reported.length && !contextWindow) return null;
  return <section aria-label="Reported usage" className="grid gap-1">
    <h3 className="text-sm font-medium">Reported usage</h3>
    <dl className="grid gap-1 text-xs">
      {reported.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value.toLocaleString()}</dd></div>)}
      {contextWindow && <div>
        <dt>Context tokens</dt><dd>{contextWindow.used.toLocaleString()} / {contextWindow.size.toLocaleString()}</dd>
      </div>}
    </dl>
  </section>;
}

export function ConversationResponseEvidence({ conversationId, runId, live, evidence, draftSource }: {
  conversationId: string;
  runId: string;
  live: boolean;
  evidence?: ResponseEvidence;
  draftSource?: ResponseDraftSource;
}) {
  return <>
    {evidence && <ObservedConversationResponse evidence={evidence} draftSource={live ? undefined : draftSource} />}
    <details>
      <summary>Approvals</summary>
      <ConversationApprovalEvidence conversationId={conversationId} runId={runId} live={live} />
    </details>
  </>;
}

export function ObservedConversationResponse({ evidence, draftSource }: { evidence: ResponseEvidence; draftSource?: ResponseDraftSource }) {
  const source = draftSource && endedStatuses.has(draftSource.run.status) ? draftSource : undefined;
  const draft = useCheckoutConversationActions(source?.destination.checkoutId ?? "", !source, (id) => source?.destination.open(id));
  const entries = evidence.responses.items.map((item, index) => ({ item, ordinal: index + 1 }));

  function addExcerpt(entry: IndexedResponse, excerpt: RecordedTextSelection) {
    if (!source) return;
    const { run, conversationId } = source;
    const { item, ordinal } = entry;
    const { sessionId, turnId, itemId } = item.location;
    const lines = excerpt.firstLine === excerpt.lastLine ? `Line ${excerpt.firstLine}` : `Lines ${excerpt.firstLine}–${excerpt.lastLine}`;
    const reference = [
      responseLabels[item.status], `Conversation ${conversationId}`, `Run ${run.id} (${run.status})`, `Source worktree ${run.checkoutId}`, `Turn ${run.turnId}`,
      sessionId && `Provider session ${sessionId}`, turnId ? `Provider turn ${turnId}` : "Provider turn ID unavailable",
      itemId ? `Provider item ${itemId}` : "Provider item ID unavailable", `Response item ${ordinal}`, lines,
    ].filter(Boolean).join(" · ");
    draft.startContextText({ title: "Recorded provider response", reference, text: excerpt.text,
      language: "markdown", confirmSource: () => confirmResponseSource(source, entry) });
  }

  return <>
    <Responses title="Final responses" items={entries.filter(({ item }) => item.status === "final")} onExcerpt={source ? addExcerpt : undefined} />
    <Responses title="Provisional and recorded responses" items={entries.filter(({ item }) => item.status !== "final")} onExcerpt={source ? addExcerpt : undefined} />
    {evidence.reasoning.length > 0 ? <details>
      <summary>Reported reasoning</summary>
      <ol aria-label="Reported reasoning" className="grid gap-1 text-xs">
        {evidence.reasoning.map((entry, index) => <li key={index}>
          <p className="text-muted-foreground">{entry.summary ? "Provider reasoning summary" : "Provider reasoning text"}</p>
          <p className="whitespace-pre-wrap break-words">{entry.text}</p>
        </li>)}
      </ol>
    </details> : evidence.reasoningTokens !== undefined && <p className="text-xs text-muted-foreground">Reasoning text unavailable.</p>}
    <Usage evidence={evidence} />
    {draft.dialog}
  </>;
}

export function ConversationApprovalEvidence({ conversationId, runId, live }: {
  conversationId: string;
  runId: string;
  live: boolean;
}) {
  const approvals = useQuery({
    queryKey: ["conversation-run", runId, "approval-evidence", conversationId, live],
    queryFn: () => invoke<Approval[]>("conversation_approvals", { conversationId, runId }),
    refetchInterval: live ? 400 : false,
  });
  return <>
    {approvals.isPending && <p className="text-xs text-muted-foreground">Loading approval history…</p>}
    {approvals.error && <p role="alert" className="text-xs">{String(approvals.error)}</p>}
    {approvals.data?.length === 0 && <p className="text-xs text-muted-foreground">No approval requests recorded.</p>}
    {Boolean(approvals.data?.length) && <ol aria-label="Approval history" className="grid gap-2 text-xs">
      {approvals.data?.map((approval) => <li key={approval.eventSequence}>
        <p>{approval.operation}{approval.location.parentItemId && " · Child tool request"}</p>
        {approval.description && <p className="whitespace-pre-wrap break-words">{approval.description}</p>}
        <p className="text-muted-foreground">
          {deliveryLabels[approval.delivery]}{approval.decision && ` · ${decisionLabels[approval.decision]}`} · Event {approval.eventSequence}
        </p>
      </li>)}
    </ol>}
  </>;
}
