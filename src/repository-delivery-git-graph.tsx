import { useState } from "react";
import { useQueries, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Position, ReactFlow, type Edge, type Node } from "@xyflow/react";
import { Button } from "@/components/ui/button";
import { deliveryGitModes, type DeliveryGitPolicy } from "@/delivery-git-policy";
import { deliveryMergeReceiptQuery, type MergeOutcome, type VerifiedMerge } from "@/delivery-merge-model";
import { commitQuery } from "@/project-agile-model";
import {
  deliveryGitControlId, type DeliverySnapshot, type DeliveryTarget, type DeliveryUnit, type DeliveryWorkflowDefinition,
} from "@/repository-delivery-model";
import type { WorkspaceProps } from "@/workbench-contract";

export function pinnedDeliveryGitNodes(definition: DeliveryWorkflowDefinition | undefined) {
  const nodes = definition?.nodes ?? [];
  const commit = nodes.find(({ instance }) => instance.kind === "verify_commit")?.instance.id;
  const gitWire = definition?.wires.find(({ from, to }) => from.node === commit && from.port === "git" &&
    to.port === "git" && nodes.some(({ instance }) => instance.id === to.node && instance.kind === "verify_merge"));
  return { commit, merge: gitWire?.to.node, gitWire };
}

function mergeWait(outcome: MergeOutcome | undefined, policy: DeliveryGitPolicy) {
  switch (outcome?.status) {
    case "merge_blocked": return outcome.reason;
    case "recovery_required": return outcome.message;
    case "conflicted": return "The approved commits conflict with the destination.";
    case "destination_checked_out": return `Review the destination checkout at ${outcome.checkout}.`;
    case "destination_moved": return `The destination moved to ${outcome.actualSha}.`;
    case "destination_drift": {
      if (outcome.recovery.status === "accepted") return "Waiting to integrate the approved destination update.";
      return "Waiting for destination drift review.";
    }
    default:
      if (policy === "user_commits_and_merges") return "Waiting for an external merge SHA.";
      if (policy === "automatic_commit_user_merge") return "Waiting for the user to start integration.";
      return "Waiting for verified integration.";
  }
}

