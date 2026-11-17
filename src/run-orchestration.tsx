import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { JobOrigin } from "@/bindings/JobOrigin";
import type { JobRequirements } from "@/bindings/JobRequirements";
import type { JobWorkspace } from "@/bindings/JobWorkspace";
import type { PipelineJobOrigin } from "@/bindings/PipelineJobOrigin";
import { Button } from "@/components/ui/button";
import { ConversationPreviousRevisions } from "@/conversation-history-controls";
import { GoalEvidence, type InspectedGoal } from "@/goal-evidence";
import { GoalVerificationPanel } from "@/goal-verification-panel";
import { WorktreeEvidence } from "@/worktree-evidence";
import { AttemptDeliveryHistory } from "@/repository-delivery-history";
import type { WorkspaceProps } from "@/workbench-contract";
import { workflowRunTarget } from "@/workflow-execution-model";

type Scope = { projectId: string; runId: string; conversationId?: string };
type SourceDocument = { revisionId: string; checkoutId: string; path: string; sha256: string; baseCommit: string | null };
type Description = { id: string; revision: number; markdown: string | null };
type WorkDescriptions = { epic?: Description; task?: Description; subtask?: Description };
type Orchestration = {
  job: {
    id: string; kind: string; taskId: string | null; ownerTaskId: string | null; origin: JobOrigin;
    operatorRequested: boolean; stage: PipelineJobOrigin | null;
  } | null;
  route: { mode: string; questionRoute: string } | null;
  stagePin: { workflowRevision: string; instanceRevision: string; definitionRevision: number; schedulerProfile: string | null } | null;
  policy: {
    revision: string;
    evidence: {
      role: string; permission: string; requirements: JobRequirements;
      source: { kind: "default" } | { kind: "project_rule"; workspace: JobWorkspace } |
        { kind: "implementation_release"; releaseId: string };
    } | null;
  } | null;
  context: {
    role: string; policyRevision: string; availableBytes: number; usedBytes: number;
    decisions: { title: string; reason: string; selectionOrder: number | null; encodedBytes: number;
      revision?: { source: "instruction"; revisionId: string } | { source: "record"; version: { revisionId: string } } }[];
  } | null;
  document: SourceDocument | null;
  taskDescriptions?: WorkDescriptions | null;
  goal: InspectedGoal | null;
  unit: { purpose: string; ordinal: number } | null;
};

function words(value: string) {
  return value.replaceAll(/[_-]/g, " ");
}

function originLabel(job: NonNullable<Orchestration["job"]>) {
  if (job.origin.type === "pipeline") return "Workflow stage";
  if (job.origin.type === "operator" || job.operatorRequested) return "Operator request";
  return "Delegated by another attempt";
}

function SavedDocument({ document }: { document: SourceDocument }) {
  const [open, setOpen] = useState(false);
  const query = useQuery({
    queryKey: ["repository-markdown", document.checkoutId, document.revisionId],
    queryFn: () => invoke<{ markdown: string } | null>("read_repository_markdown", {
      checkoutId: document.checkoutId, revisionId: document.revisionId,
    }),
    enabled: open,
    staleTime: Infinity,
  });
  return <details onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Saved Markdown · {document.path}</summary>
    <p className="break-all">Revision {document.revisionId} · SHA-256 {document.sha256}</p>
    {document.baseCommit && <p className="break-all">Base commit {document.baseCommit}</p>}
    {open && query.isPending && <p role="status">Loading saved Markdown…</p>}
    {query.error && <p role="alert">{String(query.error)}</p>}
    {query.data === null && <p>Saved Markdown unavailable.</p>}
    {query.data && <pre className="max-h-72 overflow-auto whitespace-pre-wrap">{query.data.markdown}</pre>}
  </details>;
}

function SavedDescriptions({ descriptions }: { descriptions: WorkDescriptions }) {
  const entries = [["Epic", descriptions.epic], ["Task", descriptions.task], ["Sub-Task", descriptions.subtask]] as const;
  return <details>
    <summary>Saved work descriptions</summary>
    {entries.map(([label, description]) => description && <section key={label} className="grid gap-1">
      <h4>{label} description · Revision {description.revision}</h4>
      <p className="break-all">Revision ID <code>{description.id}</code></p>
      {description.markdown && <pre className="max-h-72 overflow-auto whitespace-pre-wrap">{description.markdown}</pre>}
    </section>)}
  </details>;
}

