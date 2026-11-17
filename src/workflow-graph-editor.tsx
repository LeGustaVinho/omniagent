import { useEffect, useMemo, useRef, useState, type Dispatch, type MouseEvent, type ReactNode, type SetStateAction } from "react";
import { Handle, MarkerType, Position, useUpdateNodeInternals,
  type Connection, type Edge, type Node, type NodeChange, type NodeProps, type ReactFlowInstance } from "@xyflow/react";
import { Plus, Redo2, Trash2, Undo2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { PositionedContextActions, type MenuGroups } from "@/components/ui/context-actions";
import { PipelineNodeCatalog, workflowRegistryGroups } from "@/pipeline-catalog";
import { PipelineCanvasFrame, PipelineGraphCanvas, PipelineInspectorHeader, PipelineToolbar } from "@/pipeline-layout";
import { WorkflowAgileFields } from "@/workflow-agile-editor";
import { CommandNodeConfiguration } from "@/workflow-command";
import { DecisionNodeFields, type WorkflowDecisionConfiguration } from "@/workflow-decision-editor";
import { WorkflowMcpNodeConfiguration } from "@/workflow-mcp-tools";
import { BuildListFields, BuildTextFields, CompareModelsFields, ConditionFields, FilterMapListFields, LoopFields, SetFieldsFields, StartInputFields } from "@/workflow-node-fields";
import { AgentRoleFields } from "@/workflow-stage-model";
import type { StageGoalRoleRevision } from "@/workflow-stage-goal";
import type { ReusableReference } from "@/workflow-reusable";
import {
  changedPorts, configurationProblem, connectionProblem, defaultConfiguration, incompatibility,
  initialNodePositions, isPresetEligible, isStartNode, outputPort, portType, wireKey,
  type WorkflowCanvasEdit, type WorkflowDefinition, type WorkflowDefinitions, type WorkflowGraph, type WorkflowGraphNode,
  type WorkflowEndpoint, type WorkflowLayout, type WorkflowNodeConfiguration, type WorkflowPort, type WorkflowReuseSelection, type WorkflowWire,
} from "@/workflow-graph-model";
import "./workflow-graph-editor.css";

type WorkflowNodeBinding = {
  reference: ReusableReference;
  preset?: {
    base: WorkflowNodeConfiguration;
    fields: string[];
    stored: WorkflowNodeConfiguration;
    shown: WorkflowNodeConfiguration;
  };
};
export type WorkflowEditorState = WorkflowCanvasEdit & {
  graph: WorkflowGraph;
  nodeBindings?: Record<string, WorkflowNodeBinding>;
};

export function editorDefinitions(state: WorkflowEditorState, pinned: WorkflowDefinitions, catalog: WorkflowDefinition[],
  subflowDefinition?: (nodeId: string) => WorkflowDefinition | undefined): WorkflowDefinitions {
  const available = new Map(catalog.map((entry) => [entry.kind, entry]));
  return Object.fromEntries(state.graph.nodes.flatMap((node) => {
    const copy = state.definitionCopies && Object.hasOwn(state.definitionCopies, node.id) ? state.definitionCopies[node.id] : undefined;
    const stored = Object.hasOwn(pinned, node.id) ? pinned[node.id] : copy && pinned[copy.sourceId];
    const current = available.get(node.kind);
    let definition = stored?.kind === node.kind && state.updateDefinitions[node.id] === undefined ? stored : current;
    if (!definition) return [];
    const bound = subflowDefinition?.(node.id);
    if (bound) definition = scopedSubflowPorts(node, definition, bound);
    if (node.configuration.kind === "subflow_entry" && Array.isArray(node.configuration.outputs)) {
      definition = { ...definition, configuration: "subflow_entry", outputs: node.configuration.outputs };
    }
    if (node.kind === "find_work") {
      const query = node.configuration.query as { result?: unknown } | undefined;
      if (query?.result !== "single") return [[node.id, {
        ...definition, outputs: definition.outputs.filter(({ name }) => name !== "task" && name !== "target"),
      }]];
    }
    return [[node.id, definition]];
  }));
}

function scopedSubflowPorts(node: WorkflowGraphNode, definition: WorkflowDefinition, bound: WorkflowDefinition): WorkflowDefinition {
  if (node.kind === "subflow") return { ...definition, inputs: bound.inputs, outputs: bound.outputs };
  if (node.kind === "repeat_until") return { ...definition, inputs: bound.inputs,
    outputs: ["completed", "failed"].map((name) => ({ name, value: { kind: "control" } })) };
  if (node.kind !== "for_each") return definition;
  const { readyInput, itemInput } = node.configuration;
  const standard = definition.inputs.filter(({ name }) => name === "ready" || name === "items");
  const additional = bound.inputs.filter(({ name }) => name !== readyInput && name !== itemInput && !standard.some((port) => port.name === name));
  return { ...definition, inputs: [...standard, ...additional] };
}

type PortSide = "input" | "output";
type PortActions = {
  nodeId: string;
  activeOutput?: WorkflowEndpoint;
  activate: (side: PortSide, name: string) => void;
};
type CanvasNode = Node<{ definition?: WorkflowDefinition; problem: boolean; readOnly: boolean; portActions?: PortActions }, "workflow">;

function WorkflowCard({ id, data, selected }: NodeProps<CanvasNode>) {
  const update = useUpdateNodeInternals();
  const definition = data.definition;
  useEffect(() => { update(id); }, [definition, id, update]);
  return <WorkflowGraphNodeCard title={definition?.title ?? "Unavailable node"} detail={id} selected={selected}
    problem={data.problem} readOnly={data.readOnly} inputs={definition?.inputs ?? []} outputs={definition?.outputs ?? []} portActions={data.portActions} />;
}

export function WorkflowGraphNodeCard({ title, detail, inputs = [], outputs = [], selected, problem, readOnly, headerContent, portActions, compact, handles }: {
  title: string; detail?: string; inputs?: WorkflowPort[]; outputs?: WorkflowPort[]; selected?: boolean;
  problem?: boolean; readOnly?: boolean; headerContent?: ReactNode; portActions?: PortActions;
  compact?: boolean; handles?: ReactNode;
}) {
  return <article className="workflow-node-card" data-compact={compact || undefined} data-selected={selected || undefined} data-problem={problem || undefined}>
    {handles}
    <header><strong>{title}</strong>{detail && <small>{detail}</small>}{headerContent}</header>
    {(inputs.length > 0 || outputs.length > 0) && <div className="workflow-node-ports">
      <WorkflowPortColumn ports={inputs} side="input" readOnly={readOnly} actions={portActions} />
      <WorkflowPortColumn ports={outputs} side="output" readOnly={readOnly} actions={portActions} />
    </div>}
  </article>;
}

export function WorkflowPortColumn({ ports, side, readOnly = false, actions }: {
  ports: WorkflowPort[]; side: PortSide; readOnly?: boolean; actions?: PortActions;
}) {
  const input = side === "input";
  return <ul className="workflow-port-column" aria-label={input ? "Input ports" : "Output ports"}>
    {ports.map(({ name, value }) => <li key={name} data-side={side} data-port={value.kind} title={`${name} · ${portType(value)}`}>
      <Handle id={name} type={input ? "target" : "source"} position={input ? Position.Left : Position.Right} isConnectable={!readOnly}
        aria-label={`${side} ${name} · ${portType(value)}`} />
      {actions && !readOnly ? <button type="button" className="workflow-port-action nodrag nopan"
        aria-label={`Connect ${input ? "to" : "from"} ${actions.nodeId}.${name}`}
        aria-pressed={input ? undefined : actions.activeOutput?.node === actions.nodeId && actions.activeOutput?.port === name}
        onClick={(event) => { event.stopPropagation(); actions.activate(side, name); }}
        onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") event.stopPropagation(); }}>
        <span>{name}</span><small>{portType(value)}</small>
      </button> : <><span>{name}</span><small>{portType(value)}</small></>}
    </li>)}
  </ul>;
}

const nodeTypes = { workflow: WorkflowCard };

function connectionWire(connection: Connection): WorkflowWire | undefined {
  return connection.sourceHandle && connection.targetHandle ? {
    from: { node: connection.source, port: connection.sourceHandle },
    to: { node: connection.target, port: connection.targetHandle },
  } : undefined;
}

const readTheme = (): "light" | "dark" => document.documentElement.dataset.appTheme === "light" ? "light" : "dark";

type PendingConnection = { from: WorkflowEndpoint; replacing?: string };
type EditorHistory = { past: WorkflowEditorState[]; future: WorkflowEditorState[] };
type CanvasTarget = { kind: "pane" } | { kind: "node" | "edge"; id: string };
type CanvasMenu = { target: CanvasTarget; point: { x: number; y: number }; origin?: HTMLElement | SVGElement };
type GraphSelection = {
  nodes: { node: WorkflowGraphNode; position: { x: number; y: number }; binding?: WorkflowNodeBinding;
    definition?: NonNullable<WorkflowCanvasEdit["definitionCopies"]>[string] }[];
  wires: WorkflowWire[];
};
const historyLimit = 32;
const canvasLimit = 1_000_000;
const duplicateOffset = 36;

function pasteOffset(points: number[], anchor?: number) {
  const min = Math.min(...points);
  const max = Math.max(...points);
  const shift = anchor === undefined ? (max + duplicateOffset > canvasLimit ? -duplicateOffset : duplicateOffset) : anchor - min;
  return Math.max(-canvasLimit - min, Math.min(canvasLimit - max, shift));
}

const bindingFingerprint = (bindings: WorkflowEditorState["nodeBindings"]) =>
  JSON.stringify(Object.entries(bindings ?? {}).sort(([a], [b]) => a.localeCompare(b)).map(([id, binding]) => [id, binding.reference]));

export function WorkflowGraphEditor({ projectId, checkoutId, state, setState, definitions, catalog, problems, disabled,
  allowDefinitionUpdates = true, inspectReference, editablePresetFields, goalRoleForNode, subflowDefinitionForNode,
  onDerive, deriveDisabled }: {
  projectId: string;
  checkoutId?: string;
  state: WorkflowEditorState;
  setState: Dispatch<SetStateAction<WorkflowEditorState>>;
  definitions: WorkflowDefinitions;
  catalog: WorkflowDefinition[];
  problems: string[];
  disabled: boolean;
  allowDefinitionUpdates?: boolean | ((nodeId: string) => boolean);
  inspectReference?: (nodeId: string) => ReactNode;
  editablePresetFields?: Record<string, string[]>;
  goalRoleForNode?: (nodeId: string) => { version?: StageGoalRoleRevision; pending?: boolean } | undefined;
  subflowDefinitionForNode?: (nodeId: string) => WorkflowDefinition | undefined;
  onDerive?: (selection: WorkflowReuseSelection) => void;
  deriveDisabled?: boolean;
}) {
  const { graph, layout } = state;
  const [theme, setTheme] = useState(readTheme);
  const [selected, select] = useState(graph.nodes[0]?.id ?? "");
  const [selectedNodes, setSelectedNodes] = useState<string[]>([]);
  const issuedIds = useRef(new Set(graph.nodes.map(({ id }) => id)));
  const [catalogOpen, showCatalog] = useState(false);
  const [catalogPosition, setCatalogPosition] = useState<{ x: number; y: number }>();
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [pending, setPending] = useState<PendingConnection>();
  const [history, setHistory] = useState<EditorHistory>({ past: [], future: [] });
  const currentBindings = bindingFingerprint(state.nodeBindings);
  const expectedBindings = useRef(currentBindings);
  const [flow, setFlow] = useState<ReactFlowInstance<CanvasNode, Edge>>();
  const [menu, setMenu] = useState<CanvasMenu>();
  const [clipboard, setClipboard] = useState<GraphSelection>();
  const canvas = useRef<HTMLDivElement>(null);
  const dragging = useRef<WorkflowLayout>({});
  const [previewPositions, setPreviewPositions] = useState<WorkflowLayout>({});
  const node = graph.nodes.find(({ id }) => id === selected);
  const definition = node && definitions[node.id];
  const canUpdateDefinition = typeof allowDefinitionUpdates === "function"
    ? !!node && allowDefinitionUpdates(node.id) : allowDefinitionUpdates;
  const available = canUpdateDefinition && definition && catalog.find(({ kind, revision }) => kind === definition.kind && revision > definition.revision);
  const positions = useMemo(() => initialNodePositions(graph), [graph]);
  const edges = useMemo<Edge[]>(() => graph.wires.map((wire) => {
    const invalid = incompatibility(definitions, wire);
    const output = outputPort(definitions, wire.from);
    const control = output?.value.kind === "control";
    const color = invalid ? "var(--destructive)" : control ? "var(--muted-foreground)" : "var(--primary)";
    return {
      id: wireKey(wire), source: wire.from.node, target: wire.to.node, sourceHandle: wire.from.port, targetHandle: wire.to.port,
      type: "smoothstep", style: { stroke: color, strokeDasharray: control ? "5 4" : undefined, strokeWidth: 1.6 },
      markerEnd: { type: MarkerType.ArrowClosed, color },
      ariaLabel: `${wire.from.node}.${wire.from.port} to ${wire.to.node}.${wire.to.port}${invalid ? ` · ${invalid}` : ""}`,
    };
  }), [definitions, graph.wires]);

  useEffect(() => {
    const observer = new MutationObserver(() => setTheme(readTheme()));
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-app-theme"] });
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (currentBindings === expectedBindings.current) return;
    expectedBindings.current = currentBindings;
    setHistory({ past: [], future: [] });
  }, [currentBindings]);

  function updateState(next: WorkflowEditorState) {
    expectedBindings.current = bindingFingerprint(next.nodeBindings);
    setState(next);
  }

  function apply(next: WorkflowEditorState) {
    if (disabled || next === state) return;
    if (next.definitionCopies && !Object.keys(next.definitionCopies).length) {
      const { definitionCopies: _empty, ...withoutCopies } = next;
      next = withoutCopies;
    }
    setHistory((previous) => ({ past: [...previous.past, state].slice(-historyLimit), future: [] }));
    updateState(next);
  }

  function restore(direction: "undo" | "redo") {
    if (disabled) return;
    const { past, future } = history;
    const previous = direction === "undo" ? past.at(-1) : future.at(-1);
    if (!previous) return;
    setHistory(direction === "undo" ? { past: past.slice(0, -1), future: [...future, state] }
      : { past: [...past, state], future: future.slice(0, -1) });
    setPending(undefined);
    setConnectionError(null);
    if (!previous.graph.nodes.some(({ id }) => id === selected)) select(previous.graph.nodes[0]?.id ?? "");
    updateState(previous);
  }

  function patchGraph(patch: (graph: WorkflowGraph) => WorkflowGraph) {
    apply({ ...state, graph: patch(graph) });
  }

  function allocateId(stem: string) {
    let id = stem;
    for (let ordinal = 2; issuedIds.current.has(id) || graph.nodes.some((node) => node.id === id); ordinal += 1) id = `${stem}-${ordinal}`;
    issuedIds.current.add(id);
    return id;
  }

  function add(definition: WorkflowDefinition) {
    const id = allocateId(definition.kind.replaceAll("_", "-"));
    apply({ ...state, layout: catalogPosition ? { ...layout, [id]: catalogPosition } : layout,
      graph: { ...graph, nodes: [...graph.nodes, { id, kind: definition.kind, configuration: defaultConfiguration(definition.configuration) }] } });
    setSelectedNodes([id]);
    select(id);
    showCatalog(false);
    setCatalogPosition(undefined);
  }

  function capture(nodes: WorkflowGraphNode[]): GraphSelection {
    const ids = new Set(nodes.map(({ id }) => id));
    return {
      nodes: nodes.map((node) => {
        const { id, kind } = node;
        const position = layout[id] ?? positions[id]!;
        const binding = state.nodeBindings && Object.hasOwn(state.nodeBindings, id) ? state.nodeBindings[id] : undefined;
        const current = catalog.find((definition) => definition.kind === kind);
        const version = definitions[id];
        const retainsNativePin = !binding?.preset && !["subflow", "repeat_until", "for_each"].includes(kind)
          && current && version && version.revision !== current.revision;
        const definition = retainsNativePin ? state.definitionCopies?.[id] ?? { sourceId: id, revision: version.revision } : undefined;
        return { node, position, binding, definition };
      }),
      wires: graph.wires.filter(({ from, to }) => ids.has(from.node) && ids.has(to.node)),
    };
  }

  function insert(selection: GraphSelection, anchor?: { x: number; y: number }) {
    if (disabled || !selection.nodes.length) return;
    const ids = new Map(selection.nodes.map(({ node }) => [node.id, allocateId(node.id)]));
    const nextLayout = { ...layout };
    const definitionCopies = { ...state.definitionCopies };
    const nodeBindings = { ...state.nodeBindings };
    const dx = pasteOffset(selection.nodes.map(({ position }) => position.x), anchor?.x);
    const dy = pasteOffset(selection.nodes.map(({ position }) => position.y), anchor?.y);
    const copies = selection.nodes.map(({ node, position, binding, definition }) => {
      const id = ids.get(node.id)!;
      nextLayout[id] = { x: position.x + dx, y: position.y + dy };
      if (binding) nodeBindings[id] = binding;
      if (definition) definitionCopies[id] = definition;
      return { ...node, id };
    });
    const wires = selection.wires.map(({ from, to }) => ({ from: { ...from, node: ids.get(from.node)! }, to: { ...to, node: ids.get(to.node)! } }));
    apply({ ...state, layout: nextLayout, ...(Object.keys(definitionCopies).length && { definitionCopies }),
      ...(state.nodeBindings && { nodeBindings }),
      graph: { ...graph, nodes: [...graph.nodes, ...copies], wires: [...graph.wires, ...wires] } });
    setSelectedNodes(copies.map(({ id }) => id));
    select(copies[0]!.id);
  }

  function changeNodes(changes: NodeChange<CanvasNode>[], removedWireIds: string[] = []) {
    const selectionChanges = changes.filter((change) => change.type === "select" || change.type === "remove");
    if (selectionChanges.length) setSelectedNodes((previous) => {
      const next = new Set(previous);
      for (const change of selectionChanges) {
        if (change.type === "select" && change.selected) next.add(change.id);
        else next.delete(change.id);
      }
      return [...next];
    });
    const nextLayout = { ...layout };
    const nextDefinitions = { ...state.updateDefinitions };
    const definitionCopies = { ...state.definitionCopies };
    const nodeBindings = { ...state.nodeBindings };
    const removed = new Set<string>();
    let layoutChanged = false;
    let previewChanged = false;
    for (const change of changes) {
      if (change.type === "select" && change.selected) select(change.id);
      if (disabled) continue;
      if (change.type === "remove") {
        removed.add(change.id);
        delete nextLayout[change.id];
        delete nextDefinitions[change.id];
        delete definitionCopies[change.id];
        delete nodeBindings[change.id];
        if (change.id === selected) select("");
        if (pending?.from.node === change.id) setPending(undefined);
      } else if (change.type === "position") {
        if (change.dragging && change.position) {
          dragging.current[change.id] = change.position;
          previewChanged = true;
        } else if (!change.dragging) {
          const position = change.position ?? dragging.current[change.id];
          if (position && (position.x !== layout[change.id]?.x || position.y !== layout[change.id]?.y)) {
            nextLayout[change.id] = position;
            layoutChanged = true;
          }
        }
      }
      if ((change.type === "remove" || (change.type === "position" && !change.dragging)) && dragging.current[change.id]) {
        delete dragging.current[change.id];
        previewChanged = true;
      }
    }
    if (previewChanged) setPreviewPositions({ ...dragging.current });
    if (!removed.size && !layoutChanged && !removedWireIds.length) return;
    const removedWires = new Set(removedWireIds);
    if (pending?.replacing && removedWires.has(pending.replacing)) setPending(undefined);
    apply({ ...state, layout: nextLayout, updateDefinitions: nextDefinitions,
      ...(state.definitionCopies && { definitionCopies }), ...(state.nodeBindings && { nodeBindings }),
      graph: removed.size || removedWires.size ? { ...graph,
      nodes: graph.nodes.filter(({ id }) => !removed.has(id)),
      wires: graph.wires.filter((wire) => !removed.has(wire.from.node) && !removed.has(wire.to.node) && !removedWires.has(wireKey(wire))),
    } : graph });
  }

  function connect(wire: WorkflowWire, replacing?: string) {
    if (disabled) return;
    const wires = replacing ? graph.wires.filter((item) => wireKey(item) !== replacing) : graph.wires;
    if (replacing && wires.length === graph.wires.length) {
      setPending(undefined);
      setConnectionError("This connection is no longer available.");
      return;
    }
    const issue = connectionProblem({ ...graph, wires }, definitions, wire);
    setConnectionError(issue);
    if (issue) return;
    setPending(undefined);
    if (replacing === wireKey(wire)) return;
    patchGraph((previous) => ({ ...previous, wires: replacing
      ? previous.wires.map((item) => wireKey(item) === replacing ? wire : item) : [...previous.wires, wire] }));
  }

  function activatePort(nodeId: string, side: PortSide, port: string) {
    if (disabled) return;
    const endpoint = { node: nodeId, port };
    select(nodeId);
    if (side === "output") {
      setConnectionError(null);
      setPending({ from: endpoint, replacing: pending?.replacing });
    } else if (pending) {
      connect({ from: pending.from, to: endpoint }, pending.replacing);
    } else {
      setConnectionError("Select an output port first.");
    }
  }

  function configure(id: string, configuration: WorkflowNodeConfiguration) {
    patchGraph((previous) => ({ ...previous, nodes: previous.nodes.map((node) => node.id === id ? { ...node, configuration } : node) }));
  }

  function updateVersion(node: WorkflowGraphNode, version: WorkflowDefinition) {
    const definitionCopies = { ...state.definitionCopies };
    const updateDefinitions = { ...state.updateDefinitions };
    if (Object.hasOwn(definitionCopies, node.id)) delete definitionCopies[node.id];
    else updateDefinitions[node.id] = version.revision;
    apply({ ...state, updateDefinitions, ...(state.definitionCopies && { definitionCopies }),
      graph: { ...graph, nodes: graph.nodes.map((item) => item.id !== node.id || item.configuration.kind === version.configuration
        ? item : { ...item, configuration: defaultConfiguration(version.configuration) }) } });
  }

  function reconnect(wire: WorkflowWire) {
    if (disabled) return;
    setConnectionError(null);
    setPending({ from: wire.from, replacing: wireKey(wire) });
  }

  function openMenu(event: MouseEvent | globalThis.MouseEvent, target: CanvasTarget) {
    event.preventDefault();
    event.stopPropagation();
    const origin = event.currentTarget;
    setMenu({ target, point: { x: event.clientX, y: event.clientY },
      origin: origin instanceof HTMLElement || origin instanceof SVGElement ? origin : undefined });
  }

  function copy(text: string) {
    setConnectionError(null);
    void Promise.resolve().then(() => navigator.clipboard.writeText(text))
      .catch(() => setConnectionError("Could not copy to the clipboard."));
  }

  function menuGroups(target: CanvasTarget): { label: string; groups: MenuGroups } | undefined {
    if (target.kind === "pane") return { label: "Pipeline canvas", groups: [[
      { label: "Add node…", disabled, run: () => {
        setCatalogPosition(menu && flow ? flow.screenToFlowPosition(menu.point) : undefined);
        showCatalog(true);
      } },
      { label: "Fit graph to view", disabled: !flow, run: () => { void flow?.fitView({ padding: 0.15, maxZoom: 1 }); } },
      ...(clipboard ? [{ label: clipboard.nodes.length === 1 ? "Paste node here" : `Paste ${clipboard.nodes.length} nodes here`, disabled,
        run: () => insert(clipboard, menu && flow ? flow.screenToFlowPosition(menu.point) : undefined) }] : []),
      { label: "Copy graph JSON", run: () => copy(JSON.stringify(graph, null, 2)) },
    ], [
      { label: "Undo graph edit", disabled: disabled || !history.past.length, run: () => restore("undo") },
      { label: "Redo graph edit", disabled: disabled || !history.future.length, run: () => restore("redo") },
      ...(pending ? [{ label: "Cancel connection", run: () => { setPending(undefined); setConnectionError(null); } }] : []),
    ]] };
    if (target.kind === "node") {
      const clicked = graph.nodes.find(({ id }) => id === target.id);
      if (!clicked) return;
      const scoped = selectedNodes.includes(clicked.id) ? graph.nodes.filter(({ id }) => selectedNodes.includes(id)) : [clicked];
      const subflowNodes = scoped.filter(({ kind }) => !isStartNode(kind)).map(({ id }) => id);
      const duplicable = scoped.every(({ id, kind }) => !isStartNode(kind) && definitions[id] !== undefined);
      return { label: clicked.id, groups: [[
        { label: "Inspect node", run: () => { select(clicked.id); setConnectionError(null); } },
        { label: "Copy node ID", run: () => copy(clicked.id) },
        { label: "Copy node configuration", run: () => copy(JSON.stringify(clicked.configuration, null, 2)) },
      ], [
        ...(onDerive && isPresetEligible(clicked) && !editablePresetFields?.[clicked.id] ? [{ label: "Save node as preset…", disabled: deriveDisabled,
          run: () => onDerive({ type: "preset", nodeId: clicked.id }) }] : []),
        ...(onDerive && subflowNodes.length ? [{ label: subflowNodes.length > 1
          ? `Extract ${subflowNodes.length} selected nodes as subflow…` : "Extract node as subflow…", disabled: deriveDisabled,
          run: () => onDerive({ type: "subflow", nodeIds: subflowNodes }) }] : []),
      ], disabled ? [] : [
        ...(duplicable ? [
          { label: scoped.length > 1 ? `Duplicate ${scoped.length} selected nodes` : "Duplicate node", run: () => insert(capture(scoped)) },
          { label: scoped.length > 1 ? `Copy ${scoped.length} selected nodes` : "Copy node", run: () => setClipboard(capture(scoped)) },
        ] : []),
        ...(layout[clicked.id] ? [{ label: "Reset node position", run: () => {
          const nextLayout = { ...layout };
          delete nextLayout[clicked.id];
          apply({ ...state, layout: nextLayout });
        } }] : []),
        { label: "Remove node", destructive: true, run: () => changeNodes([{ type: "remove", id: clicked.id }]) },
      ]] };
    }
    const wire = graph.wires.find((candidate) => wireKey(candidate) === target.id);
    if (!wire) return;
    const details = `${wire.from.node}.${wire.from.port} → ${wire.to.node}.${wire.to.port}`;
    return { label: details, groups: [[
      { label: "Inspect source node", run: () => select(wire.from.node) },
      { label: "Inspect destination node", run: () => select(wire.to.node) },
      { label: "Copy connection", run: () => copy(details) },
    ], disabled ? [] : [
      { label: "Reconnect destination…", run: () => reconnect(wire) },
      { label: "Remove connection", destructive: true, run: () => changeNodes([], [target.id]) },
    ]] };
  }

  const context = menu && menuGroups(menu.target);

  const canvasNodes: CanvasNode[] = graph.nodes.map((item) => {
    const defined = definitions[item.id];
    return { id: item.id, type: "workflow", position: previewPositions[item.id] ?? layout[item.id] ?? positions[item.id]!, selected: selectedNodes.includes(item.id),
      ariaLabel: `${defined?.title ?? "Node"} ${item.id}`, data: { definition: defined, readOnly: disabled,
        problem: !defined || Boolean(configurationProblem(item.configuration, defined)),
        portActions: { nodeId: item.id, activeOutput: pending?.from, activate: (side, name) => activatePort(item.id, side, name) },
      } };
  });
  const entryKind = catalog.some(({ kind }) => kind === "typed_start") && !graph.nodes.some(({ kind }) => kind === "start") ? "typed_start" : "start";
  const groups = workflowRegistryGroups(catalog.filter(({ kind }) => !isStartNode(kind) || kind === entryKind), (entry) => {
    const alreadyPresent = isStartNode(entry.kind) && graph.nodes.some(({ kind }) => isStartNode(kind));
    return { action: alreadyPresent || disabled ? undefined : () => add(entry),
      unavailable: alreadyPresent ? "Already on the canvas" : undefined };
  });

  return <section aria-label="Pipeline graph editor" className="workflow-graph" onKeyDown={(event) => {
    if (event.key === "Escape" && pending) { setPending(undefined); setConnectionError(null); }
  }}>
    <PipelineToolbar>
      <label className="pipeline-inline-field workflow-graph-name">Pipeline name<input value={graph.name} disabled={disabled} onChange={(event) => {
        const name = event.currentTarget.value;
        patchGraph((previous) => ({ ...previous, name }));
      }} /></label>
      <Button type="button" variant="outline" disabled={disabled} aria-expanded={catalogOpen} onClick={() => {
        setCatalogPosition(undefined);
        showCatalog(!catalogOpen);
      }}>
        <Plus size={15} /> Add node
      </Button>
      <div className="workflow-history" aria-label="Graph edit history" role="group">
        <Button type="button" variant="ghost" aria-label="Undo graph edit" title="Undo graph edit" disabled={disabled || !history.past.length}
          onClick={() => restore("undo")}><Undo2 size={16} /></Button>
        <Button type="button" variant="ghost" aria-label="Redo graph edit" title="Redo graph edit" disabled={disabled || !history.future.length}
          onClick={() => restore("redo")}><Redo2 size={16} /></Button>
      </div>
    </PipelineToolbar>
    {connectionError && <p className="workflow-graph-error" role="alert">{connectionError}</p>}
    {pending && <div className="workflow-connection-status">
      <p role="status">Select an input port for {pending.from.node}.{pending.from.port}.</p>
      <Button type="button" variant="ghost" onClick={() => { setPending(undefined); setConnectionError(null); }}>Cancel connection</Button>
    </div>}
    {problems.length > 0 && <details className="workflow-graph-error"><summary>Draft diagnostics ({problems.length})</summary>
      <ul>{problems.map((issue, i) => <li key={`${issue}:${i}`}>{issue}</li>)}</ul>
    </details>}
    <PipelineCanvasFrame className="workflow-graph-frame" canvasLabel="Pipeline graph canvas" inspectorLabel="Node configuration"
      inspectorClassName="workflow-graph-inspector pipeline-form pipeline-stack" palette={catalogOpen && !disabled ?
        <section aria-label="Node catalog"><PipelineNodeCatalog groups={groups} searchLabel="Search node catalog" compact /></section> : undefined}
      canvas={<div ref={canvas} tabIndex={0} aria-label="Workflow canvas actions" className="workflow-graph-canvas" onKeyDown={(event) => {
        if (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
        if (!(event.target instanceof Element)) return;
        const anchor = event.target.closest(".react-flow__node, .react-flow__edge");
        const id = anchor?.getAttribute("data-id");
        const origin = anchor instanceof HTMLElement || anchor instanceof SVGElement ? anchor : event.currentTarget;
        const rect = origin.getBoundingClientRect();
        event.preventDefault();
        event.stopPropagation();
        setMenu({ target: id ? { kind: anchor?.classList.contains("react-flow__edge") ? "edge" : "node", id } : { kind: "pane" },
          point: { x: rect.left + 8, y: rect.top + 8 }, origin });
      }}>
        <PipelineGraphCanvas<CanvasNode, Edge> interactive={!disabled} nodes={canvasNodes} edges={edges} nodeTypes={nodeTypes} colorMode={theme}
          fitView fitViewOptions={{ padding: 0.15, maxZoom: 1 }} minZoom={0.08} maxZoom={2}
          selectionKeyCode="Shift" multiSelectionKeyCode={["Control", "Meta"]}
          nodesDraggable={!disabled} nodesConnectable={!disabled} edgesReconnectable={!disabled}
          deleteKeyCode={disabled ? null : ["Backspace", "Delete"]}
          onNodesChange={changeNodes}
          onBeforeDelete={async ({ nodes, edges }) => {
            if (!disabled) changeNodes(nodes.map(({ id }) => ({ type: "remove", id })), edges.map(({ id }) => id));
            return false;
          }}
          onEdgesChange={(changes) => {
            const removed = new Set(changes.flatMap((change) => change.type === "remove" ? [change.id] : []));
            if (!disabled && removed.size) patchGraph((previous) => ({ ...previous, wires: previous.wires.filter((wire) => !removed.has(wireKey(wire))) }));
          }}
          onNodeClick={(_, selectedNode) => { select(selectedNode.id); setConnectionError(null); }}
          onInit={setFlow}
          onPaneContextMenu={(event) => openMenu(event, { kind: "pane" })}
          onNodeContextMenu={(event, clicked) => openMenu(event, { kind: "node", id: clicked.id })}
          onEdgeContextMenu={(event, clicked) => openMenu(event, { kind: "edge", id: clicked.id })}
          onConnect={(candidate) => { const wire = connectionWire(candidate); if (wire) connect(wire); }}
          onReconnect={(previous, candidate) => { const wire = connectionWire(candidate); if (wire) connect(wire, previous.id); }}
        />
      </div>}
      inspector={<>
        <PipelineInspectorHeader detail="Configuration" title={definition?.title ?? "Select a node"} />
        <label>Inspect node<select value={node?.id ?? ""} onChange={(event) => { select(event.currentTarget.value); setConnectionError(null); }}>
          <option value="">Choose a node</option>
          {graph.nodes.map(({ id }) => <option key={id} value={id}>{definitions[id]?.title ?? "Unavailable"} · {id}</option>)}
        </select></label>
        {node && definition && <>
          <p className="text-sm text-muted-foreground">Version {definition.revision} · {definition.effect.replaceAll("_", " ")}</p>
          {inspectReference?.(node.id)}
          <NodeFields key={node.id} projectId={projectId} checkoutId={checkoutId} node={node} definition={definition}
            presetFields={editablePresetFields?.[node.id]} goalRole={goalRoleForNode?.(node.id)}
            subflow={subflowDefinitionForNode?.(node.id)}
            disabled={disabled} onChange={(configuration) => configure(node.id, configuration)} />
          {available && <VersionReview node={node} previous={definition} next={available} wires={graph.wires}
            disabled={disabled} apply={() => updateVersion(node, available)} />}
          {definition.inputs.length > 0 && <ConnectInput key={`${node.id}:${definition.revision}`} graph={graph} node={node}
            definitions={definitions} definition={definition} disabled={disabled} connect={connect} />}
          <NodeConnections node={node} wires={graph.wires} definitions={definitions} disabled={disabled} reconnect={reconnect}
            remove={(wire) => changeNodes([], [wireKey(wire)])} />
        </>}
        {node && <Button type="button" variant="outline" disabled={disabled} onClick={() => changeNodes([{ id: node.id, type: "remove" }])}>
          <Trash2 size={14} /> Remove node
        </Button>}
      </>} />
    {menu && context && <PositionedContextActions {...context} point={menu.point} onClose={() => setMenu(undefined)}
      restoreFocus={() => (menu.origin?.isConnected && menu.origin.tabIndex >= 0 ? menu.origin : canvas.current)?.focus()} />}
  </section>;
}

function NodeFields({ projectId, checkoutId, node, definition, disabled, presetFields, goalRole, subflow, onChange }: {
  projectId: string; checkoutId?: string; node: WorkflowGraphNode; definition: WorkflowDefinition; disabled: boolean; onChange: (configuration: WorkflowNodeConfiguration) => void;
  presetFields?: string[];
  goalRole?: { version?: StageGoalRoleRevision; pending?: boolean };
  subflow?: WorkflowDefinition;
}) {
  const { configuration } = node;
  const allow = (field: string) => !presetFields || presetFields.includes(field);
  const locked = disabled || presetFields?.length === 0;
  switch (definition.configuration) {
    case "none": return null;
    case "agent_role": return <AgentRoleFields projectId={projectId} checkoutId={checkoutId}
      mode={definition.kind === "plan" ? "plan" : definition.effect === "write_repository" ? "approved_write" : "read_only"}
      nodeId={node.id} nodeKind={node.kind} goalRole={goalRole}
      configuration={configuration} onChange={onChange} disabled={locked || !allow("role")} fixedModel={Boolean(presetFields)} />;
    case "required_checks": return <label>Required checks<textarea rows={3} disabled={locked || !allow("names")}
      value={Array.isArray(configuration.names) ? configuration.names.join("\n") : ""}
      onChange={(event) => onChange({ kind: "required_checks", names: event.currentTarget.value ? event.currentTarget.value.split("\n") : [] })} />
      <small>Enter one check name per line.</small>
    </label>;
    case "command": return <CommandNodeConfiguration projectId={projectId} configuration={configuration}
      onChange={onChange} disabled={locked || !allow("output")} fixedInvocation={Boolean(presetFields)} />;
    case "mcp_tool": return <WorkflowMcpNodeConfiguration projectId={projectId} configuration={configuration} onChange={onChange} disabled={locked} />;
    case "build_list": return <BuildListFields configuration={configuration} disabled={locked}
      fixedSource={Boolean(presetFields)} onChange={onChange} />;
    case "for_each":
    case "repeat_until": return <LoopFields configuration={configuration} subflow={subflow} disabled={locked} onChange={onChange} />;
    case "condition": return <ConditionFields configuration={configuration} disabled={locked || !allow("predicate")} onChange={onChange} />;
    case "set_fields": return <SetFieldsFields configuration={configuration} disabled={locked} onChange={onChange} />;
    case "filter_map_list": return <FilterMapListFields configuration={configuration} disabled={locked} onChange={onChange} />;
    case "build_text": return <BuildTextFields configuration={configuration} disabled={locked} onChange={onChange} />;
    case "compare_models": return <CompareModelsFields configuration={configuration} disabled={locked} onChange={onChange} />;
    case "typed_start": return <StartInputFields configuration={configuration} disabled={locked} onChange={onChange} />;
    case "find_work":
    case "update_progress":
    case "manage_work":
    case "load_work_context": return <WorkflowAgileFields projectId={projectId} configuration={configuration} disabled={locked} onChange={onChange} />;
    case "request_input":
    case "approval": return <DecisionNodeFields configuration={configuration as WorkflowDecisionConfiguration} disabled={locked} onChange={onChange} />;
    case "subflow_entry": return null;
    default: return <p className="pipeline-meta">The parameters for this node version are unavailable in the editor.</p>;
  }
}

function VersionReview({ node, previous, next, wires, disabled, apply }: {
  node: WorkflowGraphNode; previous: WorkflowDefinition; next: WorkflowDefinition; wires: WorkflowWire[]; disabled: boolean; apply: () => void;
}) {
  const changed = changedPorts(node.id, previous, next, wires);
  return <details className="workflow-version-review">
    <summary>Review version {next.revision}</summary>
    {changed.length > 0 ? <ul>{changed.map(({ direction, name, before, after, affected }) => <li key={`${direction}:${name}`}>
      <strong>{direction === "inputs" ? "Input" : "Output"} {name}</strong>
      {before && after ? ` · ${before} → ${after}` : before ? ` · ${before} removed` : ` · ${after} added`}
      {affected.length > 0 && <ul>{affected.map((wire) => <li key={wireKey(wire)}>{wire.from.node}.{wire.from.port} → {wire.to.node}.{wire.to.port}</li>)}</ul>}
    </li>)}</ul> : <p>Port names and types match.</p>}
    <Button type="button" variant="outline" disabled={disabled} onClick={apply}>Update to version {next.revision}</Button>
  </details>;
}

function ConnectInput({ graph, node, definition, definitions, disabled, connect }: {
  graph: WorkflowGraph; node: WorkflowGraphNode; definition: WorkflowDefinition; definitions: WorkflowDefinitions; disabled: boolean;
  connect: (wire: WorkflowWire) => void;
}) {
  const [source, setSource] = useState("");
  const [output, setOutput] = useState("");
  const [input, setInput] = useState("");
  const outputs = definitions[source]?.outputs ?? [];
  return <fieldset className="pipeline-stack" disabled={disabled}>
    <legend>Add connection</legend>
    <label>Source node<select value={source} onChange={(event) => { setSource(event.currentTarget.value); setOutput(""); }}>
      <option value="">Choose source</option>
      {graph.nodes.filter(({ id }) => id !== node.id).map(({ id }) => <option key={id} value={id}>{definitions[id]?.title} · {id}</option>)}
    </select></label>
    <label>Output port<select value={output} disabled={!outputs.length} onChange={(event) => setOutput(event.currentTarget.value)}>
      <option value="">Choose output</option>
      {outputs.map(({ name, value }) => <option key={name} value={name}>{name} · {portType(value)}</option>)}
    </select></label>
    <label>Input port<select value={input} onChange={(event) => setInput(event.currentTarget.value)}>
      <option value="">Choose input</option>
      {definition.inputs.map(({ name, value }) => <option key={name} value={name}>{name} · {portType(value)}</option>)}
    </select></label>
    <Button type="button" variant="outline" disabled={!source || !output || !input}
      onClick={() => connect({ from: { node: source, port: output }, to: { node: node.id, port: input } })}>Connect ports</Button>
  </fieldset>;
}

function NodeConnections({ node, wires, definitions, disabled, reconnect, remove }: {
  node: WorkflowGraphNode; wires: WorkflowWire[]; definitions: WorkflowDefinitions; disabled: boolean;
  reconnect: (wire: WorkflowWire) => void; remove: (wire: WorkflowWire) => void;
}) {
  const connected = wires.filter(({ from, to }) => from.node === node.id || to.node === node.id);
  if (!connected.length) return null;
  return <section aria-label="Node connections" className="pipeline-stack">
    <h3>Connections</h3>
    <ul className="workflow-connections">{connected.map((wire) => {
      const name = `${wire.from.node}.${wire.from.port} → ${wire.to.node}.${wire.to.port}`;
      return <li key={wireKey(wire)}>
        <span>{name}{incompatibility(definitions, wire) && <strong className="block">Incompatible connection</strong>}</span>
        <div className="workflow-wire-actions">
          <Button type="button" variant="ghost" disabled={disabled} onClick={() => reconnect(wire)} aria-label={`Reconnect ${name}`}>Reconnect</Button>
          <Button type="button" variant="ghost" disabled={disabled} onClick={() => remove(wire)} aria-label={`Remove ${name}`}><Trash2 size={14} /></Button>
        </div>
      </li>;
    })}</ul>
  </section>;
}
