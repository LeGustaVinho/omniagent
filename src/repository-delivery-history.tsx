import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import type { DeliverySnapshot, DeliverySource } from "@/repository-delivery-model";
import type { WorkspaceProps } from "@/workbench-contract";

type Commit = {
  sha: string;
  branch: string;
  verifiedAt: string;
  captureRunId: string;
  acceptanceId: string;
  acceptanceArtifactId?: string;
  pendingChangesRetained: boolean;
};

type VerifiedPatch = { patchBase64: string };

export function SavedDeliverySource({ document }: { document: DeliverySource }) {
  const [open, setOpen] = useState(false);
  const query = useQuery({
    queryKey: ["repository-markdown", document.checkoutId, document.id],
    queryFn: () => invoke<{ markdown: string } | null>("read_repository_markdown", {
      checkoutId: document.checkoutId, revisionId: document.id,
    }),
    enabled: open,
    staleTime: Infinity,
  });
  return <details className="pipeline-evidence" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Saved Markdown · {document.relativePath}</summary>
    {open && <>
      <p className="break-all">Revision <code>{document.id}</code> · SHA-256 <code>{document.sha256}</code></p>
      {query.isPending && <p role="status">Loading saved Markdown…</p>}
      {query.error && <p role="alert">{String(query.error)}</p>}
      {query.data === null && <p>Saved Markdown unavailable.</p>}
      {query.data && <pre className="max-h-72 overflow-auto whitespace-pre-wrap">{query.data.markdown}</pre>}
    </>}
  </details>;
}

function AcceptedPatch({ projectId, taskId, evidenceId }: { projectId: string; taskId: string; evidenceId: string }) {
  const [open, setOpen] = useState(false);
  const query = useQuery({
    queryKey: ["delivery-verified-patch", projectId, taskId, evidenceId],
    queryFn: () => invoke<VerifiedPatch | null>("verified_delivery_patch", { projectId, taskId, evidenceId }),
    enabled: open,
    staleTime: Infinity,
  });
  const patch = query.data && new TextDecoder().decode(Uint8Array.from(atob(query.data.patchBase64), (byte) => byte.charCodeAt(0)));
  return <details className="pipeline-evidence" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Accepted diff</summary>
    {open && query.isPending && <p role="status">Loading accepted diff…</p>}
    {query.error && <p role="alert">{String(query.error)}</p>}
    {query.data === null && <p>Accepted diff unavailable.</p>}
    {open && patch && <pre className="max-h-72 overflow-auto rounded bg-muted p-2 text-xs">{patch}</pre>}
  </details>;
}

export function VerifiedDeliveryHistory({ projectId, taskId, evidenceId, unitId, delivery, openActivity }: {
  projectId: string;
  taskId: string;
  evidenceId: string;
  unitId: string;
  delivery: DeliverySnapshot;
  openActivity: WorkspaceProps["openActivity"];
}) {
  const receipt = useQuery({
    queryKey: ["delivery-verified-commit", projectId, taskId, evidenceId],
    queryFn: () => invoke<Commit | null>("verified_delivery_commit", { projectId, taskId, evidenceId }),
    staleTime: Infinity,
  });
  if (receipt.error) return <p role="alert">{String(receipt.error)}</p>;
  if (receipt.data === null) return <p>Verified commit unavailable.</p>;
  if (!receipt.data) return <p role="status">Loading verified commit…</p>;

  const commit = receipt.data;
  const linkedReviews = new Set(delivery.artifacts.flatMap((artifact) => {
    const { value } = artifact;
    return artifact.unitId === unitId && value.kind === "acceptance" && value.evidence.evidence.id === commit.acceptanceId &&
      (!commit.acceptanceArtifactId || artifact.id === commit.acceptanceArtifactId) ?
      [value.evidence.reviewArtifactId] : [];
  }));
  const review = linkedReviews.size === 1 ? delivery.artifacts.find((artifact) => artifact.unitId === unitId &&
    artifact.value.kind === "review" && linkedReviews.has(artifact.id)) : undefined;
  const reviewRunId = review?.value.kind === "review" ? review.value.evidence.run_id : undefined;
  const attemptConversation = (runId: string) => delivery.jobs.find(({ job }) => job.latestAttempt?.runId === runId)
    ?.job.latestAttempt?.conversationId ?? undefined;

  return <div className="pipeline-stack">
    <p>Verified commit <code className="break-all">{commit.sha}</code></p>
    <p className="text-muted-foreground">{commit.branch} · {new Date(commit.verifiedAt).toLocaleString()}</p>
    {commit.pendingChangesRetained && <p>Additional changes stayed assigned to this work scope.</p>}
    <AcceptedPatch projectId={projectId} taskId={taskId} evidenceId={evidenceId} />
    <div className="pipeline-actions">
      <Button type="button" variant="outline" onClick={() => openActivity(commit.captureRunId, attemptConversation(commit.captureRunId))}>
        Open implementation attempt
      </Button>
      {reviewRunId ? <Button type="button" variant="outline" onClick={() => openActivity(reviewRunId, attemptConversation(reviewRunId))}>
        Open accepted candidate review
      </Button> : <p>Accepted candidate review unavailable.</p>}
    </div>
  </div>;
}
