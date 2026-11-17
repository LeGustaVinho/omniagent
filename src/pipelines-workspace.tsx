import { useEffect, useMemo, useState } from "react";
import {
  Handle, MarkerType, Position, addEdge, applyEdgeChanges, applyNodeChanges, useUpdateNodeInternals,
} from "@xyflow/react";
import type { Node, NodeProps } from "@xyflow/react";
import { ArrowUpRight, Check, CircleAlert, CirclePlay, Layers3, Plus, Save } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ModelPicker } from "@/components/model-picker";
import { ConversationTemplateEditor } from "@/conversation-template-editor";
import { RepositoryMarkdownPicker } from "@/repository-markdown-picker";
import type { WorkspaceProps } from "@/workbench-contract";
import { useGitSimulation } from "@/git-simulation";
import { PipelineAuthoringEditor, ProjectPipelineDrafts } from "@/workflow-authoring";
import { WorkflowMaintenanceCandidates, WorkflowMaintenancePlugins } from "@/workflow-maintenance";
import { WorkflowSettingsProposals } from "@/workflow-settings-proposals";
import { WorkflowExecutionWorkspace } from "@/workflow-execution";
import { workflowRunTarget, type WorkflowExecutionFocus } from "@/workflow-execution-model";
import type { ComparisonHandoff } from "@/workflow-comparison-handoff";
import { WorkflowExecutionStateBadge } from "@/workflow-execution-state";
import { WorkflowGraphNodeCard } from "@/workflow-graph-editor";
import { WorkflowComparisons } from "@/workflow-comparison";
import { WorkflowTriggers } from "@/workflow-triggers";
import { PipelineCanvasFrame, PipelineGraphCanvas, PipelineInspectorHeader, PipelineMaintenanceLayout, PipelineRolesHeading, PipelineRunsEmpty, PipelineSectionHeading, PipelineTabs, PipelineToolbar, PipelineWorkspaceFrame } from "@/pipeline-layout";
import { deliveryBranch, deliveryCommitSha, deliveryMergeSha, deliveryRunId } from "@/tasks-demo-data";
import {
  advancePipelineRun, beginPipelineRun, declineStage, interruptStage, resolveStage,
  recordPipelineGit, retryStage, runStatus, sampleModels, snapshotWorkflow, stageAttempt, validateWorkflow,
} from "./pipelines-model";
import type { PipelineRun, Stage, StageEdge, StageStatus, WorkflowRevision } from "./pipelines-model";
import { ComparisonView, PendingPipeline, PipelineMaintenance, RoleEditor, DraftRevisionHistory } from "./pipelines-secondary";

const panels = ["Builder", "Runs", "Roles", "Library", "Compare", "Maintenance"] as const;
type View = typeof panels[number];
const previewTabs = panels.map((id) => ({ id, label: id }));
const productionTabs = [
  { id: "drafts", label: "Builder" },
  { id: "runs", label: "Runs" },
  { id: "templates", label: "Roles" },
  { id: "library", label: "Library" },
  { id: "compare", label: "Compare models" },
  { id: "automations", label: "Automation rules" },
  { id: "maintenance", label: "Maintenance" },
] as const;
type ProductionView = (typeof productionTabs)[number]["id"];
type RenderNode = Node<Stage & { status?: StageStatus; inputSide?: Position; outputSide?: Position }, "stage">;

function StageCard({ id, data, selected }: NodeProps<RenderNode>) {
  const update = useUpdateNodeInternals();
  useEffect(() => update(id), [id, data.inputSide, data.outputSide, update]);
  return <WorkflowGraphNodeCard compact title={data.title || "Untitled stage"} selected={selected}
    detail={data.status ? undefined : data.mode}
    headerContent={data.status && <WorkflowExecutionStateBadge status={data.status === "succeeded" ? "completed" : data.status} />}
    handles={<>
      <Handle type="target" position={data.inputSide ?? Position.Left} />
      <Handle type="source" position={data.outputSide ?? Position.Right} />
    </>} />;
}

const nodeTypes = { stage: StageCard };
const edgeOptions = {
  markerEnd: { type: MarkerType.ArrowClosed, width: 14, height: 14, color: "#828294" },
  style: { stroke: "#828294", strokeWidth: 1.5 },
};

