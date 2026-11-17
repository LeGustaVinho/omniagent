import { useEffect, useMemo, useState, type MouseEvent, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { MarkerType, type Edge, type Node, type NodeProps } from "@xyflow/react";
import { ArrowUpRight, Workflow } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ContextActions, PositionedContextActions, useContextActionFeedback, type MenuGroups } from "@/components/ui/context-actions";
import { PipelineCanvasFrame, PipelineGraphCanvas, PipelineInspectorHeader, PipelineRunsEmpty, PipelineRevisionList, PipelineToolbar } from "@/pipeline-layout";
import type { WorkspaceProps } from "@/workbench-contract";
import { WorkflowGraphNodeCard } from "@/workflow-graph-editor";
import { initialNodePositions, wireKey } from "@/workflow-graph-model";
import { RecordedInputs, WorkflowNodeEvidence } from "@/workflow-execution-evidence";
import { WorkflowPromptRunPanel } from "@/workflow-prompt-run";
import { WorkflowExecutionAgent } from "@/workflow-execution-agent";
import { WorkflowExecutionHost } from "@/workflow-execution-host";
import { WorkflowExecutionUnitChecks } from "@/workflow-execution-candidate";
import { WorkflowDiffStage } from "@/workflow-execution-diff";
import { WorkflowApprovalInspector } from "@/workflow-approval";
import { WorkflowStageDecisionForm } from "@/workflow-stage-decisions";
import { WorkflowExecutionComparison } from "@/workflow-execution-comparison";
import { comparisonReviewStages, type ComparisonHandoff } from "@/workflow-comparison-handoff";
import { WorkflowExecutionStateBadge, workflowExecutionStateLabel } from "@/workflow-execution-state";
import {
  comparisonForNode, controlForNode, executionCanvasGraph, executionOutputForWire, needsExecutionRefresh,
  presentation, subflowForStage, workflowRunTarget, type ExecutionPinnedNode, type ExecutionRolePin,
  type ExecutionSubflow, type ExecutionUnit, type WorkflowExecutionFocus, type WorkflowExecutionSnapshot,
  type WorkflowExecutionState, type WorkflowRun,
} from "@/workflow-execution-model";

type ExecutionNode = Node<{ pinned: ExecutionPinnedNode; status: WorkflowExecutionState }, "execution">;
type ExecutionTarget = { kind: "node" | "edge"; id: string };

function ExecutionNodeCard({ data, selected }: NodeProps<ExecutionNode>) {
  const node = data.pinned;
  return <WorkflowGraphNodeCard title={node.definition.title} inputs={node.definition.inputs} outputs={node.definition.outputs}
    selected={selected} readOnly headerContent={<WorkflowExecutionStateBadge status={data.status} />} />;
}

const executionNodeTypes = { execution: ExecutionNodeCard };
const readTheme = (): "light" | "dark" => document.documentElement.dataset.appTheme === "light" ? "light" : "dark";

const roleTools = { read_only: "Repository read", plan: "Planning", approved_write: "Approved worktree write" };
const roleReview = { independent: "Independent", cross_review: "Cross-review required", human_approval: "Human approval required" };

function PinnedRole({ pin }: { pin: ExecutionRolePin }) {
  const { role, version } = pin;
  return <section aria-label={pin.requirementsOnly ? "Pinned role requirements" : "Pinned role template"}>
    <p>{pin.requirementsOnly ? "Role requirements" : "Role template"} · {role?.name ?? "Revision unavailable"}</p>
    <p>{version.destination === "library" ? "Local library" : "Project"} · <code className="break-all">{version.revision}</code></p>
    {role && !pin.requirementsOnly && <>
      <p>{role.model.provider} · {role.model.modelId}{role.effort ? ` · ${role.effort}` : ""}</p>
      <p>{roleTools[role.toolScope]} · {roleReview[role.reviewPolicy]}</p>
    </>}
  </section>;
}

const unitKey = ({ planRevision, unit }: ExecutionUnit) => `${planRevision}:${unit.id}`;

type NodeDetail = ReturnType<typeof presentation>;

