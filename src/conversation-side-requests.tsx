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

type SideOutcome = { kind: "answered"; text: string } | { kind: "unavailable"; reason: string }
  | { kind: "no_text" | "failed" | "timed_out" | "cancelled" | "interrupted" };

interface SideRequest {
  job: Job;
  contextId: string;
  sourceRunId: string;
  provider: Provider;
  profileId: string;
  model: string | null;
  effort: string | null;
  mode: SessionMode;
  purpose: "question" | "suggestion";
  question: string;
  response: { outcome: SideOutcome } | null;
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
    <SettingRow id="composer-suggestions" label="Composer suggestions" description="Eligible requests can consume provider account usage.">
      <select id="composer-suggestions" value={query.data ?? "native"} disabled={!query.isSuccess || save.isPending}
        onChange={(event) => save.mutate(event.target.value as SuggestionMode)}>
        <option value="off">Off</option><option value="native">Native when available</option>
        <option value="omniagent">Use OmniAgent when native is unavailable</option>
      </select>
    </SettingRow>
    {(query.error || save.error) && <p role="alert">{String(query.error || save.error)}</p>}
  </>;
}

export type SideSubmission = { status: "queued"; jobId: string } | { status: "skipped" }
  | { status: "unavailable"; provider: Provider; model: string | null; reason: string };

const providers: Record<Provider, string> = { codex: "Codex", claude: "Claude Code", antigravity: "Antigravity" };

function unavailableReason(reason: string) {
  switch (reason) {
    case "source_boundary_unavailable": return "An eligible completed source turn is required.";
    case "invalid_input": return "The question or saved context exceeds the side request limit.";
    case "profile_mismatch": return "The selected profile differs from the saved source.";
    case "unverified_profile": return "This profile has no verified tool-disabled route.";
    case "busy": return "The CLI is still using the source session. Try again after the main turn finishes.";
    default: return "The selected CLI has no available isolated route.";
  }
}

export function SideQuestionPanel({ conversationId, submission, onClose }: {
  conversationId: string;
  submission?: SideSubmission;
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
  const questions = saved.data?.filter(({ purpose }) => purpose === "question") ?? [];
  return <aside className="conv-activity" aria-label="Side questions">
    <header className="conv-activity-heading"><h2>Side questions</h2>
      <button type="button" className="conv-icon-button ml-auto" aria-label="Close side questions" onClick={onClose}><X size={16} /></button>
    </header>
    {submission?.status === "unavailable" && <p role="status" className="conv-alert">
      {[providers[submission.provider], submission.model].filter(Boolean).join(" · ")} · {unavailableReason(submission.reason)}
    </p>}
    {saved.isError && <p role="alert" className="conv-alert">{String(saved.error)}</p>}
    {control.isError && <p role="alert" className="conv-alert">{String(control.error)}</p>}
    {copyError && <p role="alert" className="conv-alert">{copyError}</p>}
    {!questions.length && saved.isSuccess && submission?.status !== "unavailable" && <p className="p-3 text-sm text-muted-foreground">Start a side question with /btw followed by your question.</p>}
    <ul className="grid gap-3 p-3">
      {questions.map(({ job, question, response }) => {
        const outcome = response?.outcome;
        const pending = job.queueState !== "cancelled" && (!job.latestAttempt || job.queuedRecovery || isLiveRun(job.latestAttempt));
        return <li key={job.id} className="grid gap-2 rounded border border-border p-3 text-sm">
          <p className="whitespace-pre-wrap break-words font-medium">{question}</p>
          {outcome?.kind === "answered" ? <>
            <ConversationMarkdown>{outcome.text}</ConversationMarkdown>
            <Button type="button" variant="outline" className="justify-self-start" onClick={() => {
              setCopyError("");
              void Promise.resolve().then(() => navigator.clipboard.writeText(outcome.text)).catch(() => setCopyError("Could not copy the side answer."));
            }}>Copy answer</Button>
          </> : <p className="text-muted-foreground" role="status">
            {pending ? (job.latestAttempt?.cancelRequested ? "Stopping…" : isLiveRun(job.latestAttempt) ? "Running…" : "Waiting for the queue…")
              : outcome?.kind === "unavailable" ? unavailableReason(outcome.reason)
                : job.queueState === "cancelled" || job.latestAttempt?.status === "cancelled" || outcome?.kind === "cancelled" ? "Cancelled"
                  : job.latestAttempt?.status === "interrupted" || outcome?.kind === "interrupted" ? "Interrupted"
                    : outcome?.kind === "timed_out" ? "Timed out"
                      : outcome?.kind === "no_text" ? "No answer returned" : "The side request failed"}
          </p>}
          {pending && <Button type="button" variant="outline" className="justify-self-start" disabled={control.isPending || job.latestAttempt?.cancelRequested}
            onClick={() => control.mutate(job)}>Cancel side question</Button>}
        </li>;
      })}
    </ul>
  </aside>;
}
