import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import type { VerifiedMerge } from "@/delivery-merge-model";
import { commitQuery, deliveryQuery, taskStateQuery, type VerifiedCommit, type VerifiedDeliveryUnitHistory } from "@/project-agile-model";
import type { DeliverySnapshot } from "@/repository-delivery-model";
import type { WorkspaceProps } from "@/workbench-contract";

type VerifiedPatch = { patchBase64: string };
type CandidateApproval = {
  id: string;
  acceptedAt: string;
  candidate: { id: string; tree: string; target: { projectId: string; taskId: string; unitId: string; runId: string } };
  verification: {
    id: string; candidateId: string; finishedAt: string | null;
    gates: {
      gate: { name: string; required: boolean };
      outcome: {
        status: "passed" | "failed" | "could_not_run" | "interrupted";
        reason?: string; stdout?: string; stderr?: string; truncated?: boolean;
      } | null;
    }[];
  };
};

const checkLabels = { passed: "Passed", failed: "Failed", could_not_run: "Could not run", interrupted: "Interrupted" };
const dateLabel = (date: string) => new Date(date).toLocaleString();
function openSavedAttempt(openActivity: WorkspaceProps["openActivity"], runId: string, conversationId?: string) {
  if (conversationId) openActivity(runId, conversationId);
  else openActivity(runId);
}

function AcceptedPatch({ projectId, taskId, evidenceId, initiallyOpen }: {
  projectId: string; taskId: string; evidenceId: string; initiallyOpen?: boolean;
}) {
  const [open, setOpen] = useState(Boolean(initiallyOpen));
  const query = useQuery({
    queryKey: ["delivery-verified-patch", projectId, taskId, evidenceId],
    queryFn: () => invoke<VerifiedPatch | null>("verified_delivery_patch", { projectId, taskId, evidenceId }),
    enabled: open,
    staleTime: Infinity,
  });
  const patch = query.data && new TextDecoder().decode(Uint8Array.from(atob(query.data.patchBase64), (byte) => byte.charCodeAt(0)));
  return <details className="pipeline-evidence" open={open} onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Accepted diff</summary>
    {open && query.isPending && <p role="status">Loading accepted diff…</p>}
    {query.error && <p role="alert">{String(query.error)}</p>}
    {query.data === null && <p>Accepted diff unavailable.</p>}
    {open && patch && <pre aria-label="Accepted diff" className="max-h-72 overflow-auto whitespace-pre-wrap rounded bg-muted p-2 text-xs">{patch}</pre>}
  </details>;
}

function savedWorkflowLinks(delivery: DeliverySnapshot | undefined, unitId: string | undefined, commit: VerifiedCommit) {
  if (!delivery || !unitId) return;
  const linkedReviews = new Set(delivery.artifacts.flatMap(({ id, unitId: artifactUnit, value }) =>
    artifactUnit === unitId && value.kind === "acceptance" && value.evidence.evidence.id === commit.acceptanceId &&
      (!commit.projectId || value.evidence.evidence.task.projectId === commit.projectId) &&
      (!commit.taskId || value.evidence.evidence.task.taskId === commit.taskId) &&
      (!commit.acceptanceArtifactId || id === commit.acceptanceArtifactId) ? [value.evidence.reviewArtifactId] : [],
  ));
  const review = linkedReviews.size === 1 ? delivery.artifacts.find(({ id, unitId: artifactUnit, value }) =>
    artifactUnit === unitId && value.kind === "review" && linkedReviews.has(id)) : undefined;
  const reviewRunId = review?.value.kind === "review" ? review.value.evidence.run_id : undefined;
  const capture = delivery.jobs.find(({ job }) => job.latestAttempt?.runId === commit.captureRunId)?.job;
  const reviewConversationId = delivery.jobs.find(({ job }) => job.latestAttempt?.runId === reviewRunId)
    ?.job.latestAttempt?.conversationId ?? undefined;
  const release = delivery.artifacts.find(({ value }) => value.kind === "release" &&
    value.evidence.release.id === capture?.pipeline?.release?.id);
  return {
    reviewRunId, reviewConversationId, implementationConversationId: capture?.latestAttempt?.conversationId ?? undefined,
    approvalArtifactId: release?.value.kind === "release" ? release.value.evidence.decisionArtifactId : undefined,
    planRevision: capture?.pipeline?.planRevision ?? undefined,
  };
}