function handleSide(node: Pick<Node, "position">, other: Pick<Node, "position"> | undefined, fallback: Position) {
  if (!other) return fallback;
  const x = other.position.x - node.position.x;
  const y = other.position.y - node.position.y;
  if (Math.abs(x) >= Math.abs(y) * 0.6) return x < 0 ? Position.Left : Position.Right;
  return y < 0 ? Position.Top : Position.Bottom;
}

const viewTargets: Record<string, View> = { comparison: "Compare", compare: "Compare", maintenance: "Maintenance", runs: "Runs", roles: "Roles", builder: "Builder", library: "Library" };

function productionRoute(target = ""): { view: ProductionView; draftId?: string; runId?: string; runFocus?: WorkflowExecutionFocus } {
  if (target.startsWith("run:")) {
    const [runId, search] = target.slice("run:".length).split("?", 2);
    const params = new URLSearchParams(search);
    const nodeId = params.get("node");
    return { view: "runs", runId, runFocus: nodeId ? { nodeId, artifactId: params.get("artifact") ?? undefined } : undefined };
  }
  if (target.startsWith("authoring:")) return { view: "drafts", draftId: target.slice("authoring:".length) };
  if (target.startsWith("compare-note:")) return { view: "compare" };
  const tab = productionTabs.find(({ id }) => id === target);
  if (tab) return { view: tab.id };
  if (!target || target === "builder" || target === "delivery") return { view: "drafts" };
  return { view: "runs", runId: target };
}

