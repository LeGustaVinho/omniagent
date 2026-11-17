import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Activity, Square, X } from "lucide-react";
import { useContext, useEffect, useRef, useState } from "react";
import type { RunRecord } from "@/bindings/RunRecord";
import type { SubagentAuditGap } from "@/bindings/SubagentAuditGap";
import { Button } from "@/components/ui/button";
import { ConversationAgentTree, recordChildActivity, type ChildActivity, type ChildLocation, type Delegation } from "@/conversation-children";
import type { AttachmentDraftDestination } from "@/conversation-attachment-evidence";
import { ConversationChecklistProgress } from "@/conversation-checklist-progress";
import { ConversationDeliveries, type ConversationInput } from "@/conversation-deliveries";
import { ConversationInventory } from "@/conversation-inventory";
import { ConversationSidePanel } from "@/conversation-layout";
import { ConversationProfile } from "@/conversation-profile";
import { PendingInput, type ConversationPendingInput } from "@/conversation-inputs";
import { RunOrchestrationPanel } from "@/run-orchestration";
import { RunInspectorNavigation } from "@/run-inspector-navigation";
import { ConversationQuestionTimeline, QuestionSource, questionSaveReasons, useConversationQuestions } from "@/conversation-question-timeline";
import {
  ConversationResponseEvidence, cloneTurnResponseEvidence, newTurnResponseEvidence, recordResponseEvidence,
  type ResponseObservation, type TurnResponseEvidence,
} from "@/conversation-response-evidence";
import type { QuestionFocus, WorkspaceProps } from "@/workbench-contract";
import { ConversationSubagentQuotaPanel } from "@/subagent-quota";
import { AttemptSubagentAudit } from "@/subagent-audit-controls";
import { useWorkspace } from "@/workspace-state";

type EventData =
  | { type: "dispatch_submitted"; dispatch_id: string }
  | { type: "context_read"; path: string; tool: string }
  | ResponseObservation
  | { type: "plan_proposed"; text: string }
  | { type: "error"; message: string }
  | { type: "tool"; name: string; phase: string }
  | { type: "runtime_inspected" }
  | { type: "downstream_wait_requested"; wait_id: string }
  | { type: "downstream_continuation"; wait_id: string; parent_run_id: string }
  | { type: "reasoning"; text: string; summary: boolean }
  | { type: "reasoning_usage"; tokens: number }
  | { type: "usage"; input: number | null; output: number | null; cached_input: number | null }
  | { type: "context_window_usage"; used: number; size: number }
  | { type: "subagent_audit_gap"; report: SubagentAuditGap }
  | ChildActivity;
type Page = {
  cursor: string;
  more: boolean;
  events: { data: EventData; location: ChildLocation }[];
};
type SavedQuestion = {
  event: ConversationPendingInput;
  state: "saving" | "saved" | "needs_review";
  reason: keyof typeof questionSaveReasons;
  answerRecorded: boolean;
};
type Projection = {
  cursor: string;
  streamed: string;
  final: string;
  plan: string;
  error: string;
  delegation: Delegation;
  auditGaps: SubagentAuditGap[];
  reasoning: Extract<EventData, { type: "reasoning" }>[];
  reasoningTokens?: number;
  responses: TurnResponseEvidence;
  tools: Extract<EventData, { type: "tool" }>[];
  promptSubmitted: boolean;
  fileReads: Extract<EventData, { type: "context_read" }>[];
  usage?: Extract<EventData, { type: "usage" }>;
  contextWindow?: Extract<EventData, { type: "context_window_usage" }>;
};
const emptyProjection: Projection = {
  cursor: "0", streamed: "", final: "", plan: "", error: "", reasoning: [], tools: [],
  responses: newTurnResponseEvidence(),
  delegation: { children: {}, parentActivity: {} }, auditGaps: [],
  promptSubmitted: false, fileReads: [],
};
const labels = {
  starting: "Starting", running: "Running", waiting_for_input: "Waiting for input",
  succeeded: "Completed", failed: "Failed", cancelled: "Cancelled", interrupted: "Interrupted",
};
const liveStatuses = new Set(["starting", "running", "waiting_for_input"]);

export function isLiveRun(run: Pick<RunRecord, "status"> | null | undefined) {
  return Boolean(run && liveStatuses.has(run.status));
}

