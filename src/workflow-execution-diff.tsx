import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { wiredExecutionInputs, type ExecutionPinnedNode, type WorkflowExecutionSnapshot } from "@/workflow-execution-model";

type DiffCandidate = { id: string; baseCommit: string; tree: string; branch: string; target: { taskId: string; unitId: string } };
type DiffStageAction = { action: "read_git"; taskId: string; candidateId: string; expectedBase: string; expectedTree: string }
  | { action: "verify_review_diff" | "publish_diff_report" };

const stageActions = {
  read_git: "Read selected Git diff",
  verify_review_diff: "Run candidate checks",
  publish_diff_report: "Publish diff report",
} as const;
type DiffStageKind = keyof typeof stageActions;

export function WorkflowDiffStage({ snapshot, node }: { snapshot: WorkflowExecutionSnapshot; node: ExecutionPinnedNode }) {
  const kind = node.definition.kind;
  if (kind !== "read_git" && kind !== "verify_review_diff" && kind !== "publish_diff_report") return null;
  const inputs = wiredExecutionInputs(snapshot, node);
  if (!inputs || snapshot.outputs.some(({ artifact, current }) => current && artifact.nodeInstanceId === node.instance.id)) return null;
  return <DiffStageActions key={node.instance.id} projectId={snapshot.run.projectId} runId={snapshot.run.id}
    nodeId={node.instance.id} kind={kind} inputs={inputs} />;
}

function DiffStageActions({ projectId, runId, nodeId, kind, inputs }: {
  projectId: string; runId: string; nodeId: string; kind: DiffStageKind; inputs: Record<string, string>;
}) {
  const [selected, setSelected] = useState("");
  const client = useQueryClient();
  const scope = { projectId, runId, nodeId };
  const candidates = useQuery({
    queryKey: ["workflow-diff-candidates", projectId, runId, nodeId],
    queryFn: () => invoke<DiffCandidate[]>("workflow_diff_candidates", scope),
    enabled: kind === "read_git",
  });
  const candidate = candidates.data?.find(({ id }) => id === selected);
  const execute = useMutation({
    mutationFn: (action: DiffStageAction) => invoke("execute_workflow_diff_stage", { ...scope, inputs, action }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["repository-delivery", projectId] }),
  });
  const action: DiffStageAction | undefined = kind === "read_git" ? candidate && {
    action: kind,
    taskId: candidate.target.taskId,
    candidateId: candidate.id,
    expectedBase: candidate.baseCommit,
    expectedTree: candidate.tree,
  } : { action: kind };
  const error = candidates.error ?? execute.error;
  return <section className="pipeline-stack" aria-label={stageActions[kind]}>
    {kind === "read_git" && <>
      <label className="workflow-execution-field">Saved Git candidate
        <select value={selected} disabled={execute.isPending} onChange={(event) => setSelected(event.currentTarget.value)}>
          <option value="">Select a candidate</option>
          {candidates.data?.map((entry) => <option key={entry.id} value={entry.id}>
            {entry.branch} · {entry.tree.slice(0, 12)} · {entry.target.taskId}
          </option>)}
        </select>
      </label>
      {candidates.isPending && <p role="status">Loading candidates for the pinned checkout…</p>}
      {candidates.data?.length === 0 && <p>Save a candidate in this checkout to review its diff.</p>}
      {candidate && <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-2 break-all">
        <dt>Base commit</dt><dd><code>{candidate.baseCommit}</code></dd>
        <dt>Candidate tree</dt><dd><code>{candidate.tree}</code></dd>
      </dl>}
    </>}
    <Button type="button" variant="outline" disabled={!action || execute.isPending}
      onClick={() => { if (action) execute.mutate(action); }}>{stageActions[kind]}</Button>
    {error && <p role="alert">{String(error)}</p>}
  </section>;
}