function StageConfiguration({ draft, activeId, onPatch, onConnect }: {
  draft: WorkflowRevision; activeId: string; onPatch: (patch: Partial<Stage>) => void;
  onConnect: (source: string, branch?: "migration") => void;
}) {
  const [dependency, setDependency] = useState("");
  const [conditional, setConditional] = useState(false);
  const stage = draft.nodes.find((node) => node.id === activeId)?.data;
  if (!stage) return <p className="pipeline-note">Select a stage to inspect its inputs and outputs.</p>;
  const selectedDependency = draft.nodes.find((node) => node.id === dependency);
  const isManual = stage.kind === "approval" && stage.release === "human";
  const effectiveModel = stage.model ?? draft.roles.find((role) => role.id === stage.roleId)?.model;
  const capabilityOptions = ["Interactive questions", "Repository read", "Image generation", "Audio generation", "Windows host control"];

  return (
    <div className="pipeline-form pipeline-stack">
      <label>Stage name<input value={stage.title} onChange={(event) => onPatch({ title: event.currentTarget.value })} /></label>
      <label>Role<select value={stage.roleId} onChange={(event) => onPatch({ roleId: event.currentTarget.value })}><option value="">Workflow step</option>{draft.roles.map((role) => <option value={role.id} key={role.id}>{role.title}</option>)}</select></label>
      <label>Permission profile<select value={stage.mode} onChange={(event) => onPatch({ mode: event.currentTarget.value as Stage["mode"] })}><option>read-only</option><option>plan</option><option>write</option></select></label>
      <ModelPicker label="Model override" options={sampleModels} selection={{ mode: "single", selected: stage.model, onChange: (model) => onPatch({ model }) }} />
      {stage.model && <Button variant="outline" onClick={() => onPatch({ model: null })}>Use role default</Button>}
      <div className="pipeline-form-pair">
        <label>Timeout (minutes)<input type="number" min={1} value={stage.timeoutMinutes || ""} onChange={(event) => onPatch({ timeoutMinutes: Number(event.currentTarget.value) })} /></label>
        <label>Retry limit<input type="number" min={0} value={stage.retryLimit} onChange={(event) => onPatch({ retryLimit: Number(event.currentTarget.value) })} /></label>
      </div>
      <label>Typed output<select value={stage.output} onChange={(event) => onPatch({ output: event.currentTarget.value as Stage["output"] })}>{["plan", "review", "task", "release", "patch", "checks", "decision"].map((output) => <option key={output}>{output}</option>)}</select></label>
      <label>Required capability<select value={stage.requiredCapability} onChange={(event) => onPatch({ requiredCapability: event.currentTarget.value })}><option value="">None</option>{capabilityOptions.map((capability) => <option key={capability}>{capability}</option>)}</select></label>
      {stage.requiredCapability && <p className="pipeline-meta">Example profile · {stage.requiredCapability === "Windows host control" ? "Unverified" : "Qualified"}</p>}
      <label>Max subagents<input type="number" min={0} value={stage.subagents} onChange={(event) => onPatch({ subagents: Number(event.currentTarget.value) })} /></label>
      {stage.kind === "approval" && <fieldset className="pipeline-stack">
        <legend>Implementation release</legend>
        <label>Decision source<select value={stage.release} onChange={(event) => onPatch({ release: event.currentTarget.value as Stage["release"] })}><option value="human">Human decision</option><option value="rule">User-defined rule</option></select></label>
        {!isManual && <label>Predicate<select value={stage.predicate} onChange={(event) => onPatch({ predicate: event.currentTarget.value as Stage["predicate"] })}><option value="reviews-passed">Plan present and an upstream review passed</option><option value="plan-present">Pinned plan is present</option></select></label>}
      </fieldset>}
      {stage.kind === "condition" && <p className="pipeline-meta">Conditional edges labeled “If migration” read the Boolean migration answer from the pinned plan.</p>}
      <fieldset className="pipeline-stack">
        <legend>Stage goal</legend>
        <label>Objective<input value={stage.goal} onChange={(event) => onPatch({ goal: event.currentTarget.value })} /></label>
        {stage.goal && <>
          <label>Completion evidence<textarea rows={2} value={stage.evidence} onChange={(event) => onPatch({ evidence: event.currentTarget.value })} /></label>
          <label>Time budget (minutes)<input type="number" min={1} value={stage.goalMinutes} onChange={(event) => onPatch({ goalMinutes: Number(event.currentTarget.value) })} /></label>
          <label>Continuation<select value={stage.goalContinuation} onChange={(event) => onPatch({ goalContinuation: event.currentTarget.value as Stage["goalContinuation"] })}><option value="managed">OmniAgent-managed</option><option value="native">Provider-native</option></select></label>
          {stage.goalContinuation === "native" && <p className="pipeline-meta">Native goal control · {effectiveModel?.provider === "codex" ? "Qualified for this example" : "Unverified for this profile"}</p>}
        </>}
      </fieldset>
      <fieldset className="pipeline-stack">
        <legend>Dependencies</legend>
        <ul className="pipeline-dependencies">
          {draft.edges.filter((edge) => edge.target === activeId).map((edge) => <li key={edge.id}>{draft.nodes.find((node) => node.id === edge.source)?.data.title}{edge.label && typeof edge.label === "string" ? " · " + edge.label : ""}</li>)}
        </ul>
        <label>Upstream stage<select value={dependency} onChange={(event) => { setDependency(event.currentTarget.value); setConditional(false); }}>
          <option value="">Select stage</option>
          {draft.nodes.filter((node) => node.id !== activeId && !draft.edges.some((edge) => edge.source === node.id && edge.target === activeId)).map((node) => <option key={node.id} value={node.id}>{node.data.title}</option>)}
        </select></label>
        {selectedDependency?.data.kind === "condition" && <label className="pipeline-checkline"><input type="checkbox" checked={conditional} onChange={(event) => setConditional(event.currentTarget.checked)} /> Run only if the plan requires a migration</label>}
        <Button variant="outline" disabled={!dependency} onClick={() => { onConnect(dependency, conditional ? "migration" : undefined); setDependency(""); setConditional(false); }}>Add dependency</Button>
      </fieldset>
    </div>
  );
}