export function RepositoryDeliveryGitGraph({
  scope, delivery, units, taskId, active, waitingForCommit, commitBlock, mergeOutcome, savedMerge, openWorkspace,
}: {
  scope: DeliveryTarget;
  delivery: DeliverySnapshot;
  units: DeliveryUnit[];
  taskId: string | undefined;
  active: boolean;
  waitingForCommit: boolean;
  commitBlock?: string;
  mergeOutcome?: MergeOutcome;
  savedMerge?: VerifiedMerge | null;
  openWorkspace: WorkspaceProps["openWorkspace"];
}) {
  const { commit, merge, gitWire } = pinnedDeliveryGitNodes(delivery.definition);
  const [selected, setSelected] = useState<"commit" | "merge">("commit");
  const policy = useQuery({
    queryKey: ["delivery-git-policy", scope.projectId],
    queryFn: () => invoke<DeliveryGitPolicy>("delivery_git_policy", { projectId: scope.projectId }),
    enabled: Boolean(commit),
    retry: false,
  });
  const last = units.at(-1);
  const lastCommitId = last?.commitEvidenceId ?? null;
  const commits = useQueries({
    queries: units.map((unit) => ({
      ...commitQuery(scope.projectId, unit.taskId, unit.commitEvidenceId ?? ""),
      enabled: Boolean(commit && unit.commitEvidenceId),
    })),
  });
  const recorded = useQuery({
    ...deliveryMergeReceiptQuery(scope.projectId, taskId ?? "", lastCommitId),
    enabled: Boolean(merge && taskId && lastCommitId && !savedMerge),
  });
  const mergeReceipt = savedMerge ?? recorded.data ?? (mergeOutcome?.status === "merge_verified" ? mergeOutcome.evidence : null);
  const verifiedMerge = mergeReceipt?.workflowRunId === scope.runId && mergeReceipt.commitEvidenceId === lastCommitId &&
    mergeReceipt.projectId === scope.projectId && mergeReceipt.taskId === taskId && mergeReceipt.unitId === last?.unit.id
    ? mergeReceipt : null;
  const openUnit = units.find((unit) => !unit.commitEvidenceId);
  const allCommitted = units.length > 0 && !openUnit;
  const mode = deliveryGitModes.find(([value]) => value === policy.data)?.[1];
  if (!commit) return null;

  const receipts = units.map((unit, index) => {
    const receipt = commits[index]?.data;
    return receipt?.unitId === unit.unit.id && receipt.id === unit.commitEvidenceId &&
      receipt.projectId === scope.projectId && receipt.taskId === unit.taskId ? receipt : null;
  });
  const commitStatus = allCommitted && receipts.every(Boolean) ? "Verified" : "Waiting";
  const mergeStatus = verifiedMerge ? "Verified" : "Waiting";
  const nodes: Node[] = [{
    id: commit, position: { x: 0, y: 0 }, sourcePosition: Position.Right,
    ariaLabel: "Commit node", data: { label: <div className="grid"><strong>Commit</strong><small>{commitStatus}</small></div> },
  }];
  if (merge) nodes.push({
    id: merge, position: { x: 224, y: 0 }, targetPosition: Position.Left,
    ariaLabel: "Merge node", data: { label: <div className="grid"><strong>Merge</strong><small>{mergeStatus}</small></div> },
  });
  const edges: Edge[] = gitWire ? [{
    id: `${gitWire.from.node}:git:${gitWire.to.node}`, source: gitWire.from.node, target: gitWire.to.node,
    label: "Verified commit",
  }] : [];
  function inspect(node: string) {
    setSelected(node === merge ? "merge" : "commit");
  }
  function focusControl(node: string, unit: DeliveryUnit) {
    const element = document.getElementById(deliveryGitControlId(scope.runId, node, unit.unit.id));
    element?.focus();
    element?.scrollIntoView?.({ block: "start" });
  }
  const waitingCommit = commitBlock || (!active ? "Waiting for an active, approved delivery." :
    waitingForCommit && policy.data === "user_commits_and_merges" ? "Waiting for a commit SHA from the approved branch." :
    waitingForCommit ? "Acceptance is recorded. Waiting for the verified commit." :
    "Waiting for the candidate, required checks and diff acceptance.");
  const waitingMerge = !allCommitted ? "Waiting for all verified unit commits." :
    !active ? "No verified integration is recorded for this run." :
    policy.data ? mergeWait(mergeOutcome, policy.data) : "Reading the project Git mode.";

  return <section className="pipeline-form pipeline-stack rounded border border-border p-3" aria-label="Delivery Git graph">
    <div className="pipeline-section-heading">
      <h3 className="font-medium">Delivery Git graph</h3>
      {taskId && <Button type="button" variant="outline" onClick={() => openWorkspace("tasks", taskId)}>Open linked work</Button>}
    </div>
    {mode && <p>Project Git mode · {mode}</p>}
    {policy.error && <p role="alert">{String(policy.error)}</p>}
    <div className="h-36 min-w-0 rounded border border-border">
      <ReactFlow nodes={nodes} edges={edges} onNodeClick={(_, node) => inspect(node.id)}
        nodesDraggable={false} nodesConnectable={false} elementsSelectable={false}
        minZoom={0.35} maxZoom={1.2} fitView fitViewOptions={{ padding: 0.2 }} />
    </div>
    <div role="group" aria-label="Git node details" className="flex flex-wrap gap-2">
      <Button type="button" variant="outline" aria-pressed={selected === "commit"} onClick={() => inspect(commit)}>Inspect Commit</Button>
      {merge && <Button type="button" variant="outline" aria-pressed={selected === "merge"} onClick={() => inspect(merge)}>Inspect Merge</Button>}
    </div>
    {selected === "commit" ? <section aria-label="Commit node evidence" className="pipeline-stack">
      {units.length === 0 && <p role="status">Waiting for an approved plan and logical units.</p>}
      {units.length > 0 && <ol className="pipeline-stack">
        {units.map((unit, index) => {
          const request = commits[index];
          const saved = receipts[index];
          return <li key={unit.unit.id} className="pipeline-stack rounded border border-border p-2">
            <p className="font-medium">{unit.unit.purpose}</p>
            {saved ? <p className="break-all">Verified commit · {saved.branch} · <code>{saved.sha}</code></p> :
              unit.commitEvidenceId ? <p role={request?.error ? "alert" : "status"}>{request?.error ? String(request.error) :
                request?.isPending ? "Reading the verified branch and SHA…" : "Verified commit evidence is unavailable."}</p> :
              <p role="status">{openUnit === unit ? waitingCommit : "Waiting for the preceding verified unit commit."}</p>}
            {(saved || (active && openUnit === unit)) && <Button type="button" variant="outline" className="justify-self-start"
              onClick={() => focusControl(commit, unit)}>{saved ? "Open commit evidence" : "Open commit controls"}</Button>}
          </li>;
        })}
      </ol>}
    </section> : merge && <section aria-label="Merge node evidence" className="pipeline-stack">
      {verifiedMerge ? <dl className="pipeline-stack break-all">
        <div><dt>Verified source</dt><dd>{verifiedMerge.sourceBranch} · <code>{verifiedMerge.sourceSha}</code></dd></div>
        <div><dt>Verified integration</dt><dd>{verifiedMerge.destinationBranch} · <code>{verifiedMerge.integrationSha}</code></dd></div>
      </dl> : <p role={recorded.error ? "alert" : "status"}>{recorded.error ? String(recorded.error) : waitingMerge}</p>}
      {active && allCommitted && last && <Button type="button" variant="outline" className="justify-self-start"
        onClick={() => focusControl(merge, last)}>Open merge controls</Button>}
    </section>}
  </section>;
}
