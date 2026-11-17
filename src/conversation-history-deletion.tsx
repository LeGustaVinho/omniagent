import { useId, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ConversationHistory } from "@/bindings/ConversationHistory";
import { Button } from "@/components/ui/button";

type Target =
  | { kind: "message"; conversationId: string; messageRevisionId: string }
  | { kind: "attempt_diff"; conversationId: string; runId: string };
type Mode = "selected" | "with_derived_copies";
type Preview = {
  revision: string;
  messageRevisionIds: string[];
  artifactIds: string[];
  dispatchIds: string[];
  attachmentCopyDispatchCount?: number;
  attachmentCopyRevisionCount?: number;
  referenceIds: string[];
  attemptDiffIds: string[];
  candidateIds: string[];
  nativeSessions: number;
  retainedRuns: number;
  affectedJobs: number;
  sideRequestIds: string[];
  unsettledRunIds: string[];
};
type Selection = { target: Target; mode: Mode };
type RecordedDiff = { runId: string; label: string; deleted?: boolean };

export function ConversationHistoryDeletion({ conversationId, history, attemptDiffs, disabled, onDeleted }: {
  conversationId: string;
  history?: Pick<ConversationHistory, "messages">;
  attemptDiffs?: RecordedDiff[];
  disabled?: boolean;
  onDeleted?: () => void;
}) {
  const client = useQueryClient();
  const id = useId();
  const [item, setItem] = useState(() => !history && attemptDiffs?.length === 1 ? `diff:${attemptDiffs[0]?.runId}` : "");
  const [mode, setMode] = useState<Mode>("selected");
  const messages = history?.messages ?? [];
  const recorded = useQuery({
    queryKey: ["conversation", conversationId, "history-deletion-diffs"],
    queryFn: () => invoke<RecordedDiff[]>("conversation_history_deletion_diffs", { conversationId }),
    enabled: !attemptDiffs,
  });
  const diffs = attemptDiffs ?? recorded.data ?? [];
  const message = messages.find((entry) => item === `message:${entry.revisionId}`);
  const diff = diffs.find((entry) => item === `diff:${entry.runId}`);
  const target: Target | undefined = message
    ? { kind: "message", conversationId, messageRevisionId: message.revisionId }
    : diff && { kind: "attempt_diff", conversationId, runId: diff.runId };

  const review = useMutation({
    mutationFn: (selection: Selection) => invoke<Preview>("preview_conversation_history_deletion", selection),
  });
  const confirm = useMutation({
    mutationFn: (selection: Selection & { expectedRevision: string }) =>
      invoke<Preview>("confirm_conversation_history_deletion", selection),
    onSuccess: async () => {
      review.reset();
      setItem("");
      await Promise.all([
        client.invalidateQueries({ queryKey: ["conversation"] }),
        client.invalidateQueries({ queryKey: ["conversation-run"] }),
        client.invalidateQueries({ queryKey: ["saved-attachment-preview"] }),
        client.invalidateQueries({ queryKey: ["conversation-reference-source"] }),
        client.invalidateQueries({ queryKey: ["conversation-library"] }),
        client.invalidateQueries({ queryKey: ["project-job-queue"] }),
        client.invalidateQueries({ queryKey: ["git-saved-attempts"] }),
        client.invalidateQueries({ queryKey: ["git-attempt-diff"] }),
        client.invalidateQueries({ queryKey: ["attempt-worktree-evidence"] }),
      ]);
      onDeleted?.();
    },
    onError: () => review.reset(),
  });
  const busy = disabled || review.isPending || confirm.isPending;

  function reset() {
    review.reset();
    confirm.reset();
  }

  return <section aria-labelledby={`${id}-title`} className="space-y-2 border-t border-border pt-3">
    <h3 id={`${id}-title`} className="text-sm font-medium">Delete history</h3>
    <div className="flex flex-wrap gap-2 text-sm">
      <label className="grid gap-1">
        History item
        <select value={item} disabled={busy} onChange={(event) => { setItem(event.currentTarget.value); reset(); }}>
          <option value="">Select a revision or diff</option>
          {messages.map((entry) => <option key={entry.revisionId} value={`message:${entry.revisionId}`}>
            {entry.role === "user" ? "Prompt" : "Answer"} · {entry.text ? entry.text.slice(0, 76) : "Deleted history item"}
          </option>)}
          {diffs.map((entry) => <option key={entry.runId} value={`diff:${entry.runId}`}>
            Diff · {entry.label}{entry.deleted ? " · Deleted history item" : ""}
          </option>)}
        </select>
      </label>
      <label className="grid gap-1">
        Scope
        <select value={mode} disabled={busy} onChange={(event) => { setMode(event.currentTarget.value as Mode); reset(); }}>
          <option value="selected">Delete selected item</option>
          <option value="with_derived_copies">Delete with derived copies</option>
        </select>
      </label>
    </div>
    <Button type="button" variant="outline"
      disabled={busy || !target || ((message?.text === "" || diff?.deleted) && mode === "selected")}
      onClick={() => { if (target) { reset(); review.mutate({ target, mode }); } }}>
      {review.isPending ? "Checking impact…" : "Review deletion impact"}
    </Button>
    {review.data && review.variables?.target.conversationId === conversationId && <div className="space-y-2">
      <DeletionImpact preview={review.data} />
      <Button type="button" variant="outline" className="text-destructive" disabled={busy || review.data.unsettledRunIds.length > 0}
        onClick={() => {
          if (review.data && review.variables) confirm.mutate({ ...review.variables, expectedRevision: review.data.revision });
        }}>
        {confirm.isPending ? "Deleting…" : "Confirm deletion"}
      </Button>
    </div>}
    {(review.error || confirm.error || recorded.error) && <p role="alert" className="conv-alert">
      {String(review.error || confirm.error || recorded.error)}
    </p>}
    {confirm.isSuccess && <p role="status" className="text-sm">History payload deleted.</p>}
  </section>;
}

