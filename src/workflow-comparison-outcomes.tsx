import { ConversationMarkdown } from "@/conversation-markdown";

export type CandidateOutcome = {
  jobId: string;
  runId: string;
  reportedModel: string | null;
  durationSeconds: number | null;
  inputTokens: number | null;
  outputTokens: number | null;
  cachedInputTokens: number | null;
  reasoningTokens: number | null;
  response: string | null;
  reasoning: { text: string; summary: boolean }[];
  failure: { phase: string; cause: string | null } | null;
};

export function comparisonOutcome(states: string[]) {
  if (states.some((state) => !["succeeded", "failed", "cancelled", "interrupted"].includes(state))) return "Comparison in progress";
  if (states.every((state) => state === "succeeded")) return "Comparison complete";
  if (states.some((state) => state === "succeeded")) return "Partial result";
  if (states.every((state) => state === "cancelled")) return "Comparison cancelled";
  return "Comparison failed";
}

export function ComparisonCandidateOutcome({ outcome, succeeded }: { outcome?: CandidateOutcome; succeeded: boolean }) {
  if (!outcome) return null;
  const measurements = [
    ["Input tokens", outcome.inputTokens], ["Output tokens", outcome.outputTokens],
    ["Cached input tokens", outcome.cachedInputTokens], ["Reasoning tokens", outcome.reasoningTokens],
  ] as const;
  return <div className="pipeline-stack min-w-0">
    <dl className="pipeline-data text-sm">
      <dt>Reported model</dt><dd className="break-all">{outcome.reportedModel ?? "Unreported"}</dd>
      {outcome.durationSeconds !== null && <><dt>Duration</dt><dd>{outcome.durationSeconds} s</dd></>}
      {measurements.map(([label, value]) => value !== null && <div className="contents" key={label}><dt>{label}</dt><dd>{value}</dd></div>)}
    </dl>
    {outcome.response !== null && <article aria-label="Original response" className="break-words"><ConversationMarkdown>{outcome.response}</ConversationMarkdown></article>}
    {succeeded && outcome.response === null && <p className="text-sm text-muted-foreground">The original response is unavailable.</p>}
    {outcome.failure && <p className="text-sm">{outcome.failure.cause?.replaceAll("_", " ")} · {outcome.failure.phase.replaceAll("_", " ")}</p>}
    {outcome.reasoning.length > 0 && <details className="pipeline-evidence"><summary>Available reasoning</summary>
      {outcome.reasoning.map(({ text }, index) => <pre key={index} className="whitespace-pre-wrap break-words text-sm">{text}</pre>)}
    </details>}
  </div>;
}
