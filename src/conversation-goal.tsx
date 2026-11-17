import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ArrowRight, CircleHelp, Clock3, Coins, ListChecks, Repeat2, Target, X } from "lucide-react";
import { Dialog } from "radix-ui";
import { useId, useRef, useState, type FormEvent } from "react";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { ControlTooltip } from "@/components/ui/control-tooltip";

export type GoalDefinition = {
  objective: string;
  criteria: string[];
  maxTurns: number | null;
  maxSeconds: number | null;
  tokenBudget: number | null;
};
type GoalControlState = "active" | "pausing" | "paused" | "cancelling" | "cancelled" | "needs_review";
type Verification = "verifying" | "failed" | "completed";
export type GoalTransitionScope = {
  projectId: string;
  checkoutId: string;
  checkoutRoot: string;
  provider: string;
  profileId: string;
  model: string | null;
  effort: string | null;
  mode: string;
};
export type GoalTransitionReview = {
  revisionId: string;
  definition: GoalDefinition;
  continuationOwner: "managed" | "native";
  controlState: GoalControlState;
  checkpoint: { ordinal: number; runId: string; nextAction: string } | null;
  checkpointContextId: string | null;
  checkpointRunId: string | null;
  stopReviewRunId: string | null;
  scope: GoalTransitionScope;
  reviewToken: string;
};
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
  transition: GoalTransitionReview | null;
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

export function GoalExecutionScope({ scope }: { scope: GoalTransitionScope }) {
  return <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-1 text-xs">
    <dt>Checkout</dt><dd><code className="break-all">{scope.checkoutRoot}</code></dd>
    <dt>Profile</dt><dd>{scope.provider} · {scope.profileId}</dd>
    {scope.model && <><dt>Model</dt><dd>{scope.model}</dd></>}
    {scope.effort && <><dt>Effort</dt><dd>{scope.effort}</dd></>}
    <dt>Execution mode</dt><dd>{label(scope.mode)}</dd>
  </dl>;
}

export function GoalCheckpointSummary({ review }: { review: GoalTransitionReview }) {
  return <>
    <p className="whitespace-pre-wrap break-words">{review.definition.objective}</p>
    <p className="text-xs">{review.continuationOwner === "native" ? "Native" : "OmniAgent"} goal · {label(review.controlState)}</p>
    {review.checkpoint && <p className="text-xs">Checkpoint {review.checkpoint.ordinal} · {label(review.checkpoint.nextAction)}</p>}
    {review.checkpointRunId && <p className="break-all text-xs">Checkpoint run <code>{review.checkpointRunId}</code></p>}
    {!review.checkpointRunId && review.checkpointContextId && <p className="break-all text-xs">Checkpoint context <code>{review.checkpointContextId}</code></p>}
    <GoalExecutionScope scope={review.scope} />
  </>;
}

