import { useState } from "react";
import { useQueries, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { DeliveryAgent } from "@/repository-delivery-agent";
import {
  matchingJob, matchingReview, type DeliveryActions, type DeliveryInputs,
  type DeliverySnapshot, type DeliveryTarget, type DeliveryUnit,
} from "@/repository-delivery-model";
import { WorktreeEvidence } from "@/worktree-evidence";
import type { WorkspaceProps } from "@/workbench-contract";

type Capture = { kind: "saved"; artifact: { revision: string; candidate: { changes: { untracked: { path: string }[] } | null } } } |
  { kind: "unavailable"; reason: string };
type Verification = {
  id: string;
  candidateId: string;
  startedAt: string;
  finishedAt: string | null;
  gates: { gate: { name: string; required: boolean }; outcome: { status: string; reason?: string } | null }[];
};
type Acceptance = { candidate: { id: string }; verification: { id: string } };
type Commit = { sha: string; branch: string; verifiedAt: string; captureRunId: string; pendingChangesRetained: boolean };
type UnitOutcome = { status: "waiting_for_commit" } | { status: "artifact"; artifact: { id: string } };

function SavedCommit({ projectId, taskId, evidenceId }: { projectId: string; taskId: string; evidenceId: string }) {
  const receipt = useQuery({
    queryKey: ["delivery-verified-commit", projectId, taskId, evidenceId],
    queryFn: () => invoke<Commit | null>("verified_delivery_commit", { projectId, taskId, evidenceId }),
  });
  if (receipt.error) return <p role="alert">{String(receipt.error)}</p>;
  if (!receipt.data) return <p>Loading verified commit…</p>;
  const commit = receipt.data;
  return <div className="pipeline-stack">
    <p>Verified commit <code className="break-all">{commit.sha}</code></p>
    <p className="text-muted-foreground">{commit.branch} · {new Date(commit.verifiedAt).toLocaleString()}</p>
    {commit.pendingChangesRetained && <p>Additional changes stayed assigned to this work scope.</p>}
  </div>;
}

export function RepositoryDeliveryUnit({ scope, unit, delivery, implementationInputs, actions, openActivity, active }: {
  scope: DeliveryTarget;
  unit: DeliveryUnit;
  delivery: DeliverySnapshot;
  implementationInputs: DeliveryInputs;
  actions: DeliveryActions;
  openActivity: WorkspaceProps["openActivity"];
  active: boolean;
}) {
  const { taskId, unit: planned, commitEvidenceId } = unit;
  const { id: unitId } = planned;
  const [untracked, setUntracked] = useState<string[]>([]);
  const [tracked, setTracked] = useState("");
  const [criteria, setCriteria] = useState<number[]>([]);
  const [sha, setSha] = useState("");
  const [manualWait, setManualWait] = useState(false);
  const artifacts = delivery.artifacts.filter((artifact) => artifact.unitId === unitId);
  const implementation = matchingJob(delivery, "implement", implementationInputs, unitId);
  const attemptId = implementation?.latestAttempt?.runId;
  const capture = useQuery({
    queryKey: ["delivery-capture", scope.projectId, attemptId],
    queryFn: () => invoke<Capture | null>("attempt_worktree_evidence", { projectId: scope.projectId, runId: attemptId }),
    enabled: active && implementation?.latestAttempt?.status === "succeeded",
  });
  const selected = useQuery({
    queryKey: ["delivery-selection", scope.projectId, taskId, unitId],
    queryFn: () => invoke<{ id: string } | null>("current_selected_candidate", { projectId: scope.projectId, taskId, unitId }),
    enabled: active && Boolean(attemptId),
  });
  const candidate = artifacts.find((artifact) => artifact.value.kind === "candidate" &&
    artifact.value.evidence.id === selected.data?.id && artifact.value.evidence.target.runId === attemptId);
  const reviewInputs: DeliveryInputs = candidate ? { candidate: candidate.id } : {};
  const reviewing = candidate ? matchingJob(delivery, "review-candidate", reviewInputs, unitId) : undefined;
  const review = matchingReview(delivery, reviewing);
  const checks = artifacts.filter((artifact) => artifact.value.kind === "checks");
  const checkResults = useQueries({ queries: checks.flatMap(({ value }) => {
    if (value.kind !== "checks") return [];
    const verificationId = value.evidence.id;
    return [{
      queryKey: ["delivery-verification", scope.projectId, taskId, verificationId],
      queryFn: () => invoke<Verification | null>("inspect_candidate_verification", { projectId: scope.projectId, taskId, verificationId }),
      enabled: active && Boolean(candidate),
    }];
  }) });
  const newestCheck = checkResults.flatMap(({ data }, index) => {
    const artifact = checks[index];
    return artifact && data && candidate?.value.kind === "candidate" && data.candidateId === candidate.value.evidence.id ?
      [{ artifact, data }] : [];
  }).sort((a, b) => b.data.startedAt.localeCompare(a.data.startedAt))[0];
  const acceptances = artifacts.filter((artifact) => artifact.value.kind === "acceptance" && artifact.value.evidence.reviewArtifactId === review?.id);
  const acceptanceResults = useQueries({ queries: acceptances.flatMap(({ value }) => {
    if (value.kind !== "acceptance") return [];
    const acceptanceId = value.evidence.evidence.id;
    return [{
      queryKey: ["delivery-acceptance", scope.projectId, taskId, acceptanceId],
      queryFn: () => invoke<Acceptance | null>("inspect_candidate_acceptance", { projectId: scope.projectId, taskId, acceptanceId }),
      enabled: active && Boolean(newestCheck),
    }];
  }) });
  const accepted = acceptances.find((_, index) => acceptanceResults[index]?.data?.verification.id === newestCheck?.data.id &&
    Boolean(newestCheck?.data.id));
  const policy = useQuery({
    queryKey: ["delivery-git-policy", scope.projectId],
    queryFn: () => invoke<"automatic_commit_user_merge" | "user_commits_and_merges">("delivery_git_policy", { projectId: scope.projectId }),
    enabled: Boolean(accepted),
  });
  const captureArtifact = capture.data?.kind === "saved" ? capture.data.artifact : undefined;
  const readyToAccept = Boolean(newestCheck?.data.finishedAt &&
    newestCheck.data.gates.every(({ gate, outcome }) => !gate.required || outcome?.status === "passed"));
  const gateError = checkResults.find((result) => result.error)?.error;
  const error = capture.error ?? selected.error ?? policy.error ?? gateError;
  const requestUnit = (nodeId: string, inputs: DeliveryInputs, request: object) => invoke<UnitOutcome>("execute_repository_delivery_unit", {
    input: { ...scope, taskId, unitId, nodeId, inputs, request },
  });
  const validSha = /^([0-9a-f]{40}|[0-9a-f]{64})$/i.test(sha.trim());
  const manual = manualWait || policy.data === "user_commits_and_merges";

  return <section className="pipeline-form pipeline-stack" aria-label={planned.purpose}>
    <h3 className="font-medium">{planned.ordinal}. {planned.purpose}</h3>
    {commitEvidenceId && <SavedCommit projectId={scope.projectId} taskId={taskId} evidenceId={commitEvidenceId} />}
    {!commitEvidenceId && !active && <p className="text-muted-foreground">Waiting for the preceding verified commit.</p>}
    {active && <>
      <DeliveryAgent scope={scope} nodeId="implement" inputs={implementationInputs} unitId={unitId}
        job={implementation} label="Implementation" actions={actions} openActivity={openActivity} />
      {implementation?.latestAttempt?.status === "succeeded" && attemptId && <WorktreeEvidence projectId={scope.projectId} runId={attemptId} ownerTaskId={implementation.worktreeOwnerTaskId} />}
      {capture.data?.kind === "unavailable" && <p role="alert">{capture.data.reason}</p>}
      {captureArtifact && attemptId && <details className="pipeline-evidence" open={!candidate}>
        <summary>Candidate selection</summary>
        <label className="pipeline-stack">Tracked paths to include, one per line (blank includes all)
          <textarea value={tracked} onChange={(event) => setTracked(event.currentTarget.value)} />
        </label>
        {captureArtifact.candidate.changes?.untracked.map(({ path }) => <label key={path} className="flex items-center gap-2">
          <input type="checkbox" checked={untracked.includes(path)} onChange={(event) => {
            const checked = event.currentTarget.checked;
            setUntracked((previous) => checked ? [...previous, path] : previous.filter((name) => name !== path));
          }} /> {path}
        </label>)}
        <Button variant="outline" disabled={actions.busy} onClick={() => actions.execute(async () => {
          const trackedPaths = tracked.split("\n").map((path) => path.trim()).filter(Boolean);
          const selection = await invoke<{ id: string }>("select_worktree_candidate", {
            target: { projectId: scope.projectId, taskId, unitId, runId: attemptId, captureRevision: captureArtifact.revision },
            selection: { trackedPaths: trackedPaths.length ? trackedPaths : null, untrackedPaths: untracked },
          });
          setCriteria([]);
          return invoke("publish_repository_delivery_candidate", { ...scope, taskId, selectionId: selection.id });
        })}>Save candidate selection</Button>
      </details>}
      {candidate?.value.kind === "candidate" && <>
        <p className="break-all">Candidate tree <code>{candidate.value.evidence.tree}</code></p>
        <DeliveryAgent key={candidate.id} scope={scope} nodeId="review-candidate" inputs={reviewInputs}
          unitId={unitId} job={reviewing} label="Candidate review" actions={actions} openActivity={openActivity} />
        {!review && reviewing?.latestAttempt?.status === "succeeded" && <Button variant="outline" disabled={actions.busy}
          onClick={() => actions.execute(() => invoke("publish_repository_delivery_evidence", {
            ...scope, nodeId: "review-candidate", attemptId: reviewing.latestAttempt?.runId, planSequence: null, expectedPlanRevision: null,
          }))}>Record candidate review</Button>}
        <Button variant="outline" disabled={actions.busy}
          onClick={() => actions.execute(() => requestUnit("verify-candidate", { candidate: candidate.id }, { action: "verify_candidate" }))}>
          {newestCheck ? "Rerun configured checks" : "Run configured checks"}
        </Button>
        {newestCheck && <ul aria-label="Candidate checks" className="pipeline-stack">
          {newestCheck.data.gates.map(({ gate, outcome }) => <li key={gate.name}>
            {gate.name} · {gate.required ? "Required" : "Optional"} · {outcome?.status.replaceAll("_", " ") ?? "Pending"}
            {outcome?.reason && <p className="text-muted-foreground">{outcome.reason}</p>}
          </li>)}
          {!newestCheck.data.gates.length && <li>No checks are configured.</li>}
        </ul>}
        {review && newestCheck && !accepted && <>
          <fieldset className="pipeline-stack">
            <legend>Confirm completion criteria</legend>
            {planned.completionCriteria.map((criterion, index) => <label key={index} className="flex items-center gap-2">
              <input type="checkbox" checked={criteria.includes(index)} onChange={(event) => {
                const checked = event.currentTarget.checked;
                setCriteria((current) => checked ? [...current, index] : current.filter((entry) => entry !== index));
              }} /> {criterion}
            </label>)}
          </fieldset>
          <Button disabled={actions.busy || !readyToAccept || criteria.length !== planned.completionCriteria.length}
            onClick={() => actions.execute(async () => {
              const acceptance = await requestUnit("accept", { candidate: candidate.id, checks: newestCheck.artifact.id, review: review.id }, {
                action: "accept_candidate", completionCriteria: planned.completionCriteria,
              });
              if (acceptance.status !== "artifact") return acceptance;
              const outcome = await requestUnit("commit", { candidate: candidate.id, acceptance: acceptance.artifact.id }, {
                action: "verify_commit", externalSha: null,
              });
              setManualWait(outcome.status === "waiting_for_commit");
              return outcome;
            })}>Accept diff</Button>
        </>}
        {accepted && <>
          <p role="status">In review · Waiting for commit</p>
          <label className="pipeline-stack">{manual ? "Full commit SHA" : "External commit SHA (optional)"}
            <input value={sha} onChange={(event) => setSha(event.currentTarget.value)} />
          </label>
          <Button disabled={actions.busy || !policy.data || ((manual || Boolean(sha.trim())) && !validSha)}
            onClick={() => actions.execute(async () => {
              const outcome = await requestUnit("commit", { candidate: candidate.id, acceptance: accepted.id }, {
                action: "verify_commit", externalSha: sha.trim() || null,
              });
              setManualWait(outcome.status === "waiting_for_commit");
              return outcome;
            })}>{manual || sha.trim() ? "Verify commit" : "Create verified commit"}</Button>
        </>}
      </>}
      {error && <p role="alert">{String(error)}</p>}
    </>}
  </section>;
}
