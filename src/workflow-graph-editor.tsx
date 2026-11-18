import { useEffect, useMemo, useRef, useState, type Dispatch, type MouseEvent, type ReactNode, type SetStateAction } from "react";
import { Handle, MarkerType, MiniMap, Position, useUpdateNodeInternals,
  type Connection, type Edge, type Node, type NodeChange, type NodeProps, type ReactFlowInstance, type Viewport, type XYPosition } from "@xyflow/react";
import { ChevronRight, CircleAlert, Focus, GitBranch, MessageSquareText, Redo2, SlidersHorizontal, Trash2, Undo2, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ControlTooltip } from "@/components/ui/control-tooltip";
import { PositionedContextActions, type MenuGroups } from "@/components/ui/context-actions";
import { PipelineCanvasFrame, PipelineGraphCanvas, PipelineToolbar } from "@/pipeline-layout";
import type { PipelineInspectorView } from "@/pipeline-layout";
import { WorkflowControlFields } from "@/workflow-control-editor";
import { StartInputFields } from "@/workflow-node-fields";
import { AgentRoleFields } from "@/workflow-stage-model";
import type { StageGoalRoleRevision } from "@/workflow-stage-goal";
import type { ReusableReference } from "@/workflow-reusable";
import {
  changedPorts, configurationProblem, configuredBranches, connectionProblem, defaultConfiguration, incompatibility,
  initialNodePositions, isPresetEligible, isStartNode, isSupportedWorkflowNode, outputPort, portType, readableGraphViewport, wireKey,
  type WorkflowCanvasEdit, type WorkflowDefinition, type WorkflowDefinitions, type WorkflowGraph, type WorkflowGraphNode,
  type WorkflowEndpoint, type WorkflowLayout, type WorkflowNodeConfiguration, type WorkflowPort, type WorkflowReuseSelection, type WorkflowWire,
} from "@/workflow-graph-model";
import "./workflow-graph-editor.css";

type WorkflowNodeBinding = {
  reference?: ReusableReference;
  slots?: Record<string, ReusableReference>;
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
    if (!isSupportedWorkflowNode(node.kind)) return [];
    const copy = state.definitionCopies && Object.hasOwn(state.definitionCopies, node.id) ? state.definitionCopies[node.id] : undefined;
    const stored = Object.hasOwn(pinned, node.id) ? pinned[node.id] : copy && pinned[copy.sourceId];
    const current = available.get(node.kind);
    let definition = stored?.kind === node.kind && state.updateDefinitions[node.id] === undefined ? stored : current;
    if (!definition) return [];
    const bound = subflowDefinition?.(node.id);
    if (bound) definition = scopedSubflowPorts(node, definition, bound);
    if (node.kind === "fork" || node.kind === "join_results") {
      const branches = configuredBranches(node.configuration);
      const control = (name: string): WorkflowPort => ({ name, value: { kind: "control" } });
      const data = (name: string): WorkflowPort => ({ name, value: { kind: "data", artifact: "json_object" } });
      definition = node.kind === "fork" ? {
        ...definition, outputs: branches.flatMap((name) => [control(`branch:${name}`), data(`context:${name}`)]),
      } : { ...definition, inputs: branches.flatMap((name) => [control(`branch:${name}`), data(`result:${name}`)]) };
    }
    if (node.configuration.kind === "subflow_entry" && Array.isArray(node.configuration.outputs)) {
      definition = { ...definition, configuration: "subflow_entry", outputs: node.configuration.outputs };
    }
    return [[node.id, definition]];
  }));
}

function scopedSubflowPorts(node: WorkflowGraphNode, definition: WorkflowDefinition, bound: WorkflowDefinition): WorkflowDefinition {
  if (node.kind === "subflow") return { ...definition, title: bound.title, inputs: bound.inputs, outputs: bound.outputs,
    inputSchemas: bound.inputSchemas, outputSchemas: bound.outputSchemas };
  if (node.kind === "loop") {
    const input = bound.inputSchemas?.[String(node.configuration.contextInput)]?.properties?.input;
    const item = bound.outputSchemas?.[String(node.configuration.resultOutput)];
    return { ...definition, inputSchemas: input ? { input } : undefined, outputSchemas: item ? {
      results: { type: "object", properties: { iterations: { type: "array", items: item }, reason: { type: "string" } },
        required: ["iterations", "reason"], additionalProperties: false },
    } : undefined };
  }
  return definition;
}

type PortSide = "input" | "output";
type PortActions = {
  nodeId: string;
  activeOutput?: WorkflowEndpoint;
  activate: (side: PortSide, name: string) => void;
};
type CanvasNode = Node<{ definition?: WorkflowDefinition; promptRole?: string; problem: boolean; readOnly: boolean; inspected: boolean; portActions?: PortActions }, "workflow">;

