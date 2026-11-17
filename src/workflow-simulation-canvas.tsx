import { useEffect, useMemo, useState, type ReactNode } from "react";
import { MarkerType, type Edge, type Node, type NodeProps } from "@xyflow/react";
import { PipelineCanvasFrame, PipelineGraphCanvas, PipelineInspectorHeader } from "@/pipeline-layout";
import { WorkflowExecutionStateBadge, workflowExecutionStateLabel } from "@/workflow-execution-state";
import { WorkflowGraphNodeCard } from "@/workflow-graph-editor";
import { initialNodePositions, wireKey, type WorkflowEndpoint, type WorkflowLayout, type WorkflowPort, type WorkflowWire } from "@/workflow-graph-model";

type SimulationNode = { id: string; title: string; inputs: WorkflowPort[]; outputs: WorkflowPort[] };
type Signal = "selected" | "waiting" | "skipped";
type PathState = Signal | "unrecorded";
type StepState = "completed" | "failed" | "waiting" | "skipped";

export type WorkflowSimulationGraph = { nodes: SimulationNode[]; wires: WorkflowWire[]; layout?: WorkflowLayout };
export type WorkflowSimulationStep = {
  nodeId: string; state: StepState; reason?: string | null;
  inputs: { port: string; from: WorkflowEndpoint; state: Signal }[];
};

type CanvasNode = Node<{ node: SimulationNode; state?: StepState }, "simulation">;
type CanvasEdge = Edge<{ state: PathState }>;

const paths: Record<PathState, { label: string; color: string; width: number; dash?: string; opacity?: number }> = {
  selected: { label: "Selected path", color: "var(--primary)", width: 2.4 },
  waiting: { label: "Waiting", color: "var(--warning)", width: 1.8, dash: "6 4" },
  skipped: { label: "Skipped", color: "var(--muted-foreground)", width: 1.4, dash: "3 4", opacity: 0.55 },
  unrecorded: { label: "No recorded signal", color: "var(--muted-foreground)", width: 1.4, opacity: 0.55 },
};

const readTheme = (): "light" | "dark" => document.documentElement.dataset.appTheme === "light" ? "light" : "dark";

function SimulationNodeCard({ data: { node, state }, selected }: NodeProps<CanvasNode>) {
  return <WorkflowGraphNodeCard title={node.title} detail={node.id} inputs={node.inputs} outputs={node.outputs}
    selected={selected} readOnly headerContent={state ? <WorkflowExecutionStateBadge status={state} /> : undefined} />;
}

const nodeTypes = { simulation: SimulationNodeCard };

function simulationEdge(wire: WorkflowWire, destination?: WorkflowSimulationStep): CanvasEdge {
  const { from, to } = wire;
  const state = destination?.inputs.find((input) =>
    input.port === to.port && input.from.node === from.node && input.from.port === from.port)?.state ?? "unrecorded";
  const path = paths[state];
  return {
    id: wireKey(wire), source: from.node, target: to.node, sourceHandle: from.port, targetHandle: to.port,
    type: "smoothstep", data: { state },
    style: { stroke: path.color, strokeWidth: path.width, strokeDasharray: path.dash, opacity: path.opacity },
    markerEnd: { type: MarkerType.ArrowClosed, color: path.color },
    ariaLabel: `${from.node}.${from.port} to ${to.node}.${to.port} · ${path.label}`,
  };
}

export function WorkflowSimulationCanvas({ graph, steps, renderStep }: {
  graph: WorkflowSimulationGraph;
  steps: readonly WorkflowSimulationStep[];
  renderStep: (nodeId: string) => ReactNode;
}) {
  const [selected, select] = useState(graph.nodes[0]?.id);
  const [theme, setTheme] = useState(readTheme);
  const active = graph.nodes.find(({ id }) => id === selected) ?? graph.nodes[0];
  const stepByNode = useMemo(() => new Map(steps.map((step) => [step.nodeId, step])), [steps]);
  const positions = useMemo(() => initialNodePositions(graph), [graph]);
  const edges = useMemo(() => graph.wires.map((wire) => simulationEdge(wire, stepByNode.get(wire.to.node))), [graph, stepByNode]);
  const signals = new Set(edges.map((edge) => edge.data?.state));
  const nodes = graph.nodes.map<CanvasNode>((node) => {
    const state = stepByNode.get(node.id)?.state;
    return {
      id: node.id, type: "simulation", position: graph.layout?.[node.id] ?? positions[node.id]!, selected: node.id === active?.id,
      ariaLabel: `${node.title} · ${node.id}${state ? " · " + workflowExecutionStateLabel[state] : ""}`, data: { node, state },
    };
  });

  useEffect(() => {
    const observer = new MutationObserver(() => setTheme(readTheme()));
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-app-theme"] });
    return () => observer.disconnect();
  }, []);

  if (nodes.length === 0) return <p className="pipeline-note">This revision has no graph nodes to inspect.</p>;

  return <div className="pipeline-stack">
    {signals.size > 0 && <ul className="flex flex-wrap gap-3 text-xs text-muted-foreground" aria-label="Simulation path states">
      {Array.from(signals).map((state) => state && <li key={state} className="flex items-center gap-1.5">
        <span className="inline-block w-4 border-b-2" aria-hidden="true"
          style={{ borderColor: paths[state].color, borderBottomStyle: paths[state].dash ? "dashed" : "solid" }} />
        {paths[state].label}
      </li>)}
    </ul>}
    <PipelineCanvasFrame className="rounded-md border border-border"
      canvasLabel="Simulated workflow graph" inspectorLabel="Simulation node evidence"
      canvas={<div className="absolute inset-0">
        <PipelineGraphCanvas<CanvasNode, CanvasEdge> nodes={nodes} edges={edges} nodeTypes={nodeTypes} colorMode={theme}
          fitView fitViewOptions={{ padding: 0.15, maxZoom: 1 }} minZoom={0.08} maxZoom={2}
          nodesDraggable={false} nodesConnectable={false} edgesReconnectable={false} deleteKeyCode={null}
          onNodeClick={(_, node) => select(node.id)}
          onNodesChange={(changes) => {
            for (const change of changes) if (change.type === "select" && change.selected) select(change.id);
          }} />
      </div>}
      inspector={<>
        <PipelineInspectorHeader detail="Simulation details" title={active?.title ?? "No stage selected"}>
          <label className="workflow-execution-field">
            Inspect simulated stage
            <select value={active?.id} onChange={(event) => select(event.currentTarget.value)}>
              {nodes.map((node) => <option key={node.id} value={node.id}>{node.ariaLabel}</option>)}
            </select>
          </label>
        </PipelineInspectorHeader>
        {active && (stepByNode.has(active.id) ? renderStep(active.id) : <p className="pipeline-note">No recorded step for this stage.</p>)}
      </>}
    />
  </div>;
}
