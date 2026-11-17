import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Background, Controls, MarkerType, ReactFlow, type Edge, type Node, type NodeProps } from "@xyflow/react";
import { Button } from "@/components/ui/button";
import type { WorkspaceProps } from "@/workbench-contract";
import { WorkflowPortColumn } from "@/workflow-graph-editor";
import { initialNodePositions, wireKey } from "@/workflow-graph-model";
import { WorkflowNodeEvidence } from "@/workflow-execution-evidence";
import { WorkflowExecutionAgent } from "@/workflow-execution-agent";
import { WorkflowExecutionUnitChecks } from "@/workflow-execution-candidate";
import { WorkflowApprovalInspector } from "@/workflow-approval";
import {
  controlForNode, presentation, workflowRunTarget, type ExecutionPinnedNode, type ExecutionRolePin, type WorkflowExecutionSnapshot, type WorkflowExecutionState, type WorkflowRun,
} from "@/workflow-execution-model";
import "./workflow-execution.css";

const stateLabel: Record<WorkflowExecutionState, string> = {
  queued: "Queued", running: "Running", waiting: "Waiting", skipped: "Skipped", completed: "Completed", failed: "Failed", interrupted: "Interrupted", cancelled: "Cancelled",
};

type ExecutionNode = Node<{ pinned: ExecutionPinnedNode; status: WorkflowExecutionState }, "execution">;

function State({ status }: { status: WorkflowExecutionState }) {
  return <span className="workflow-execution-state" data-status={status}>{stateLabel[status]}</span>;
}

function ExecutionNodeCard({ data, selected }: NodeProps<ExecutionNode>) {
  const node = data.pinned;
  return <article className="workflow-node-card" data-selected={selected || undefined}>
    <header><strong>{node.definition.title}</strong><State status={data.status} /></header>
    <div className="workflow-node-ports">
      <WorkflowPortColumn ports={node.definition.inputs} side="input" readOnly />
      <WorkflowPortColumn ports={node.definition.outputs} side="output" readOnly />
    </div>
  </article>;
}

const executionNodeTypes = { execution: ExecutionNodeCard };

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

