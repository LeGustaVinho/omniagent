import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import type { DeliveryGitPolicy } from "@/delivery-git-policy";
import { deliveryMergeReceiptQuery, type AcceptedDestinationUpdate, type DeliveryMergePreview, type DestinationDriftOutcome, type MergeAttempt, type MergeOutcome } from "@/delivery-merge-model";
import { deliveryGitControlId, type DeliveryActions, type DeliveryTarget, type DeliveryUnit } from "@/repository-delivery-model";

export function RepositoryDeliveryMerge({ scope, unit, gitArtifactId, actions, automaticOutcome, mergeNodeId = "merge", onOutcome }: {
  scope: DeliveryTarget;
  unit: DeliveryUnit;
  gitArtifactId: string;
  actions: DeliveryActions;
  automaticOutcome: MergeOutcome | undefined;
  mergeNodeId?: string;
  onOutcome?: MergeAttempt;
}) {
  const { projectId, runId: workflowRunId } = scope;
  const { taskId, commitEvidenceId } = unit;
  const unitId = unit.unit.id;
  const [externalSha, setExternalSha] = useState("");
  const [outcome, setOutcome] = useState<MergeOutcome>();
  const [acceptedUpdate, setAcceptedUpdate] = useState<AcceptedDestinationUpdate>();
  const recorded = useQuery(deliveryMergeReceiptQuery(projectId, taskId, commitEvidenceId));
  const preview = useQuery({
    queryKey: ["delivery-merge", projectId, taskId, commitEvidenceId, "preview"],
    queryFn: () => invoke<DeliveryMergePreview>("delivery_merge_preview", { projectId, taskId, workflowRunId, unitId, commitEvidenceId }),
    enabled: Boolean(commitEvidenceId) && recorded.isSuccess && !recorded.data,
    retry: false,
  });
  const policy = useQuery({
    queryKey: ["delivery-git-policy", projectId],
    queryFn: () => invoke<DeliveryGitPolicy>("delivery_git_policy", { projectId }),
    enabled: Boolean(commitEvidenceId) && recorded.isSuccess && !recorded.data,
  });
  const sha = externalSha.trim();
  const validSha = /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/i.test(sha);
  const destination = preview.data;
  const status = outcome ?? automaticOutcome;
  const receipt = recorded.data ?? (status?.status === "merge_verified" ? status.evidence : undefined);
  const recovery = status?.status === "destination_drift" ? status.recovery : acceptedUpdate;
  const local = policy.data !== undefined && policy.data !== "user_commits_and_merges";
  const error = recorded.error ?? preview.error ?? policy.error;

  function execute(perform: boolean, external: string | null, driftAcceptanceId: string | null = null, prepareDrift = false) {
    if (!destination || actions.busy) return;
    actions.execute(async () => {
      const next = await invoke<MergeOutcome>("execute_repository_delivery_unit", { input: {
        ...scope, taskId, unitId, nodeId: mergeNodeId, inputs: { git: gitArtifactId },
        request: {
          action: "verify_merge", expectedSourceBranch: destination.sourceBranch,
          expectedSourceSha: destination.sourceSha, destinationBranch: destination.destinationBranch,
          expectedDestinationSha: destination.expectedDestinationSha, externalSha: external, perform, driftAcceptanceId, prepareDrift,
        },
      } });
      if (next.status === "destination_drift") setAcceptedUpdate(next.recovery.status === "accepted" ? next.recovery : undefined);
      setOutcome(next);
      onOutcome?.(next);
      return next;
    });
  }

  function acceptRecovery(previewId: string, verificationId: string) {
    actions.execute(async () => {
      const updated = await invoke<DestinationDriftOutcome>("accept_delivery_destination_drift", {
        projectId, taskId, workflowRunId, previewId, verificationId,
      });
      setAcceptedUpdate(updated.status === "accepted" ? updated : undefined);
      const next = { status: "destination_drift", recovery: updated } as const;
      setOutcome(next);
      onOutcome?.(next);
      return updated;
    });
  }

  return <section id={deliveryGitControlId(workflowRunId, mergeNodeId, unitId)} tabIndex={-1}
    className="pipeline-form pipeline-stack" aria-label="Delivery integration">
    <h3 className="font-medium">Delivery integration</h3>
    {receipt ? <>
      <p role="status">Verified merge</p>
      <dl className="pipeline-stack break-all">
        <div><dt>Source commit</dt><dd>{receipt.sourceBranch} · <code>{receipt.sourceSha}</code></dd></div>
        <div><dt>Destination commit</dt><dd>{receipt.destinationBranch} · <code>{receipt.integrationSha}</code></dd></div>
      </dl>
    </> : <>
      {(recorded.isPending || preview.isFetching) && !destination && <p role="status">Loading integration evidence…</p>}
      {destination && <>
        <dl className="pipeline-stack break-all">
          <div><dt>Approved source</dt><dd>{destination.sourceBranch} · <code>{destination.sourceSha}</code></dd></div>
          <div><dt>Expected destination</dt><dd>{destination.destinationBranch} · <code>{destination.expectedDestinationSha}</code></dd></div>
        </dl>
        {policy.data && recovery?.status !== "requires_acceptance" && recovery?.status !== "accepted" && recovery?.status !== "paused" &&
          <Button type="button" variant="outline" disabled={actions.busy} onClick={() => execute(local, null)}>{
            recovery?.status === "preparing" || recovery?.status === "checks_changed" ? "Check updated candidate"
              : local ? "Merge approved delivery" : "Review destination"
          }</Button>}
        <form className="pipeline-stack" onSubmit={(event) => {
          event.preventDefault();
          if (validSha) execute(false, sha, recovery?.status === "accepted" ? recovery.acceptanceId : null);
        }}>
          <label className="pipeline-stack">External integration SHA
            <input value={externalSha} onChange={(event) => setExternalSha(event.currentTarget.value)} />
          </label>
          <Button type="submit" variant="outline" disabled={!validSha || actions.busy}>Verify external merge</Button>
        </form>
      </>}
      {status?.status === "waiting_for_merge" && <p role="status">Waiting for merge</p>}
      {status?.status === "destination_moved" && <p role="alert" className="break-all">The destination branch advanced to <code>{status.actualSha}</code>.</p>}
      {status?.status === "conflicted" && <p role="alert">The approved source conflicts with the destination branch.</p>}
      {status?.status === "destination_checked_out" && <p role="alert">The destination is checked out in {status.checkout}. Integrate there and verify the resulting SHA.</p>}
      {status?.status === "merge_blocked" && <p role="alert">{status.reason}</p>}
      {status?.status === "recovery_required" && <div role="alert" className="break-all">
        <p>{status.message}</p>
        <p>Destination checkout <code>{status.checkout}</code></p>
        <p>Preserved recovery files <code>{status.recoveryDirectory}</code></p>
      </div>}
      {recovery && <DestinationUpdate recovery={recovery} busy={actions.busy} onAccept={acceptRecovery}
        scope={{ projectId, taskId, workflowRunId }}
        onPrepare={() => execute(false, null, null, true)}
        onMerge={local ? (id) => execute(true, null, id) : undefined} />}
    </>}
    {error && <p role="alert">{String(error)}</p>}
  </section>;
}

