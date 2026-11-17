import { useEffect, useMemo, useRef, useState, type Dispatch, type ReactNode, type SetStateAction } from "react";
import { Handle, MarkerType, Position, useUpdateNodeInternals,
  type Connection, type Edge, type Node, type NodeChange, type NodeProps } from "@xyflow/react";
import { Plus, Redo2, Trash2, Undo2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { PipelineNodeCatalog, workflowRegistryGroups } from "@/pipeline-catalog";
import { PipelineCanvasFrame, PipelineGraphCanvas, PipelineInspectorHeader, PipelineToolbar } from "@/pipeline-layout";
import { WorkflowAgileFields } from "@/workflow-agile-editor";
import { CommandNodeConfiguration } from "@/workflow-command";
import { DecisionNodeFields, type WorkflowDecisionConfiguration } from "@/workflow-decision-editor";
import { WorkflowMcpNodeConfiguration } from "@/workflow-mcp-tools";
import { StructuredObjectField } from "@/workflow-structured-fields";
import { BuildListFields, BuildTextFields, CandidateAcceptanceFields, CompareModelsFields, ConditionFields, FilterMapListFields, IntegratedChecksFields, LoopFields, RequiredChecksFields, RunBacklogFields, SetFieldsFields, StartInputFields, TestPhaseFields } from "@/workflow-node-fields";
import { AgentRoleFields } from "@/workflow-stage-model";
import type { StageGoalRoleRevision } from "@/workflow-stage-goal";
import {
  changedPorts, configurationProblem, connectionProblem, defaultConfiguration, incompatibility,
  initialNodePositions, isStartNode, outputPort, portType, wireKey,
  type WorkflowCanvasEdit, type WorkflowDefinition, type WorkflowDefinitions, type WorkflowGraph, type WorkflowGraphNode,
  type WorkflowEndpoint, type WorkflowLayout, type WorkflowNodeConfiguration, type WorkflowPort, type WorkflowWire,
} from "@/workflow-graph-model";
import "./workflow-graph-editor.css";

export type WorkflowEditorState = WorkflowCanvasEdit & { graph: WorkflowGraph };

export function editorDefinitions(state: WorkflowEditorState, pinned: WorkflowDefinitions, catalog: WorkflowDefinition[],
  subflowDefinition?: (nodeId: string) => WorkflowDefinition | undefined): WorkflowDefinitions {
  const available = new Map(catalog.map((entry) => [entry.kind, entry]));
  return Object.fromEntries(state.graph.nodes.flatMap((node) => {
    const stored = Object.hasOwn(pinned, node.id) ? pinned[node.id] : undefined;
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
    if (node.kind === "verify_test_phase") {
      const { phase } = node.configuration;
      definition = { ...definition, inputs: definition.inputs.filter(({ name }) =>
        name === "tests" ? phase === "red" : !["candidate", "prior"].includes(name) || phase !== "baseline") };
    }
    if (node.kind === "accept_candidate" && node.configuration.kind === "none") {
      definition = { ...definition, inputs: definition.inputs.filter(({ name }) => name !== "quality" && name !== "build") };
    }
    return [[node.id, definition]];
  }));
}

function scopedSubflowPorts(node: WorkflowGraphNode, definition: WorkflowDefinition, bound: WorkflowDefinition): WorkflowDefinition {
  if (node.kind === "subflow") return { ...definition, inputs: bound.inputs, outputs: bound.outputs };
  if (node.kind === "repeat_until") {
    const exports = Array.isArray(node.configuration.exports) ? node.configuration.exports : [];
    return { ...definition, inputs: bound.inputs, outputs: [
      ...["completed", "failed"].map((name) => ({ name, value: { kind: "control" } as const })),
      ...bound.outputs.filter(({ name, value }) => value.kind === "data" && exports.includes(name)
        && name !== "completed" && name !== "failed"),
    ] };
  }
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
const historyLimit = 32;

export function WorkflowGraphEditor({ projectId, checkoutId, state, setState, definitions, catalog, problems, disabled,
  allowDefinitionUpdates = true, inspectReference, editablePresetFields, goalRoleForNode, subflowDefinitionForNode, backlogBound, historyScope }: {
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
  backlogBound?: (nodeId: string) => boolean;
  historyScope?: string;
}) {
  const { graph, layout } = state;
  const [theme, setTheme] = useState(readTheme);
  const [selected, select] = useState(graph.nodes[0]?.id ?? "");
  const issuedIds = useRef(new Set(graph.nodes.map(({ id }) => id)));
  const [catalogOpen, showCatalog] = useState(false);
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [pending, setPending] = useState<PendingConnection>();
  const [history, setHistory] = useState<EditorHistory>({ past: [], future: [] });
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

  useEffect(() => { setHistory({ past: [], future: [] }); }, [historyScope]);

  function apply(next: WorkflowEditorState) {
    if (disabled || next === state) return;
    setHistory((previous) => ({ past: [...previous.past, state].slice(-historyLimit), future: [] }));
    setState(next);
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
    setState(previous);
  }

  function patchGraph(patch: (graph: WorkflowGraph) => WorkflowGraph) {
    apply({ ...state, graph: patch(graph) });
  }

  function add(definition: WorkflowDefinition) {
    const stem = definition.kind.replaceAll("_", "-");
    let id = stem;
    for (let ordinal = 2; issuedIds.current.has(id) || graph.nodes.some((node) => node.id === id); ordinal += 1) id = `${stem}-${ordinal}`;
    issuedIds.current.add(id);
    patchGraph((previous) => ({ ...previous, nodes: [...previous.nodes, { id, kind: definition.kind,
      configuration: defaultConfiguration(definition.configuration) }] }));
    select(id);
    showCatalog(false);
  }

  function changeNodes(changes: NodeChange<CanvasNode>[], removedWireIds: string[] = []) {
    const nextLayout = { ...layout };
    const nextDefinitions = { ...state.updateDefinitions };
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
    apply({ layout: nextLayout, updateDefinitions: nextDefinitions, graph: removed.size || removedWires.size ? { ...graph,
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
    apply({ ...state, updateDefinitions: { ...state.updateDefinitions, [node.id]: version.revision },
      graph: { ...graph, nodes: graph.nodes.map((item) => item.id !== node.id
        || definitions[item.id]?.configuration === version.configuration
        || item.configuration.kind === version.configuration
        || !configurationProblem(item.configuration, version)
        ? item : { ...item, configuration: defaultConfiguration(version.configuration) }) } });
  }

  const canvasNodes: CanvasNode[] = graph.nodes.map((item) => {
    const defined = definitions[item.id];
    return { id: item.id, type: "workflow", position: previewPositions[item.id] ?? layout[item.id] ?? positions[item.id]!, selected: item.id === selected,
      ariaLabel: `${defined?.title ?? "Node"} ${item.id}`, data: { definition: defined, readOnly: disabled,
        problem: !defined || Boolean(configurationProblem(item.configuration, defined, backlogBound?.(item.id))),
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
      <Button type="button" variant="outline" disabled={disabled} aria-expanded={catalogOpen} onClick={() => showCatalog(!catalogOpen)}>
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
      canvas={<div className="workflow-graph-canvas">
        <PipelineGraphCanvas<CanvasNode, Edge> interactive={!disabled} nodes={canvasNodes} edges={edges} nodeTypes={nodeTypes} colorMode={theme}
          fitView fitViewOptions={{ padding: 0.15, maxZoom: 1 }} minZoom={0.08} maxZoom={2}
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
            backlogBound={backlogBound?.(node.id)}
            disabled={disabled} onChange={(configuration) => configure(node.id, configuration)} />
          {available && <VersionReview node={node} previous={definition} next={available} wires={graph.wires}
            disabled={disabled} apply={() => updateVersion(node, available)} />}
          {definition.inputs.length > 0 && <ConnectInput key={`${node.id}:${definition.revision}`} graph={graph} node={node}
            definitions={definitions} definition={definition} disabled={disabled} connect={connect} />}
          <NodeConnections node={node} wires={graph.wires} definitions={definitions} disabled={disabled} reconnect={(wire) => {
            setConnectionError(null);
            setPending({ from: wire.from, replacing: wireKey(wire) });
          }} remove={(wire) => {
            if (pending?.replacing === wireKey(wire)) setPending(undefined);
            patchGraph((previous) => ({ ...previous, wires: previous.wires.filter((item) => wireKey(item) !== wireKey(wire)) }));
          }} />
        </>}
        {node && <Button type="button" variant="outline" disabled={disabled} onClick={() => changeNodes([{ id: node.id, type: "remove" }])}>
          <Trash2 size={14} /> Remove node
        </Button>}
      </>} />
  </section>;
}

function NodeFields({ projectId, checkoutId, node, definition, disabled, presetFields, goalRole, subflow, backlogBound, onChange }: {
  projectId: string; checkoutId?: string; node: WorkflowGraphNode; definition: WorkflowDefinition; disabled: boolean; onChange: (configuration: WorkflowNodeConfiguration) => void;
  presetFields?: string[];
  goalRole?: { version?: StageGoalRoleRevision; pending?: boolean };
  subflow?: WorkflowDefinition;
  backlogBound?: boolean;
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
    case "required_checks":
    case "candidate_checks": return <RequiredChecksFields configuration={configuration} disabled={locked}
      fixedNames={!allow("names")} fixedGroups={!allow("parallelGroups")}
      strategyOptions={!presetFields && definition.inputs.some(({ name }) => name === "strategy")}
      stageOptions={definition.configuration === "candidate_checks" && !presetFields} onChange={onChange} />;
    case "command": return <CommandNodeConfiguration projectId={projectId} configuration={configuration}
      onChange={onChange} disabled={locked || !allow("output")} fixedInvocation={Boolean(presetFields)} />;
    case "mcp_tool": return <WorkflowMcpNodeConfiguration projectId={projectId} configuration={configuration} onChange={onChange} disabled={locked} />;
    case "build_list": return <BuildListFields configuration={configuration} disabled={locked}
      fixedSource={Boolean(presetFields)} onChange={onChange} />;
    case "for_each":
    case "repeat_until": return <LoopFields configuration={configuration} subflow={subflow} disabled={locked} onChange={onChange} />;
    case "run_backlog": return <RunBacklogFields projectId={projectId} configuration={configuration}
      backlogBound={backlogBound} disabled={locked} onChange={onChange} />;
    case "verify_test_phase": return <TestPhaseFields projectId={projectId} configuration={configuration} disabled={locked} onChange={onChange} />;
    case "integrated_checks": return <IntegratedChecksFields projectId={projectId} configuration={configuration} disabled={locked} onChange={onChange} />;
    case "candidate_acceptance": return <CandidateAcceptanceFields configuration={configuration} disabled={locked} onChange={onChange} />;
    case "search_public_repositories": return <label>Maximum repositories<input type="number" min={1} max={30} step={1}
      disabled={locked} value={typeof configuration.limit === "number" ? configuration.limit : ""}
      onChange={(event) => onChange({ ...configuration, limit: event.currentTarget.value === "" ? null : event.currentTarget.valueAsNumber })} /></label>;
    case "condition": return <ConditionFields configuration={configuration} disabled={locked || !allow("predicate")} onChange={onChange} />;
    case "set_fields": return <SetFieldsFields configuration={configuration} disabled={locked} onChange={onChange} />;
    case "filter_map_list": return <FilterMapListFields configuration={configuration} disabled={locked} onChange={onChange} />;
    case "build_text": return <BuildTextFields configuration={configuration} disabled={locked} onChange={onChange} />;
    case "validate_output": return <StructuredObjectField label="Output JSON Schema" value={configuration.schema} disabled={locked}
      onChange={(schema) => onChange({ ...configuration, schema })} />;
    case "quality_gate": return <>
      <p className="pipeline-meta">Use field paths into the evidence array, in wire order.</p>
      <StructuredObjectField label="Quality rules" value={configuration.rules} disabled={locked}
        onChange={(rules) => onChange({ ...configuration, rules })} />
    </>;
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
