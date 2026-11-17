import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState, type ReactNode } from "react";
import type { JobInspection } from "@/bindings/JobInspection";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import { ConversationMarkdown } from "@/conversation-markdown";
import { deliveryMergeReceiptQuery } from "@/delivery-merge-model";
import { downloadArtifact } from "@/download-artifact";
import { JobFailurePanel } from "@/job-failure";
import { WorkflowStructuredEvidence } from "@/workflow-structured-evidence";
import { commitQuery } from "@/project-agile-model";
import { queueBlockerReason } from "@/queue-model";
import { QueueWriterReconciliation } from "@/queue-writer-reconciliation";
import type { WorkspaceProps } from "@/workbench-contract";
import { AttemptStageRequirements } from "@/workflow-stage-requirements";
import { WorkflowMcpToolRun } from "@/workflow-mcp-tools";
import { WorkflowMaintenanceDelivery } from "@/workflow-maintenance";
import { executionOutputForWire, type ExecutionArtifact, type ExecutionControl, type ExecutionJob, type ExecutionOutput, type ExecutionPinnedNode,
  type WorkflowExecutionSnapshot } from "@/workflow-execution-model";

type Navigation = Pick<WorkspaceProps, "checkout" | "openWorkspace" | "openActivity" | "openQueueJob">;
type Task = { projectId: string; taskId: string };
type Git = { id: string; task: Task };
type Merge = Git & { commitEvidenceId: string };
type Approval = { task: Task; source: string; planRevision: string; ruleEvidence?: { revision: string } };
type ArtifactScope = { projectId: string; runId: string };
type Pull = { number: number; headSha: string; baseSha: string; headBranch: string; baseBranch: string };

const text = (kind: string) => kind.replaceAll("_", " ");

function pinnedNodeRevision(snapshot: WorkflowExecutionSnapshot, nodeId: string) {
  return snapshot.definition.nodes.find(({ instance }) => instance.id === nodeId)?.revision;
}

function recordedArtifact(snapshot: WorkflowExecutionSnapshot, output?: ExecutionOutput) {
  return {
    projectId: snapshot.run.projectId,
    workflowRunId: snapshot.run.id,
    workflowVersion: snapshot.run.version,
    artifact: output?.artifact ?? null,
    ...(output && {
      producerRevision: pinnedNodeRevision(snapshot, output.artifact.nodeInstanceId),
      current: output.current,
    }),
  };
}

function EvidenceActions({ direction, port, id, record, children }: {
  direction: "input" | "output";
  port: string;
  id: string;
  record: () => object;
  children: ReactNode;
}) {
  const { feedback, copy, run } = useContextActionFeedback();
  return <>
    <ContextActions label={`${port} ${direction} evidence ${id}`} groups={[[
      { label: `Copy ${direction} evidence JSON`, run: () => copy(JSON.stringify(record(), null, 2)) },
      { label: `Download ${direction} evidence (.json)`, run: () => void run(async () => {
        const filename = id.replace(/[^a-z0-9_-]/gi, "_").slice(0, 80);
        downloadArtifact(`workflow-${direction}-${filename}.json`,
          new Blob([JSON.stringify(record(), null, 2)], { type: "application/json" }));
      }) },
    ]]}><div className="min-w-0 flex-1">{children}</div></ContextActions>
    {feedback && <p className="text-xs" role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
  </>;
}

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
  const synchronizedHead = receipt?.sourceSyncs?.at(-1)?.headSha;
  return <div className="workflow-execution-links">
    {error && <p role="alert">{String(error)}</p>}
    {saved && <p>Verified commit · {saved.branch} · <code>{saved.sha}</code></p>}
    {synchronizedHead && <p>Synchronized PR head · <code>{synchronizedHead}</code></p>}
    {receipt && <p>Verified integration · {receipt.destinationBranch} · <code>{receipt.integrationSha}</code></p>}
    {saved && <Button type="button" variant="outline" onClick={() => navigation.openWorkspace("git", `attempt:${saved.captureRunId}`)}>Open saved diff</Button>}
    <LinkedWork task={task} projectId={scope.projectId} navigation={navigation} />
  </div>;
}