function StageRunDetails({ run, activeId, actions, links }: {
  run: PipelineRun; activeId: string; actions: { resolve: (migration?: boolean) => void; decline: () => void; retry: () => void; interrupt: () => void };
  links: Pick<WorkspaceProps, "openWorkspace" | "openChanges" | "openActivity">;
}) {
  const [migration, setMigration] = useState(false);
  const node = run.workflow.nodes.find((item) => item.id === activeId);
  const attempt = stageAttempt(run, activeId);
  if (!attempt || !node) return <p className="pipeline-note">Select a stage to view its attempts.</p>;
  const data = node.data;
  const role = run.workflow.roles.find((item) => item.id === data.roleId);
  const canRetry = attempt.status === "failed" && attempt.number <= data.retryLimit;
  return (
    <div className="pipeline-stack">
      <span className="pipeline-status pipeline-self-start" data-status={attempt.status}>{attempt.status}</span>
      <p>{attempt.detail}</p>
      {attempt.git && <div className="pipeline-evidence">
        <dl className="pipeline-data"><dt>Branch</dt><dd>{attempt.git.branch}</dd><dt>SHA</dt><dd><code>{attempt.git.sha}</code></dd></dl>
      </div>}
      <dl className="pipeline-data"><dt>Workflow revision</dt><dd>{run.workflow.revision}</dd><dt>Attempt</dt><dd>{attempt.number}</dd><dt>Profile</dt><dd>{data.mode}</dd><dt>Output contract</dt><dd>{data.output}</dd></dl>
      {attempt.status === "waiting" && data.kind === "plan" && (
        <form className="pipeline-form pipeline-stack pipeline-evidence" onSubmit={(event) => { event.preventDefault(); actions.resolve(migration); }}>
          <fieldset className="pipeline-checks">
            <legend>Does this release need a migration?</legend>
            <label><input type="radio" checked={!migration} onChange={() => setMigration(false)} /> No, documentation only</label>
            <label><input type="radio" checked={migration} onChange={() => setMigration(true)} /> Yes, include migration review</label>
          </fieldset>
          <Button type="submit">Answer question</Button>
        </form>
      )}
      {attempt.status === "waiting" && (data.kind === "approval" || data.kind === "accept") && (
        <section className="pipeline-evidence pipeline-stack">
          <strong>{data.kind === "approval" ? "Review implementation scope" : "Review candidate evidence"}</strong>
          <dl className="pipeline-data"><dt>Plan revision</dt><dd>docs/release.md · 41ac02e</dd><dt>Worktree</dt><dd>release/notes</dd><dt>Decision</dt><dd>Human confirmation</dd></dl>
          <div className="pipeline-actions"><Button onClick={() => actions.resolve()}>{data.kind === "approval" ? "Approve implementation" : "Accept diff"}</Button>{data.kind === "accept" && <Button variant="outline" onClick={() => links.openChanges("saved")}>Open saved diff</Button>}<Button variant="outline" onClick={actions.decline}>Decline</Button></div>
        </section>
      )}
      {attempt.status === "waiting" && (data.kind === "commit" || data.kind === "merge") && (
        <Button variant="outline" onClick={() => links.openWorkspace("git", "saved")}>Open Git delivery <ArrowUpRight size={14} /></Button>
      )}
      {attempt.status === "waiting" && data.goal && !["plan", "approval", "accept", "commit", "merge"].includes(data.kind) && (
        <section className="pipeline-evidence pipeline-stack"><strong>{data.goal}</strong><p>{data.evidence}</p><p className="pipeline-meta">{data.goalContinuation === "native" ? "Provider-native" : "OmniAgent-managed"} · {data.goalMinutes} min limit</p><Button onClick={() => actions.resolve()}>Confirm goal evidence</Button></section>
      )}
      <div className="pipeline-actions">
        {canRetry && <Button onClick={actions.retry}>Retry stage</Button>}
        {(attempt.status === "running" || attempt.status === "waiting") && <Button variant="outline" onClick={actions.interrupt}>Interrupt stage</Button>}
        {data.kind === "task" && attempt.status === "succeeded" && <Button variant="outline" onClick={() => links.openWorkspace("tasks", "release")}>Open task <ArrowUpRight size={14} /></Button>}
        {data.output === "patch" && attempt.status === "succeeded" && <Button variant="outline" onClick={() => links.openChanges("saved")}>Open saved diff</Button>}
        {data.kind === "implementation" && <Button variant="outline" onClick={() => links.openActivity(deliveryRunId)}>Activity</Button>}
      </div>
      <details className="pipeline-evidence"><summary>Context and exact input</summary><p className="pipeline-meta">Context 41ac02e · release/notes · pinned role requirements</p><pre>{role?.instructions || "Structured input from the recorded upstream stages."}</pre></details>
      <details className="pipeline-evidence" open={attempt.number > 1}><summary>Attempt history</summary><ol className="pipeline-attempts">{run.attempts[activeId]?.map((record) => <li key={record.number}><strong>Attempt {record.number} · {record.status}</strong><p>{record.detail}</p></li>)}</ol></details>
    </div>
  );
}

function PipelineLibrary() {
  return <section className="pipeline-view-content pipeline-stack" aria-label="Workflow library">
    <PipelineSectionHeading title="Library" icon={<Layers3 size={20} aria-hidden />} />
    <p className="text-sm text-muted-foreground">No items in the library.</p>
  </section>;
}

