import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Target } from "lucide-react";
import { Dialog } from "radix-ui";
import { useState, type FormEvent } from "react";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ModelPicker } from "@/components/model-picker";

export type GoalDefinition = {
  objective: string;
  criteria: string[];
  maxTurns: number | null;
  maxSeconds: number | null;
  tokenBudget: number | null;
};
type GoalControlState = "active" | "pausing" | "paused" | "cancelling" | "cancelled" | "needs_review";
type Verification = "verifying" | "failed" | "completed";
type Goal = {
  id: string;
  definition: GoalDefinition;
  continuationOwner: "managed" | "native";
  controlState: GoalControlState;
  provider: string;
  model: string | null;
  job: { projectId: string };
  checkpoint: { ordinal: number; runId: string; nextAction: string } | null;
  verification: { status: Verification } | null;
};
export type ConversationGoalOverview = {
  goal: Goal;
  activity: {
    blocker: string | null;
    attempt: { runId: string; conversationId: string } | null;
    evidence: {
      nativeObservation: { observation: { state: string; tokensUsed: number | null; tokenBudget: number | null; elapsedSeconds: number | null } } | null;
      usage: { inputTokens: number | null; outputTokens: number | null } | null;
    } | null;
  };
  history: { revisionId: string; objective: string; controlState: GoalControlState; verification: Verification | null; runId: string | null }[];
  managedReason: string | null;
};

const goalKey = (conversationId: string) => ["conversation", conversationId, "goal-overview"];

export function useConversationGoal(conversationId: string, visible: boolean) {
  const client = useQueryClient();
  const key = goalKey(conversationId);
  const overview = useQuery({
    queryKey: key, enabled: visible,
    queryFn: () => invoke<ConversationGoalOverview | null>("conversation_goal_overview", { conversationId }),
    refetchInterval: (query) => visible && pendingGoal(query.state.data?.goal) ? 750 : false,
  });
  const refresh = () => client.invalidateQueries({ queryKey: key });
  return { overview, refresh };
}

function pendingGoal(goal?: Goal) {
  return goal && goal.controlState !== "cancelled" && goal.verification?.status !== "completed";
}

export function hasCurrentGoal(overview: ConversationGoalOverview | null | undefined) {
  return Boolean(pendingGoal(overview?.goal));
}

export function goalDefinition(objective: string): GoalDefinition {
  return { objective, criteria: [], maxTurns: null, maxSeconds: null, tokenBudget: null };
}

const label = (state: string) => state.charAt(0).toUpperCase() + state.slice(1).replaceAll("_", " ");

export function ConversationGoalCard({ value, refresh, openRun }: {
  value: ConversationGoalOverview;
  refresh: () => Promise<unknown>;
  openRun: (id: string) => void;
}) {
  const { goal, activity, history, managedReason } = value;
  const action = useMutation({
    mutationFn: (change: "pause" | "resume" | "cancel") => change === "resume"
      ? invoke("resume_goal", { projectId: goal.job.projectId, revisionId: goal.id })
      : invoke("request_goal_stop", { projectId: goal.job.projectId, revisionId: goal.id, action: change }),
    onSettled: refresh,
  });
  const native = activity.evidence?.nativeObservation?.observation;
  const usage = activity.evidence?.usage;
  const status = goal.verification?.status === "completed" ? "Completed"
    : goal.controlState !== "active" ? label(goal.controlState)
      : activity.blocker ?? label(native?.state ?? "active");
  const attempt = activity.attempt?.runId ?? goal.checkpoint?.runId;
  return <section className="conv-goal conv-goal-live" aria-label="Conversation goal">
    <header className="conv-goal-heading"><Target size={16} aria-hidden /><strong>Goal</strong><span role="status">{status}</span></header>
    <p className="whitespace-pre-wrap break-words">{goal.definition.objective}</p>
    {goal.definition.criteria.length > 0 && <ul aria-label="Goal criteria" className="list-disc pl-4">
      {goal.definition.criteria.map((criterion, index) => <li key={index}>{criterion}</li>)}
    </ul>}
    <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-1 text-xs">
      <dt>Model</dt><dd>{goal.model ?? goal.provider}</dd>
      <dt>Continuation</dt><dd>{goal.continuationOwner === "native" ? "Native" : "OmniAgent"}</dd>
      {goal.definition.maxTurns !== null && <><dt>Turn limit</dt><dd>{goal.definition.maxTurns}</dd></>}
      {goal.definition.maxSeconds !== null && <><dt>Time limit</dt><dd>{goal.definition.maxSeconds} s</dd></>}
      {(native?.tokenBudget ?? goal.definition.tokenBudget) !== null && <><dt>Token budget</dt><dd>{native?.tokenBudget ?? goal.definition.tokenBudget}</dd></>}
      {native?.tokensUsed != null && <><dt>Tokens used</dt><dd>{native.tokensUsed}</dd></>}
      {native?.elapsedSeconds != null && <><dt>Elapsed</dt><dd>{native.elapsedSeconds} s</dd></>}
      {native?.tokensUsed == null && usage?.inputTokens != null && <><dt>Last reported input tokens</dt><dd>{usage.inputTokens}</dd></>}
      {native?.tokensUsed == null && usage?.outputTokens != null && <><dt>Last reported output tokens</dt><dd>{usage.outputTokens}</dd></>}
    </dl>
    {managedReason && <p className="text-xs text-muted-foreground">{managedReason}</p>}
    <div className="flex flex-wrap gap-2">
      {pendingGoal(goal) && goal.controlState === "active" && <button type="button" className="conv-quiet-button" disabled={action.isPending}
        onClick={() => action.mutate("pause")}>Pause goal</button>}
      {goal.controlState === "paused" && <button type="button" className="conv-quiet-button" disabled={action.isPending}
        onClick={() => action.mutate("resume")}>Resume goal</button>}
      {pendingGoal(goal) && ["active", "paused", "needs_review"].includes(goal.controlState) &&
        <button type="button" className="conv-quiet-button" disabled={action.isPending} onClick={() => action.mutate("cancel")}>Cancel goal</button>}
      {attempt && <button type="button" className="conv-quiet-button" onClick={() => openRun(attempt)}>Open goal activity</button>}
    </div>
    {action.error && <p role="alert" className="conv-alert">{String(action.error)}</p>}
    {history.length > 0 && <details><summary>Earlier goals</summary><ul className="mt-1 grid gap-1">
      {history.map((item) => <li key={item.revisionId}>
        {item.objective} · {label(item.verification ?? item.controlState)}
        {item.runId && <button type="button" className="conv-quiet-button" onClick={() => openRun(item.runId!)}>Open activity</button>}
      </li>)}
    </ul></details>}
  </section>;
}