type DriftScope = { projectId: string; taskId: string; workflowRunId: string };

function DestinationUpdate({ recovery, busy, scope, onAccept, onPrepare, onMerge }: {
  recovery: DestinationDriftOutcome;
  busy: boolean;
  scope: DriftScope;
  onAccept: (previewId: string, verificationId: string) => void;
  onPrepare: () => void;
  onMerge?: (acceptanceId: string) => void;
}) {
  if (recovery.status === "paused" || recovery.status === "destination_moved") {
    return <>
      <p role="alert" className="break-all">Integration paused. Reviewed destination <code>{recovery.expectedDestinationSha}</code>.
        Current destination <code>{recovery.actualSha ?? "Unavailable"}</code>.</p>
      {recovery.status === "paused" && <Button type="button" variant="outline" disabled={busy}
        onClick={onPrepare}>Prepare updated candidate</Button>}
    </>;
  }
  if (recovery.status === "preparing") return <p role="status">Checking the updated candidate…</p>;
  if (recovery.status === "checks_changed") return <p role="alert">The required checks changed. Check the updated candidate again.</p>;
  if (recovery.status === "conflicted") return <div role="alert">
    <p>Conflicts at <code className="break-all">{recovery.destinationSha}</code></p>
    <ul>{recovery.paths.map((path) => <li className="break-all" key={path}>{path}</li>)}</ul>
  </div>;
  const { preview, verification } = recovery;
  return <div className="pipeline-stack" aria-label="Updated destination candidate">
    <p className="break-all">Destination <code>{preview.destinationSha}</code> · Candidate tree <code>{preview.tree}</code></p>
    {!!preview.changedPaths.length && <ul aria-label="Files in the updated candidate">{preview.changedPaths.map((path) => <li key={path}>{path}</li>)}</ul>}
    <DestinationPatch scope={scope} previewId={preview.id} />
    {verification.gates.length ? <ul aria-label="Updated candidate checks">
      {verification.gates.map(({ gate, outcome }) => <li key={gate.name}>
        {gate.name} · {gate.required ? "Required" : "Optional"} · {outcome?.status.replaceAll("_", " ") ?? "Pending"}
        {outcome?.reason && <p className="text-muted-foreground">{outcome.reason}</p>}
      </li>)}
    </ul> : <p>No checks are configured.</p>}
    {recovery.status === "checks_failed" && <p role="alert">The updated candidate did not pass its required checks.</p>}
    {recovery.status === "requires_acceptance" && <Button type="button" disabled={busy}
      onClick={() => onAccept(preview.id, verification.id)}>Accept updated candidate</Button>}
    {recovery.status === "accepted" && onMerge && <Button type="button" disabled={busy}
      onClick={() => onMerge(recovery.acceptanceId)}>Merge updated candidate</Button>}
  </div>;
}

function DestinationPatch({ scope, previewId }: { scope: DriftScope; previewId: string }) {
  const patch = useQuery({
    queryKey: ["delivery-merge", scope.projectId, scope.taskId, scope.workflowRunId, "drift-patch", previewId],
    queryFn: () => invoke<{ patchBase64: string; truncated: boolean }>("delivery_destination_drift_patch", { ...scope, previewId }),
    staleTime: Infinity,
    retry: false,
  });
  const text = patch.data && new TextDecoder().decode(Uint8Array.from(atob(patch.data.patchBase64), (byte) => byte.charCodeAt(0)));
  return <details className="pipeline-evidence">
    <summary>Updated candidate diff</summary>
    {patch.isPending && <p role="status">Reading the pinned diff…</p>}
    {patch.error && <p role="alert">{String(patch.error)}</p>}
    {patch.data?.truncated && <p>The preview reached its size limit.</p>}
    {patch.data && <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words">{text || "No file differences."}</pre>}
  </details>;
}