function CandidateChecks({ approval }: { approval: CandidateApproval }) {
  const checks = approval.verification;
  return <section className="grid gap-1" aria-label="Recorded candidate checks">
    <h5 className="font-medium">Recorded candidate checks</h5>
    {checks.finishedAt && <p>Checks finished <time dateTime={checks.finishedAt}>{dateLabel(checks.finishedAt)}</time></p>}
    {checks.gates.length === 0 && <p>No checks were configured for this candidate.</p>}
    <ul className="grid gap-1">
      {checks.gates.map(({ gate, outcome }) => <li key={gate.name}>
        <p>{gate.name} · {gate.required ? "Required" : "Optional"} · {outcome ? checkLabels[outcome.status] : "Pending"}</p>
        {outcome?.reason && <p>{outcome.reason}</p>}
        {(outcome?.stdout || outcome?.stderr) && <details className="pipeline-evidence">
          <summary>Output for {gate.name}</summary>
          <pre className="max-h-48 overflow-auto whitespace-pre-wrap">{[outcome.stdout, outcome.stderr].filter(Boolean).join("\n")}</pre>
          {outcome.truncated && <p>Saved output was truncated.</p>}
        </details>}
      </li>)}
    </ul>
  </section>;
}

export function VerifiedDeliveryHistory({
  projectId, taskId, evidenceId, unitId, delivery, history, openActivity, openWorkspace, showPatch, currentRunId,
}: {
  projectId: string;
  taskId: string;
  evidenceId: string;
  unitId?: string;
  delivery?: DeliverySnapshot;
  history?: VerifiedDeliveryUnitHistory;
  openActivity?: WorkspaceProps["openActivity"];
  openWorkspace?: WorkspaceProps["openWorkspace"];
  showPatch?: boolean;
  currentRunId?: string;
}) {
  const receipt = useQuery({ ...commitQuery(projectId, taskId, evidenceId), enabled: !history });
  const commit = history?.commit ?? receipt.data;
  const accepted = useQuery({
    queryKey: ["delivery-acceptance", projectId, taskId, commit?.acceptanceId],
    queryFn: () => invoke<CandidateApproval | null>("inspect_candidate_acceptance", {
      projectId, taskId, acceptanceId: commit!.acceptanceId,
    }),
    enabled: Boolean(commit?.acceptanceId && commit.verificationId),
    staleTime: Infinity,
  });
  if (receipt.error) return <p role="alert">{String(receipt.error)}</p>;
  if (commit === null) return <p>Verified commit unavailable.</p>;
  if (!commit) return <p role="status">Loading verified commit…</p>;
  const proof = accepted.data;
  const approval = proof && proof.id === commit.acceptanceId && proof.verification.id === commit.verificationId &&
    proof.verification.candidateId === commit.candidateRevision && proof.candidate.id === commit.candidateRevision &&
    proof.candidate.tree === commit.tree && proof.candidate.target.projectId === projectId &&
    proof.candidate.target.taskId === taskId && proof.candidate.target.unitId === (commit.unitId || unitId) &&
    proof.candidate.target.runId === commit.captureRunId ? proof : undefined;
  const linked = savedWorkflowLinks(delivery, unitId, commit);
  const reviewRunId = history ? history.review?.runId : linked?.reviewRunId;
  const reviewConversationId = history ? history.review?.conversationId : linked?.reviewConversationId;
  const implementationConversationId = history?.implementation.conversationId ?? linked?.implementationConversationId;
  const approvalArtifactId = history ? history.approvalArtifactId : linked?.approvalArtifactId;
  const planRevision = history?.planRevision ?? linked?.planRevision;
  const workflowRunId = history?.workflowRunId ?? delivery?.run.id;

  return <section className="pipeline-stack" aria-label={commit.purpose ? `Delivery unit ${commit.purpose}` : "Committed delivery unit"}>
    {commit.purpose && <h5 className="font-medium break-words">{commit.purpose} · {commit.branch}</h5>}
    <p>Verified commit <code className="break-all">{commit.sha}</code></p>
    {!commit.purpose && <p className="break-all">Working branch <code>{commit.branch}</code></p>}
    {commit.verifiedAt && <p>Verified <time dateTime={commit.verifiedAt}>{dateLabel(commit.verifiedAt)}</time></p>}
    {commit.candidateRevision && <p>Accepted candidate <code className="break-all">{commit.candidateRevision}</code></p>}
    {commit.tree && <p>Candidate tree <code className="break-all">{commit.tree}</code></p>}
    {commit.pendingChangesRetained && <p>Additional changes stayed assigned to this work scope.</p>}
    {approvalArtifactId && <p>Plan approval <code className="break-all">{approvalArtifactId}</code>
      {planRevision && <> · Plan revision <code className="break-all">{planRevision}</code></>}</p>}
    {commit.acceptanceId && <p>Candidate approval <code className="break-all">{commit.acceptanceId}</code>
      {approval && <> · <time dateTime={approval.acceptedAt}>{dateLabel(approval.acceptedAt)}</time></>}</p>}
    {accepted.error && <p role="alert">{String(accepted.error)}</p>}
    {accepted.isFetching && !proof && <p role="status">Loading candidate checks…</p>}
    {accepted.isSuccess && !approval && <p>The saved candidate checks could not be matched to this commit.</p>}
    {approval && <CandidateChecks approval={approval} />}
    <AcceptedPatch projectId={projectId} taskId={taskId} evidenceId={evidenceId} initiallyOpen={showPatch} />
    <div className="pipeline-actions">
      {openActivity && commit.captureRunId !== currentRunId && <Button type="button" variant="outline"
        onClick={() => openSavedAttempt(openActivity, commit.captureRunId, implementationConversationId)}>
        Open implementation attempt
      </Button>}
      {openActivity && reviewRunId && reviewRunId !== currentRunId && <Button type="button" variant="outline"
        onClick={() => openSavedAttempt(openActivity, reviewRunId, reviewConversationId)}>
        Open accepted candidate review
      </Button>}
      {openWorkspace && <Button type="button" variant="outline" onClick={() => openWorkspace("git", `attempt:${commit.captureRunId}`)}>
        Open saved Git diff
      </Button>}
      {openWorkspace && workflowRunId && <Button type="button" variant="outline" onClick={() => openWorkspace("pipelines", workflowRunId)}>
        Open unit workflow
      </Button>}
    </div>
  </section>;
}