function WorkflowCard({ id, data, selected }: NodeProps<CanvasNode>) {
  const update = useUpdateNodeInternals();
  const definition = data.definition;
  useEffect(() => { update(id); }, [definition, id, update]);
  return <WorkflowGraphNodeCard title={definition?.title ?? "Unavailable or retired node"} detail={data.promptRole ?? id} prompt={data.promptRole !== undefined}
    selected={selected || data.inspected}
    problem={data.problem} readOnly={data.readOnly} inputs={definition?.inputs ?? []} outputs={definition?.outputs ?? []} portActions={data.portActions} />;
}

export function WorkflowGraphNodeCard({ title, detail, inputs = [], outputs = [], selected, problem, readOnly, headerContent, portActions, compact, handles, prompt }: {
  title: string; detail?: string; inputs?: WorkflowPort[]; outputs?: WorkflowPort[]; selected?: boolean;
  problem?: boolean; readOnly?: boolean; headerContent?: ReactNode; portActions?: PortActions;
  compact?: boolean; handles?: ReactNode; prompt?: boolean;
}) {
  return <article className="workflow-node-card" data-compact={compact || undefined} data-selected={selected || undefined} data-problem={problem || undefined} data-prompt={prompt || undefined}>
    {handles}
    <header><strong>{prompt && <MessageSquareText size={14} aria-hidden />}{title}</strong>{detail && <small>{detail}</small>}{headerContent}</header>
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

function suggestedDataWires(graph: WorkflowGraph, definitions: WorkflowDefinitions, flow: WorkflowWire) {
  const producer = definitions[flow.from.node];
  const consumer = definitions[flow.to.node];
  if (!producer || !consumer) return [];
  return producer.outputs.filter(({ value }) => value.kind === "data").flatMap((output) =>
    consumer.inputs.flatMap((input) => {
      if (output.value.kind !== "data" || input.value.kind !== "data" || output.value.artifact !== input.value.artifact
        || graph.wires.some(({ to }) => to.node === flow.to.node && to.port === input.name)) return [];
      const wire = { from: { node: flow.from.node, port: output.name }, to: { node: flow.to.node, port: input.name } };
      return connectionProblem(graph, definitions, wire) ? [] : [wire];
    }));
}

function connectionWire(connection: Connection): WorkflowWire | undefined {
  return connection.sourceHandle && connection.targetHandle ? {
    from: { node: connection.source, port: connection.sourceHandle },
    to: { node: connection.target, port: connection.targetHandle },
  } : undefined;
}

const readTheme = (): "light" | "dark" => document.documentElement.dataset.appTheme === "light" ? "light" : "dark";

type PendingConnection = { endpoint: WorkflowEndpoint; side: PortSide; replacing?: string };
type EditorHistory = { past: WorkflowEditorState[]; future: WorkflowEditorState[] };
type CanvasTarget = { kind: "pane" } | { kind: "node" | "edge"; id: string } | { kind: "selection"; ids: string[] };
type CanvasMenu = { target: CanvasTarget; point: XYPosition; origin?: HTMLElement | SVGElement };
type InsertToolsOptions = { open: boolean; onOpenChange: (open: boolean) => void;
  onCloseAutoFocus: (event: Event) => void; canvasActions?: ReactNode };
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
  JSON.stringify(Object.entries(bindings ?? {}).sort(([a], [b]) => a.localeCompare(b)).map(([id, binding]) =>
    [id, binding.reference, binding.slots]));

export type WorkflowGraphView = { selected?: string; viewport?: Viewport; inspector: PipelineInspectorView };

export function WorkflowGraphEditor({ projectId, checkoutId, state, setState, definitions, catalog, problems, disabled, view,
  allowDefinitionUpdates = true, inspectReference, editablePresetFields, goalRoleForNode, subflowDefinitionForNode,
  onDerive, deriveDisabled, fullHeight = false, toolbarStart,
  promptRoles, inspectPrompt, insertTools, cloneBinding, nameLabel = "Pipeline name" }: {
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
  fullHeight?: boolean;
  toolbarStart?: ReactNode;
  view?: WorkflowGraphView;
  promptRoles?: Record<string, string>;
  nameLabel?: string;
  inspectPrompt?: (id: string) => ReactNode;
  insertTools?: (insert: (kind: string, binding?: WorkflowNodeBinding, configuration?: WorkflowNodeConfiguration) => void,
    options: InsertToolsOptions) => ReactNode;
  cloneBinding?: (binding: WorkflowNodeBinding) => WorkflowNodeBinding;
}) {
  const { graph, layout } = state;
  const hasEnd = graph.nodes.some(({ kind }) => kind === "end");
  const [theme, setTheme] = useState(readTheme);
  const [selected, select] = useState(() => graph.nodes.find(({ id }) => id === view?.selected)?.id ?? graph.nodes[0]?.id ?? "");
  useEffect(() => { if (view) view.selected = selected; }, [view, selected]);
  const [selectedNodes, setSelectedNodes] = useState<string[]>([]);
  const issuedIds = useRef(new Set(graph.nodes.map(({ id }) => id)));
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [pending, setPending] = useState<PendingConnection>();
  const [suggestDataFor, setSuggestDataFor] = useState<WorkflowWire>();
  const [history, setHistory] = useState<EditorHistory>({ past: [], future: [] });
  const currentBindings = bindingFingerprint(state.nodeBindings);
  const expectedBindings = useRef(currentBindings);
  const [flow, setFlow] = useState<ReactFlowInstance<CanvasNode, Edge>>();
  const [inspectorRequest, revealInspector] = useState(0);
  const [menu, setMenu] = useState<CanvasMenu>();
  const [insertion, setInsertion] = useState<{ contextual: boolean; position?: XYPosition }>();
  const restoreCanvasAfterInsertion = useRef(false);
  const [clipboard, setClipboard] = useState<GraphSelection>();
  const canvas = useRef<HTMLDivElement>(null);
  const dragging = useRef<WorkflowLayout>({});
  const [previewPositions, setPreviewPositions] = useState<WorkflowLayout>({});
  const node = graph.nodes.find(({ id }) => id === selected);
  const definition = node && definitions[node.id];
  const isPrompt = node && !!promptRoles && Object.hasOwn(promptRoles, node.id);
  const dataSuggestions = !disabled && suggestDataFor && graph.wires.some((wire) => wireKey(wire) === wireKey(suggestDataFor))
    ? suggestedDataWires(graph, definitions, suggestDataFor) : [];
  const canUpdateDefinition = typeof allowDefinitionUpdates === "function"
    ? !!node && allowDefinitionUpdates(node.id) : allowDefinitionUpdates;
  const available = canUpdateDefinition && definition && catalog.find(({ kind, revision }) => kind === definition.kind && revision > definition.revision);
  const positions = useMemo(() => initialNodePositions(graph), [graph]);
  const [openingViewport] = useState(() => {
    if (view?.viewport) return view.viewport;
    if (!fullHeight || !graph.nodes.length) return;
    const entry = graph.nodes.find(({ kind }) => isStartNode(kind)) ?? graph.nodes[0]!;
    return readableGraphViewport(graph.nodes.map(({ id }) => layout[id] ?? positions[id]!), layout[entry.id] ?? positions[entry.id]);
  });
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

  function focusNode(id: string) {
    select(id);
    setConnectionError(null);
    if (!id || !flow) return;
    const selected = flow.getNode?.(id);
    const position = selected?.position ?? layout[id] ?? positions[id];
    if (!position) return;
    void flow.setCenter?.(position.x + (selected?.measured?.width ?? 248) / 2,
      position.y + (selected?.measured?.height ?? 88) / 2,
      { zoom: Math.max(flow.getZoom?.() ?? 0.85, 0.85), duration: 180 });
  }

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
    setSuggestDataFor(undefined);
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

  function capture(nodes: WorkflowGraphNode[]): GraphSelection {
    const ids = new Set(nodes.map(({ id }) => id));
    return {
      nodes: nodes.map((node) => {
        const { id, kind } = node;
        const position = layout[id] ?? positions[id]!;
        const binding = state.nodeBindings && Object.hasOwn(state.nodeBindings, id) ? state.nodeBindings[id] : undefined;
        const current = catalog.find((definition) => definition.kind === kind);
        const version = definitions[id];
        const retainsNativePin = !binding?.preset && kind !== "subflow" && kind !== "loop"
          && current && version && version.revision !== current.revision;
        const definition = retainsNativePin ? state.definitionCopies?.[id] ?? { sourceId: id, revision: version.revision } : undefined;
        return { node, position, binding, definition };
      }),
      wires: graph.wires.filter(({ from, to }) => ids.has(from.node) && ids.has(to.node)),
    };
  }

  function insert(selection: GraphSelection, anchor?: { x: number; y: number }, clone = true) {
    if (disabled || !selection.nodes.length || selection.nodes.some(({ node }) => !isSupportedWorkflowNode(node.kind))) return;
    const ids = new Map(selection.nodes.map(({ node }) => [node.id, allocateId(node.id)]));
    const nextLayout = { ...layout };
    const definitionCopies = { ...state.definitionCopies };
    const nodeBindings = { ...state.nodeBindings };
    const dx = pasteOffset(selection.nodes.map(({ position }) => position.x), anchor?.x);
    const dy = pasteOffset(selection.nodes.map(({ position }) => position.y), anchor?.y);
    const copies = selection.nodes.map(({ node, position, binding, definition }) => {
      const id = ids.get(node.id)!;
      nextLayout[id] = { x: position.x + dx, y: position.y + dy };
      if (binding) nodeBindings[id] = clone ? cloneBinding?.(binding) ?? binding : binding;
      if (definition) definitionCopies[id] = definition;
      if (node.configuration.kind !== "join_results" || typeof node.configuration.fork !== "string") return { ...node, id };
      const fork = ids.get(node.configuration.fork) ?? null;
      return { ...node, id, configuration: { ...node.configuration, fork,
        cancelRemaining: fork !== null && node.configuration.cancelRemaining === true } };
    });
    const wires = selection.wires.map(({ from, to }) => ({ from: { ...from, node: ids.get(from.node)! }, to: { ...to, node: ids.get(to.node)! } }));
    apply({ ...state, layout: nextLayout, ...(Object.keys(definitionCopies).length && { definitionCopies }),
      ...(Object.keys(nodeBindings).length || state.nodeBindings ? { nodeBindings } : {}),
      graph: { ...graph, nodes: [...graph.nodes, ...copies], wires: [...graph.wires, ...wires] } });
    setSelectedNodes(copies.map(({ id }) => id));
    select(copies[0]!.id);
  }

  function insertNew(kind: string, binding?: WorkflowNodeBinding, configuration?: WorkflowNodeConfiguration) {
    const definition = catalog.find((entry) => entry.kind === kind);
    if (!definition || !isSupportedWorkflowNode(kind)) return;
    const bounds = canvas.current?.getBoundingClientRect();
    const point = insertion?.position ?? (bounds && flow ? flow.screenToFlowPosition({
      x: bounds.left + bounds.width / 2,
      y: bounds.top + bounds.height / 2,
    }) : { x: 270, y: 120 });
    insert({ nodes: [{ node: { id: kind.replaceAll("_", "-"), kind,
      configuration: configuration ?? defaultConfiguration(definition.configuration) }, position: { x: 0, y: 0 }, binding }], wires: [] }, point, false);
    revealInspector((request) => request + 1);
  }

  function isBoundary(kind: string) {
    return hasEnd && (isStartNode(kind) || kind === "end");
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
        if (graph.nodes.some(({ id, kind }) => id === change.id && isBoundary(kind))) continue;
        removed.add(change.id);
        delete nextLayout[change.id];
        delete nextDefinitions[change.id];
        delete definitionCopies[change.id];
        delete nodeBindings[change.id];
        if (change.id === selected) select("");
        if (pending?.endpoint.node === change.id) setPending(undefined);
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
    if (pending?.replacing && (removedWires.has(pending.replacing) || graph.wires.some((wire) =>
      wireKey(wire) === pending.replacing && (removed.has(wire.from.node) || removed.has(wire.to.node))))) setPending(undefined);
    apply({ ...state, layout: nextLayout, updateDefinitions: nextDefinitions,
      ...(state.definitionCopies && { definitionCopies }), ...(state.nodeBindings && { nodeBindings }),
      graph: removed.size || removedWires.size ? { ...graph,
      nodes: graph.nodes.filter(({ id }) => !removed.has(id)).map((node) => {
        const { configuration } = node;
        return configuration.kind === "join_results" && typeof configuration.fork === "string" && removed.has(configuration.fork)
          ? { ...node, configuration: { ...configuration, fork: null, cancelRemaining: false } } : node;
      }),
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
    if (promptRoles && Object.hasOwn(promptRoles, wire.from.node) && Object.hasOwn(promptRoles, wire.to.node)
      && outputPort(definitions, wire.from)?.value.kind === "control") setSuggestDataFor(wire);
    patchGraph((previous) => ({ ...previous, wires: replacing
      ? previous.wires.map((item) => wireKey(item) === replacing ? wire : item) : [...previous.wires, wire] }));
  }

  function activatePort(nodeId: string, side: PortSide, port: string) {
    if (disabled) return;
    const endpoint = { node: nodeId, port };
    select(nodeId);
    if (side === "output") {
      if (pending?.side === "input") {
        connect({ from: endpoint, to: pending.endpoint }, pending.replacing);
        return;
      }
      setConnectionError(null);
      setPending({ endpoint, side, replacing: pending?.replacing });
    } else if (pending?.side === "output") {
      connect({ from: pending.endpoint, to: endpoint }, pending.replacing);
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
      graph: { ...graph, nodes: graph.nodes.map((item) => item.id !== node.id
        || definitions[item.id]?.configuration === version.configuration
        || item.configuration.kind === version.configuration
        || !configurationProblem(item.configuration, version)
        ? item : { ...item, configuration: defaultConfiguration(version.configuration) }) } });
  }

  function reconnect(wire: WorkflowWire, change: "source" | "destination" = "destination") {
    if (disabled) return;
    setConnectionError(null);
    setPending({ endpoint: change === "source" ? wire.to : wire.from,
      side: change === "source" ? "input" : "output", replacing: wireKey(wire) });
  }

  function openMenu(event: MouseEvent | globalThis.MouseEvent, target: CanvasTarget) {
    event.preventDefault();
    event.stopPropagation();
    const point = { x: event.clientX, y: event.clientY };
    if (target.kind === "pane" && openNodePicker(point)) return;
    const origin = event.currentTarget;
    setMenu({ target, point,
      origin: origin instanceof HTMLElement || origin instanceof SVGElement ? origin : undefined });
  }

  function openNodePicker(point?: XYPosition) {
    if (!insertTools || disabled) return false;
    restoreCanvasAfterInsertion.current = point !== undefined;
    setInsertion({ contextual: point !== undefined, position: point && flow?.screenToFlowPosition(point) });
    return true;
  }

  function copy(text: string) {
    setConnectionError(null);
    void Promise.resolve().then(() => navigator.clipboard.writeText(text))
      .catch(() => setConnectionError("Could not copy to the clipboard."));
  }

  function scopedNodeActions(scoped: WorkflowGraphNode[]): MenuGroups {
    const first = scoped[0]!;
    const multiple = scoped.length > 1;
    const selection = `${scoped.length} selected nodes`;
    const subflowNodes = scoped.filter(({ kind }) => isSupportedWorkflowNode(kind) && !isStartNode(kind) && !isBoundary(kind)).map(({ id }) => id);
    const duplicable = scoped.every(({ id, kind }) => !isStartNode(kind) && !isBoundary(kind) && definitions[id] !== undefined);
    return [[
      ...(multiple ? [{ label: "Fit selection to view", disabled: !flow,
        run: () => { void flow?.fitView({ nodes: scoped.map(({ id }) => ({ id })), padding: 0.15, maxZoom: 1 }); } }] : []),
      ...(onDerive && subflowNodes.length ? [{ label: subflowNodes.length > 1
        ? `Extract ${subflowNodes.length} selected nodes as ${hasEnd ? "Function" : "subflow"}…`
        : `Extract node as ${hasEnd ? "Function" : "subflow"}…`, disabled: deriveDisabled,
        run: () => onDerive({ type: "subflow", nodeIds: subflowNodes }) }] : []),
    ], disabled ? [] : [
      ...(duplicable ? [
        { label: multiple ? `Duplicate ${selection}` : "Duplicate node", run: () => insert(capture(scoped)) },
        { label: multiple ? `Copy ${selection}` : "Copy node", run: () => setClipboard(capture(scoped)) },
      ] : []),
      ...(!multiple && layout[first.id] ? [{ label: "Reset node position", run: () => {
        const nextLayout = { ...layout };
        delete nextLayout[first.id];
        apply({ ...state, layout: nextLayout });
      } }] : []),
      ...(!scoped.some(({ kind }) => isBoundary(kind)) ? [{ label: multiple ? `Remove ${selection}` : "Remove node", destructive: true,
        run: () => changeNodes(scoped.map(({ id }) => ({ type: "remove", id }))) }] : []),
    ]];
  }

  function menuGroups(target: CanvasTarget, anchor?: XYPosition): { label: string; groups: MenuGroups } | undefined {
    if (target.kind === "pane") return { label: "Pipeline canvas", groups: [[
      { label: "Fit graph to view", disabled: !flow, run: () => { void flow?.fitView({ padding: 0.15, maxZoom: 1 }); } },
      ...(clipboard ? [{ label: clipboard.nodes.length === 1 ? "Paste node here" : `Paste ${clipboard.nodes.length} nodes here`, disabled,
        run: () => insert(clipboard, anchor) }] : []),
      { label: "Copy graph JSON", run: () => copy(JSON.stringify(graph, null, 2)) },
    ], [
      { label: "Undo graph edit", disabled: disabled || !history.past.length, run: () => restore("undo") },
      { label: "Redo graph edit", disabled: disabled || !history.future.length, run: () => restore("redo") },
      ...(pending ? [{ label: "Cancel connection", run: () => { setPending(undefined); setConnectionError(null); } }] : []),
    ]] };
    if (target.kind === "selection") {
      const scoped = graph.nodes.filter(({ id }) => target.ids.includes(id));
      if (scoped.length !== target.ids.length || scoped.length < 2) return;
      return { label: `${scoped.length} selected nodes`, groups: scopedNodeActions(scoped) };
    }
    if (target.kind === "node") {
      const clicked = graph.nodes.find(({ id }) => id === target.id);
      if (!clicked) return;
      const scoped = selectedNodes.includes(clicked.id) ? graph.nodes.filter(({ id }) => selectedNodes.includes(id)) : [clicked];
      return { label: clicked.id, groups: [[
        { label: "Inspect node", run: () => { select(clicked.id); setConnectionError(null); revealInspector((request) => request + 1); } },
        { label: "Copy node ID", run: () => copy(clicked.id) },
        { label: "Copy node configuration", run: () => copy(JSON.stringify(clicked.configuration, null, 2)) },
      ], [
        ...(!hasEnd && onDerive && isPresetEligible(clicked) && !editablePresetFields?.[clicked.id] ? [{ label: "Save node as preset…", disabled: deriveDisabled,
          run: () => onDerive({ type: "preset", nodeId: clicked.id }) }] : []),
      ], ...scopedNodeActions(scoped)] };
    }
    const wire = graph.wires.find((candidate) => wireKey(candidate) === target.id);
    if (!wire) return;
    const details = `${wire.from.node}.${wire.from.port} → ${wire.to.node}.${wire.to.port}`;
    return { label: details, groups: [[
      { label: "Inspect source node", run: () => select(wire.from.node) },
      { label: "Inspect destination node", run: () => select(wire.to.node) },
      { label: "Copy connection", run: () => copy(details) },
    ], disabled ? [] : [
      { label: "Reconnect source…", run: () => reconnect(wire, "source") },
      { label: "Reconnect destination…", run: () => reconnect(wire) },
      { label: "Remove connection", destructive: true, run: () => changeNodes([], [target.id]) },
    ]] };
  }

  const context = menu && menuGroups(menu.target, flow?.screenToFlowPosition(menu.point));
  const canvasActions = insertion?.contextual && menuGroups({ kind: "pane" }, insertion.position);

  const canvasNodes: CanvasNode[] = graph.nodes.map((item) => {
    const defined = definitions[item.id];
    return { id: item.id, type: "workflow", position: previewPositions[item.id] ?? layout[item.id] ?? positions[item.id]!, selected: selectedNodes.includes(item.id),
      ariaLabel: `${defined?.title ?? "Node"} ${item.id}`, data: { definition: defined, promptRole: promptRoles?.[item.id], readOnly: disabled, inspected: item.id === selected,
        problem: !defined || Boolean(configurationProblem(item.configuration, defined)),
        portActions: { nodeId: item.id, activeOutput: pending?.side === "output" ? pending.endpoint : undefined,
          activate: (side, name) => activatePort(item.id, side, name) },
      } };
  });
  const settingsAvailable = Boolean(node && definition &&
    (definition.configuration !== "none" || node.kind === "subflow" || state.nodeBindings?.[node.id]));
  const wireCount = node ? graph.wires.filter(({ from, to }) => from.node === node.id || to.node === node.id).length : 0;

  return <section aria-label="Pipeline graph editor" className="workflow-graph" data-full-height={fullHeight || undefined} onKeyDown={(event) => {
    if (event.key === "Escape") {
      if (pending) { setPending(undefined); setConnectionError(null); }
      setSuggestDataFor(undefined);
    }
  }}>
    <PipelineToolbar>
      {toolbarStart}
      {insertTools?.(insertNew, {
        open: Boolean(insertion),
        onOpenChange: (open) => { if (open) openNodePicker(); else setInsertion(undefined); },
        onCloseAutoFocus: (event) => {
          if (!restoreCanvasAfterInsertion.current) return;
          restoreCanvasAfterInsertion.current = false;
          event.preventDefault();
          canvas.current?.focus();
        },
        canvasActions: canvasActions && <details className="pipeline-evidence">
          <summary>Canvas actions</summary>
          <div className="pipeline-actions">{canvasActions.groups.flat().map((action) => "run" in action && <Button
            key={action.label} type="button" variant="outline" disabled={action.disabled} onClick={() => {
              setInsertion(undefined);
              action.run();
            }}>{action.label}</Button>)}</div>
        </details>,
      })}
      <label className="pipeline-inline-field workflow-graph-name"><span>{nameLabel}</span><input aria-label={nameLabel} value={graph.name} disabled={disabled} onChange={(event) => {
        const name = event.currentTarget.value;
        patchGraph((previous) => ({ ...previous, name }));
      }} /></label>
      <div className="workflow-history" aria-label="Graph edit history" role="group">
        <Button type="button" variant="ghost" aria-label="Undo graph edit" title="Undo graph edit" disabled={disabled || !history.past.length}
          onClick={() => restore("undo")}><Undo2 size={16} /></Button>
        <Button type="button" variant="ghost" aria-label="Redo graph edit" title="Redo graph edit" disabled={disabled || !history.future.length}
          onClick={() => restore("redo")}><Redo2 size={16} /></Button>
      </div>
      {problems.length > 0 && <details className="workflow-graph-diagnostics">
        <summary><CircleAlert size={15} aria-hidden /> Draft diagnostics ({problems.length})</summary>
        <ul>{problems.map((issue, i) => <li key={`${issue}:${i}`}>{issue}</li>)}</ul>
      </details>}
    </PipelineToolbar>
    <PipelineCanvasFrame className="workflow-graph-frame" canvasLabel="Pipeline graph canvas" inspectorLabel="Node configuration" view={view?.inspector}
      collapsibleInspector={fullHeight} inspectorRequest={inspectorRequest} inspectorClassName="workflow-graph-inspector pipeline-form"
      canvas={<div ref={canvas} tabIndex={0} aria-label="Workflow canvas actions" className="workflow-graph-canvas" onKeyDown={(event) => {
        if (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
        if (!(event.target instanceof Element)) return;
        const anchor = event.target.closest(".react-flow__node, .react-flow__edge, .react-flow__nodesselection-rect");
        const id = anchor?.getAttribute("data-id");
        const origin = anchor instanceof HTMLElement || anchor instanceof SVGElement ? anchor : event.currentTarget;
        const rect = origin.getBoundingClientRect();
        event.preventDefault();
        event.stopPropagation();
        const target: CanvasTarget = id ? { kind: anchor?.classList.contains("react-flow__edge") ? "edge" : "node", id }
          : anchor?.classList.contains("react-flow__nodesselection-rect") && selectedNodes.length > 1
            ? { kind: "selection", ids: selectedNodes } : { kind: "pane" };
        if (target.kind === "pane" && openNodePicker({ x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 })) return;
        setMenu({ target,
          point: { x: rect.left + 8, y: rect.top + 8 }, origin });
      }}>
        {(connectionError || pending || dataSuggestions.length > 0) && <div className="workflow-graph-feedback">
          {connectionError && <p className="workflow-graph-error" role="alert"><CircleAlert size={15} aria-hidden />{connectionError}</p>}
          {pending && <div className="workflow-connection-status">
            <p role="status">Select an {pending.side === "output" ? "input" : "output"} port for {pending.endpoint.node}.{pending.endpoint.port}.</p>
            <Button type="button" variant="ghost" onClick={() => { setPending(undefined); setConnectionError(null); }}>Cancel connection</Button>
          </div>}
          {dataSuggestions.length > 0 && !pending && <section className="workflow-data-suggestions" aria-label="Suggested data connections">
            <header><span role="status">Connect compatible data</span>
              <Button type="button" variant="ghost" className="ui-icon-button" aria-label="Dismiss data suggestions"
                onClick={() => setSuggestDataFor(undefined)}><X size={15} aria-hidden /></Button>
            </header>
            <ul>{dataSuggestions.map((wire) => <li key={wireKey(wire)}>
              <Button type="button" variant="outline" onClick={() => connect(wire)}>
                {wire.from.port} <span aria-hidden>→</span> {wire.to.port}
              </Button>
            </li>)}</ul>
          </section>}
        </div>}
        <PipelineGraphCanvas<CanvasNode, Edge> interactive={!disabled} nodes={canvasNodes} edges={edges} nodeTypes={nodeTypes} colorMode={theme}
          fitView={!openingViewport} defaultViewport={openingViewport} fitViewOptions={{ padding: 0.15, maxZoom: 1 }} minZoom={fullHeight ? 0.025 : 0.08} maxZoom={2}
          compactControls={fullHeight} canvasTools={fullHeight && graph.nodes.length >= 16 ?
            <MiniMap pannable zoomable ariaLabel="Pipeline overview" nodeColor="var(--primary)" /> : undefined}
          onMoveEnd={view && ((_, viewport) => { view.viewport = viewport; })}
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
          onSelectionContextMenu={(event, nodes) => openMenu(event, { kind: "selection", ids: nodes.map(({ id }) => id) })}
          onConnect={(candidate) => { const wire = connectionWire(candidate); if (wire) connect(wire); }}
          onReconnect={(previous, candidate) => { const wire = connectionWire(candidate); if (wire) connect(wire, previous.id); }}
        />
      </div>}
      inspector={<>
        <header className="workflow-inspector-heading">
          <span className="workflow-inspector-icon"><SlidersHorizontal size={17} aria-hidden /></span>
          <div><h2>{definition?.title ?? "Select a node"}</h2>
            {node && <code title={node.id}>{node.id}</code>}
          </div>
        </header>
        <div className="workflow-inspector-jump">
          <label>Inspect node<select value={node?.id ?? ""} onChange={(event) => focusNode(event.currentTarget.value)}>
            <option value="">Choose a node</option>
            {graph.nodes.map(({ id }) => <option key={id} value={id}>{definitions[id]?.title ?? "Unavailable"} · {id}</option>)}
          </select></label>
          <ControlTooltip label="Center selected node">
            <Button type="button" variant="outline" className="ui-icon-button" aria-label="Center selected node"
              disabled={!node || !flow} onClick={() => { if (node) focusNode(node.id); }}><Focus size={16} aria-hidden /></Button>
          </ControlTooltip>
        </div>
        {node && definition && <>
          {isPrompt ? inspectPrompt?.(node.id) : <>
          <div className="workflow-inspector-meta">
            <span>Version {definition.revision}</span>
            {definition.effect !== "none" && <span>{definition.effect.replaceAll("_", " ")}</span>}
          </div>
          {settingsAvailable && <details key={`settings:${node.id}`} className="workflow-inspector-group" open>
            <summary><SlidersHorizontal size={15} aria-hidden /> Settings <ChevronRight size={14} aria-hidden /></summary>
            <div className="pipeline-stack">
              {inspectReference?.(node.id)}
              <NodeFields projectId={projectId} checkoutId={checkoutId} node={node} definition={definition} nodes={graph.nodes}
                presetFields={editablePresetFields?.[node.id]} goalRole={goalRoleForNode?.(node.id)}
                subflow={subflowDefinitionForNode?.(node.id)}
                disabled={disabled} onChange={(configuration) => configure(node.id, configuration)} />
            </div>
          </details>}
          {available && <VersionReview node={node} previous={definition} next={available} wires={graph.wires}
            disabled={disabled} apply={() => updateVersion(node, available)} />}
          </>}
          {(wireCount > 0 || definition.inputs.length > 0) && <details key={`connections:${node.id}`} className="workflow-inspector-group" open={!settingsAvailable}>
            <summary><GitBranch size={15} aria-hidden /> Connections <span>{wireCount}</span><ChevronRight size={14} aria-hidden /></summary>
            <div className="pipeline-stack">
              <NodeConnections node={node} wires={graph.wires} definitions={definitions} disabled={disabled} reconnect={reconnect}
                remove={(wire) => changeNodes([], [wireKey(wire)])} />
              {definition.inputs.length > 0 && <ConnectInput key={`${node.id}:${definition.revision}`} graph={graph} node={node}
                definitions={definitions} definition={definition} disabled={disabled} connect={connect} />}
            </div>
          </details>}
        </>}
        {node && !disabled && !isBoundary(node.kind) && <Button type="button" variant="ghost" className="workflow-remove-node" onClick={() => changeNodes([{ id: node.id, type: "remove" }])}>
          <Trash2 size={14} /> Remove node
        </Button>}
      </>} />
    {menu && context && <PositionedContextActions {...context} point={menu.point} onClose={() => setMenu(undefined)}
      restoreFocus={() => (menu.origin?.isConnected && menu.origin.tabIndex >= 0 ? menu.origin : canvas.current)?.focus()} />}
  </section>;
}

function NodeFields({ projectId, checkoutId, node, definition, disabled, presetFields, goalRole, subflow, onChange, nodes }: {
  projectId: string; checkoutId?: string; node: WorkflowGraphNode; definition: WorkflowDefinition; disabled: boolean; onChange: (configuration: WorkflowNodeConfiguration) => void;
  presetFields?: string[];
  goalRole?: { version?: StageGoalRoleRevision; pending?: boolean };
  subflow?: WorkflowDefinition;
  nodes: WorkflowGraphNode[];
}) {
  const { configuration } = node;
  const allow = (field: string) => !presetFields || presetFields.includes(field);
  const locked = disabled || presetFields?.length === 0;
  switch (definition.configuration) {
    case "none": return null;
    case "end": return null;
    case "fork":
    case "join_results":
    case "wait":
    case "human":
    case "loop": return <WorkflowControlFields configuration={configuration} disabled={locked} nodes={nodes} subflow={subflow} onChange={onChange} />;
    case "agent_role": return <AgentRoleFields projectId={projectId} checkoutId={checkoutId}
      mode={definition.effect === "write_repository" ? "approved_write" : "read_only"}
      nodeId={node.id} nodeKind={node.kind} goalRole={goalRole}
      configuration={configuration} onChange={onChange} disabled={locked || !allow("role")} fixedModel={Boolean(presetFields)} />;
    case "typed_start": return definition.outputSchemas?.inputs ? <div className="pipeline-stack">
      <label className="pipeline-checkline">
        <input type="checkbox" checked={configuration.conversation === true} disabled={locked} onChange={(event) =>
          onChange({ ...configuration, conversation: event.currentTarget.checked })} /> Conversation entry
      </label>
      {configuration.conversation === true && <small className="pipeline-meta">The chat supplies message and history fields declared in this Schema.</small>}
    </div> : <StartInputFields configuration={configuration} disabled={locked} onChange={onChange} />;
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