function OrchestrationDetails({ data, scope, live, navigation, showArtifacts }: {
  data: Orchestration; scope: Scope; live: boolean;
  navigation?: Pick<WorkspaceProps, "openActivity" | "openWorkspace">;
  showArtifacts: boolean;
}) {
  const { context, document, goal, job, policy, route, stagePin, taskDescriptions, unit } = data;
  const stage = job?.stage;
  const taskId = job?.taskId;
  const requestingRun = job?.origin.type === "agent" ? job.origin.runId : undefined;
  const source = policy?.evidence?.source;
  return <>
    {goal && <>
      <GoalEvidence goal={goal} runId={scope.runId} openActivity={navigation?.openActivity} />
      <GoalVerificationPanel projectId={scope.projectId} runId={scope.runId} live={live} />
    </>}
    <dl className="grid gap-1">
      {unit && <><dt>Unit {unit.ordinal} objective</dt><dd className="break-words">{unit.purpose}</dd></>}
      {job && <><dt>Routing source</dt><dd>{originLabel(job)}</dd></>}
      {stage && <>
        <dt>Workflow stage</dt><dd className="capitalize">{words(stage.nodeInstanceId)}</dd>
        {stage.planRevision && <><dt>Plan revision</dt><dd className="break-all">{stage.planRevision}</dd></>}
        {stage.release && <><dt>Implementation release</dt><dd className="break-all">{stage.release.id}</dd></>}
      </>}
      {stagePin && <>
        <dt>Workflow revision</dt><dd className="break-all">{stagePin.workflowRevision}</dd>
        <dt>Stage revision</dt><dd className="break-all">{stagePin.instanceRevision} · Definition {stagePin.definitionRevision}</dd>
        {stagePin.schedulerProfile && <><dt>Stage scheduler profile</dt><dd className="capitalize">{words(stagePin.schedulerProfile)}</dd></>}
      </>}
      {route && <>
        <dt>Dispatch mode</dt><dd className="capitalize">{words(route.mode)}</dd>
        <dt>Question route</dt><dd className="capitalize">{words(route.questionRoute)}</dd>
      </>}
      {policy && <>
        <dt>Admitted policy revision</dt><dd className="break-all">{policy.revision}</dd>
        <dt>Effective rule at admission</dt>
        {policy.evidence && source ? <>
          <dd className="capitalize">{words(policy.evidence.role)} · {words(policy.evidence.permission)} · {words(source.kind)}
            {source.kind === "project_rule" && ` · ${words(source.workspace.type)} scope`}
          </dd>
          {source.kind === "project_rule" && source.workspace.type !== "project" && <>
            <dt>Rule scope ID</dt><dd className="break-all">{source.workspace.type === "checkout" ? source.workspace.checkoutId : source.workspace.taskId}</dd>
          </>}
          {!!policy.evidence.requirements.capabilities.length && <><dt>Required capabilities</dt><dd>{policy.evidence.requirements.capabilities.map(words).join(", ")}</dd></>}
          {!!policy.evidence.requirements.customizations.length && <>
            <dt>Required customizations</dt>
            <dd><ul>{policy.evidence.requirements.customizations.map((entry, index) => <li key={index} className="break-all">
              {words(entry.type)} · {"path" in entry ? entry.path : entry.id}
            </li>)}</ul></dd>
          </>}
        </> : <dd>Rule details unavailable for this attempt.</dd>}
      </>}
      {context && <>
        <dt>Context selection</dt><dd>{context.role} · {context.usedBytes.toLocaleString()} of {context.availableBytes.toLocaleString()} bytes</dd>
        <dt>Context policy revision</dt><dd className="break-all">{context.policyRevision}</dd>
      </>}
    </dl>
    {context && context.decisions.length > 0 && <details>
      <summary>Context selection reasons</summary>
      <ul className="grid gap-1">{context.decisions.map((decision, index) => <li key={index}>
        <span className="break-words">{decision.title}</span> · {words(decision.reason)} · {decision.encodedBytes.toLocaleString()} bytes
        {decision.selectionOrder !== null && ` · Included ${decision.selectionOrder + 1}`}
        {decision.revision && <p className="break-all">Revision <code>{decision.revision.source === "record" ?
          decision.revision.version.revisionId : decision.revision.revisionId}</code></p>}
      </li>)}</ul>
    </details>}
    {document && <SavedDocument document={document} />}
    {taskDescriptions && <SavedDescriptions descriptions={taskDescriptions} />}
    {navigation && <div className="flex flex-wrap gap-2">
      {taskId && <Button variant="outline" onClick={() => navigation.openWorkspace("tasks", taskId)}>Open task</Button>}
      {stage && <Button variant="outline" onClick={() => navigation.openWorkspace("pipelines",
        stagePin ? workflowRunTarget(stage.pipelineRunId) : stage.pipelineRunId)}>Open workflow</Button>}
      {requestingRun && <Button variant="outline" onClick={() => navigation.openActivity(requestingRun)}>Open requesting attempt</Button>}
    </div>}
    {showArtifacts && job && <AttemptArtifacts scope={scope} job={job} navigation={navigation} />}
  </>;
}

