import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { X } from "lucide-react";
import { useState } from "react";
import type { Job } from "@/bindings/Job";
import type { Provider } from "@/bindings/Provider";
import type { SessionMode } from "@/bindings/SessionMode";
import { Button } from "@/components/ui/button";
import { ConversationMarkdown } from "@/conversation-markdown";
import { isLiveRun } from "@/conversation-run";
import { SettingRow } from "@/settings-controls";

type SideOutcome = { kind: "answered"; text: string; route: string } | { kind: "unavailable"; reason: string }
  | { kind: "no_text" | "failed" | "timed_out" | "cancelled" | "interrupted" };

export type OutputFixture = "audio_generation" | "browser_control" | "host_control";

export type OutputFixtureReceipt = {
  tool: string;
  revision: number;
  providerCallId: string;
  result:
    | { kind: "audio_generation"; voice: string; phrase: string; byteLen: number; sha256: string }
    | { kind: "browser_control"; browser: string; target: string; operation: string; result: string }
    | { kind: "host_control"; operation: string; before: string; after: string };
};

export interface SideRequest {
  job: Job;
  contextId: string;
  sourceRunId: string;
  provider: Provider;
  profileId: string;
  model: string | null;
  effort: string | null;
  mode: SessionMode;
  purpose: "question" | "suggestion" | "validation";
  question: string;
  response: { outcome: SideOutcome; outputFixture?: OutputFixtureReceipt } | null;
  historyPayloadDeleted?: boolean;
  outputFixture?: OutputFixture | null;
  evidenceId: string | null;
  evidenceCheckedAt: string | null;
}

type SuggestionMode = "off" | "native" | "omniagent";

export function useSuggestionMode(enabled: boolean) {
  return useQuery({
    queryKey: ["composer-suggestion-mode"],
    queryFn: () => invoke<SuggestionMode>("composer_suggestion_mode"),
    enabled,
  });
}

export function useSideRequests(conversationId: string, enabled: boolean) {
  return useQuery({
    queryKey: ["conversation", conversationId, "side-requests"],
    queryFn: () => invoke<SideRequest[]>("conversation_side_requests", { conversationId }),
    enabled,
    refetchInterval: enabled ? 1000 : false,
  });
}

export function ComposerSuggestionSettings() {
  const query = useSuggestionMode(true);
  const client = useQueryClient();
  const save = useMutation({
    mutationFn: (mode: SuggestionMode) => invoke<SuggestionMode>("set_composer_suggestion_mode", { mode }),
    onSuccess: (mode) => client.setQueryData(["composer-suggestion-mode"], mode),
  });
  return <>
    <SettingRow id="composer-suggestions" label="Composer suggestions" description="Generating a suggestion can consume model usage.">
      <select id="composer-suggestions" value={query.data ?? "native"} disabled={!query.isSuccess || save.isPending}
        onChange={(event) => save.mutate(event.target.value as SuggestionMode)}>
        <option value="off">Off</option><option value="native">Native when available</option>
        <option value="omniagent">Use OmniAgent when native support is unavailable</option>
      </select>
    </SettingRow>
    {(query.error || save.error) && <p role="alert">{String(query.error || save.error)}</p>}
  </>;
}

export type SideSubmission = { status: "queued"; jobId: string } | { status: "skipped" }
  | { status: "unavailable"; provider: Provider; model: string | null; reason: string };

const providers: Record<Provider, string> = { codex: "Codex", claude: "Claude Code", antigravity: "Antigravity" };

export function sideUnavailableReason(reason: string) {
  switch (reason) {
    case "tool_free_fresh_route_unavailable": return "This profile has no qualified tool-free route for a side question.";
    case "source_boundary_unavailable": return "An eligible completed source turn is required.";
    case "invalid_input": return "The question or saved context exceeds the side request limit.";
    case "profile_mismatch": return "The selected profile differs from the saved source.";
    case "account_changed": return "The authenticated CLI account changed or could not be confirmed.";
    case "unverified_profile": return "This profile has no verified tool-disabled route.";
    case "output_fixture_unavailable": return "The selected CLI, runner, or installed fixture tool is unavailable.";
    case "busy": return "The CLI is still using the source session. Try again after the main turn finishes.";
    default: return "The selected CLI has no available isolated route.";
  }
}