function ProjectPipelineMaintenance({ checkout, openActivity, openWorkspace }: Pick<WorkspaceProps, "checkout" | "openActivity" | "openWorkspace">) {
  return <PipelineMaintenanceLayout proposals={<WorkflowSettingsProposals checkout={checkout} onOpenRun={openActivity} compact />}
    releases={<WorkflowMaintenanceCandidates checkout={checkout} openWorkspace={openWorkspace} />}
    inventory={<WorkflowMaintenancePlugins />} />;
}

export default function PipelinesWorkspace(props: WorkspaceProps) {
  if (props.simulation) return <PreviewPipelinesWorkspace {...props} />;
  return <ProjectPipelinesWorkspace key={props.checkout.checkoutId} {...props} />;
}

function ProjectPipelinesWorkspace(props: WorkspaceProps) {
  const [handoff, setHandoff] = useState<ComparisonHandoff>();
  useEffect(() => {
    setHandoff((current) => current && workflowRunTarget(current.runId) === props.target ? current : undefined);
  }, [props.target]);
  const { view, draftId, runId, runFocus } = productionRoute(props.target);
  return <PipelineWorkspaceFrame>
    <PipelineTabs views={productionTabs} active={view} onSelect={(tab) => props.openWorkspace("pipelines", tab)} />
    <div className="pipeline-view">
      {view === "templates" ? <ConversationTemplateEditor checkout={props.checkout} openWorkspace={props.openWorkspace} />
        : view === "compare" ? <WorkflowComparisons key={props.target ?? "compare"} {...props} onUseInPipeline={(destination) => {
          setHandoff(destination);
          props.openWorkspace("pipelines", workflowRunTarget(destination.runId));
        }} />
          : view === "automations" ? <WorkflowTriggers key={props.checkout.checkoutId} {...props} />
          : view === "runs" ? <WorkflowExecutionWorkspace {...props} runId={runId} runFocus={runFocus}
            comparisonHandoff={handoff?.runId === runId ? handoff : undefined} />
          : view === "library" ? <PipelineLibrary />
          : view === "maintenance" ? <ProjectPipelineMaintenance {...props} />
          : draftId ? <PipelineAuthoringEditor key={draftId} {...props} draftId={draftId} />
            : <ProjectPipelineDrafts {...props} />}
    </div>
  </PipelineWorkspaceFrame>;
}

