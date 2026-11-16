import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { JobInspection } from "@/bindings/JobInspection";
import { Button } from "@/components/ui/button";
import { deliveryMergeReceiptQuery } from "@/delivery-merge-model";
import { JobFailurePanel } from "@/job-failure";
import { commitQuery } from "@/project-agile-model";
import { queueBlockerReason } from "@/queue-model";
import type { WorkspaceProps } from "@/workbench-contract";
import type { ExecutionArtifact, ExecutionJob, ExecutionPinnedNode, WorkflowExecutionSnapshot } from "@/workflow-execution-model";

type Navigation = Pick<WorkspaceProps, "openWorkspace" | "openActivity" | "openQueueJob">;
type Task = { projectId: string; taskId: string };
type Git = { id: string; task: Task };
type Merge = Git & { commitEvidenceId: string };
type Approval = { task: Task; source: string; planRevision: string; ruleEvidence?: { revision: string } };
type ArtifactScope = { projectId: string; runId: string };

const text = (kind: string) => kind.replaceAll("_", " ");

function LinkedWork({ task, projectId, navigation }: { task?: Task; projectId: string; navigation: Navigation }) {
  return task?.projectId === projectId ? <Button type="button" variant="outline" onClick={() => navigation.openWorkspace("tasks", task.taskId)}>Open Agile work</Button> : null;
}

function GitEvidence({ evidence, merge, scope, navigation }: { evidence: Git; merge?: Merge; scope: ArtifactScope; navigation: Navigation }) {
  const task = evidence.task;
  const commitId = merge?.commitEvidenceId ?? evidence.id;
  const commit = useQuery({ ...commitQuery(scope.projectId, task.taskId, commitId), enabled: task.projectId === scope.projectId });
  const integrated = useQuery({ ...deliveryMergeReceiptQuery(scope.projectId, task.taskId, merge?.commitEvidenceId ?? null),
    enabled: Boolean(merge && task.projectId === scope.projectId) });
  const saved = commit.data?.id === commitId && commit.data.projectId === scope.projectId && commit.data.taskId === task.taskId ? commit.data : undefined;
  const receipt = integrated.data?.id === merge?.id && integrated.data?.workflowRunId === scope.runId ? integrated.data : undefined;
  const error = commit.error ?? integrated.error;
  return <div className="workflow-execution-links">
    {error && <p role="alert">{String(error)}</p>}
    {saved && <p>Verified commit · {saved.branch} · <code>{saved.sha}</code></p>}
    {receipt && <p>Verified integration · {receipt.destinationBranch} · <code>{receipt.integrationSha}</code></p>}
    {saved && <Button type="button" variant="outline" onClick={() => navigation.openWorkspace("git", `attempt:${saved.captureRunId}`)}>Open saved diff</Button>}
    <LinkedWork task={task} projectId={scope.projectId} navigation={navigation} />
  </div>;
}

function ArtifactLinks({ artifact, scope, navigation }: { artifact: ExecutionArtifact; scope: ArtifactScope; navigation: Navigation }) {
  const { kind, evidence } = artifact.value;
  switch (kind) {
    case "task": return <LinkedWork task={evidence as Task} projectId={scope.projectId} navigation={navigation} />;
    case "worktree": case "checks": return <LinkedWork task={(evidence as { task: Task }).task} projectId={scope.projectId} navigation={navigation} />;
    case "approval": {
      const approval = evidence as Approval;
      return <div className="workflow-execution-links" aria-label="Approval evidence">
        <p>Implementation approved · {text(approval.source)}</p>
        <p>Plan <code>{approval.planRevision}</code></p>
        {approval.ruleEvidence && <p>Rule revision <code>{approval.ruleEvidence.revision}</code></p>}
        <LinkedWork task={approval.task} projectId={scope.projectId} navigation={navigation} />
      </div>;
    }
    case "candidate": {
      const { target } = evidence as { target: Task & { runId: string } };
      return <div className="workflow-execution-links">
        <Button type="button" variant="outline" onClick={() => navigation.openWorkspace("git", `attempt:${target.runId}`)}>Open candidate diff</Button>
        <LinkedWork task={target} projectId={scope.projectId} navigation={navigation} />
      </div>;
    }
    case "git": return <GitEvidence evidence={evidence as Git} scope={scope} navigation={navigation} />;
    case "merge": return <GitEvidence evidence={evidence as Merge} merge={evidence as Merge} scope={scope} navigation={navigation} />;
    case "plan": case "review": return <Button type="button" variant="outline" onClick={() => navigation.openActivity((evidence as { run_id: string }).run_id)}>Open source attempt</Button>;
    case "command_result": return <Button type="button" variant="outline" onClick={() => navigation.openActivity((evidence as { runId: string }).runId)}>Open command output and logs</Button>;
    default: return null;
  }
}