export function ConversationGoalCard({ value, refresh, openRun }: {
  value: ConversationGoalOverview;
  refresh: () => Promise<unknown>;
  openRun: (id: string) => void;
}) {
  const { goal, activity, history, managedReason, transition } = value;
  const [resumeReview, setResumeReview] = useState<GoalTransitionReview>();
  const [stopReview, setStopReview] = useState<{ revisionId: string; runId: string }>();
  const stopRunId = transition?.stopReviewRunId;
  const ids = { projectId: goal.job.projectId, revisionId: goal.id };
  const action = useMutation({
    mutationFn: (change: { type: "pause" | "cancel" } | { type: "resume"; reviewToken: string } | { type: "confirm_stop"; runId: string }) => {
      if (change.type === "resume") return invoke("resume_goal", { ...ids, reviewToken: change.reviewToken });
      if (change.type === "confirm_stop") return invoke(goal.continuationOwner === "native" ? "confirm_native_goal_stop" : "confirm_managed_goal_stop", {
        ...ids, runId: change.runId,
      });
      return invoke("request_goal_stop", { ...ids, action: change.type });
    },
    onSuccess: () => { setResumeReview(undefined); setStopReview(undefined); },
    onSettled: refresh,
  });
  const resumeCurrent = resumeReview?.reviewToken === transition?.reviewToken && goal.controlState === "paused";
  const stopCurrent = stopReview?.revisionId === goal.id && stopReview?.runId === stopRunId && goal.controlState === "needs_review";
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
        onClick={() => action.mutate({ type: "pause" })}>Pause goal</button>}
      {goal.controlState === "paused" && transition && <button type="button" className="conv-quiet-button" disabled={action.isPending}
        onClick={() => setResumeReview(transition)}>Resume goal</button>}
      {pendingGoal(goal) && ["active", "paused"].includes(goal.controlState) &&
        <button type="button" className="conv-quiet-button" disabled={action.isPending} onClick={() => action.mutate({ type: "cancel" })}>Cancel goal</button>}
      {goal.controlState === "needs_review" && stopRunId && <button type="button" className="conv-quiet-button" disabled={action.isPending}
        onClick={() => setStopReview({ revisionId: goal.id, runId: stopRunId })}>Review executor stop</button>}
      {attempt && <button type="button" className="conv-quiet-button" onClick={() => openRun(attempt)}>Open goal activity</button>}
    </div>
    {resumeReview && <section aria-label="Review goal resume" className="grid gap-2 rounded-md border border-border p-2">
      <GoalCheckpointSummary review={resumeReview} />
      {resumeReview.continuationOwner === "native" && <p className="text-xs">Resume from this checkpoint in a fresh provider session.</p>}
      {!resumeCurrent && <p role="status">The goal checkpoint changed. Open the resume review again.</p>}
      <button type="button" className="conv-quiet-button" disabled={action.isPending || !resumeCurrent}
        onClick={() => action.mutate({ type: "resume", reviewToken: resumeReview.reviewToken })}>Confirm goal resume</button>
    </section>}
    {stopReview && <section aria-label="Review executor stop" className="grid gap-2 rounded-md border border-border p-2">
      <p className="break-all text-xs">Executor run <code>{stopReview.runId}</code></p>
      {goal.continuationOwner === "managed" && <p className="text-xs">Confirm after checking that this executor has stopped.</p>}
      <button type="button" className="conv-quiet-button" disabled={action.isPending || !stopCurrent}
        onClick={() => action.mutate({ type: "confirm_stop", runId: stopReview.runId })}>
        {goal.continuationOwner === "native" ? "Verify saved stop evidence" : "Confirm executor stopped"}
      </button>
    </section>}
    {action.error && <p role="alert" className="conv-alert">{String(action.error)}</p>}
    {history.length > 0 && <details><summary>Earlier goals</summary><ul className="mt-1 grid gap-1">
      {history.map((item) => <li key={item.revisionId}>
        {item.objective} · {label(item.verification ?? item.controlState)}
        {item.runId && <button type="button" className="conv-quiet-button" onClick={() => openRun(item.runId!)}>Open activity</button>}
      </li>)}
    </ul></details>}
  </section>;
}