function PreviewPipelinesWorkspace(props: WorkspaceProps) {
  const { simulation, target, startSimulation } = props;
  const git = useGitSimulation();
  const [view, setView] = useState<View>("Builder");
  const [draft, setDraft] = useState<WorkflowRevision | null>({ label: "Untitled pipeline", revision: 1, nodes: [], edges: [], roles: [] });
  const [revisions, setRevisions] = useState<WorkflowRevision[]>([]);
  const [runs, setRuns] = useState<PipelineRun[]>([]);
  const [runId, setRunId] = useState<number | null>(null);
  const [selectedId, setSelectedId] = useState("");
  const [inspectorRequest, revealInspector] = useState(0);
  function viewStage(id: string) {
    setSelectedId(id);
    revealInspector((request) => request + 1);
  }
  const [status, setStatus] = useState("");
  const [issues, setIssues] = useState<string[]>([]);

  const activeRun = runs.find((run) => run.id === runId);
  const inspectingRun = view === "Runs" ? activeRun : undefined;
  const graph = inspectingRun?.workflow ?? draft;
  const activeNode = graph?.nodes.find((node) => node.id === selectedId);
  const resultLinks = graph?.comparisonRuns ?? [];
  const latestRun = runs.at(-1);
  const live = latestRun && ["queued", "running", "waiting"].includes(runStatus(latestRun));
  const actionableStage = activeRun?.workflow.nodes.find((node) => stageAttempt(activeRun, node.id)?.status === "waiting");
  const failedStage = activeRun?.workflow.nodes.find((node) => stageAttempt(activeRun, node.id)?.status === "failed");
  const canvasView = view === "Builder" || view === "Runs";
  const [theme, setTheme] = useState<"light" | "dark">(() => document.documentElement.dataset.appTheme === "light" ? "light" : "dark");

  useEffect(() => {
    const attributes = new MutationObserver(() => setTheme(document.documentElement.dataset.appTheme === "light" ? "light" : "dark"));
    attributes.observe(document.documentElement, { attributes: true, attributeFilter: ["data-app-theme"] });
    return () => attributes.disconnect();
  }, []);

  useEffect(() => {
    if (target && viewTargets[target]) setView(viewTargets[target]);
  }, [target]);

  useEffect(() => {
    if (!latestRun) return;
    const progressed = advancePipelineRun(latestRun);
    const next = recordPipelineGit(progressed, {
      commit: git.candidateCommitted && !git.drift && (simulation?.delivery === "accepted" || simulation?.delivery === "merged")
        ? { branch: deliveryBranch, sha: deliveryCommitSha } : undefined,
      merge: simulation?.delivery === "merged" && !git.drift ? { branch: "main", sha: deliveryMergeSha } : undefined,
    });
    if (next === latestRun) return;
    const timer = window.setTimeout(() => setRuns((items) => items.map((item) => item === latestRun ? next : item)), 650);
    return () => window.clearTimeout(timer);
  }, [latestRun, git.candidateCommitted, git.drift, simulation?.delivery]);

  useEffect(() => {
    if (!latestRun || !simulation || !latestRun.workflow.nodes.some(({ id }) => id === "implementation")) return;
    const planning = stageAttempt(latestRun, "plan")?.status;
    const release = stageAttempt(latestRun, "approval")?.status;
    const implementation = stageAttempt(latestRun, "implementation")?.status;
    const verification = stageAttempt(latestRun, "verify")?.status;
    const acceptance = stageAttempt(latestRun, "accept")?.status;
    switch (simulation.delivery) {
      case "note":
        if (planning === "waiting" || planning === "succeeded") simulation.advanceDelivery("start");
        break;
      case "question":
        if (planning === "succeeded") simulation.advanceDelivery("answer");
        break;
      case "plan":
        if (release === "succeeded") simulation.advanceDelivery("approve");
        break;
      case "queued":
        if (implementation === "running" || implementation === "succeeded") simulation.advanceDelivery("dispatch");
        break;
      case "running":
        if (implementation === "succeeded") simulation.advanceDelivery("finish");
        if (implementation === "failed") simulation.advanceDelivery("fail");
        break;
      case "failed":
        if (implementation === "queued" || implementation === "running") simulation.advanceDelivery("retry");
        break;
      case "review":
        if (verification === "succeeded") simulation.advanceDelivery("verify");
        break;
      case "verified":
        if (acceptance === "succeeded") git.acceptCandidate(simulation);
        break;
    }
  }, [latestRun, simulation, git]);

  function simulate() {
    startSimulation("pipelines");
  }

  function patchStage(patch: Partial<Stage>) {
    setDraft((document) => document && ({
      ...document,
      nodes: document.nodes.map((node) => node.id === selectedId ? { ...node, ariaLabel: patch.title ?? node.ariaLabel, data: { ...node.data, ...patch } } : node),
    }));
    setIssues([]);
  }

  function save() {
    if (!draft) return;
    const latestRevision = revisions.at(-1)?.revision ?? draft.revision;
    const revision = snapshotWorkflow({ ...draft, label: draft.label.trim() || "Untitled workflow", revision: latestRevision + 1 });
    setRevisions([...revisions, revision]);
    setDraft(revision);
    setStatus("Saved revision " + revision.revision + ".");
  }

  function updateRun(update: (run: PipelineRun) => PipelineRun) {
    if (runId === null) return;
    setRuns((items) => items.map((item) => item.id === runId ? update(item) : item));
  }

  function start() {
    if (!draft || (latestRun && ["queued", "running", "waiting"].includes(runStatus(latestRun)))) return;
    const validation = validateWorkflow(draft);
    if (validation.length) {
      setIssues(validation);
      return;
    }
    const snapshot = snapshotWorkflow({ ...draft, mergeRequired: git.mergeRequired });
    const previous = revisions.find((revision) => revision.revision === draft.revision);
    const unchanged = previous && JSON.stringify(snapshotWorkflow(previous)) === JSON.stringify(snapshot);
    const pinned = unchanged ? previous : { ...snapshot, revision: (revisions.at(-1)?.revision ?? draft.revision) + 1 };
    if (!unchanged) {
      setRevisions([...revisions, pinned]);
      setDraft(pinned);
    }
    const run = beginPipelineRun(runs.length + 1, pinned);
    setRuns([...runs, run]);
    setRunId(run.id);
    setSelectedId(pinned.nodes[0]?.id ?? "");
    setView("Runs");
    setIssues([]);
    setStatus("Run #" + run.id + " pinned to revision " + pinned.revision + ".");
  }

  const nodes = useMemo<RenderNode[]>(() => graph?.nodes.map((node) => {
    const input = graph.edges.find((edge) => edge.target === node.id);
    const output = graph.edges.find((edge) => edge.source === node.id);
    return {
      ...node,
      selected: node.id === selectedId,
      data: {
        ...node.data,
        status: inspectingRun ? stageAttempt(inspectingRun, node.id)?.status : undefined,
        inputSide: handleSide(node, graph.nodes.find((peer) => peer.id === input?.source), Position.Left),
        outputSide: handleSide(node, graph.nodes.find((peer) => peer.id === output?.target), Position.Right),
      },
    };
  }) ?? [], [graph, selectedId, inspectingRun]);

  return (
    <PipelineWorkspaceFrame actions={view === "Builder" && <>
      <Button variant="outline" onClick={save} disabled={!draft}><Save size={15} /> Save revision</Button>
      <Button onClick={start} disabled={!draft?.nodes.length || Boolean(live)}><CirclePlay size={15} /> Run pipeline</Button>
    </>}>
      <PipelineTabs views={previewTabs} active={view} onSelect={setView} />
      <div className="pipeline-view" hidden={!canvasView}>
        <PipelineToolbar>
          {view === "Runs" ? <label className="pipeline-inline-field">Execution
            <select value={runId ?? ""} onChange={(event) => {
              const id = Number(event.currentTarget.value);
              const run = runs.find((item) => item.id === id);
              if (run) { setRunId(id); setSelectedId(run.workflow.nodes[0]?.id ?? ""); }
            }}>
              {!runs.length && <option value="">No runs</option>}
              {[...runs].reverse().map((run) => <option key={run.id} value={run.id}>Run #{run.id} · revision {run.workflow.revision}</option>)}
            </select>
          </label> : <label className="pipeline-inline-field">Pipeline
            <input aria-label="Pipeline name" disabled={!draft} value={draft?.label ?? ""} placeholder="No pipeline selected" onChange={(event) => setDraft(draft && { ...draft, label: event.currentTarget.value })} />
          </label>}
          {inspectingRun && <div className="pipeline-actions">
            {actionableStage && <Button variant="outline" onClick={() => viewStage(actionableStage.id)}>View waiting stage</Button>}
            {failedStage && <Button variant="outline" onClick={() => viewStage(failedStage.id)}>View failed stage</Button>}
            <span className="pipeline-status" data-status={runStatus(inspectingRun)}>{runStatus(inspectingRun)}</span>
          </div>}
        </PipelineToolbar>
        {(status || resultLinks.length > 0) && <p className="pipeline-banner" role="status">{status}{resultLinks.length > 0 ? " Comparison inputs · " + resultLinks.join(", ") : ""}</p>}
        {issues.length > 0 && <div className="pipeline-banner pipeline-warning" role="alert"><CircleAlert size={17} /><ul>{issues.map((issue, index) => <li key={index}>{issue}</li>)}</ul></div>}
        {view === "Runs" && !inspectingRun ? <PipelineRunsEmpty onOpenBuilder={() => setView("Builder")} /> :
          <PipelineCanvasFrame canvasLabel="Simulation graph canvas" inspectorLabel="Stage inspector" inspectorRequest={inspectorRequest}
          canvas={graph ? (
              <PipelineGraphCanvas<RenderNode, StageEdge>
                interactive={view === "Builder"}
                key={view}
                nodes={nodes}
                edges={graph.edges}
                nodeTypes={nodeTypes}
                defaultEdgeOptions={edgeOptions}
                colorMode={theme}
                fitView
                fitViewOptions={{ padding: 0.18, maxZoom: 1 }}
                minZoom={0.2}
                maxZoom={2}
                nodesDraggable={view === "Builder"}
                nodesConnectable={view === "Builder"}
                deleteKeyCode={view === "Builder" ? ["Backspace", "Delete"] : null}
                edgesFocusable
                onNodeClick={(_, node) => setSelectedId(node.id)}
                onNodesChange={(changes) => {
                  for (const change of changes) {
                    if (change.type === "select" && change.selected) setSelectedId(change.id);
                  }
                  if (view === "Builder") setDraft((document) => document && { ...document, nodes: applyNodeChanges(changes, document.nodes) });
                }}
                onEdgesChange={(changes) => {
                  if (view === "Builder") setDraft((document) => document && { ...document, edges: applyEdgeChanges(changes, document.edges) });
                }}
                onConnect={(connection) => {
                  if (view === "Builder") setDraft((document) => document && { ...document, edges: addEdge(connection, document.edges) });
                }}
              />
            ) : (
              <div className="pipeline-grid">
                <PendingPipeline label="No pipeline selected" onSimulate={simulate} />
              </div>
            )}
          inspector={<>
            <PipelineInspectorHeader detail={view === "Runs" ? "Execution details" : "Stage properties"}
              title={activeNode?.data.title || "No stage selected"}>
              {graph && graph.nodes.length > 0 && <label className="pipeline-stage-jump"><span className="sr-only">Inspect stage</span><select value={activeNode?.id ?? ""} onChange={(event) => setSelectedId(event.currentTarget.value)}>
                {!activeNode && <option value="">Select a stage</option>}
                {graph.nodes.map((stage) => <option value={stage.id} key={stage.id}>{stage.data.title || "Untitled stage"}</option>)}
              </select></label>}
            </PipelineInspectorHeader>
            {inspectingRun ? (
              <StageRunDetails key={inspectingRun.id + ":" + selectedId} run={inspectingRun} activeId={selectedId} links={props}
                actions={{ resolve: (migration) => updateRun((run) => resolveStage(run, selectedId, migration)), decline: () => updateRun((run) => declineStage(run, selectedId)), retry: () => updateRun((run) => retryStage(run, selectedId)), interrupt: () => updateRun((run) => interruptStage(run, selectedId)) }} />
            ) : draft && view === "Builder" ? (
              <StageConfiguration key={selectedId} draft={draft} activeId={selectedId} onPatch={patchStage} onConnect={(source, branch) => setDraft({
                ...draft, edges: addEdge({ id: source + "-" + selectedId, source, target: selectedId, label: branch ? "If migration" : undefined, data: { branch } }, draft.edges),
              })} />
            ) : view === "Builder" && !simulation ? (
              <RepositoryMarkdownPicker key={props.checkout.checkoutId} checkoutId={props.checkout.checkoutId} />
            ) : <p className="pipeline-note">Stage properties appear here when a workflow is open.</p>}
          </>} />}
      </div>
      <div className="pipeline-view pipeline-view-content" hidden={view !== "Roles"}>
        <PipelineRolesHeading />
        {draft ? <div className="pipeline-stack">
          <div className="pipeline-actions"><Button variant="outline" onClick={() => {
            setDraft({ label: "Untitled workflow", revision: revisions.at(-1)?.revision ?? draft.revision, nodes: [], edges: [], roles: draft.roles });
            setStatus("New pipeline draft.");
            setView("Builder");
          }}><Plus size={15} /> New draft</Button><Button variant="outline" onClick={save}><Save size={15} /> Save revision</Button></div>
          <DraftRevisionHistory revisions={revisions} onLoad={(revision) => { setDraft(revision); setSelectedId(revision.nodes[0]?.id ?? ""); setView("Builder"); setStatus("Opened saved revision " + revision.revision + "."); }} />
          <RoleEditor roles={draft.roles} onChange={(roles) => setDraft({ ...draft, roles })} />
        </div> : <PendingPipeline label="No roles" onSimulate={simulate} />}
      </div>
      <div className="pipeline-view pipeline-view-content" hidden={view !== "Compare"}>
        <ComparisonView simulated={Boolean(simulation)} onSimulate={simulate} onUse={(ids) => { setDraft((document) => document && { ...document, comparisonRuns: ids }); setView("Builder"); setStatus("Selected comparison responses linked to the workflow draft."); }} />
      </div>
      <div className="pipeline-view pipeline-view-content" hidden={view !== "Library"}>
        <PipelineLibrary />
      </div>
      <div className="pipeline-view" hidden={view !== "Maintenance"}>
        <PipelineMaintenance simulation={simulation} startSimulation={startSimulation} openWorkspace={props.openWorkspace} openChanges={props.openChanges} />
      </div>
    </PipelineWorkspaceFrame>
  );
}