function subflowPresentation(snapshot: WorkflowExecutionSnapshot, group: ExecutionSubflow,
  details: readonly NodeDetail[], unit?: ExecutionUnit): NodeDetail {
  const selected = (port: string) => {
    const endpoint = group.outputs[port];
    return endpoint && controlForNode(snapshot, endpoint.node, unit)?.selectedPorts.includes(endpoint.port);
  };
  const failed = details.find(({ status }) => status === "failed" || status === "interrupted");
  if (selected("failed") || selected("falhou")) return { status: "failed", reason: failed?.reason };
  if (selected("completed")) return { status: "completed" };
  for (const status of ["running", "queued", "waiting", "failed", "interrupted", "cancelled"] as const) {
    const member = details.find((detail) => detail.status === status);
    if (member) return { status, reason: member.reason };
  }
  return { status: details.some(({ status }) => status === "completed") ? "completed" : "skipped" };
}

function WorkflowExecutionCanvas({ snapshot, navigation, runPicker, comparisonHandoff, runFocus }: {
  snapshot: WorkflowExecutionSnapshot;
  navigation: Pick<WorkspaceProps, "checkout" | "openActivity" | "openWorkspace" | "openQueueJob">;
  runPicker: ReactNode;
  comparisonHandoff?: ComparisonHandoff;
  runFocus?: WorkflowExecutionFocus;
}) {
  const { definition, run, launch } = snapshot;
  const origin = launch?.origin;
  const workId = origin && origin.kind !== "editor" ? origin.workId : undefined;
  const review = comparisonHandoff && comparisonHandoff.planRevision === run.planRevision &&
    comparisonReviewStages(snapshot, comparisonHandoff.sourceCheckoutId).some(({ instance }) => instance.id === comparisonHandoff.nodeId)
    ? comparisonHandoff : undefined;
  const focusedNode = definition.nodes.find(({ instance }) => instance.id === runFocus?.nodeId);
  const focusedOutput = focusedNode && runFocus?.artifactId ? snapshot.outputs.find(({ artifact }) =>
    artifact.id === runFocus.artifactId && artifact.nodeInstanceId === focusedNode.instance.id)?.artifact : undefined;
  const [selected, setSelected] = useState(focusedNode?.instance.id ?? review?.nodeId ?? definition.nodes[0]?.instance.id ?? "");
  const [inspectorRequest, revealInspector] = useState(focusedNode ? 1 : 0);
  function viewStage(id: string) {
    if (!graph.nodes.some(({ instance }) => instance.id === id)) {
      setSubflowPath(subflowForStage(definition, id)?.path ?? "");
    }
    setSelected(id);
    revealInspector((request) => request + 1);
  }
  const [selectedUnit, setSelectedUnit] = useState<string>();
  const [subflowPath, setSubflowPath] = useState(() =>
    subflowForStage(definition, focusedNode?.instance.id ?? review?.nodeId ?? "")?.path ?? "");
  const [theme, setTheme] = useState(readTheme);
  const [menu, setMenu] = useState<{ target: ExecutionTarget; point: { x: number; y: number }; origin?: HTMLElement | SVGElement }>();
  const { copy, feedback } = useContextActionFeedback();
  useEffect(() => {
    const observer = new MutationObserver(() => setTheme(readTheme()));
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-app-theme"] });
    return () => observer.disconnect();
  }, []);
  const unit = snapshot.units.find((entry) => unitKey(entry) === selectedUnit)
    ?? snapshot.units.find(({ unit }) => unit.id === focusedOutput?.unitId)
    ?? snapshot.units.find(({ planRevision }) => planRevision === run.planRevision);
  const subflow = definition.subflows?.find(({ path }) => path === subflowPath);
  const graph = useMemo(() => executionCanvasGraph(definition, subflow), [definition, subflow]);
  function openSubflow(path: string) {
    const scope = definition.subflows?.find((entry) => entry.path === path);
    setSubflowPath(path);
    setSelected(executionCanvasGraph(definition, scope).nodes[0]?.instance.id ?? "");
  }
  const concreteStages = definition.nodes
    .map((node) => {
      const unitScoped = snapshot.unitNodeIds.includes(node.instance.id);
      const pending = snapshot.pendingDecisions?.find((decision) => decision.nodeId === node.instance.id &&
        decision.nodeRevision === node.revision && decision.planRevision === run.planRevision &&
        (unitScoped ? unit && unit.planRevision === run.planRevision && decision.unitId === unit.unit.id : decision.unitId === null));
      const comparison = comparisonForNode(snapshot, node, unit);
      const detail: ReturnType<typeof presentation> = pending ? {
        status: "waiting",
        reason: pending.request.kind === "approval" || pending.request.kind === "human" && pending.request.interaction === "approval"
          ? "Waiting for operator approval." : "Waiting for operator input.",
      } : comparison && comparison.stage.state !== "settled" ? {
        status: comparison.stage.state,
      } : presentation(snapshot, node, unit);
      return { node, detail, pending, comparison };
    });
  const staged = new Map(concreteStages.map((entry) => [entry.node.instance.id, entry]));
  const stages = graph.nodes.map((node) => {
    const group = graph.subflows.get(node.instance.id);
    if (!group) return { ...staged.get(node.instance.id)!, group: undefined };
    const descendants = group.nodes.flatMap((id) => {
      const stage = staged.get(id);
      return stage ? [stage.detail] : [];
    });
    return { node, detail: subflowPresentation(snapshot, group, descendants, unit), group, pending: undefined, comparison: undefined };
  });
  const positions = useMemo(() => initialNodePositions({
    nodes: graph.nodes.map(({ instance }) => instance),
    wires: graph.wires,
  }), [graph]);
  const active = stages.find(({ node }) => node.instance.id === selected);
  const approval = active?.node.instance.kind === "approve_implementation";
  const scopedStages = subflow ? concreteStages.filter(({ node }) => subflow.nodes.includes(node.instance.id)) : concreteStages;
  const waiting = scopedStages.find(({ pending }) => pending) ?? scopedStages.find(({ detail }) => detail.status === "waiting");
  const failed = scopedStages.find(({ detail }) => detail.status === "failed" || detail.status === "interrupted");
  const nodes: ExecutionNode[] = stages.map(({ node, detail }) => ({
    id: node.instance.id, position: snapshot.layout?.[node.instance.id] ?? positions[node.instance.id]!, type: "execution", selected: node.instance.id === selected,
    ariaLabel: `${node.definition.title} · ${workflowExecutionStateLabel[detail.status]}`, data: { pinned: node, status: detail.status },
  }));
  const edges: Edge[] = graph.wires.map((wire) => {
    const output = graph.nodes.find(({ instance }) => instance.id === wire.from.node)?.definition.outputs.find(({ name }) => name === wire.from.port);
    const control = output?.value.kind === "control";
    const color = control ? "var(--muted-foreground)" : "var(--primary)";
    return { id: wireKey(wire), type: "smoothstep", source: wire.from.node, sourceHandle: wire.from.port,
      target: wire.to.node, targetHandle: wire.to.port, markerEnd: { type: MarkerType.ArrowClosed, color },
      style: { stroke: color, strokeDasharray: control ? "5 4" : undefined }, ariaLabel: `${wire.from.node}.${wire.from.port} to ${wire.to.node}.${wire.to.port}` };
  });
  function openMenu(event: MouseEvent, target: ExecutionTarget) {
    event.preventDefault();
    event.stopPropagation();
    const origin = event.currentTarget;
    setMenu({ target, point: { x: event.clientX, y: event.clientY },
      origin: origin instanceof HTMLElement || origin instanceof SVGElement ? origin : undefined });
  }
  function canvasActions(target: ExecutionTarget): { label: string; groups: MenuGroups } | undefined {
    if (target.kind === "edge") {
      const wire = graph.wires.find((wire) => wireKey(wire) === target.id);
      if (!wire) return;
      const label = `${wire.from.node}.${wire.from.port} → ${wire.to.node}.${wire.to.port}`;
      return { label, groups: [[
        { label: "Inspect source stage", run: () => viewStage(wire.from.node) },
        { label: "Inspect destination stage", run: () => viewStage(wire.to.node) },
        { label: "Copy connection", run: () => copy(label) },
      ]] };
    }
    const stage = stages.find(({ node }) => node.instance.id === target.id);
    if (!stage) return;
    if (stage.group) {
      const path = stage.group.path;
      return { label: stage.node.definition.title, groups: [[
        { label: "Inspect execution", run: () => viewStage(path) },
        { label: "Open internal graph", run: () => openSubflow(path) },
        { label: "Copy pinned revision", run: () => copy(stage.node.revision) },
      ]] };
    }
    const queueLink = navigation.openQueueJob;
    const isScoped = snapshot.unitNodeIds.includes(target.id);
    const jobs = snapshot.jobs.filter(({ nodeId, unitId, job }) => nodeId === target.id && (isScoped
      ? unit && unitId === unit.unit.id && (job.pipeline?.planRevision == null || job.pipeline.planRevision === unit.planRevision)
      : unitId === null && (job.pipeline?.planRevision == null || job.pipeline.planRevision === run.planRevision)));
    const attempts = jobs.flatMap(({ attempts }) => attempts);
    return { label: stage.node.definition.title, groups: [[
      { label: stage.pending ? "Review pending decision" : "Inspect stage", run: () => viewStage(target.id) },
      ...(attempts.length ? [{ label: "Inspect attempt", items: attempts.map((attempt) => ({
        label: `Attempt ${attempt.ordinal} · ${attempt.runId}`, run: () => navigation.openActivity(attempt.runId, attempt.conversationId ?? undefined),
      })) }] : []),
      ...(queueLink && jobs.length ? [{ label: "Inspect queue job", items: jobs.map(({ job }) => ({ label: job.id, run: () => queueLink(job.id) })) }] : []),
    ], [
      { label: "Copy stage ID", run: () => copy(target.id) },
      { label: "Copy pinned stage revision", run: () => copy(stage.node.revision) },
      ...(stage.detail.reason ? [{ label: "Copy stage status details", run: () => copy(stage.detail.reason!) }] : []),
    ]] };
  }
  const context = menu && canvasActions(menu.target);
  return <section className="pipeline-view" aria-label="Pinned workflow execution">
    <PipelineToolbar>
      <div className="workflow-run-selectors">
        {runPicker}
        {!!definition.subflows?.length && <label className="workflow-execution-field">Pinned subflow
          <select value={subflowPath} onChange={(event) => openSubflow(event.currentTarget.value)}>
            <option value="">Entire workflow</option>
            {definition.subflows.map(({ name, path }) => <option key={path} value={path}>{name} · {path}</option>)}
          </select>
        </label>}
        {snapshot.units.length > 0 && <label className="workflow-execution-field">Logical unit
          <select value={unit ? unitKey(unit) : ""} onChange={(event) => setSelectedUnit(event.currentTarget.value)}>
            {!unit && <option value="">Select a saved unit</option>}
            {snapshot.units.map((entry) => <option key={unitKey(entry)} value={unitKey(entry)}>
              {entry.unit.ordinal}. {entry.unit.purpose}{entry.planRevision !== run.planRevision && " · Earlier plan"}
            </option>)}
          </select>
        </label>}
      </div>
      <div className="pipeline-actions">
        {waiting && <Button variant="outline" type="button" onClick={() => viewStage(waiting.node.instance.id)}>View waiting stage</Button>}
        {failed && <Button variant="outline" type="button" onClick={() => viewStage(failed.node.instance.id)}>View failed stage</Button>}
      </div>
    </PipelineToolbar>
    {comparisonHandoff && !review && !snapshot.jobs.some(({ nodeId, inputs }) =>
      nodeId === comparisonHandoff.nodeId && inputs?.plan === comparisonHandoff.planRevision) &&
      <p className="pipeline-banner" role="status">The selected plan review changed. Choose a current destination from Compare models.</p>}
    {feedback && <p className="pipeline-banner" role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
    <header className="workflow-run-summary">
      <div><h2>{definition.name}</h2>
        <p>Pinned workflow revision <code className="break-all">{run.version.revision}</code></p>
        {origin?.kind === "conversation" && (!launch?.checkoutId || launch.checkoutId === navigation.checkout.checkoutId
          ? <Button type="button" variant="ghost"
            onClick={() => navigation.openWorkspace("conversations", origin.conversationId)}>Open source conversation</Button>
          : <p>Conversation turn from another worktree</p>)}
        {origin?.kind === "backlog" && <Button type="button" variant="ghost" className="gap-2"
          onClick={() => navigation.openWorkspace("pipelines", workflowRunTarget(origin.parentRunId, { nodeId: origin.nodeId }))}>
          <Workflow size={14} aria-hidden /> Open parent backlog <ArrowUpRight size={14} aria-hidden />
        </Button>}
        {workId && <p>Agile work <code className="break-all">{workId}</code></p>}
        {launch?.sourceCommit && <p>Source commit <code title={launch.sourceCommit}>{launch.sourceCommit.slice(0, 12)}</code></p>}
        {launch?.checklistItem && <p>Checklist item <code className="break-all">{launch.checklistItem.itemId}</code>
          {" · "}Revision {launch.checklistItem.revision} · {launch.checklistItem.checked ? "Checked" : "Reopened"}</p>}
      </div>
      {subflow && <details>
        <summary>Pinned subflow outputs</summary>
        <p>Revision <code className="break-all">{subflow.version.revision}</code></p>
        <ul>{Object.entries(subflow.outputs).map(([port, endpoint]) => <li key={port}>
          {port} · <code>{endpoint.node}.{endpoint.port}</code>
        </li>)}</ul>
      </details>}
    </header>
    <PipelineCanvasFrame className="workflow-run-frame" canvasLabel="Workflow execution graph" inspectorLabel="Execution node evidence" inspectorRequest={inspectorRequest}
      canvas={<div className="workflow-graph-canvas" onKeyDown={(event) => {
        if (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
        if (!(event.target instanceof Element)) return;
        const origin = event.target.closest(".react-flow__node, .react-flow__edge");
        const id = origin?.getAttribute("data-id");
        if (!id || !(origin instanceof HTMLElement || origin instanceof SVGElement)) return;
        event.preventDefault();
        event.stopPropagation();
        const rect = origin.getBoundingClientRect();
        setMenu({ target: { kind: origin.classList.contains("react-flow__edge") ? "edge" : "node", id },
          point: { x: rect.left + 8, y: rect.top + 8 }, origin });
      }}><PipelineGraphCanvas<ExecutionNode, Edge> key={subflowPath} nodes={nodes} edges={edges} nodeTypes={executionNodeTypes}
        nodesDraggable={false} nodesConnectable={false} edgesReconnectable={false} deleteKeyCode={null}
        onNodeContextMenu={(event, node) => openMenu(event, { kind: "node", id: node.id })}
        onEdgeContextMenu={(event, edge) => openMenu(event, { kind: "edge", id: edge.id })}
        onNodeClick={(_, node) => setSelected(node.id)} onNodesChange={(changes) => {
          const selectedNode = changes.find((change) => change.type === "select" && change.selected);
          if (selectedNode?.type === "select") setSelected(selectedNode.id);
        }} colorMode={theme}
        fitView fitViewOptions={{ padding: 0.15, maxZoom: 1 }} minZoom={0.08} maxZoom={2} /></div>}
      inspector={<>
        <PipelineInspectorHeader detail="Execution details" title={active?.node.definition.title ?? "No stage selected"}>
          <label className="workflow-execution-field">Inspect execution node
            <select value={selected} onChange={(event) => setSelected(event.currentTarget.value)}>
              {stages.map(({ node, detail }) => <option key={node.instance.id} value={node.instance.id}>{node.definition.title} · {workflowExecutionStateLabel[detail.status]}</option>)}
            </select>
          </label>
        </PipelineInspectorHeader>
        {active && <div className="pipeline-stack">
          <WorkflowExecutionStateBadge status={active.detail.status} />
          {active.group ? <section className="workflow-record-card pipeline-stack" aria-label="Subflow execution">
            {active.detail.reason && <p>{active.detail.reason}</p>}
            <Button type="button" variant="outline" className="w-fit" onClick={() => openSubflow(active.group!.path)}>
              Open internal graph <ArrowUpRight size={14} aria-hidden />
            </Button>
            {active.detail.status === "completed" && Object.entries(active.group.outputs).map(([port, endpoint]) => {
              const artifact = executionOutputForWire(snapshot, endpoint, unit)?.artifact;
              if (!artifact) return null;
              return <section key={port} aria-label={`Result on ${port}`}>
                <h3>{port}</h3>
                <pre className="max-h-40 overflow-auto whitespace-pre-wrap text-xs">
                  {typeof artifact.value.evidence === "string" ? artifact.value.evidence : JSON.stringify(artifact.value.evidence, null, 2)}
                </pre>
                <Button type="button" variant="ghost" onClick={() => viewStage(endpoint.node)}>Inspect result source</Button>
              </section>;
            })}
          </section> : <>
          <WorkflowPromptRunPanel snapshot={snapshot} nodeId={active.node.instance.id} unit={unit} navigation={navigation} />
          {snapshot.rolePins?.filter((pin) => pin.nodeId === active.node.instance.id && pin.nodeRevision === active.node.revision)
            .map((pin) => <PinnedRole key={`${pin.version.destination}:${pin.version.id}:${pin.version.revision}`} pin={pin} />)}
          {approval && <WorkflowApprovalInspector key={`approval:${active.node.instance.id}`}
            scope={{ projectId: run.projectId, runId: run.id }} nodeId={active.node.instance.id} planRevision={run.planRevision} />}
          {!approval && active.detail.reason && <p>{active.detail.reason}</p>}
          {active.node.instance.kind === "fork" && <ul aria-label="Fork branches" className="pipeline-stack">
            {snapshot.forkBranches?.filter(({ forkId, unitId }) => forkId === active.node.instance.id &&
              (unitId === null || unitId === unit?.unit.id)).map(({ branch, state }) => <li className="pipeline-actions" key={branch}>
              <span>{branch}</span><WorkflowExecutionStateBadge status={state} />
            </li>)}
          </ul>}
          {active.pending && <WorkflowStageDecisionForm key={active.pending.requestId}
            projectId={run.projectId} runId={run.id} pending={active.pending}
            evidence={Object.keys(active.pending.inputs).length > 0 && <RecordedInputs inputs={active.pending.inputs}
              snapshot={snapshot} scope={{ projectId: run.projectId, runId: run.id }} navigation={navigation}
              consumer={{ nodeInstanceId: active.pending.nodeId, nodeRevision: active.pending.nodeRevision,
                unitId: active.pending.unitId, planRevision: active.pending.planRevision, decisionRequestId: active.pending.requestId }} />} />}
          {active.comparison && <WorkflowExecutionComparison projectId={run.projectId}
            stage={active.comparison.stage} recorded={active.comparison.recorded}
            openActivity={navigation.openActivity} openQueueJob={navigation.openQueueJob} />}
          {!approval && !!active.detail.missing?.length && <section aria-label="Waiting for dependencies">
            <p>Waiting for recorded upstream outputs.</p>
            <ul className="list-disc pl-4">{active.detail.missing.map((wire) => <li key={wireKey(wire)}>
              {wire.to.port} needs {wire.from.node}.{wire.from.port}
            </li>)}</ul>
          </section>}
          <WorkflowExecutionAgent snapshot={snapshot} node={active.node} unit={unit} openActivity={navigation.openActivity}
            initialComparison={review?.nodeId === active.node.instance.id ? review.selection : undefined} />
          <WorkflowExecutionHost snapshot={snapshot} node={active.node} />
          <WorkflowExecutionUnitChecks key={`checks:${active.node.instance.id}`} snapshot={snapshot} node={active.node} unit={unit} />
          <WorkflowDiffStage snapshot={snapshot} node={active.node} />
          {snapshot.backlogExecutions?.filter(({ nodeId, tasks }) => nodeId === active.node.instance.id && tasks.length > 0)
            .map(({ backlogArtifactId, tasks }) => <section key={backlogArtifactId} className="workflow-record-card pipeline-stack" aria-label="Backlog child workflows">
              <h3>Task runs</h3>
              <ul className="pipeline-stack">{tasks.map(({ key, workflowRunId }) => <li key={workflowRunId}>
                <Button type="button" variant="outline" className="gap-2"
                  onClick={() => navigation.openWorkspace("pipelines", workflowRunTarget(workflowRunId))}>
                  <Workflow size={14} aria-hidden /> Open run for {key} <ArrowUpRight size={14} aria-hidden />
                </Button>
              </li>)}</ul>
            </section>)}
          <WorkflowNodeEvidence key={`${active.node.instance.id}:${unit ? unitKey(unit) : ""}`} node={active.node} snapshot={snapshot}
            unitId={unit?.unit.id} focusedArtifactId={focusedOutput?.id}
            control={controlForNode(snapshot, active.node.instance.id, unit)} navigation={navigation} />
          </>}
        </div>}
      </>} />
    {menu && context && <PositionedContextActions {...context} point={menu.point} onClose={() => setMenu(undefined)}
      restoreFocus={() => { if (menu.origin?.isConnected && menu.origin.tabIndex >= 0) menu.origin.focus(); }} />}
  </section>;
}

export function WorkflowExecutionWorkspace(props: WorkspaceProps & { runId?: string; runFocus?: WorkflowExecutionFocus; comparisonHandoff?: ComparisonHandoff }) {
  const { checkout: { projectId }, runId, runFocus, openWorkspace } = props;
  const runs = useQuery({ queryKey: ["repository-delivery", projectId, "list"], queryFn: () => invoke<WorkflowRun[]>("list_repository_deliveries", { projectId }) });
  const execution = useQuery({
    queryKey: ["repository-delivery", projectId, "execution", runId], queryFn: () => invoke<WorkflowExecutionSnapshot | null>("inspect_workflow_execution", { projectId, runId }),
    enabled: Boolean(runId),
    refetchInterval: ({ state }) => needsExecutionRefresh(state.data) ? 900 : false,
  });
  const [clipboard, setClipboard] = useState<{ message: string; failed: boolean }>();
  const loadError = execution.error ?? runs.error;
  function goToRun(id: string) {
    openWorkspace("pipelines", id ? workflowRunTarget(id) : "runs");
  }
  function copy(text: string) {
    setClipboard(undefined);
    void Promise.resolve().then(() => navigator.clipboard.writeText(text)).then(
      () => setClipboard({ message: "Copied to the clipboard.", failed: false }),
      () => setClipboard({ message: "Could not copy to the clipboard.", failed: true }),
    );
  }
  function actions(run: WorkflowRun): MenuGroups {
    const origin = execution.data?.run.id === run.id ? execution.data.launch?.origin : undefined;
    const workId = origin && origin.kind !== "editor" ? origin.workId : undefined;
    const plan = run.planRevision;
    return [[
      { label: "Open execution graph", run: () => goToRun(run.id) },
      ...(workId ? [{ label: "Open linked work", run: () => openWorkspace("tasks", workId) }] : []),
    ], [
      { label: "Copy workflow run ID", run: () => copy(run.id) },
      { label: "Copy pinned workflow revision", run: () => copy(run.version.revision) },
      ...(plan ? [{ label: "Copy plan revision", run: () => copy(plan) }] : []),
    ]];
  }
  const activeRun = execution.data?.run ?? runs.data?.find(({ id }) => id === runId);
  const runPicker = <ContextActions label={activeRun?.id ?? "Workflow run"} className="workflow-run-picker"
    groups={activeRun ? actions(activeRun) : []}><label className="workflow-execution-field">Saved workflow run
    <select value={runId ?? ""} onChange={(event) => goToRun(event.currentTarget.value)}>
      <option value="">Select a workflow run</option>
      {runId && !runs.data?.some(({ id }) => id === runId) && <option value={runId}>{runId}</option>}
      {runs.data?.map(({ id, version }) => <option key={id} value={id}>{id} · {version.revision.slice(0, 12)}</option>)}
    </select>
  </label></ContextActions>;
  return <section className="pipeline-view" aria-label="Workflow runs">
    {loadError && <p className="pipeline-banner" role="alert">{String(loadError)}</p>}
    {clipboard && <p className="pipeline-banner" role={clipboard.failed ? "alert" : "status"}>{clipboard.message}</p>}
    {execution.data ? <WorkflowExecutionCanvas key={workflowRunTarget(execution.data.run.id, runFocus)}
      snapshot={execution.data} navigation={props} runPicker={runPicker} runFocus={runFocus} comparisonHandoff={props.comparisonHandoff} /> : <>
      <PipelineToolbar><div className="workflow-run-selectors">{runPicker}</div></PipelineToolbar>
      {!runId && runs.data?.length === 0 ? (
        <PipelineRunsEmpty onOpenBuilder={() => openWorkspace("pipelines", "drafts")} />
      ) : (
        <div className="workflow-runs-empty">
          {runId && execution.isPending && <p role="status">Loading workflow execution…</p>}
          {!runId && runs.isPending && <p role="status">Loading workflow runs…</p>}
          {execution.data === null && <p>The requested execution is unavailable in this project.</p>}
          {!runId && !!runs.data?.length && <section aria-label="Saved workflow runs" className="pipeline-stack">
            <PipelineRevisionList kind="run" onOpen={goToRun} entries={runs.data.map((run) => ({
              id: run.id, name: run.id, detail: `Workflow revision ${run.version.revision.slice(0, 12)}`, menu: actions(run),
            }))} />
          </section>}
        </div>
      )}
    </>}
  </section>;
}
