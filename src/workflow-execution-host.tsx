import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { repositoryNotePlanningKey } from "@/repository-note-planning";
import {
  controlForNode, wiredExecutionInputs, type ExecutionPinnedNode, type WorkflowExecutionSnapshot,
} from "@/workflow-execution-model";

type HostKind = "link_work" | "prepare_worktree";
type HostAction = { action: "link_existing_task"; taskId: string }
  | { action: "create_task"; title: string }
  | { action: "prepare_worktree" };
type TaskOption = { id: string; title: string };

export function WorkflowExecutionHost({ snapshot, node }: {
  snapshot: WorkflowExecutionSnapshot;
  node: ExecutionPinnedNode;
}) {
  const { kind } = node.definition;
  if (kind !== "link_work" && kind !== "prepare_worktree") return null;
  const nodeId = node.instance.id;
  const inputs = wiredExecutionInputs(snapshot, node);
  if (!inputs || controlForNode(snapshot, nodeId)?.state === "skipped") return null;
  const completed = snapshot.outputs.some(({ artifact, current }) => {
    if (!current || artifact.nodeInstanceId !== nodeId) return false;
    if (kind === "link_work") return artifact.port === "task";
    return artifact.port === "release" && artifact.value.kind === "release" &&
      (artifact.value.evidence as { decisionArtifactId: string }).decisionArtifactId === inputs.decision;
  });
  if (completed) return null;
  const { projectId, id: runId } = snapshot.run;
  const origin = snapshot.launch?.origin;
  const launchWork = origin?.kind === "agile" || origin?.kind === "rule" ? origin.workId : undefined;
  return <HostActions key={JSON.stringify([runId, nodeId, inputs, launchWork])}
    projectId={projectId} runId={runId} nodeId={nodeId} kind={kind} inputs={inputs} launchWork={launchWork} />;
}

function HostActions({ projectId, runId, nodeId, kind, inputs, launchWork }: {
  projectId: string;
  runId: string;
  nodeId: string;
  kind: HostKind;
  inputs: Record<string, string>;
  launchWork?: string;
}) {
  const [selected, setSelected] = useState("");
  const [title, setTitle] = useState("");
  const client = useQueryClient();
  const tasks = useQuery({
    queryKey: ["repository-delivery", projectId, "tasks"],
    queryFn: () => invoke<TaskOption[]>("repository_delivery_tasks", { projectId }),
    enabled: kind === "link_work",
  });
  const execute = useMutation({
    mutationFn: (action: HostAction) => invoke("execute_repository_delivery_host", { projectId, runId, nodeId, inputs, action }),
    onSuccess: () => Promise.all([
      client.invalidateQueries({ queryKey: ["repository-delivery", projectId] }),
      client.invalidateQueries({ queryKey: ["project-tasks", projectId] }),
      client.invalidateQueries({ queryKey: ["registered-checkouts"] }),
      client.invalidateQueries({ queryKey: repositoryNotePlanningKey(projectId) }),
    ]),
  });
  const selectedWork = launchWork ?? tasks.data?.find(({ id }) => id === selected)?.id;
  const launchTitle = tasks.data?.find(({ id }) => id === launchWork)?.title;
  const error = execute.error ?? tasks.error;

  return <section className="pipeline-stack" aria-label={kind === "link_work" ? "Link workflow work" : "Prepare workflow worktree"}>
    {kind === "link_work" ? <>
      {launchWork ? <p>Selected at launch · {launchTitle ?? launchWork}</p> : <label className="workflow-execution-field">Existing Task or Sub-Task
        <select value={selected} disabled={execute.isPending} onChange={(event) => setSelected(event.currentTarget.value)}>
          <option value="">Select work</option>
          {tasks.data?.map((task) => <option key={task.id} value={task.id}>{task.title}</option>)}
        </select>
      </label>}
      <Button type="button" variant="outline" disabled={!selectedWork || execute.isPending}
        onClick={() => { if (selectedWork) execute.mutate({ action: "link_existing_task", taskId: selectedWork }); }}>Link work</Button>
      {!launchWork && <>
        <label className="workflow-execution-field">New Task title
          <input value={title} disabled={execute.isPending} onChange={(event) => setTitle(event.currentTarget.value)} />
        </label>
        <Button type="button" variant="outline" disabled={!title.trim() || execute.isPending}
          onClick={() => execute.mutate({ action: "create_task", title: title.trim() })}>Create and link Task</Button>
      </>}
    </> : <Button type="button" disabled={execute.isPending}
      onClick={() => execute.mutate({ action: "prepare_worktree" })}>Prepare managed worktree</Button>}
    {error && <p role="alert">{String(error)}</p>}
  </section>;
}