function AgentResponseEvidence({ artifact, scope, openActivity }: {
  artifact: ExecutionArtifact; scope: ArtifactScope; openActivity: Navigation["openActivity"];
}) {
  const [expanded, setExpanded] = useState(false);
  const source = artifact.value.evidence as { runId: string };
  const response = useQuery({
    queryKey: ["conversation", "workflow-agent-response", scope.projectId, scope.runId, artifact.id],
    queryFn: () => invoke<string>("workflow_agent_response", { ...scope, artifactId: artifact.id }),
    enabled: expanded,
    retry: false,
    gcTime: 0,
  });
  return <div className="pipeline-stack">
    <details onToggle={(event) => setExpanded(event.currentTarget.open)}>
      <summary>Read recorded agent response</summary>
      {expanded && (response.isFetching ? <p role="status">Loading the recorded response…</p>
        : response.error ? <p role="alert">{String(response.error)}</p>
          : response.data !== undefined && <ConversationMarkdown>{response.data}</ConversationMarkdown>)}
    </details>
    <Button type="button" className="justify-self-start" variant="outline" onClick={() => openActivity(source.runId)}>Open original agent response</Button>
  </div>;
}

function ArtifactLinks({ artifact, scope, navigation }: { artifact: ExecutionArtifact; scope: ArtifactScope; navigation: Navigation }) {
  const { kind, evidence } = artifact.value;
  switch (kind) {
    case "task": return <LinkedWork task={evidence as Task} projectId={scope.projectId} navigation={navigation} />;
    case "agile_target": return <LinkedWork task={(evidence as { task: Task }).task} projectId={scope.projectId} navigation={navigation} />;
    case "agile_progress_receipt": {
      const progress = evidence as { target: { task: Task }; changed: boolean };
      return <div className="workflow-execution-links">
        <p>{progress.changed ? "Progress updated" : "Progress already matched the request"}</p>
        <LinkedWork task={progress.target.task} projectId={scope.projectId} navigation={navigation} />
      </div>;
    }
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
    case "git_inspection": {
      const { candidate } = evidence as { candidate: { branch: string; tree: string; baseCommit: string; target: Task & { runId: string } } };
      return <div className="workflow-execution-links">
        <p>{candidate.branch} · <code>{candidate.tree}</code></p>
        <p>Base <code>{candidate.baseCommit}</code></p>
        <Button type="button" variant="outline" onClick={() => navigation.openWorkspace("git", `attempt:${candidate.target.runId}`)}>Open captured diff</Button>
        <LinkedWork task={candidate.target} projectId={scope.projectId} navigation={navigation} />
      </div>;
    }
    case "diff_report": {
      const report = evidence as { reviewRunId: string; requiredChecksPassed: boolean; checksId: string };
      return <div className="workflow-execution-links">
        <p>{report.requiredChecksPassed ? "Required checks passed" : "Required checks failed"} · <code>{report.checksId}</code></p>
        <Button type="button" variant="outline" onClick={() => navigation.openActivity(report.reviewRunId)}>Open the pinned review</Button>
      </div>;
    }
    case "merge": return <GitEvidence evidence={evidence as Merge} merge={evidence as Merge} scope={scope} navigation={navigation} />;
    case "github_pull_request": {
      const { pull, requiresHuman } = evidence as { pull: Pull; requiresHuman: boolean };
      return <div className="workflow-execution-links">
        <p>Pull request #{pull.number}{requiresHuman && " · Requires approval"}</p>
        <p>{pull.headBranch} · <code>{pull.headSha}</code></p>
        <p>{pull.baseBranch} · <code>{pull.baseSha}</code></p>
      </div>;
    }
    case "github_ci": {
      const { snapshot } = evidence as { snapshot: { pullNumber: number; state: string; testedSha: string;
        checks: { id: number; source: string; context: string; state: string }[] } };
      return <div className="workflow-execution-links">
        <p>PR #{snapshot.pullNumber} CI · {text(snapshot.state)} · <code>{snapshot.testedSha}</code></p>
        {!!snapshot.checks.length && <ul aria-label="GitHub checks">
          {snapshot.checks.map(({ id, source, context, state }) => <li key={`${source}:${id}`}>{context} · {text(state)}</li>)}
        </ul>}
      </div>;
    }
    case "plan": case "review": return <Button type="button" variant="outline" onClick={() => navigation.openActivity((evidence as { run_id: string }).run_id)}>Open source attempt</Button>;
    case "backlog_plan": {
      const plan = evidence as { parentRunId: string; task: { task: Task } };
      return <div className="workflow-execution-links">
        <LinkedWork task={plan.task.task} projectId={scope.projectId} navigation={navigation} />
        <Button type="button" variant="outline" onClick={() => navigation.openWorkspace("pipelines", `run:${plan.parentRunId}`)}>Open source backlog</Button>
      </div>;
    }
    case "agent_response": return <AgentResponseEvidence artifact={artifact} scope={scope} openActivity={navigation.openActivity} />;
    case "command_result": return <Button type="button" variant="outline" onClick={() => navigation.openActivity((evidence as { runId: string }).runId)}>Open command output and logs</Button>;
    default: return null;
  }
}