export function useConversationRun(conversationId: string, run: RunRecord | null | undefined, cancelRequested: boolean) {
  const queryClient = useQueryClient();
  const runId = run?.id;
  const target = { conversationId, runId };
  const live = isLiveRun(run);
  const projectionKey = ["conversation-run", runId, "output", live];
  const inputKey = ["conversation-run", runId, "pending"];
  const [stopState, setStopState] = useState<{ runId: string; error?: string }>();
  const result = useQuery({
    queryKey: projectionKey,
    enabled: Boolean(runId),
    refetchInterval: live ? 200 : false,
    queryFn: async () => {
      const prior = queryClient.getQueryData<Projection>(projectionKey)
        ?? queryClient.getQueryData<Projection>(["conversation-run", runId, "output", true])
        ?? emptyProjection;
      const current = {
        ...prior,
        reasoning: [...prior.reasoning],
        responses: cloneTurnResponseEvidence(prior.responses),
        tools: [...prior.tools],
        fileReads: [...prior.fileReads],
        delegation: { children: { ...prior.delegation.children }, parentActivity: { ...prior.delegation.parentActivity } },
        auditGaps: [...prior.auditGaps],
      };
      let page: Page;
      do {
        page = await invoke<Page>("conversation_events", { ...target, after: current.cursor });
        current.cursor = page.cursor;
        for (const event of page.events) {
          const data = event.data;
          if (data.type === "child_activity") recordChildActivity(current.delegation, data, event.location);
          if (data.type === "subagent_audit_gap") current.auditGaps.push(data.report);
          if (event.location.parentItemId !== null) continue;
          if (data.type === "dispatch_submitted") current.promptSubmitted = true;
          if (data.type === "context_read" && current.promptSubmitted) current.fileReads.push(data);
          if (data.type === "assistant_delta") current.streamed += data.text;
          if (data.type === "assistant_message") current.final += data.text;
          if (data.type === "assistant_delta" || data.type === "assistant_message" || data.type === "assistant_final") {
            recordResponseEvidence(current.responses, data, event.location);
          }
          if (data.type === "plan_proposed") current.plan = data.text;
          if (data.type === "error") current.error = data.message;
          if (data.type === "reasoning") current.reasoning.push(data);
          if (data.type === "reasoning_usage") current.reasoningTokens = data.tokens;
          if (data.type === "tool") current.tools.push(data);
          if (data.type === "runtime_inspected") current.tools.push({ type: "tool", name: "inspect_omniagent", phase: "Scope inspected" });
          if (data.type === "downstream_wait_requested") current.tools.push({ type: "tool", name: "wait_for_jobs", phase: `Wait ${data.wait_id} recorded` });
          if (data.type === "downstream_continuation") current.tools.push({ type: "tool", name: "wait_for_jobs", phase: `Continued wait ${data.wait_id}` });
          if (data.type === "usage") current.usage = data;
          if (data.type === "context_window_usage") current.contextWindow = data;
        }
      } while (page.more);
      return current;
    },
  });
  const inputs = useQuery({
    queryKey: inputKey,
    enabled: live,
    refetchInterval: live ? 200 : false,
    queryFn: () => invoke<ConversationPendingInput[]>("pending_conversation_inputs", target),
  });
  const saved = useQuery({
    queryKey: ["conversation-run", runId, "saved-questions", live],
    enabled: Boolean(runId),
    refetchInterval: (query) => live ? 200 : query.state.data?.length ? 800 : false,
    queryFn: () => invoke<SavedQuestion[]>("saved_conversation_questions", target),
  });
  const currentStop = stopState?.runId === runId ? stopState : undefined;
  const stopping = cancelRequested || Boolean(currentStop && !currentStop.error);
  async function stop() {
    if (!runId || stopping) return;
    setStopState({ runId });
    try {
      await invoke("stop_conversation_turn", target);
    } catch (error) {
      setStopState({ runId, error: String(error) });
    }
  }

  return {
    projection: result.data,
    output: result.data?.plan || result.data?.final || result.data?.streamed,
    error: currentStop?.error || result.data?.error || result.error || inputs.error || saved.error,
    inputs: live ? inputs.data ?? [] : [],
    savedQuestions: saved.data ?? [],
    refreshRequests: () => {
      if (live) void inputs.refetch();
      void saved.refetch();
      void queryClient.invalidateQueries({ queryKey: ["conversation-run", runId, "question-timeline"] });
      void queryClient.invalidateQueries({ queryKey: ["conversation-run", runId, "approval-evidence"] });
      void queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "latest-job"] });
      void queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "latest-run"] });
      void queryClient.invalidateQueries({ queryKey: ["project-job-queue"] });
    },
    live,
    stop,
    stopping,
  };
}

export function StopConversation({ stop, stopping }: ReturnType<typeof useConversationRun>) {
  return <Button type="button" variant="outline" className="conv-stop" disabled={stopping} onClick={() => void stop()}>
    <Square className="size-3.5" aria-hidden /> {stopping ? "Stopping…" : "Stop"}
  </Button>;
}

