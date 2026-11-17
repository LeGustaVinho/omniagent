import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { GoalVerificationPanel } from "@/goal-verification-panel";
import { readStageGoal } from "@/workflow-stage-goal";
import { wiredExecutionInputs, type ExecutionPinnedNode, type ExecutionUnit, type WorkflowExecutionSnapshot } from "@/workflow-execution-model";

type Capture = { kind: "saved"; artifact: { revision: string; candidate: { changes: { untracked: { path: string }[] } | null } } }
  | { kind: "unavailable"; reason: string };
type Selection = { id: string; target: { runId: string } };

export function WorkflowExecutionUnitChecks({ snapshot, node, unit }: {
  snapshot: WorkflowExecutionSnapshot; node: ExecutionPinnedNode; unit?: ExecutionUnit;
}) {
  const client = useQueryClient();
  const { projectId, id: runId, planRevision } = snapshot.run;
  const nodeId = node.instance.id;
  const unitId = unit?.unit.id;
  const verify = useMutation({
    mutationFn: (input: { unitId: string; taskId: string; inputs: Record<string, string> }) =>
      invoke("execute_repository_delivery_unit", { input: { projectId, runId, nodeId, ...input, request: { action: "verify_candidate" } } }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["repository-delivery", projectId] }),
  });
  const inputs = node.instance.kind === "verify_candidate" && unit?.planRevision === planRevision
    ? wiredExecutionInputs(snapshot, node, unit) : null;
  if (!inputs?.candidate || !unit || !unitId || snapshot.outputs.some(({ artifact }) =>
    artifact.nodeInstanceId === nodeId && artifact.unitId === unitId && artifact.port === "checks")) return null;
  return <>
    <Button variant="outline" disabled={verify.isPending}
      onClick={() => verify.mutate({ unitId, taskId: unit.taskId, inputs })}>Run configured candidate checks</Button>
    {verify.error && <p role="alert">{String(verify.error)}</p>}
  </>;
}

export function WorkflowExecutionCandidate({ snapshot, node, unit, attemptId }: {
  snapshot: WorkflowExecutionSnapshot; node: ExecutionPinnedNode; unit: ExecutionUnit; attemptId: string;
}) {
  const [trackedInput, setTrackedInput] = useState("");
  const [untrackedPaths, setUntrackedPaths] = useState<string[]>([]);
  const client = useQueryClient();
  const { projectId, id: runId } = snapshot.run;
  const { taskId, unit: { id: unitId } } = unit;
  const nodeId = node.instance.id;
  const requirements = node.instance.configuration.requirements as { goal?: unknown } | undefined;
  const goal = readStageGoal(requirements?.goal);
  const capture = useQuery({
    queryKey: ["delivery-capture", projectId, attemptId],
    queryFn: () => invoke<Capture | null>("attempt_worktree_evidence", { projectId, runId: attemptId }),
  });
  const selectionKey = ["delivery-selection", projectId, taskId, unitId];
  const selection = useQuery({
    queryKey: selectionKey,
    queryFn: () => invoke<Selection | null>("current_selected_candidate", { projectId, taskId, unitId }),
    enabled: Boolean(goal),
  });
  const selected = selection.data?.target.runId === attemptId ? selection.data : undefined;
  const publish = (selectionId: string) => invoke("publish_repository_delivery_candidate", { projectId, runId, taskId, nodeId, selectionId });
  const update = useMutation({
    mutationFn: (operation: () => Promise<unknown>) => operation(),
    onSuccess: () => Promise.all([
      client.invalidateQueries({ queryKey: selectionKey }),
      client.invalidateQueries({ queryKey: ["repository-delivery", projectId] }),
      client.invalidateQueries({ queryKey: ["goal-verification", projectId, attemptId] }),
    ]),
  });
  const candidate = capture.data?.kind === "saved" ? capture.data.artifact : undefined;
  const error = capture.error ?? selection.error ?? update.error;
  return <section className="pipeline-stack" aria-label="Candidate selection">
    {capture.isPending && <p role="status">Loading captured changes…</p>}
    {capture.data?.kind === "unavailable" && <p role="alert">{capture.data.reason}</p>}
    {error && <p role="alert">{String(error)}</p>}
    {candidate && <details className="pipeline-evidence" open={!selected}>
      <summary>Select captured changes</summary>
      <label className="pipeline-stack">Tracked paths, one per line (blank includes all)
        <textarea value={trackedInput} onChange={(event) => setTrackedInput(event.currentTarget.value)} />
      </label>
      {candidate.candidate.changes?.untracked.map(({ path }) => <label key={path} className="flex items-center gap-2">
        <input type="checkbox" checked={untrackedPaths.includes(path)} onChange={(event) => {
          const checked = event.currentTarget.checked;
          setUntrackedPaths((previous) => checked ? [...previous, path] : previous.filter((entry) => entry !== path));
        }} /> {path}
      </label>)}
      <Button variant="outline" disabled={update.isPending} onClick={() => update.mutate(async () => {
        const paths = trackedInput.split(/\r?\n/).map((path) => path.trim()).filter(Boolean);
        const next = await invoke<Selection>("select_worktree_candidate", {
          target: { projectId, taskId, unitId, runId: attemptId, captureRevision: candidate.revision },
          selection: { trackedPaths: paths.length ? paths : null, untrackedPaths },
        });
        if (!goal) await publish(next.id);
      })}>Save candidate selection</Button>
    </details>}
    {selected && goal && <GoalVerificationPanel key={selected.id} projectId={projectId} runId={attemptId}
      candidateId={selected.id} requiredEvidence={goal.criteria.map(({ evidence }) => evidence)}
      actions={(result) => <div className="flex flex-wrap gap-2">
        {result.status !== "completed" && goal.criteria.some(({ evidence }) => evidence === "candidate_gate") &&
          <Button variant="outline" disabled={update.isPending} onClick={() => update.mutate(() =>
            invoke("verify_workflow_goal_candidate", { projectId, runId, taskId, nodeId, selectionId: selected.id }))}>Run goal candidate checks</Button>}
        <Button variant="outline" disabled={update.isPending || result.status !== "completed" || Boolean(result.candidateId && result.candidateId !== selected.id)}
          onClick={() => update.mutate(() => publish(selected.id))}>Publish verified candidate</Button>
      </div>} />}
  </section>;
}
