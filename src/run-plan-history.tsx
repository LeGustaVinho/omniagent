import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { useCheckoutConversationActions } from "@/checkout-conversation-actions";
import { Button } from "@/components/ui/button";
import { useContextActionFeedback } from "@/components/ui/context-actions";
import { RecordedTextActions } from "@/recorded-text-actions";
import { SavedAttemptComparison, type AttemptChoice } from "@/saved-attempt-comparison";
import type { WorkspaceProps } from "@/workbench-contract";

type PlanVersion = {
  revisionId: string;
  sourceRunId: string;
  eventSequence: string;
  text: string;
  current: boolean;
  approvalArtifactId: string | null;
  releaseId: string | null;
  releaseRevoked: boolean | null;
};
type PlanHistory = {
  request: { baseRevisionId: string; feedback: string } | null;
  versions: PlanVersion[];
};

export function RunPlanHistory({ projectId, runId, openActivity, draftDestination }: {
  projectId: string;
  runId: string;
  openActivity: WorkspaceProps["openActivity"];
  draftDestination?: { checkoutId: string; open: (conversationId: string) => void };
}) {
  const [open, setOpen] = useState(false);
  const [compared, setCompared] = useState<string>();
  const { copy, feedback } = useContextActionFeedback();
  const draft = useCheckoutConversationActions(draftDestination?.checkoutId ?? "", !draftDestination, (id) => draftDestination?.open(id));
  const history = useQuery({
    queryKey: ["repository-delivery", projectId, "plan-history", runId],
    enabled: open,
    queryFn: () => invoke<PlanHistory | null>("repository_plan_history", { projectId, runId }),
  });
  const versions = history.data?.versions ?? [];
  const choices: AttemptChoice[] = versions.map(({ revisionId, sourceRunId, eventSequence, current, approvalArtifactId, releaseId, releaseRevoked }) => ({
    id: revisionId, runId: sourceRunId, label: `Proposal ${eventSequence}${current ? " · Selected plan" : ""}`,
    provenance: [{ label: "Plan revision", value: revisionId }, { label: "Proposal event", value: eventSequence },
      ...(approvalArtifactId ? [{ label: "Approval decision", value: approvalArtifactId }] : []),
      ...(releaseId ? [{ label: releaseRevoked ? "Revoked release" : "Approved release", value: releaseId }] : []),
    ],
  }));
  return <details onToggle={(event) => setOpen(event.currentTarget.open)} className="min-w-0">
    <summary className="cursor-pointer">Plan revision history</summary>
    {open && <div className="grid gap-2 pt-2 text-sm">
      {history.isPending && <p role="status">Loading plan history…</p>}
      {history.error && <p role="alert">{String(history.error)}</p>}
      {history.data === null && <p>No saved workflow plan revisions for this attempt.</p>}
      {history.data?.request && <section aria-label="Plan revision request" className="grid gap-1">
        <h3 className="font-medium">Revision request</h3>
        <p className="break-all">Base version <code>{history.data.request.baseRevisionId}</code></p>
        <p className="whitespace-pre-wrap break-words">{history.data.request.feedback}</p>
      </section>}
      {versions.map(({ revisionId, sourceRunId, eventSequence, text, current, approvalArtifactId, releaseId, releaseRevoked }) => {
        const label = `Proposal ${eventSequence}${current ? " · Selected plan" : ""}`;
        const openAttempt = () => openActivity(sourceRunId);
        return <RecordedTextActions key={revisionId} label={label} text={text} className="run-evidence-menu rounded border border-border p-2"
          bodyClassName="max-h-80 overflow-auto whitespace-pre-wrap break-words" excerptAction="Review selected plan excerpt…"
          onExcerpt={draftDestination ? (excerpt) => {
            const lines = excerpt.lastLine === excerpt.firstLine ? `Line ${excerpt.firstLine}` : `Lines ${excerpt.firstLine}–${excerpt.lastLine}`;
            draft.startContextText({ title: "Saved plan feedback", reference: `Plan revision ${revisionId} · Run ${sourceRunId} · Proposal ${eventSequence} · ${lines}`,
              text: excerpt.text, language: "markdown", confirmSource: async () => {
                const fresh = await invoke<PlanHistory | null>("repository_plan_history", { projectId, runId });
                if (!fresh?.versions.some((entry) => entry.revisionId === revisionId && entry.sourceRunId === sourceRunId
                  && entry.eventSequence === eventSequence && entry.text === text)) {
                  throw new Error("The saved plan excerpt is unavailable. Reopen the plan history.");
                }
              },
            }, "Review this excerpt from the saved plan. Draft feedback that can be used to revise it.");
          } : undefined} groups={[[
          ...(sourceRunId !== runId ? [{ label: "Open plan attempt", run: openAttempt }] : []),
          ...(versions.some((version) => version.sourceRunId !== sourceRunId) ? [{ label: "Compare saved plans…", run: () => setCompared(revisionId) }] : []),
        ], [
          { label: "Copy plan text", run: () => copy(text) },
          { label: "Copy plan revision ID", run: () => copy(revisionId) },
          { label: "Copy source run ID", run: () => copy(sourceRunId) },
          ...(approvalArtifactId ? [{ label: "Copy approval decision ID", run: () => copy(approvalArtifactId) }] : []),
          ...(releaseId ? [{ label: "Copy release ID", run: () => copy(releaseId) }] : []),
        ]]}>{(body) => <details className="min-w-0 flex-1">
            <summary className="cursor-pointer">{label}</summary>
            <p className="break-all">Plan version <code>{revisionId}</code></p>
            {approvalArtifactId && <p className="break-all">Approval decision <code>{approvalArtifactId}</code></p>}
            {releaseId && <p className="break-all">
              {releaseRevoked ? "Revoked release" : "Approved release"} <code>{releaseId}</code>
            </p>}
            {body}
            {sourceRunId !== runId && <Button type="button" variant="outline" onClick={openAttempt}>Open plan attempt</Button>}
          </details>}
        </RecordedTextActions>;
      })}
      {compared && <SavedAttemptComparison title="Compare saved plans" description="Plan revisions recorded in this attempt’s workflow history."
        scope={[projectId, runId, "plan"]} choices={choices} sourceId={compared} language="markdown" close={() => setCompared(undefined)}
        openAttempt={(id) => openActivity(id)} load={async ({ id }) => {
          const version = versions.find(({ revisionId }) => revisionId === id);
          return version && { text: version.text };
        }} />}
      {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
      {draft.dialog}
    </div>}
  </details>;
}