function AttemptArtifacts({ scope, job, navigation }: {
  scope: Scope;
  job: NonNullable<Orchestration["job"]>;
  navigation?: Pick<WorkspaceProps, "openActivity" | "openWorkspace">;
}) {
  return <>
    <WorktreeEvidence projectId={scope.projectId} runId={scope.runId} ownerTaskId={job.ownerTaskId} />
    {job.taskId && <AttemptDeliveryHistory projectId={scope.projectId} runId={scope.runId} navigation={navigation} />}
  </>;
}

function useRunOrchestration(scope: Scope, live: boolean, enabled = true) {
  return useQuery({
    queryKey: ["run-orchestration", scope.projectId, scope.runId, scope.conversationId],
    queryFn: () => invoke<Orchestration | null>("run_orchestration", scope),
    enabled,
    refetchInterval: enabled && live ? 1500 : false,
  });
}

export function RunArtifactEvidence({ scope, live, navigation }: {
  scope: Scope; live: boolean;
  navigation?: Pick<WorkspaceProps, "openActivity" | "openWorkspace">;
}) {
  const record = useRunOrchestration(scope, live);
  if (record.isPending) return <p role="status">Loading worktree and delivery records…</p>;
  if (record.error) return <p role="alert">{String(record.error)}</p>;
  const job = record.data?.job;
  return job ? <AttemptArtifacts scope={scope} job={job} navigation={navigation} />
    : <p>No worktree or delivery record for this attempt.</p>;
}

export function RunOrchestrationPanel({ scope, live, navigation, showArtifacts = true }: {
  scope: Scope; live: boolean;
  navigation?: Pick<WorkspaceProps, "openActivity" | "openWorkspace">;
  showArtifacts?: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const [showRevisions, setShowRevisions] = useState(false);
  const record = useRunOrchestration(scope, live, expanded);
  return <details onToggle={(event) => setExpanded(event.currentTarget.open)} className="min-w-0">
    <summary>{showArtifacts ? "Orchestration and worktree" : "Orchestration details"}</summary>
    {expanded && <div className="grid gap-2">
      {record.isPending && <p role="status">Loading routing and policy…</p>}
      {record.error && <p role="alert">{String(record.error)}</p>}
      {record.data === null && <p>Orchestration evidence is unavailable.</p>}
      {record.data && <OrchestrationDetails data={record.data} scope={scope} live={live} navigation={navigation} showArtifacts={showArtifacts} />}
      {scope.conversationId && <details onToggle={(event) => setShowRevisions(event.currentTarget.open)}>
        <summary>Context revisions</summary>
        {showRevisions && <ConversationPreviousRevisions conversationId={scope.conversationId} />}
      </details>}
    </div>}
  </details>;
}
