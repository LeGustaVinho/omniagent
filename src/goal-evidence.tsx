import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { Provider } from "@/bindings/Provider";
import { Button } from "@/components/ui/button";
import type { WorkspaceProps } from "@/workbench-contract";

type NativeObservation = {
  state: string;
  reportedState: string | null;
  tokensUsed: number | null;
  tokenBudget: number | null;
  elapsedSeconds: number | null;
  evaluation: { verdict: string | null; reason: string | null; evaluator: string | null } | null;
};

export type InspectedGoal = {
  revisionId: string;
  definition: {
    objective: string; criteria: string[];
    maxTurns: number | null; maxSeconds: number | null; tokenBudget: number | null;
  };
  continuationOwner: "managed" | "native";
  controlState: string;
  blocker: string | null;
  checkpoint: {
    ordinal: number; runId: string; nextAction: string;
    report: { evidence: string; action: string } | null;
  } | null;
  attempt: { runId: string; conversationId: string } | null;
  evidence: {
    provider: Provider;
    usage: { eventSequence: string; inputTokens: number | null; outputTokens: number | null; cachedInputTokens: number | null } | null;
    nativeStart: { eventSequence: string; frame: string; outcome: string | null } | null;
    nativeTurns: { turnId: string; startedEvent: string; finishedEvent: string | null; outcome: string | null }[];
    nativeObservation: { eventSequence: string; observation: NativeObservation } | null;
    providerResult: string | null;
  } | null;
};

const formatState = (key: string) => key.replaceAll("_", " ");
const providerLabels: Record<Provider, string> = { codex: "Codex", claude: "Claude", antigravity: "Antigravity" };

function AttemptEvidence({ evidence }: { evidence: NonNullable<InspectedGoal["evidence"]> }) {
  const { nativeStart, nativeTurns, nativeObservation, providerResult, usage } = evidence;
  const provider = providerLabels[evidence.provider];
  const observation = nativeObservation?.observation;
  const evaluation = observation?.evaluation;
  return <>
    {usage && <dl className="grid gap-1">
      <dt>Attempt usage · {provider} event {usage.eventSequence}</dt>
      <dd>
        {usage.inputTokens?.toLocaleString() ?? "Unreported"} input · {usage.outputTokens?.toLocaleString() ?? "Unreported"} output
        {usage.cachedInputTokens !== null && ` · ${usage.cachedInputTokens.toLocaleString()} cached input`}
      </dd>
    </dl>}
    {nativeStart && <details>
      <summary>Exact application goal command</summary>
      <p>Application event {nativeStart.eventSequence} · {nativeStart.outcome ? formatState(nativeStart.outcome) : "Delivery unconfirmed"}</p>
      <pre className="max-h-60 overflow-auto whitespace-pre-wrap break-words">{nativeStart.frame}</pre>
    </details>}
    {(observation || nativeTurns.length > 0 || providerResult) && <details>
      <summary>Native goal evidence · {provider}</summary>
      {providerResult && <p>Recorded provider result <span className="capitalize">{formatState(providerResult)}</span></p>}
      {observation && <dl className="grid gap-1">
        <dt>Reported goal state · event {nativeObservation?.eventSequence}</dt>
        <dd className="capitalize">{observation.reportedState ?? formatState(observation.state)}</dd>
        {observation.tokensUsed !== null && <><dt>Provider-reported goal tokens</dt>
          <dd>{observation.tokensUsed.toLocaleString()}{observation.tokenBudget !== null && ` / ${observation.tokenBudget.toLocaleString()}`}</dd></>}
        {observation.elapsedSeconds !== null && <><dt>Provider-reported elapsed time</dt><dd>{observation.elapsedSeconds} seconds</dd></>}
        {evaluation && <>
          <dt>Provider evaluator{evaluation.evaluator && ` · ${evaluation.evaluator}`}</dt>
          {evaluation.verdict && <dd>{evaluation.verdict}</dd>}
          {evaluation.reason && <dd className="whitespace-pre-wrap">{evaluation.reason}</dd>}
        </>}
      </dl>}
      {nativeTurns.length > 0 && <ol aria-label="Provider-reported continuation turns">
        {nativeTurns.map((turn) => <li key={turn.startedEvent} className="break-words">
          <code>{turn.turnId}</code> · Start event {turn.startedEvent}
          {turn.finishedEvent && ` · End event ${turn.finishedEvent}`}
          {turn.outcome && ` · ${formatState(turn.outcome)}`}
        </li>)}
      </ol>}
    </details>}
  </>;
}

export function GoalEvidence({ goal, runId, openActivity }: {
  goal: InspectedGoal;
  runId?: string;
  openActivity?: WorkspaceProps["openActivity"];
}) {
  const { attempt, blocker, checkpoint, continuationOwner, definition, evidence } = goal;
  const budgets = [
    definition.maxTurns !== null && `${definition.maxTurns} turns`,
    definition.maxSeconds !== null && `${definition.maxSeconds} seconds`,
    definition.tokenBudget !== null && `${definition.tokenBudget.toLocaleString()} tokens`,
  ].filter(Boolean);
  return <section className="grid gap-2" aria-label="Goal evidence">
    <dl className="grid gap-1">
      <dt>Goal objective</dt><dd className="whitespace-pre-wrap break-words">{definition.objective}</dd>
      <dt>Goal revision</dt><dd className="break-all">{goal.revisionId}</dd>
      <dt>Continuation owner</dt><dd>{continuationOwner === "managed" ? "OmniAgent" : "Provider"}</dd>
      <dt>Goal control</dt><dd className="capitalize">{formatState(goal.controlState)}</dd>
      {budgets.length > 0 && <><dt>Goal budget</dt><dd>{budgets.join(" · ")}</dd></>}
      {checkpoint && <>
        <dt>Latest checkpoint</dt><dd className="capitalize">Turn {checkpoint.ordinal} · {formatState(checkpoint.nextAction)}</dd>
      </>}
      {blocker && <><dt>Goal blocker</dt><dd>{blocker}</dd></>}
    </dl>
    {definition.criteria.length > 0 && <details>
      <summary>Goal criteria</summary>
      <ol className="list-decimal pl-5">{definition.criteria.map((criterion, index) => <li key={index}>{criterion}</li>)}</ol>
    </details>}
    {checkpoint?.report && <details>
      <summary>Agent checkpoint report</summary>
      <p className="whitespace-pre-wrap break-words">{checkpoint.report.evidence}</p>
    </details>}
    {evidence && <AttemptEvidence evidence={evidence} />}
    {attempt && attempt.runId !== runId && openActivity && <Button type="button" variant="outline" className="justify-self-start"
      onClick={() => openActivity(attempt.runId, attempt.conversationId)}>Open goal attempt and exact input</Button>}
  </section>;
}

export function TaskGoals({ projectId, taskId, openActivity }: {
  projectId: string; taskId: string; openActivity: WorkspaceProps["openActivity"];
}) {
  const [open, setOpen] = useState(false);
  const query = useQuery({
    queryKey: ["project-task-goals", projectId, taskId],
    queryFn: () => invoke<InspectedGoal[]>("project_task_goals", { projectId, taskId }),
    enabled: open,
    refetchInterval: open ? 2500 : false,
  });
  return <details className="pipeline-evidence" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Goals</summary>
    {open && query.isPending && <p role="status">Loading goals…</p>}
    {query.error && <p role="alert">{String(query.error)}</p>}
    {query.data?.length === 0 && <p>No goals linked to this work item.</p>}
    {query.data?.map((goal) => <GoalEvidence key={goal.revisionId} goal={goal} openActivity={openActivity} />)}
  </details>;
}