function RecordedInputs({ inputs, snapshot, scope, navigation }: { inputs: Record<string, string> | null; snapshot: WorkflowExecutionSnapshot; scope: ArtifactScope; navigation: Navigation }) {
  if (inputs === null) return <p>Exact data inputs were not recorded for this execution.</p>;
  const entries = Object.entries(inputs);
  if (entries.length === 0) return <p>No data inputs were supplied.</p>;
  return <ul className="pipeline-stack" aria-label="Recorded data inputs">
    {entries.map(([port, id]) => {
      const output = snapshot.outputs.find(({ artifact }) => artifact.id === id)?.artifact;
      return <li key={port} className="break-words"><strong>{port}</strong>
        {output && <> · {output.nodeInstanceId}.{output.port} · {text(output.value.kind)}</>}
        <p><code className="break-all">{id}</code></p>
        {output && <details><summary>Open input evidence</summary>
          <ArtifactLinks artifact={output} scope={scope} navigation={navigation} />
          <pre className="workflow-evidence-json">{JSON.stringify(output.value.evidence, null, 2)}</pre>
        </details>}
      </li>;
    })}
  </ul>;
}

function WorkflowJobEvidence({ entry, snapshot, scope, navigation }: { entry: ExecutionJob; snapshot: WorkflowExecutionSnapshot; scope: ArtifactScope; navigation: Navigation }) {
  const { job, attempts, inputs } = entry;
  const inspection = useQuery({ queryKey: ["workflow-execution-job", scope.projectId, scope.runId, job.id],
    queryFn: () => invoke<JobInspection | null>("inspect_workflow_execution_job", { ...scope, jobId: job.id }),
    refetchInterval: ["starting", "running", "waiting_for_input"].includes(job.latestAttempt?.status ?? "") || !job.latestAttempt && job.queueState === "queued" ? 1200 : false,
  });
  const latest = job.latestAttempt;
  const recovery = inspection.data?.job.latestAttempt?.runId === latest?.runId ? inspection.data?.recoveryActions : undefined;
  const blockers = inspection.data?.blockers ?? [];
  return <section className="workflow-record-card" aria-label={`Scheduled job ${job.id}`}>
    <p>Job <code>{job.id}</code></p>
    {job.pipeline?.planRevision && <p>Plan <code>{job.pipeline.planRevision}</code></p>}
    {job.pipeline?.release && <p>Implementation release <code>{job.pipeline.release.id}</code></p>}
    {inspection.error && <p role="alert">{String(inspection.error)}</p>}
    {!!blockers.length && <ul aria-label="Admission blockers">{blockers.map((blocker, index) => <li key={index}>{queueBlockerReason(blocker)}</li>)}</ul>}
    {!!recovery?.length && <p>Permitted recovery · {recovery.map((action) => action === "retry" ? "Retry saved input" : "Resume checkpoint").join(" · ")}</p>}
    <div className="workflow-execution-links">
      {navigation.openQueueJob && <Button type="button" variant="outline" onClick={() => navigation.openQueueJob?.(job.id)}>Open queue job</Button>}
      {job.taskId && <LinkedWork task={{ projectId: scope.projectId, taskId: job.taskId }} projectId={scope.projectId} navigation={navigation} />}
    </div>
    <details><summary>Job inputs</summary><RecordedInputs inputs={inputs} snapshot={snapshot} scope={scope} navigation={navigation} /></details>
    <ol aria-label="Attempt history" className="pipeline-stack">{attempts.map((attempt) => <li key={attempt.runId} className="workflow-record-card">
      <p>Attempt {attempt.ordinal} · <strong className="capitalize">{text(attempt.status)}</strong></p>
      <p><code>{attempt.runId}</code></p>
      {attempt.previousRunId && <p>Previous attempt <code>{attempt.previousRunId}</code></p>}
      <Button type="button" variant="outline" onClick={() => navigation.openActivity(attempt.runId, attempt.conversationId ?? undefined)}>
        {job.kind === "command" ? "Open command logs" : "Open prompts and activity"}
      </Button>
      {(attempt.status === "failed" || attempt.status === "interrupted") && <JobFailurePanel projectId={scope.projectId} runId={attempt.runId} />}
    </li>)}</ol>
  </section>;
}