export function GoalSettingsDialog({ objective, options, loadingModels, selected, onSelect, value, refresh, openRun, pending, error, canStart, onStart, onClose }: {
  objective: string;
  options: ModelOption[];
  loadingModels?: boolean;
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
  const formId = useId();
  const opener = useRef<HTMLElement | null>(null);
  const objectiveField = useRef<HTMLTextAreaElement>(null);
  const activeGoal = value && hasCurrentGoal(value);
  const modelAvailable = options.some(({ selectable, selection }) => selectable && selection.provider === selected?.provider
    && selection.profileId === selected.profileId && selection.modelId === selected.modelId);
  const limits = [
    { field: "maxTurns", title: "Turn limit", icon: Repeat2, max: 4294967295 },
    { field: "maxSeconds", title: "Time limit in seconds", icon: Clock3, max: 4294967295 },
    { field: "tokenBudget", title: "Token budget", icon: Coins, max: Number.MAX_SAFE_INTEGER },
  ] as const;
  function submit(event: FormEvent) {
    event.preventDefault();
    if (!canStart || pending || !definition.objective.trim()) return;
    onStart({ ...definition, objective: definition.objective.trim(), criteria: definition.criteria.map((item) => item.trim()).filter(Boolean) });
  }
  return <Dialog.Root open onOpenChange={(open) => { if (!open && !pending) onClose(); }}>
    <Dialog.Portal><Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog conv-dialog-layout conv-goal-dialog" onOpenAutoFocus={(event) => {
        opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
        if (objectiveField.current) {
          event.preventDefault();
          objectiveField.current.focus({ preventScroll: true });
        }
      }} onCloseAutoFocus={(event) => {
        if (opener.current?.getClientRects().length) {
          event.preventDefault();
          opener.current.focus({ preventScroll: true });
        }
      }}>
        <header className="conv-dialog-header">
          <span className="conv-dialog-mark"><Target size={20} aria-hidden /></span>
          <div><Dialog.Title>Goal Mode</Dialog.Title>
            <Dialog.Description>Set an objective the agent can work toward over multiple turns.</Dialog.Description></div>
          <Dialog.Close asChild><Button type="button" variant="ghost" className="ui-icon-button" aria-label="Close goal settings" disabled={pending}><X size={17} aria-hidden /></Button></Dialog.Close>
        </header>
        <div className="conv-dialog-body">
          {activeGoal ? <ConversationGoalCard value={value} refresh={refresh} openRun={openRun} />
            : <form id={formId} onSubmit={submit} className="conv-dialog-form conv-goal-form">
              <label htmlFor={`${formId}-objective`}><span className="conv-goal-label"><Target size={15} aria-hidden /> Objective</span>
                <textarea id={`${formId}-objective`} ref={objectiveField} rows={3} required placeholder="Prepare the release and verify the Windows package."
                  value={definition.objective} onChange={(event) => setDefinition({ ...definition, objective: event.target.value })} />
              </label>
              <div className="conv-goal-model"><ModelPicker label="Goal model" options={options} combinedCatalog loading={loadingModels}
                selection={{ mode: "single", selected, onChange: onSelect }} /></div>
              <div className="conv-goal-field"><label htmlFor={`${formId}-criteria`} className="conv-goal-label"><ListChecks size={15} aria-hidden /> Success criteria <small aria-hidden>Optional</small></label>
                <textarea id={`${formId}-criteria`} aria-describedby={`${formId}-criteria-help`} rows={3} value={definition.criteria.join("\n")}
                  placeholder={'Installer passes the smoke test\nRelease notes cover the changes'}
                  onChange={(event) => setDefinition({ ...definition, criteria: event.target.value.split("\n") })} />
                <small id={`${formId}-criteria-help`}>Enter one criterion per line.</small>
              </div>
              <fieldset className="conv-goal-limits">
                <legend>Run limits <ControlTooltip label="Leave any field empty to omit that limit."><button type="button" className="ui-icon-button" aria-label="About run limits"><CircleHelp size={15} aria-hidden /></button></ControlTooltip></legend>
                {limits.map(({ field, title, icon: Icon, max }) => <label key={field}><span className="conv-goal-label"><Icon size={15} aria-hidden /> {title}</span>
                  <input type="number" min="1" max={max} step="1" placeholder="No limit" value={definition[field] ?? ""}
                    onChange={(event) => setDefinition({ ...definition, [field]: event.target.value ? event.target.valueAsNumber : null })} /></label>)}
              </fieldset>
              {!pending && !canStart && definition.objective.trim() && <p role="status" className="conv-goal-start-note">{modelAvailable
                ? "Wait for this conversation to be ready before starting the goal." : "Choose an available model to start this goal."}</p>}
            </form>}
          {error && <p role="alert" className="conv-alert">{error}</p>}
        </div>
        <footer className="conv-dialog-actions">
          <Dialog.Close asChild><Button type="button" variant="outline" disabled={pending}>Close</Button></Dialog.Close>
          {!activeGoal && <Button type="submit" form={formId} disabled={pending || !canStart || !definition.objective.trim()}>
            {pending ? "Starting…" : "Start goal"}<ArrowRight size={16} aria-hidden />
          </Button>}
        </footer>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