function WorkflowExecutionCanvas({ snapshot, navigation }: {
  snapshot: WorkflowExecutionSnapshot;
  navigation: Pick<WorkspaceProps, "checkout" | "openActivity" | "openWorkspace" | "openQueueJob">;
}) {
  const { definition, run, launch } = snapshot;
  const [selected, setSelected] = useState(definition.nodes[0]?.instance.id ?? "");
  const [selectedUnit, setSelectedUnit] = useState<string>();
  const [subflowPath, setSubflowPath] = useState("");
  const unit = snapshot.units.find(({ unit }) => unit.id === selectedUnit) ?? snapshot.units.find(({ planRevision }) => planRevision === run.planRevision);
  const subflow = definition.subflows?.find(({ path }) => path === subflowPath);
  const graph = useMemo(() => ({
    name: subflow?.name ?? definition.name,
    nodes: definition.nodes.filter(({ instance }) => !subflow || subflow.nodes.includes(instance.id)).map(({ instance }) => instance),
    wires: definition.wires.filter(({ from, to }) => !subflow || subflow.nodes.includes(from.node) && subflow.nodes.includes(to.node)),
  }), [definition, subflow]);
  const stages = definition.nodes.filter(({ instance }) => graph.nodes.some(({ id }) => id === instance.id))
    .map((node) => ({ node, detail: presentation(snapshot, node, unit) }));
  const positions = useMemo(() => initialNodePositions(graph), [graph]);
  const active = stages.find(({ node }) => node.instance.id === selected);
  const approval = active?.node.instance.kind === "approve_implementation";
  const nodes: ExecutionNode[] = stages.map(({ node, detail }) => ({
    id: node.instance.id, position: positions[node.instance.id]!, type: "execution", selected: node.instance.id === selected,
    ariaLabel: `${node.definition.title} · ${stateLabel[detail.status]}`, data: { pinned: node, status: detail.status },
  }));
  const edges: Edge[] = graph.wires.map((wire) => {
    const output = definition.nodes.find(({ instance }) => instance.id === wire.from.node)?.definition.outputs.find(({ name }) => name === wire.from.port);
    const control = output?.value.kind === "control";
    const color = control ? "var(--muted-foreground)" : "var(--primary)";
    return { id: wireKey(wire), type: "smoothstep", source: wire.from.node, sourceHandle: wire.from.port,
      target: wire.to.node, targetHandle: wire.to.port, markerEnd: { type: MarkerType.ArrowClosed, color },
      style: { stroke: color, strokeDasharray: control ? "5 4" : undefined }, ariaLabel: `${wire.from.node}.${wire.from.port} to ${wire.to.node}.${wire.to.port}` };
  });
  return <section className="pipeline-stack" aria-label="Pinned workflow execution">
    <header className="workflow-execution-links">
      <div className="mr-auto"><h2 className="text-lg font-semibold">{definition.name}</h2>
        <p>Pinned workflow revision <code className="break-all">{run.version.revision}</code></p>
        {launch && launch.origin.kind !== "editor" && <p>Agile work <code className="break-all">{launch.origin.workId}</code></p>}
        {launch?.checklistItem && <p>Checklist item <code className="break-all">{launch.checklistItem.itemId}</code>
          {" · "}Revision {launch.checklistItem.revision} · {launch.checklistItem.checked ? "Checked" : "Reopened"}</p>}
      </div>
      <Button variant="outline" type="button" onClick={() => navigation.openWorkspace("pipelines", run.id)}>Open delivery controls</Button>
    </header>
    {!!definition.subflows?.length && <label className="workflow-execution-field">Pinned subflow
      <select value={subflowPath} onChange={(event) => {
        const path = event.currentTarget.value;
        setSubflowPath(path);
        setSelected(definition.subflows?.find((entry) => entry.path === path)?.nodes[0] ?? definition.nodes[0]?.instance.id ?? "");
      }}>
        <option value="">Entire workflow</option>
        {definition.subflows.map(({ name, path }) => <option key={path} value={path}>{name} · {path}</option>)}
      </select>
    </label>}
    {subflow && <details>
      <summary>Pinned subflow outputs</summary>
      <p>Revision <code className="break-all">{subflow.version.revision}</code></p>
      <ul>{Object.entries(subflow.outputs).map(([port, endpoint]) => <li key={port}>
        {port} · <code>{endpoint.node}.{endpoint.port}</code>
      </li>)}</ul>
    </details>}
    {snapshot.units.length > 0 && <label className="workflow-execution-field">Logical unit
      <select value={unit?.unit.id ?? ""} onChange={(event) => setSelectedUnit(event.currentTarget.value)}>
        {!unit && <option value="">Select a saved unit</option>}
        {snapshot.units.map((entry) => <option key={entry.unit.id} value={entry.unit.id}>
          {entry.unit.ordinal}. {entry.unit.purpose}{entry.planRevision !== run.planRevision && " · Earlier plan"}
        </option>)}
      </select>
    </label>}
    <div className="workflow-execution-layout">
      <div className="workflow-graph-canvas" aria-label="Workflow execution graph">
        <ReactFlow<ExecutionNode, Edge> key={subflowPath} nodes={nodes} edges={edges} nodeTypes={executionNodeTypes}
          nodesDraggable={false} nodesConnectable={false} edgesReconnectable={false} deleteKeyCode={null}
          onNodeClick={(_, node) => setSelected(node.id)} colorMode={document.documentElement.dataset.appTheme === "light" ? "light" : "dark"}
          fitView fitViewOptions={{ padding: 0.15, maxZoom: 1 }} minZoom={0.08} maxZoom={2}>
          <Background gap={18} size={1} /><Controls showInteractive={false} />
        </ReactFlow>
      </div>
      <aside aria-label="Execution node evidence" className="pipeline-stack workflow-record-card">
        <label className="workflow-execution-field">Inspect execution node
          <select value={selected} onChange={(event) => setSelected(event.currentTarget.value)}>
            {stages.map(({ node, detail }) => <option key={node.instance.id} value={node.instance.id}>{node.definition.title} · {stateLabel[detail.status]}</option>)}
          </select>
        </label>
        {active && <>
          <div><h3 className="font-semibold">{active.node.definition.title}</h3><State status={active.detail.status} /></div>
          {snapshot.rolePins?.filter((pin) => pin.nodeId === active.node.instance.id && pin.nodeRevision === active.node.revision)
            .map((pin) => <PinnedRole key={`${pin.version.destination}:${pin.version.id}:${pin.version.revision}`} pin={pin} />)}
          {approval && <WorkflowApprovalInspector key={active.node.instance.id}
            scope={{ projectId: run.projectId, runId: run.id }} nodeId={active.node.instance.id} planRevision={run.planRevision} />}
          {!approval && active.detail.reason && <p>{active.detail.reason}</p>}
          {!approval && !!active.detail.missing?.length && <section aria-label="Waiting for dependencies">
            <p>Waiting for recorded upstream outputs.</p>
            <ul className="list-disc pl-4">{active.detail.missing.map((wire) => <li key={wireKey(wire)}>
              {wire.to.port} needs {wire.from.node}.{wire.from.port}
            </li>)}</ul>
          </section>}
          <WorkflowExecutionAgent snapshot={snapshot} node={active.node} unit={unit} openActivity={navigation.openActivity} />
          <WorkflowExecutionUnitChecks key={active.node.instance.id} snapshot={snapshot} node={active.node} unit={unit} />
          <WorkflowNodeEvidence key={`${active.node.instance.id}:${unit?.unit.id ?? ""}`} node={active.node} snapshot={snapshot}
            unitId={unit?.unit.id} control={controlForNode(snapshot, active.node.instance.id, unit)} navigation={navigation} />
        </>}
      </aside>
    </div>
  </section>;
}