export function WorkflowNodeEvidence({ node, snapshot, unitId, navigation }: { node: ExecutionPinnedNode; snapshot: WorkflowExecutionSnapshot; unitId?: string; navigation: Navigation }) {
  const { id, projectId } = snapshot.run;
  const scope = { runId: id, projectId };
  const belongs = (nodeId: string, unit: string | null) => nodeId === node.instance.id &&
    (!snapshot.unitNodeIds.includes(nodeId) || !unitId || unit === unitId);
  const jobs = snapshot.jobs.filter(({ nodeId, unitId }) => belongs(nodeId, unitId));
  const outputs = snapshot.outputs.filter(({ artifact }) => belongs(artifact.nodeInstanceId, artifact.unitId));
  return <div className="pipeline-stack">
    <details><summary>Pinned node configuration</summary>
      <p>Definition version {node.definition.revision} · <code className="break-all">{node.revision}</code></p>
      <pre className="workflow-evidence-json">{JSON.stringify(node.instance.configuration, null, 2)}</pre>
    </details>
    {!!jobs.length && <section aria-label="Scheduled executions" className="pipeline-stack">
      <h3>Scheduled executions</h3>
      {jobs.map((job) => <WorkflowJobEvidence key={job.job.id} entry={job} snapshot={snapshot} scope={scope} navigation={navigation} />)}
    </section>}
    {!!outputs.length && <section aria-label="Recorded outputs" className="pipeline-stack">
      <h3>Recorded outputs</h3>
      {outputs.map(({ artifact, current, inputReceipts }) => <section key={artifact.id} className="workflow-record-card" aria-label={`${artifact.port} output`}>
        <p><strong>{artifact.port}</strong> · {text(artifact.value.kind)}{!current && <> · Earlier execution</>}</p>
        <p><code>{artifact.id}</code></p>
        <ArtifactLinks artifact={artifact} scope={scope} navigation={navigation} />
        <details><summary>Open output evidence</summary><pre className="workflow-evidence-json">{JSON.stringify(artifact.value.evidence, null, 2)}</pre></details>
        {!jobs.length && inputReceipts.length === 0 && <RecordedInputs inputs={null} snapshot={snapshot} scope={scope} navigation={navigation} />}
        {inputReceipts.map((receipt) => <details key={`${receipt.executionKey}:${receipt.revision}`}>
          <summary>Input evidence{receipt.executionKey && ` · Plan ${receipt.executionKey.slice(0, 12)}`}</summary>
          <RecordedInputs inputs={receipt.inputs} snapshot={snapshot} scope={scope} navigation={navigation} />
        </details>)}
      </section>)}
    </section>}
    {!jobs.length && !outputs.length && <p>No execution evidence has been recorded for this node.</p>}
  </div>;
}