export function ConversationRequests({ activity, conversationId, runId, focusQuestion }: {
  activity: ReturnType<typeof useConversationRun>;
  conversationId: string;
  runId: string;
  focusQuestion?: QuestionFocus;
}) {
  const requests = useRef<HTMLElement>(null);
  const focused = useRef<QuestionFocus>(undefined);
  const hasQuestions = activity.savedQuestions.length > 0 || activity.inputs.some(({ data }) => data.type === "question");
  const questions = useConversationQuestions({ conversationId, runId, live: activity.live }, hasQuestions);
  const sourceFor = (requestId: string | number) => {
    const match = questions.data?.find(({ origin }) => origin.requestId === requestId);
    return match && <QuestionSource source={match} />;
  };
  useEffect(() => {
    if (!focusQuestion || focused.current === focusQuestion || !requests.current) return;
    const id = JSON.stringify(focusQuestion.requestId);
    const target = Array.from(requests.current.querySelectorAll<HTMLElement>("[data-question-request]"))
      .find((question) => question.dataset.questionRequest === id);
    if (!target) return;
    target.focus();
    focused.current = focusQuestion;
  }, [focusQuestion, activity.inputs, activity.savedQuestions]);
  if (!activity.inputs.length && !activity.savedQuestions.length) return null;
  const needsReview = !activity.inputs.length &&
    activity.savedQuestions.every((question) => question.state === "needs_review");
  const answered = activity.savedQuestions.length > 0 && !activity.inputs.length &&
    activity.savedQuestions.every((question) => question.answerRecorded);
  const savedIds = new Set(activity.savedQuestions.map(({ event }) => event.data.request_id));
  return (
    <section ref={requests} aria-label="Pending requests" className="conv-requests" tabIndex={-1}>
      <p className="conv-waiting" role="status">{needsReview ? "Question needs review" : answered ? "Waiting to continue" : "Waiting for input"}</p>
      {activity.inputs.filter(({ data }) => !savedIds.has(data.request_id)).map((request) => <PendingInput key={JSON.stringify(request.data.request_id)}
        input={request} conversationId={conversationId} runId={runId} onResolved={activity.refreshRequests}
        questionSource={request.data.type === "question" ? sourceFor(request.data.request_id) : undefined} />)}
      {activity.savedQuestions.map(({ event, state, reason, answerRecorded }) => event.data.type === "question" && (
        state === "saved" && !answerRecorded
          ? <PendingInput key={JSON.stringify(event.data.request_id)} input={event} savedReason={questionSaveReasons[reason]}
              conversationId={conversationId} runId={runId} onResolved={activity.refreshRequests} questionSource={sourceFor(event.data.request_id)} />
          :
        <div key={JSON.stringify(event.data.request_id)} data-question-request={JSON.stringify(event.data.request_id)}
          tabIndex={-1} className="grid gap-2 rounded-md border border-border bg-card p-3">
          <p className="text-sm font-medium">{event.data.normalized?.message || "Input requested"}</p>
          {sourceFor(event.data.request_id)}
          <p className="text-xs text-muted-foreground">{answerRecorded ? "Answer recorded" : state === "saving" ? "Saving the turn checkpoint…" : questionSaveReasons[reason]}</p>
          {!answerRecorded && event.data.normalized?.fields.map((field) => <div key={field.id} className="grid gap-1 text-sm">
            <p>{field.label}</p>
            {field.description && <p className="text-xs text-muted-foreground">{field.description}</p>}
            {field.input.kind === "choice" && <ul className="list-disc pl-5 text-xs">
              {field.input.options.map(({ id, label }) => <li key={id}>{label}</li>)}
            </ul>}
          </div>)}
        </div>))}
    </section>
  );
}