function DeletionImpact({ preview }: { preview: Preview }) {
  const affected = [
    ["Message revisions", preview.messageRevisionIds],
    ["Excerpts and summaries", preview.artifactIds],
    ["Recorded dispatches", preview.dispatchIds],
    ["Conversation references", preview.referenceIds],
    ["Recorded diffs", preview.attemptDiffIds],
    ["Candidate diffs", preview.candidateIds],
    ["Side request payloads", preview.sideRequestIds],
  ] as const;
  return <div aria-label="Deletion impact" className="rounded border border-border p-3 text-sm">
    <p>The selected payloads will be permanently removed from live chat history. Audit IDs will stay available.</p>
    <dl className="my-2 space-y-1">
      {affected.filter(([, ids]) => ids.length > 0).map(([label, ids]) => <div key={label}>
        <dt className="font-medium">{label} · {ids.length}</dt>
        <dd className="break-all text-xs text-muted-foreground">{ids.join(", ")}</dd>
      </div>)}
      {(preview.attachmentCopyDispatchCount ?? 0) > 0 && <div>
        <dt>Dispatches containing reused attachment copies</dt><dd>{preview.attachmentCopyDispatchCount}</dd>
      </div>}
      {(preview.attachmentCopyRevisionCount ?? 0) > 0 && <div>
        <dt>Message revisions containing reused attachment copies</dt><dd>{preview.attachmentCopyRevisionCount}</dd>
      </div>}
      <div><dt>Sessions revoked</dt><dd>{preview.nativeSessions}</dd></div>
      <div><dt>Affected jobs</dt><dd>{preview.affectedJobs}</dd></div>
    </dl>
    {preview.unsettledRunIds.length > 0 && <p role="status">Settle {preview.unsettledRunIds.length} affected attempt(s) before confirming.</p>}
  </div>;
}