export function SideQuestionPanel({ conversationId, submission, validation, onClose }: {
  conversationId: string;
  submission?: SideSubmission;
  validation?: { pending: boolean; error?: Error | null; onStart: () => void };
  onClose: () => void;
}) {
  const client = useQueryClient();
  const queryKey = ["conversation", conversationId, "side-requests"];
  const saved = useSideRequests(conversationId, true);
  const [copyError, setCopyError] = useState("");
  const control = useMutation({
    mutationFn: (job: Job) => {
      const identity = { projectId: job.projectId, jobId: job.id };
      if (job.latestAttempt && isLiveRun(job.latestAttempt)) {
        return invoke("interrupt_queued_job_attempt", { ...identity, runId: job.latestAttempt.runId });
      }
      return invoke("set_pending_job_state", { ...identity, state: "cancelled" });
    },
    onSettled: () => client.invalidateQueries({ queryKey }),
  });
  const discard = useMutation({
    mutationFn: (jobId: string) => invoke<void>("delete_conversation_side_question", { conversationId, jobId }),
    onSuccess: async (_, jobId) => {
      await client.cancelQueries({ queryKey, exact: true });
      client.setQueryData<SideRequest[]>(queryKey, (current) => current?.filter(({ job }) => job.id !== jobId));
      await client.invalidateQueries({ queryKey, exact: true });
    },
  });
  const requests = saved.data?.filter(({ purpose }) => purpose !== "suggestion") ?? [];
  return <aside className="conv-activity" aria-label="Side questions">
    <header className="conv-activity-heading"><h2>Side questions</h2>
      <button type="button" className="conv-icon-button ml-auto" aria-label="Close side questions" onClick={onClose}><X size={16} /></button>
    </header>
    {submission?.status === "unavailable" && <p role="status" className="conv-alert">
      {[providers[submission.provider], submission.model].filter(Boolean).join(" · ")} · {sideUnavailableReason(submission.reason)}
    </p>}
    {saved.isError && <p role="alert" className="conv-alert">{String(saved.error)}</p>}
    {control.isError && <p role="alert" className="conv-alert">{String(control.error)}</p>}
    {discard.isError && <p role="alert" className="conv-alert">{String(discard.error)}</p>}
    {validation?.error && <p role="alert" className="conv-alert">{String(validation.error)}</p>}
    {copyError && <p role="alert" className="conv-alert">{copyError}</p>}
    {validation && <div className="px-3 pt-3">
      <Button type="button" variant="outline" disabled={validation.pending} onClick={validation.onStart}>
        {validation.pending ? "Queueing validation…" : "Validate side questions"}
      </Button>
      <p className="mt-1 text-sm text-muted-foreground">Runs a disposable fixture through the selected model and can consume account usage.</p>
    </div>}
    {!requests.length && saved.isSuccess && submission?.status !== "unavailable" && <p className="p-3 text-sm text-muted-foreground">Start a side question with /btw followed by your question.</p>}
    <ul className="grid gap-3 p-3">
      {requests.map(({ job, question, response, purpose, provider, model, contextId, sourceRunId, evidenceId, historyPayloadDeleted }) => {
        const outcome = response?.outcome;
        const fixture = purpose === "validation";
        const active = isLiveRun(job.latestAttempt);
        const pending = job.queueState !== "cancelled" && (!job.latestAttempt || job.queuedRecovery || active);
        return <li key={job.id} className="grid gap-2 rounded border border-border p-3 text-sm">
          <p className="whitespace-pre-wrap break-words font-medium">
            {fixture ? ["Side question validation", providers[provider], model].filter(Boolean).join(" · ") : question}
          </p>
          {!fixture && <dl className="grid gap-1 break-all text-xs text-muted-foreground">
            <div><dt className="inline">Context revision</dt>{" "}<dd className="inline"><code>{contextId}</code></dd></div>
            <div><dt className="inline">Source run</dt>{" "}<dd className="inline"><code>{sourceRunId}</code></dd></div>
          </dl>}
          {outcome?.kind === "answered" && !fixture ? <>
            <ConversationMarkdown>{outcome.text}</ConversationMarkdown>
            <Button type="button" variant="outline" className="justify-self-start" onClick={() => {
              setCopyError("");
              void Promise.resolve().then(() => navigator.clipboard.writeText(outcome.text)).catch(() => setCopyError("Could not copy the side answer."));
            }}>Copy answer</Button>
          </> : <p className="text-muted-foreground" role="status">
            {historyPayloadDeleted ? "Saved answer and source context deleted"
              : pending ? (job.latestAttempt?.cancelRequested ? "Stopping…" : job.latestAttempt?.status === "starting" ? "Starting…" : active ? "Running…" : job.queueState === "paused" ? "Paused in the queue" : "Waiting for the queue…")
              : outcome?.kind === "unavailable" ? sideUnavailableReason(outcome.reason)
                : job.queueState === "cancelled" || job.latestAttempt?.status === "cancelled" || outcome?.kind === "cancelled" ? "Cancelled"
                  : job.latestAttempt?.status === "interrupted" || outcome?.kind === "interrupted" ? "Interrupted"
                    : outcome?.kind === "timed_out" ? "Timed out"
                      : fixture ? (job.latestAttempt?.status === "succeeded" && evidenceId ? "Validation passed" : "Validation failed")
                      : outcome?.kind === "no_text" ? "No answer returned" : "The side request failed"}
          </p>}
          {(pending || !fixture) && <div className="flex flex-wrap gap-2">
            {pending && <Button type="button" variant="outline" disabled={control.isPending || job.latestAttempt?.cancelRequested}
              onClick={() => control.mutate(job)}>{fixture ? "Cancel validation" : "Cancel side question"}</Button>}
            {!fixture && <Button type="button" variant="outline" disabled={discard.isPending}
              onClick={() => discard.mutate(job.id)}>Delete side question</Button>}
          </div>}
        </li>;
      })}
    </ul>
  </aside>;
}
