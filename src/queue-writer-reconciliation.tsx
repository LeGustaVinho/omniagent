import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { projectQueueQuery } from "@/queue-model";

type Receipt = { sourceRunId: string; reviewRevision: string; reviewedAt: string };
type WriterReview = {
  sourceRunId: string;
  checkoutId: string;
  revision: string;
  startingHead: string;
  observedHead: string;
  observedStatus: string[];
  indexRestored: boolean;
  worktreeRestored: boolean;
  dispatches: { id: string; ordinal: number; delivery: string }[];
  launchRequested: boolean;
  receipt: Receipt | null;
};

export function QueueWriterReconciliation({ projectId, jobId, runId }: { projectId: string; jobId: string; runId: string }) {
  const [open, setOpen] = useState(false);
  const [acknowledgedRevision, setAcknowledgedRevision] = useState<string>();
  const client = useQueryClient();
  const scope = { projectId, jobId, runId };
  const review = useQuery({
    queryKey: ["interrupted-writer-review", projectId, jobId, runId],
    queryFn: () => invoke<WriterReview | null>("interrupted_writer_review", scope),
    enabled: open,
  });
  const confirm = useMutation({
    mutationFn: (expectedRevision: string) => invoke<Receipt>("reconcile_interrupted_writer", {
      ...scope, expectedRevision, externalEffectsReconciled: true,
    }),
    onSuccess: async () => {
      await Promise.all([
        review.refetch(),
        client.invalidateQueries({ queryKey: projectQueueQuery(projectId).queryKey }),
        client.invalidateQueries({ queryKey: ["workflow-execution-job", projectId] }),
        client.invalidateQueries({ queryKey: ["repository-delivery", projectId] }),
      ]);
    },
  });
  const snapshot = review.data;
  const restored = snapshot && snapshot.startingHead === snapshot.observedHead && snapshot.indexRestored && snapshot.worktreeRestored;
  const checked = Boolean(snapshot && acknowledgedRevision === snapshot.revision);
  const error = review.error ?? confirm.error;
  return <details className="grid gap-2 rounded border border-border p-3" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary className="cursor-pointer text-sm font-semibold">Review interrupted write</summary>
    {open && <div className="mt-2 grid gap-2 text-sm">
      {review.isPending && <p role="status">Checking the saved write checkpoint…</p>}
      {error && <p role="alert">{String(error)}</p>}
      {review.data === null && <p>No interrupted write needs reconciliation for this attempt.</p>}
      {snapshot && <>
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 break-all">
          <dt>Starting commit</dt><dd><code>{snapshot.startingHead}</code></dd>
          <dt>Current commit</dt><dd><code>{snapshot.observedHead}</code></dd>
          <dt>Index</dt><dd>{snapshot.indexRestored ? "Matches checkpoint" : "Differs from checkpoint"}</dd>
          <dt>Worktree</dt><dd>{snapshot.worktreeRestored ? "Matches checkpoint" : "Differs from checkpoint"}</dd>
        </dl>
        {!!snapshot.observedStatus.length && <pre aria-label="Current worktree status" className="max-h-40 overflow-auto whitespace-pre-wrap rounded bg-muted p-2">{snapshot.observedStatus.join("\n")}</pre>}
        {!!snapshot.dispatches.length && <ul aria-label="Recorded prompt deliveries" className="grid gap-1">
          {snapshot.dispatches.map(({ id, ordinal, delivery }) => <li key={id}>Prompt {ordinal} · {delivery.replaceAll("_", " ")}</li>)}
        </ul>}
        {snapshot.receipt ? <p role="status">Write reconciliation recorded · {snapshot.receipt.reviewedAt}</p> : <>
          <Button type="button" variant="outline" disabled={confirm.isPending || review.isFetching} onClick={() => {
            setAcknowledgedRevision(undefined);
            void review.refetch();
          }}>Refresh worktree evidence</Button>
          {!restored && <p>Restore the commit, index and worktree to the saved checkpoint to continue.</p>}
          <label><input type="checkbox" checked={checked} disabled={!restored || confirm.isPending || review.isFetching}
            onChange={(event) => setAcknowledgedRevision(event.currentTarget.checked ? snapshot.revision : undefined)} />
            {" "}I reviewed the interrupted attempt and reconciled its effects outside this worktree.
          </label>
          <Button type="button" variant="outline" disabled={!checked || !restored || confirm.isPending || review.isFetching}
            onClick={() => confirm.mutate(snapshot.revision)}>Confirm reconciliation</Button>
        </>}
      </>}
    </div>}
  </details>;
}