export function SavedDeliveryMerge({ projectId, taskId, workflowRunId, receipt, unitEvidenceId, showCompletionSha }: {
  projectId: string; taskId: string; workflowRunId: string; receipt?: VerifiedMerge | null; unitEvidenceId?: string;
  showCompletionSha?: boolean;
}) {
  const saved = useQuery({
    queryKey: ["verified-delivery-merge", projectId, taskId, workflowRunId],
    queryFn: () => invoke<VerifiedMerge | null>("verified_delivery_merge", { projectId, taskId, workflowRunId }),
    enabled: receipt === undefined,
    staleTime: 15_000,
  });
  const merge = receipt ?? saved.data;
  if (saved.error && receipt === undefined) return <p role="alert">{String(saved.error)}</p>;
  if (!merge || merge.projectId !== projectId || merge.taskId !== taskId || merge.workflowRunId !== workflowRunId ||
    (unitEvidenceId && !merge.commitEvidenceIds.includes(unitEvidenceId))) return null;
  return <section className="grid gap-1" aria-label="Recorded integration">
    <h5 className="font-medium">Recorded integration</h5>
    <p>Destination branch <code className="break-all">{merge.destinationBranch}</code></p>
    <p>Integration SHA <code className="break-all">{merge.integrationSha}</code></p>
    {showCompletionSha && <p>Completion SHA <code className="break-all">{merge.sourceSha}</code></p>}
    <p>Merge verified <time dateTime={merge.verifiedAt}>{dateLabel(merge.verifiedAt)}</time></p>
  </section>;
}

function AttemptDeliveryCompletion({ projectId, history }: { projectId: string; history: VerifiedDeliveryUnitHistory }) {
  const { commit, workflowRunId } = history;
  const deliveries = useQuery(deliveryQuery(projectId, commit.taskId));
  const state = useQuery(taskStateQuery(projectId, commit.taskId));
  const completed = deliveries.data?.findLast(({ evidence }) => evidence.kind === "verified_commit" &&
    evidence.workflowRunId === workflowRunId && evidence.unitEvidenceIds.includes(commit.id));
  const pending = state.data?.pendingMerge;
  const completionSha = completed?.evidence.kind === "verified_commit" ? completed.evidence.completionSha :
    pending?.workflowRunId === workflowRunId && pending.commitEvidenceIds.includes(commit.id) ? pending.sourceSha : undefined;
  const error = deliveries.error ?? state.error;
  return <>
    {error && <p role="alert">{String(error)}</p>}
    {completionSha && <p>Completion SHA <code className="break-all">{completionSha}</code></p>}
    <SavedDeliveryMerge projectId={projectId} taskId={commit.taskId} workflowRunId={workflowRunId}
      receipt={completed?.merge} unitEvidenceId={commit.id} showCompletionSha={!completionSha} />
  </>;
}

export function AttemptDeliveryHistory({ projectId, runId, navigation }: {
  projectId: string;
  runId: string;
  navigation?: Pick<WorkspaceProps, "openActivity" | "openWorkspace">;
}) {
  const evidence = useQuery({
    queryKey: ["verified-attempt-deliveries", projectId, runId],
    queryFn: () => invoke<VerifiedDeliveryUnitHistory[]>("verified_attempt_deliveries", { projectId, runId }),
    refetchInterval: 5_000,
  });
  if (evidence.error) return <p role="alert">{String(evidence.error)}</p>;
  if (!evidence.data?.length) return null;
  return <section className="pipeline-stack" aria-label="Delivery evidence for this attempt">
    <h4 className="font-medium">Delivery evidence for this attempt</h4>
    {evidence.data.map((history) => <div className="pipeline-evidence" key={history.commit.id}>
      <VerifiedDeliveryHistory projectId={projectId} taskId={history.commit.taskId} evidenceId={history.commit.id}
        history={history} currentRunId={runId} openActivity={navigation?.openActivity} openWorkspace={navigation?.openWorkspace} />
      <AttemptDeliveryCompletion projectId={projectId} history={history} />
    </div>)}
  </section>;
}
