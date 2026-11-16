import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { Provider } from "@/bindings/Provider";
import type { FieldValue, QuestionAction, RequestId } from "@/conversation-inputs";

const providers: Record<Provider, string> = { codex: "Codex", claude: "Claude Code", antigravity: "Antigravity" };
const actions: Record<QuestionAction, string> = { accept: "Replied", decline: "Declined", cancel: "Cancelled" };
const states = {
  pending: "Waiting for input", saving: "Saving the turn checkpoint", saved: "Question saved",
  needs_review: "Checkpoint needs review", prepared: "Reply prepared", submitted: "Submitted to the CLI",
  unconfirmed: "Delivery unconfirmed", recorded: "Answer recorded", continued: "Linked to a continuation", closed: "Question closed",
};
export const questionSaveReasons = {
  timeout: "The live reply deadline expired.",
  provider_expired: "The provider closed the live request.",
  turn_stopped: "The turn ended with this question open.",
  runtime_restart: "The runtime restarted with this question open.",
  transport_closed: "The provider connection closed.",
  structured_output: "The turn returned a question.",
};

type TimelineEntry = {
  sequence: string;
  origin: { runId: string; requestId: RequestId };
  provider: Provider;
  route: "native" | "structured_turn";
  parentItemId: string | null;
  message: string | null;
  state: keyof typeof states;
  reason: keyof typeof questionSaveReasons | null;
  continuationRunId: string | null;
  reply: { action: QuestionAction; fields: { id: string; label: string; value: FieldValue | null }[] } | null;
};

type QuestionScope = {
  conversationId: string;
  runId: string;
  live: boolean;
};

export function useConversationQuestions({ conversationId, runId, live }: QuestionScope, enabled = true) {
  return useQuery({
    queryKey: ["conversation-run", runId, "question-timeline", conversationId],
    queryFn: () => invoke<TimelineEntry[]>("conversation_question_timeline", { conversationId, runId }),
    enabled,
    refetchInterval: ({ state }) => live || state.data?.some(({ state, reply }) =>
      state === "saved" || state === "saving" || (state === "recorded" && reply?.action === "accept")) ? 800 : false,
  });
}

export function QuestionSource({ source }: { source: Pick<TimelineEntry, "provider" | "route" | "parentItemId"> }) {
  return <p className="text-xs text-muted-foreground">
    {providers[source.provider]} · {source.route === "native" ? "Native form" : "Structured question"}
    {source.parentItemId && <> · Subagent <code className="break-all">{source.parentItemId}</code></>}
  </p>;
}

export function ConversationQuestionTimeline(scope: QuestionScope) {
  const query = useConversationQuestions(scope);
  if (!query.error && !query.data?.length) return null;
  return <section aria-label="Question timeline" className="grid gap-2">
    <h2 className="text-sm font-medium">Question timeline</h2>
    {query.error && <p role="alert" className="text-sm text-red-300">{String(query.error)}</p>}
    {query.data?.length ? <ol className="grid gap-2">
      {query.data.map(({ sequence, provider, route, parentItemId, message, state, reason, continuationRunId, reply }) =>
        <li key={sequence} className="grid gap-1 rounded-md border border-border bg-card p-3 text-sm">
          <p className="whitespace-pre-wrap font-medium">{message || "Input requested"}</p>
          <QuestionSource source={{ provider, route, parentItemId }} />
          <p className="text-xs text-muted-foreground">
            {reply && `${actions[reply.action]} · `}{states[state]}
          </p>
          {reason && <p className="text-xs text-muted-foreground">{questionSaveReasons[reason]}</p>}
          {Boolean(reply?.fields.length) && <dl className="grid gap-1.5">
            {reply?.fields.map(({ id, label, value }) => <div key={id} className="grid gap-0.5">
              <dt className="font-medium">{label}</dt>
              <dd className="whitespace-pre-wrap break-words">
                {Array.isArray(value) ? <ul className="list-disc pl-5">
                  {value.map((choice) => <li key={choice}>{choice}</li>)}
                </ul> : value === null ? "Protected answer" : typeof value === "boolean" ? (value ? "Yes" : "No") : value}
              </dd>
            </div>)}
          </dl>}
          {continuationRunId && <p className="break-all text-xs text-muted-foreground">Continuation <code>{continuationRunId}</code></p>}
        </li>)}
    </ol> : null}
  </section>;
}