type InputEvidence = [port: string, artifactId: string];
type InputConsumer = {
  nodeInstanceId: string;
  nodeRevision?: string;
  unitId?: string | null;
  planRevision?: string | null;
  jobId?: string;
  triggeringRunId?: string | null;
  decisionRequestId?: string;
  inputReceipt?: { executionKey: string; revision: string };
};

export function RecordedInputs({ inputs, snapshot, scope, navigation, consumer, forwarded = false }: {
  inputs: Record<string, string> | InputEvidence[] | null;
  snapshot: WorkflowExecutionSnapshot;
  scope: ArtifactScope;
  navigation: Navigation;
  consumer?: InputConsumer;
  forwarded?: boolean;
}) {
  if (inputs === null) return <p>Exact data inputs were not recorded for this execution.</p>;
  const entries = Array.isArray(inputs) ? inputs : Object.entries(inputs);
  if (entries.length === 0) return <p>No data inputs were supplied.</p>;
  return <ul className="pipeline-stack" aria-label={forwarded ? "Approved iteration outputs" : "Recorded data inputs"}>
    {entries.map(([port, id]) => {
      const output = snapshot.outputs.find(({ artifact }) => artifact.id === id);
      const artifact = output?.artifact;
      return <li key={`${port}:${id}`} className="break-words">
        <EvidenceActions direction="input" port={port} id={id} record={() => ({
          ...recordedArtifact(snapshot, output),
          direction: "input",
          input: { port, artifactId: id, ...consumer },
        })}>
          <strong>{port}</strong>
          {artifact && <> · {artifact.nodeInstanceId}.{artifact.port} · {text(artifact.value.kind)}</>}
          <p><code className="break-all">{id}</code></p>
          {artifact && <details><summary>{forwarded ? "Open exported evidence" : "Open input evidence"}</summary>
            <ArtifactLinks artifact={artifact} scope={scope} navigation={navigation} />
            <pre className="workflow-evidence-json">{JSON.stringify(artifact.value.evidence, null, 2)}</pre>
          </details>}
        </EvidenceActions>
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
  const failed = latest?.status === "failed" || latest?.status === "interrupted";
  const blockers = inspection.data?.blockers ?? [];
  return <section className="workflow-record-card" aria-label={`Scheduled job ${job.id}`}>
    <p>Job <code>{job.id}</code></p>
    {job.pipeline?.planRevision && <p>Plan <code>{job.pipeline.planRevision}</code></p>}
    {job.pipeline?.release && <p>Implementation release <code>{job.pipeline.release.id}</code></p>}
    {inspection.error && <p role="alert">{String(inspection.error)}</p>}
    {!!blockers.length && <ul aria-label="Admission blockers">{blockers.map((blocker, index) => <li key={index}>{queueBlockerReason(blocker)}</li>)}</ul>}
    {!!recovery?.length && <p>Permitted recovery · {recovery.map((action) => action === "retry" ? "Retry saved input" : "Resume checkpoint").join(" · ")}</p>}
    {recovery?.length === 0 && failed && !job.queuedRecovery && <p>No recovery action is currently available.</p>}
    <div className="workflow-execution-links">
      {navigation.openQueueJob && <Button type="button" variant="outline" onClick={() => navigation.openQueueJob?.(job.id)}>Open queue job</Button>}
      {job.taskId && <LinkedWork task={{ projectId: scope.projectId, taskId: job.taskId }} projectId={scope.projectId} navigation={navigation} />}
    </div>
    <details><summary>Job inputs</summary><RecordedInputs inputs={inputs} snapshot={snapshot} scope={scope} navigation={navigation}
      consumer={{ nodeInstanceId: entry.nodeId, nodeRevision: pinnedNodeRevision(snapshot, entry.nodeId), unitId: entry.unitId,
        jobId: job.id, planRevision: job.pipeline?.planRevision }} /></details>
    <ol aria-label="Attempt history" className="pipeline-stack">{attempts.map((attempt) => <li key={attempt.runId} className="workflow-record-card">
      <p>Attempt {attempt.ordinal} · <strong className="capitalize">{text(attempt.status)}</strong></p>
      <p><code>{attempt.runId}</code></p>
      {attempt.previousRunId && <p>Previous attempt <code>{attempt.previousRunId}</code></p>}
      {job.kind !== "command" && <AttemptStageRequirements projectId={scope.projectId} runId={attempt.runId} />}
      <Button type="button" variant="outline" onClick={() => navigation.openActivity(attempt.runId, attempt.conversationId ?? undefined)}>
        {job.kind === "command" ? "Open command logs" : "Open prompts and activity"}
      </Button>
      {(attempt.status === "failed" || attempt.status === "interrupted") && <JobFailurePanel projectId={scope.projectId} runId={attempt.runId} />}
      {attempt.runId === latest?.runId && attempt.status === "interrupted" && job.mode === "approved_write" && job.kind === "agent" &&
        <QueueWriterReconciliation projectId={scope.projectId} jobId={job.id} runId={attempt.runId} />}
    </li>)}</ol>
  </section>;
}

function ControlEvidence({ control, snapshot, jobs, navigation }: {
  control: ExecutionControl; snapshot: WorkflowExecutionSnapshot; jobs: ExecutionJob[]; navigation: Navigation;
}) {
  const runId = control.sourceRunId;
  const attempt = runId ? snapshot.jobs.flatMap(({ attempts }) => attempts).find((entry) => entry.runId === runId) : undefined;
  const shownInJobs = jobs.some(({ attempts }) => attempts.some((entry) => entry.runId === runId));
  const showFailure = !shownInJobs && (attempt?.status === "failed" || attempt?.status === "interrupted");
  const inputPorts = snapshot.definition.nodes.find(({ instance }) => instance.id === control.nodeId)?.definition.inputs;
  const inputs = (control.dependencies ?? []).flatMap<InputEvidence>(({ toPort, artifactId }) =>
    artifactId && inputPorts?.find(({ name }) => name === toPort)?.value.kind !== "control" ? [[toPort, artifactId]] : []);
  const exports = Object.entries(control.forwardedOutputs ?? {}).map<InputEvidence>(([port, output]) => [port, output.artifactId]);
  return <section className="workflow-record-card" aria-label="Recorded control decision">
    {control.reason && <p>{control.reason}</p>}
    {control.selectedPorts.length > 0 && <p>Selected ports · {control.selectedPorts.map(text).join(" · ")}</p>}
    {exports.length > 0 && <RecordedInputs inputs={exports} snapshot={snapshot} forwarded
      scope={{ projectId: snapshot.run.projectId, runId: snapshot.run.id }} navigation={navigation} />}
    {!!control.dependencies?.length && <ul aria-label="Control dependencies">{control.dependencies.map(({ from, toPort, selected }) => <li key={`${from.node}:${from.port}:${toPort}`}>
      {toPort} · {from.node}.{from.port} · {selected ? "Selected" : "Skipped input"}
    </li>)}</ul>}
    {inputs.length > 0 && <RecordedInputs inputs={inputs} snapshot={snapshot}
      scope={{ projectId: snapshot.run.projectId, runId: snapshot.run.id }} navigation={navigation}
      consumer={{ nodeInstanceId: control.nodeId, nodeRevision: pinnedNodeRevision(snapshot, control.nodeId),
        unitId: control.unitId, planRevision: control.planRevision, triggeringRunId: control.sourceRunId }} />}
    {runId && <p>Triggering attempt <code>{runId}</code></p>}
    {attempt && <>
      <Button type="button" variant="outline" onClick={() => navigation.openActivity(attempt.runId, attempt.conversationId ?? undefined)}>Open triggering attempt</Button>
      {showFailure && <JobFailurePanel projectId={snapshot.run.projectId} runId={attempt.runId} />}
    </>}
  </section>;
}

export function WorkflowNodeEvidence({ node, snapshot, unitId, focusedArtifactId, control, navigation }: {
  node: ExecutionPinnedNode; snapshot: WorkflowExecutionSnapshot; unitId?: string; focusedArtifactId?: string;
  control?: ExecutionControl; navigation: Navigation;
}) {
  const { id, projectId } = snapshot.run;
  const scope = { runId: id, projectId };
  const belongs = (nodeId: string, unit: string | null) => nodeId === node.instance.id &&
    (!snapshot.unitNodeIds.includes(nodeId) || !unitId || unit === unitId);
  const jobs = snapshot.jobs.filter(({ nodeId, unitId }) => belongs(nodeId, unitId));
  const outputs = snapshot.outputs.filter(({ artifact }) => belongs(artifact.nodeInstanceId, artifact.unitId));
  const focusedOutput = outputs.find(({ artifact }) => artifact.id === focusedArtifactId)?.artifact.id;
  const focusedCard = useRef<HTMLElement | null>(null);
  useEffect(() => {
    if (!focusedOutput) return;
    const frame = requestAnimationFrame(() => focusedCard.current?.scrollIntoView?.({ block: "nearest" }));
    return () => cancelAnimationFrame(frame);
  }, [focusedOutput]);
  const pastControl = snapshot.control.filter((receipt) => receipt !== control && belongs(receipt.nodeId, receipt.unitId ?? null));
  const argumentsWire = node.definition.kind === "mcp_tool" ? snapshot.definition.wires.find(({ to }) => to.node === node.instance.id && to.port === "arguments") : undefined;
  const argumentsArtifact = argumentsWire && executionOutputForWire(snapshot, argumentsWire.from)?.artifact.id;
  const latestMcpJob = jobs.at(-1);
  const currentMcpJob = latestMcpJob && (latestMcpJob.job.latestAttempt || latestMcpJob.inputs?.arguments === argumentsArtifact) ? latestMcpJob.job : undefined;
  const maintenanceWire = node.definition.kind === "activate_maintenance" && snapshot.definition.wires.find(({ to }) =>
    to.node === node.instance.id && to.port === "git");
  const maintenanceUnit = maintenanceWire && snapshot.units.filter((unit) => unit.planRevision === snapshot.run.planRevision)
    .reduce<(typeof snapshot.units)[number] | undefined>((latest, unit) =>
      !latest || unit.unit.ordinal > latest.unit.ordinal ? unit : latest, undefined);
  const maintenanceGit = maintenanceWire && maintenanceUnit && maintenanceUnit.unit.id === unitId &&
    executionOutputForWire(snapshot, maintenanceWire.from, maintenanceUnit)?.artifact;
  const maintenanceCommit = maintenanceGit && maintenanceGit.value.kind === "git" ? maintenanceGit.value.evidence as Git : undefined;
  return <div className="pipeline-stack">
    <details><summary>Pinned node configuration</summary>
      <p>Definition version {node.definition.revision} · <code className="break-all">{node.revision}</code></p>
      <pre className="workflow-evidence-json">{JSON.stringify(node.instance.configuration, null, 2)}</pre>
    </details>
    {node.definition.kind === "mcp_tool" && navigation.checkout.projectId === projectId && <WorkflowMcpToolRun
      {...scope} checkoutId={navigation.checkout.checkoutId} nodeId={node.instance.id} argumentsArtifact={argumentsArtifact}
      waitsForArguments={Boolean(argumentsWire && !argumentsArtifact)} job={currentMcpJob} />}
    {maintenanceGit && maintenanceCommit?.task.projectId === projectId && <WorkflowMaintenanceDelivery
      projectId={projectId} workflowRunId={id} workflowId={snapshot.run.version.id} nodeId={node.instance.id}
      gitArtifactId={maintenanceGit.id} taskId={maintenanceCommit.task.taskId} commitEvidenceId={maintenanceCommit.id} />}
    {control && <ControlEvidence control={control} snapshot={snapshot} jobs={jobs} navigation={navigation} />}
    {pastControl.map((receipt) => <details key={`${receipt.planRevision ?? ""}:${receipt.unitId ?? ""}`}>
      <summary>Earlier control decision · {text(receipt.state)}{receipt.planRevision && ` · Plan ${receipt.planRevision.slice(0, 12)}`}</summary>
      <ControlEvidence control={receipt} snapshot={snapshot} jobs={jobs} navigation={navigation} />
    </details>)}
    {!!jobs.length && <section aria-label="Scheduled executions" className="pipeline-stack">
      <h3>Scheduled executions</h3>
      {jobs.map((job) => <WorkflowJobEvidence key={job.job.id} entry={job} snapshot={snapshot} scope={scope} navigation={navigation} />)}
    </section>}
    {!!outputs.length && <section aria-label="Recorded outputs" className="pipeline-stack">
      <h3>Recorded outputs</h3>
      {outputs.map((output) => {
        const { artifact, current, inputReceipts } = output;
        const consumer = { nodeInstanceId: artifact.nodeInstanceId, nodeRevision: node.revision, unitId: artifact.unitId };
        return <section key={artifact.id} className={artifact.id === focusedOutput ? "workflow-record-card ring-1 ring-primary" : "workflow-record-card"} ref={artifact.id === focusedOutput ? focusedCard : undefined} aria-label={`${artifact.port} output`}>
          <EvidenceActions direction="output" port={artifact.port} id={artifact.id} record={() => ({
            ...recordedArtifact(snapshot, output),
            direction: "output",
            inputReceipts,
          })}>
            <p><strong>{artifact.port}</strong> · {text(artifact.value.kind)}{!current && <> · Earlier execution</>}</p>
            <p><code>{artifact.id}</code></p>
            <WorkflowStructuredEvidence artifact={artifact} producer={node.instance.kind} projectId={projectId} openWorkspace={navigation.openWorkspace} />
            <ArtifactLinks artifact={artifact} scope={scope} navigation={navigation} />
            <details><summary>Open output evidence</summary><pre className="workflow-evidence-json">{JSON.stringify(artifact.value.evidence, null, 2)}</pre></details>
          </EvidenceActions>
          {!jobs.length && inputReceipts.length === 0 && <p>Exact data inputs were not recorded for this execution.</p>}
          {inputReceipts.map(({ executionKey, revision, inputs }) => <details key={`${executionKey}:${revision}`}>
            <summary>Input evidence{executionKey && ` · Plan ${executionKey.slice(0, 12)}`}</summary>
            <RecordedInputs inputs={inputs} snapshot={snapshot} scope={scope} navigation={navigation}
              consumer={{ ...consumer, inputReceipt: { executionKey, revision } }} />
          </details>)}
        </section>;
      })}
    </section>}
    {!control && !pastControl.length && !jobs.length && !outputs.length && <p>No execution evidence has been recorded for this node.</p>}
  </div>;
}
