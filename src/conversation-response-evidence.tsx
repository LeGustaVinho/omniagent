import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ChildLocation } from "@/conversation-children";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import { downloadArtifact } from "@/download-artifact";

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

function Responses({ title, items }: { title: string; items: ResponseItem[] }) {
  const { copy, feedback, run } = useContextActionFeedback();
  if (!items.length) return null;
  return <details>
    <summary>{title}</summary>
    <ol aria-label={title} className="grid gap-2 text-xs">
      {items.map((item, index) => {
        const { itemId, turnId } = item.location;
        return <li key={index}>
          <ContextActions label={`${responseLabels[item.status]} ${index + 1}`} groups={[[
            { label: "Copy recorded response", run: () => copy(item.text) },
            { label: "Download response as text", run: () => void run(async () => {
              downloadArtifact(`${item.status}-response-${index + 1}.txt`, new Blob([item.text], { type: "text/plain;charset=utf-8" }));
            }) },
          ], [
            ...(itemId ? [{ label: "Copy provider item ID", run: () => copy(itemId) }] : []),
            ...(turnId ? [{ label: "Copy provider turn ID", run: () => copy(turnId) }] : []),
          ]]}>
            <div className="min-w-0 flex-1">
              <p className="text-muted-foreground">{responseLabels[item.status]}</p>
              <p className="whitespace-pre-wrap break-words">{item.text}</p>
            </div>
          </ContextActions>
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

export function ConversationResponseEvidence({ conversationId, runId, live, evidence }: {
  conversationId: string;
  runId: string;
  live: boolean;
  evidence?: ResponseEvidence;
}) {
  return <>
    {evidence && <ObservedConversationResponse evidence={evidence} />}
    <details>
      <summary>Approvals</summary>
      <ConversationApprovalEvidence conversationId={conversationId} runId={runId} live={live} />
    </details>
  </>;
}

export function ObservedConversationResponse({ evidence }: { evidence: ResponseEvidence }) {
  return <>
    <Responses title="Final responses" items={evidence.responses.items.filter((item) => item.status === "final")} />
    <Responses title="Provisional and recorded responses" items={evidence.responses.items.filter((item) => item.status !== "final")} />
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