export function GoalSettingsDialog({ objective, options, selected, onSelect, value, refresh, openRun, pending, error, canStart, onStart, onClose }: {
  objective: string;
  options: ModelOption[];
  selected: ModelSelection | null;
  onSelect: (model: ModelSelection | null) => void;
  value: ConversationGoalOverview | null | undefined;
  refresh: () => Promise<unknown>;
  openRun: (id: string) => void;
  pending: boolean;
  error?: string;
  canStart: boolean;
  onStart: (definition: GoalDefinition) => void;
  onClose: () => void;
}) {
  const [definition, setDefinition] = useState(() => goalDefinition(objective));
  function submit(event: FormEvent) {
    event.preventDefault();
    if (!canStart || pending || !definition.objective.trim()) return;
    onStart({ ...definition, objective: definition.objective.trim(), criteria: definition.criteria.map((item) => item.trim()).filter(Boolean) });
  }
  return <Dialog.Root open onOpenChange={(open) => { if (!open && !pending) onClose(); }}>
    <Dialog.Portal><Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog max-h-[90vh] overflow-auto">
        <Dialog.Title>Goal Mode</Dialog.Title>
        <Dialog.Description>Set the objective and any limits for this conversation.</Dialog.Description>
        {value && hasCurrentGoal(value) ? <ConversationGoalCard value={value} refresh={refresh} openRun={openRun} />
          : <form onSubmit={submit} className="grid gap-2">
            <label>Objective<textarea required value={definition.objective} onChange={(event) => setDefinition({ ...definition, objective: event.target.value })} /></label>
            <label>Criteria, one per line<textarea value={definition.criteria.join("\n")}
              onChange={(event) => setDefinition({ ...definition, criteria: event.target.value.split("\n") })} /></label>
            <ModelPicker label="Goal model" layout="inline" options={options} selection={{ mode: "single", selected, onChange: onSelect }} />
            {([["maxTurns", "Turn limit", 4294967295], ["maxSeconds", "Time limit in seconds", 4294967295], ["tokenBudget", "Token budget", Number.MAX_SAFE_INTEGER]] as const)
              .map(([field, title, max]) => <label key={field}>{title}<input type="number" min="1" max={max} step="1"
                value={definition[field] ?? ""} onChange={(event) => setDefinition({ ...definition, [field]: event.target.value ? event.target.valueAsNumber : null })} /></label>)}
            <button type="submit" className="conv-quiet-button" disabled={pending || !canStart || !definition.objective.trim()}>{pending ? "Starting…" : "Start goal"}</button>
          </form>}
        {error && <p role="alert" className="conv-alert">{error}</p>}
        <Dialog.Close asChild><button type="button" className="conv-quiet-button" disabled={pending}>Close</button></Dialog.Close>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