export function WorkflowExecutionWorkspace(props: WorkspaceProps & { runId?: string }) {
  const { checkout: { projectId }, runId, openWorkspace } = props;
  const runs = useQuery({ queryKey: ["repository-delivery", projectId, "list"], queryFn: () => invoke<WorkflowRun[]>("list_repository_deliveries", { projectId }) });
  const execution = useQuery({
    queryKey: ["repository-delivery", projectId, "execution", runId], queryFn: () => invoke<WorkflowExecutionSnapshot | null>("inspect_workflow_execution", { projectId, runId }),
    enabled: Boolean(runId),
    refetchInterval: ({ state }) => state.data?.jobs.some(({ job }) => job.queuedRecovery || job.downstreamWait ||
      ["starting", "running", "waiting_for_input"].includes(job.latestAttempt?.status ?? "") || job.queueState === "queued" && !job.latestAttempt) ? 900 : false,
  });
  const loadError = execution.error ?? runs.error;
  return <section className="pipeline-stack p-4" aria-label="Workflow runs">
    <label className="workflow-execution-field">Saved workflow run
      <select value={runId ?? ""} onChange={(event) => openWorkspace("pipelines", event.currentTarget.value ? workflowRunTarget(event.currentTarget.value) : "runs")}>
        <option value="">Select a workflow run</option>
        {runId && !runs.data?.some(({ id }) => id === runId) && <option value={runId}>{runId}</option>}
        {runs.data?.map(({ id, version }) => <option key={id} value={id}>{id} · {version.revision.slice(0, 12)}</option>)}
      </select>
    </label>
    {loadError && <p role="alert">{String(loadError)}</p>}
    {runId && execution.isPending && <p role="status">Loading workflow execution…</p>}
    {execution.data === null && <p>The requested execution is unavailable in this project.</p>}
    {!runId && runs.data?.length === 0 && <p>No workflow runs are recorded for this project.</p>}
    {execution.data && <WorkflowExecutionCanvas key={execution.data.run.id} snapshot={execution.data} navigation={props} />}
  </section>;
}