export function ConversationActivity({ conversationId, run, projection, onClose }: {
  conversationId: string;
  run: RunRecord;
  projection?: Projection;
  onClose: () => void;
}) {
  const checkout = useWorkspace((state) => state.checkout);
  const openWorkspace = useContext(RunInspectorNavigation)?.openWorkspace;
  const draftDestination = openWorkspace && checkout?.projectId === run.projectId ? {
    checkoutId: checkout.checkoutId, open: (id: string) => { openWorkspace("conversations", id); onClose(); },
  } : undefined;
  const [showInput, setShowInput] = useState(false);
  const heading = useRef<HTMLButtonElement>(null);
  const live = isLiveRun(run);
  useEffect(() => { heading.current?.focus(); }, []);
  const scope = { conversationId, runId: run.id, live };
  return (
    <ConversationSidePanel label="Conversation activity" onKeyDown={(event) => {
      if (event.key !== "Escape") return;
      event.stopPropagation();
      onClose();
    }}>
      <header className="conv-activity-heading">
        <Activity className="size-4" aria-hidden /><h2>Activity</h2>
        <button ref={heading} type="button" className="conv-icon-button ml-auto" aria-label="Close activity" onClick={onClose}>
          <X aria-hidden />
        </button>
      </header>
      <div className="conv-activity-content">
      <dl>
        <dt>Execution</dt><dd role="status">{labels[run.status]}</dd>
        <dt>Run ID</dt><dd className="break-all font-mono">{run.id}</dd>
      </dl>
      <RunOrchestrationPanel scope={{ projectId: run.projectId, conversationId, runId: run.id }} live={live} />
      <ConversationActivityAgents {...scope} projection={projection} />
      <ConversationChecklistProgress projectId={run.projectId} runId={run.id} live={live} />
      <ConversationActivityTimeline {...scope} projection={projection} />
      <ConversationResponseEvidence {...scope} evidence={projection}
        draftSource={draftDestination && { conversationId, run, destination: draftDestination }} />
      <ConversationActivityContext {...scope} projectId={run.projectId} projection={projection} />
      <Button type="button" variant="outline" aria-expanded={showInput} onClick={() => setShowInput((visible) => !visible)}>Exact input</Button>
      {showInput && <ConversationExactInput {...scope} checkoutId={run.checkoutId} draftDestination={draftDestination} />}
      </div>
    </ConversationSidePanel>
  );
}

type ConversationActivityScope = { conversationId: string; runId: string; live: boolean };
type ActivityProjection = ConversationActivityScope & { projection?: Projection };

export function conversationRunStatus(run: RunRecord) {
  const { status } = run;
  const tone = status === "failed" || status === "interrupted" ? "danger"
    : status === "succeeded" ? "success" : status === "waiting_for_input" ? "warning"
      : isLiveRun(run) ? "progress" : undefined;
  return { label: labels[status], tone } as const;
}

export function ConversationActivityAgents({ conversationId, runId, live, projection, openActivity }: ActivityProjection & {
  openActivity?: WorkspaceProps["openActivity"];
}) {
  return <>
    <ConversationAgentTree conversationId={conversationId} runId={runId} live={live}
      delegation={projection?.delegation ?? emptyProjection.delegation} openActivity={openActivity} />
    <ConversationSubagentQuotaPanel conversationId={conversationId} runId={runId} live={live} />
    <AttemptSubagentAudit conversationId={conversationId} runId={runId} live={live} gaps={projection?.auditGaps ?? []} />
  </>;
}

export function ConversationActivityTimeline({ projection, ...scope }: ActivityProjection) {
  return <>
    <ConversationQuestionTimeline {...scope} />
    {!!projection?.tools.length && <details>
      <summary>Tool activity</summary>
      <ol>{projection.tools.map((tool, index) => <li key={index}>{tool.name} · {tool.phase}</li>)}</ol>
    </details>}
  </>;
}

export function ConversationActivityContext({ projectId, projection, ...scope }: ActivityProjection & { projectId: string }) {
  return <>
    <ConversationInventory conversationId={scope.conversationId} runId={scope.runId} />
    <ConversationProfile projectId={projectId} {...scope} />
    {!!projection?.fileReads.length && <details>
      <summary>Reported file reads after input</summary>
      <ol>{projection.fileReads.map((read, index) => <li key={index} className="break-all text-xs">{read.path} · {read.tool}</li>)}</ol>
    </details>}
  </>;
}

export function ConversationExactInput({ conversationId, runId, live, checkoutId, draftDestination }: ConversationActivityScope & {
  checkoutId?: string; draftDestination?: AttachmentDraftDestination;
}) {
  const input = useQuery({
    queryKey: ["conversation-run", runId, "input"],
    refetchInterval: live ? 400 : false,
    queryFn: () => invoke<ConversationInput | null>("conversation_input", { conversationId, runId }),
  });
  return <section className="min-w-0 rounded-md border border-border bg-card p-3" aria-label="Exact provider input">
    {input.isPending && <p className="text-xs text-muted-foreground">Loading input…</p>}
    {input.error && <p role="alert" className="text-sm text-red-300">{String(input.error)}</p>}
    {input.data === null && <p className="text-xs text-muted-foreground">No input has been prepared.</p>}
    {input.data && <ConversationDeliveries input={input.data} conversationId={conversationId} runId={runId} live={live}
      checkoutId={checkoutId} draftDestination={draftDestination} />}
  </section>;
}
