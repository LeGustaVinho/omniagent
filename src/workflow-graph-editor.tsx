import { useEffect, useMemo, useRef, useState, type Dispatch, type SetStateAction } from "react";
import { Background, Controls, Handle, MarkerType, Position, ReactFlow, useUpdateNodeInternals,
  type Connection, type Edge, type Node, type NodeProps } from "@xyflow/react";
import { Plus, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { CommandNodeConfiguration } from "@/workflow-command";
import { WorkflowMcpNodeConfiguration } from "@/workflow-mcp-tools";
import { AgentRoleFields } from "@/workflow-stage-model";
import {
  changedPorts, configurationProblem, connectionProblem, defaultConfiguration, incompatibility,
  initialNodePositions, outputPort, portType, wireKey,
  ITERATION_PORT_FIELDS, MAX_ITERATION_ITEMS,
  type WorkflowCanvasEdit, type WorkflowDefinition, type WorkflowDefinitions, type WorkflowGraph, type WorkflowGraphNode,
  type WorkflowNodeConfiguration, type WorkflowPort, type WorkflowWire,
} from "@/workflow-graph-model";
import "./workflow-graph-editor.css";

export type WorkflowEditorState = WorkflowCanvasEdit & { graph: WorkflowGraph };

export function editorDefinitions(state: WorkflowEditorState, pinned: WorkflowDefinitions, catalog: WorkflowDefinition[]): WorkflowDefinitions {
  const available = new Map(catalog.map((entry) => [entry.kind, entry]));
  return Object.fromEntries(state.graph.nodes.flatMap((node) => {
    const stored = Object.hasOwn(pinned, node.id) ? pinned[node.id] : undefined;
    const current = available.get(node.kind);
    const definition = stored?.kind === node.kind && state.updateDefinitions[node.id] === undefined ? stored : current;
    return definition ? [[node.id, definition]] : [];
  }));
}

type CanvasNode = Node<{ definition?: WorkflowDefinition; problem: boolean }, "workflow">;

function WorkflowCard({ id, data, selected }: NodeProps<CanvasNode>) {
  const update = useUpdateNodeInternals();
  const definition = data.definition;
  useEffect(() => { update(id); }, [definition, id, update]);
  return <article className="workflow-node-card" data-selected={selected || undefined} data-problem={data.problem || undefined}>
    <header><strong>{definition?.title ?? "Unavailable node"}</strong><small>{id}</small></header>
    {definition && <div className="workflow-node-ports">
      <WorkflowPortColumn ports={definition.inputs} side="input" />
      <WorkflowPortColumn ports={definition.outputs} side="output" />
    </div>}
  </article>;
}

export function WorkflowPortColumn({ ports, side, readOnly = false }: { ports: WorkflowPort[]; side: "input" | "output"; readOnly?: boolean }) {
  const input = side === "input";
  return <ul className="workflow-port-column" aria-label={input ? "Input ports" : "Output ports"}>
    {ports.map(({ name, value }) => <li key={name} data-side={side} data-port={value.kind} title={`${name} · ${portType(value)}`}>
      <Handle id={name} type={input ? "target" : "source"} position={input ? Position.Left : Position.Right} isConnectable={!readOnly}
        aria-label={`${side} ${name} · ${portType(value)}`} />
      <span>{name}</span><small>{portType(value)}</small>
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

export function WorkflowGraphEditor({ projectId, checkoutId, state, setState, definitions, catalog, problems, disabled, allowDefinitionUpdates = true }: {
  projectId: string;
  checkoutId?: string;
  state: WorkflowEditorState;
  setState: Dispatch<SetStateAction<WorkflowEditorState>>;
  definitions: WorkflowDefinitions;
  catalog: WorkflowDefinition[];
  problems: string[];
  disabled: boolean;
  allowDefinitionUpdates?: boolean;
}) {
  const { graph, layout } = state;
  const [theme, setTheme] = useState(readTheme);
  const [selected, select] = useState(graph.nodes[0]?.id ?? "");
  const issuedIds = useRef(new Set(graph.nodes.map(({ id }) => id)));
  const [catalogOpen, showCatalog] = useState(false);
  const [query, search] = useState("");
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const node = graph.nodes.find(({ id }) => id === selected);
  const definition = node && definitions[node.id];
  const available = allowDefinitionUpdates && definition && catalog.find(({ kind, revision }) => kind === definition.kind && revision > definition.revision);
  const positions = useMemo(() => initialNodePositions(graph), [graph]);
  const canvasNodes = useMemo<CanvasNode[]>(() => graph.nodes.map((item) => ({
    id: item.id, type: "workflow", position: layout[item.id] ?? positions[item.id]!, selected: item.id === selected,
    ariaLabel: `${definitions[item.id]?.title ?? "Node"} ${item.id}`,
    data: { definition: definitions[item.id], problem: !definitions[item.id] || Boolean(configurationProblem(item.configuration, definitions[item.id]!)) },
  })), [definitions, graph.nodes, layout, positions, selected]);
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
  const entries = catalog.filter((candidate) =>
    !["for_each_begin", "for_each_gate", "for_each_collect"].includes(candidate.kind) &&
    `${candidate.title} ${candidate.kind} ${candidate.effect}`.toLowerCase().includes(query.trim().toLowerCase()));

  useEffect(() => {
    const observer = new MutationObserver(() => setTheme(readTheme()));
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-app-theme"] });
    return () => observer.disconnect();
  }, []);

  function patchGraph(patch: (graph: WorkflowGraph) => WorkflowGraph) {
    setState((previous) => ({ ...previous, graph: patch(previous.graph) }));
  }

  function add(definition: WorkflowDefinition) {
    const stem = definition.kind.replaceAll("_", "-");
    let id = stem;
    for (let ordinal = 2; issuedIds.current.has(id); ordinal += 1) id = `${stem}-${ordinal}`;
    issuedIds.current.add(id);
    patchGraph((previous) => ({ ...previous, nodes: [...previous.nodes, { id, kind: definition.kind,
      configuration: defaultConfiguration(definition.configuration) }] }));
    select(id);
    showCatalog(false);
    search("");
  }

  function removeNode(id: string) {
    setState((previous) => {
      const layout = { ...previous.layout };
      const updateDefinitions = { ...previous.updateDefinitions };
      delete layout[id];
      delete updateDefinitions[id];
      return { layout, updateDefinitions, graph: { ...previous.graph,
        nodes: previous.graph.nodes.filter((item) => item.id !== id),
        wires: previous.graph.wires.filter(({ from, to }) => from.node !== id && to.node !== id),
      } };
    });
    if (id === selected) select("");
  }

  function connect(wire: WorkflowWire) {
    const issue = connectionProblem(graph, definitions, wire);
    setConnectionError(issue);
    if (!issue) patchGraph((previous) => ({ ...previous, wires: [...previous.wires, wire] }));
  }

  function configure(id: string, configuration: WorkflowNodeConfiguration) {
    patchGraph((previous) => ({ ...previous, nodes: previous.nodes.map((node) => node.id === id ? { ...node, configuration } : node) }));
  }

  function updateVersion(node: WorkflowGraphNode, version: WorkflowDefinition) {
    setState((previous) => ({ ...previous, updateDefinitions: { ...previous.updateDefinitions, [node.id]: version.revision },
      graph: { ...previous.graph, nodes: previous.graph.nodes.map((item) => item.id !== node.id || item.configuration.kind === version.configuration
        ? item : { ...item, configuration: defaultConfiguration(version.configuration) }) } }));
  }

  return <section aria-label="Pipeline graph editor" className="workflow-graph">
    <div className="workflow-graph-toolbar pipeline-form">
      <label>Pipeline name<input value={graph.name} disabled={disabled} onChange={(event) => {
        const name = event.currentTarget.value;
        patchGraph((previous) => ({ ...previous, name }));
      }} /></label>
      <Button type="button" variant="outline" disabled={disabled} aria-expanded={catalogOpen} onClick={() => showCatalog(!catalogOpen)}>
        <Plus size={15} /> Add node
      </Button>
    </div>
    {catalogOpen && <section aria-label="Node catalog" className="workflow-node-catalog pipeline-form">
      <label>Search node catalog<input type="search" value={query} onChange={(event) => search(event.currentTarget.value)} /></label>
      {entries.length ? <ul>{entries.map((entry) => <li key={entry.kind}><Button type="button" variant="outline"
        disabled={disabled || entry.kind === "start" && graph.nodes.some(({ kind }) => kind === "start")}
        onClick={() => add(entry)}>{entry.title}</Button></li>)}</ul> : <p>No matching nodes.</p>}
    </section>}
    {connectionError && <p className="workflow-graph-error" role="alert">{connectionError}</p>}
    {problems.length > 0 && <details className="workflow-graph-error" open><summary>Draft diagnostics ({problems.length})</summary>
      <ul>{problems.map((issue, i) => <li key={`${issue}:${i}`}>{issue}</li>)}</ul>
    </details>}
    <div className="workflow-graph-split">
      <div className="workflow-graph-canvas" aria-label="Pipeline graph canvas">
        <ReactFlow<CanvasNode, Edge> nodes={canvasNodes} edges={edges} nodeTypes={nodeTypes} colorMode={theme}
          fitView fitViewOptions={{ padding: 0.15, maxZoom: 1 }} minZoom={0.08} maxZoom={2}
          nodesDraggable={!disabled} nodesConnectable={!disabled} edgesReconnectable={false}
          deleteKeyCode={disabled ? null : ["Backspace", "Delete"]}
          onNodesChange={(changes) => {
            for (const change of changes) {
              if (change.type === "select" && change.selected) select(change.id);
              if (disabled) continue;
              if (change.type === "remove") removeNode(change.id);
              if (change.type === "position" && change.position) {
                const position = change.position;
                setState((previous) => ({ ...previous, layout: { ...previous.layout, [change.id]: position } }));
              }
            }
          }}
          onEdgesChange={(changes) => {
            const removed = changes.flatMap((change) => change.type === "remove" ? [change.id] : []);
            if (!disabled && removed.length) patchGraph((previous) => ({ ...previous,
              wires: previous.wires.filter((wire) => !removed.includes(wireKey(wire))),
            }));
          }}
          onNodeClick={(_, selectedNode) => { select(selectedNode.id); setConnectionError(null); }}
          onConnect={(candidate) => { const wire = connectionWire(candidate); if (!disabled && wire) connect(wire); }}
        >
          <Background gap={18} size={1} /><Controls showInteractive={!disabled} />
        </ReactFlow>
      </div>
      <aside aria-label="Node configuration" className="workflow-graph-inspector pipeline-form pipeline-stack">
        <label>Inspect node<select value={node?.id ?? ""} onChange={(event) => { select(event.currentTarget.value); setConnectionError(null); }}>
          <option value="">Choose a node</option>
          {graph.nodes.map(({ id }) => <option key={id} value={id}>{definitions[id]?.title ?? "Unavailable"} · {id}</option>)}
        </select></label>
        {node && definition && <>
          <p className="text-sm text-muted-foreground">Version {definition.revision} · {definition.effect.replaceAll("_", " ")}</p>
          <NodeFields key={node.id} projectId={projectId} checkoutId={checkoutId} node={node} definition={definition} disabled={disabled} onChange={(configuration) => configure(node.id, configuration)} />
          {available && <VersionReview node={node} previous={definition} next={available} wires={graph.wires}
            disabled={disabled} apply={() => updateVersion(node, available)} />}
          {definition.inputs.length > 0 && <ConnectInput key={`${node.id}:${definition.revision}`} graph={graph} node={node}
            definitions={definitions} definition={definition} disabled={disabled} connect={connect} />}
          <NodeConnections node={node} wires={graph.wires} definitions={definitions} disabled={disabled} remove={(wire) =>
            patchGraph((previous) => ({ ...previous, wires: previous.wires.filter((item) => wireKey(item) !== wireKey(wire)) }))} />
        </>}
        {node && <Button type="button" variant="outline" disabled={disabled} onClick={() => removeNode(node.id)}><Trash2 size={14} /> Remove node</Button>}
      </aside>
    </div>
  </section>;
}

function NodeFields({ projectId, checkoutId, node, definition, disabled, onChange }: {
  projectId: string; checkoutId?: string; node: WorkflowGraphNode; definition: WorkflowDefinition; disabled: boolean; onChange: (configuration: WorkflowNodeConfiguration) => void;
}) {
  const { configuration } = node;
  switch (definition.configuration) {
    case "none": return null;
    case "agent_role": return <AgentRoleFields projectId={projectId} checkoutId={checkoutId}
      mode={definition.kind === "plan" ? "plan" : definition.effect === "write_repository" ? "approved_write" : "read_only"}
      configuration={configuration} onChange={onChange} disabled={disabled} />;
    case "required_checks": return <label>Required checks<textarea rows={3} disabled={disabled}
      value={Array.isArray(configuration.names) ? configuration.names.join("\n") : ""}
      onChange={(event) => onChange({ kind: "required_checks", names: event.currentTarget.value ? event.currentTarget.value.split("\n") : [] })} />
      <small>Enter one check name per line.</small>
    </label>;
    case "command": return <CommandNodeConfiguration projectId={projectId} configuration={configuration} onChange={onChange} disabled={disabled} />;
    case "mcp_tool": return <WorkflowMcpNodeConfiguration projectId={projectId} configuration={configuration} onChange={onChange} disabled={disabled} />;
    case "build_list": return <BuildListFields configuration={configuration} disabled={disabled} onChange={onChange} />;
    case "for_each": return <>
      <p className="pipeline-meta">Select a pinned subflow in the package references and map its public ports.</p>
      <label>Maximum items<input type="number" min={1} max={MAX_ITERATION_ITEMS} step={1} disabled={disabled}
        value={typeof configuration.maxItems === "number" ? configuration.maxItems : ""}
        onChange={(event) => onChange({ ...configuration, maxItems: event.currentTarget.value === "" ? null : Number(event.currentTarget.value) })} /></label>
      {ITERATION_PORT_FIELDS.map(([field, label]) => <label key={field}>{label}<input disabled={disabled}
        value={typeof configuration[field] === "string" ? configuration[field] : ""}
        onChange={(event) => onChange({ ...configuration, [field]: event.currentTarget.value })} /></label>)}
    </>;
    case "condition": return <>
      <p className="pipeline-meta">Use a JSON Pointer into the source artifact, such as <code>/evidence/relativePath</code>. Equality compares JSON values by type.</p>
      <JsonConfiguration configuration={configuration} kind="condition" disabled={disabled} onChange={onChange} />
    </>;
    default: return <JsonConfiguration key={`${node.id}:${definition.revision}`} configuration={configuration}
      kind={definition.configuration} disabled={disabled} onChange={onChange} />;
  }
}

function BuildListFields({ configuration, disabled, onChange }: {
  configuration: WorkflowNodeConfiguration; disabled: boolean; onChange: (configuration: WorkflowNodeConfiguration) => void;
}) {
  const external = configuration.source === "evidence";
  return <>
    <label>List source<select disabled={disabled} value={external ? "evidence" : "values"}
      onChange={(event) => onChange(event.currentTarget.value === "evidence"
        ? { kind: "build_list", source: "evidence", pointer: "/evidence" }
        : defaultConfiguration("build_list"))}>
      <option value="values">Configured items</option><option value="evidence">Source artifact</option>
    </select></label>
    {external ? <label>Array pointer<input disabled={disabled}
      value={typeof configuration.pointer === "string" ? configuration.pointer : ""}
      onChange={(event) => onChange({ ...configuration, pointer: event.currentTarget.value })} />
      <small>Connect the Value input to the source artifact.</small>
    </label> : <ListValues values={configuration.values} disabled={disabled}
      onChange={(values) => onChange({ kind: "build_list", source: "values", values })} />}
  </>;
}

function ListValues({ values, disabled, onChange }: {
  values: unknown; disabled: boolean; onChange: (values: unknown[] | null) => void;
}) {
  const [text, setText] = useState(() => JSON.stringify(values ?? [], null, 2));
  return <label>Items (JSON array)<textarea rows={4} value={text} disabled={disabled} spellCheck={false}
    onChange={(event) => {
      const draft = event.currentTarget.value;
      setText(draft);
      try {
        const parsed: unknown = JSON.parse(draft);
        onChange(Array.isArray(parsed) ? parsed : null);
      } catch {
        onChange(null);
      }
    }} /></label>;
}

function JsonConfiguration({ configuration, kind, disabled, onChange }: {
  configuration: WorkflowNodeConfiguration; kind: string; disabled: boolean; onChange: (configuration: WorkflowNodeConfiguration) => void;
}) {
  const [json, setJson] = useState(() => JSON.stringify(configuration, null, 2));
  const [error, setError] = useState(false);
  return <><label>Node parameters<textarea rows={5} value={json} spellCheck={false} disabled={disabled}
    onChange={(event) => {
      const next = event.currentTarget.value;
      setJson(next);
      try {
        const parsed: unknown = JSON.parse(next);
        if (!parsed || typeof parsed !== "object" || !("kind" in parsed) || parsed.kind !== kind) throw new Error();
        onChange({ ...parsed, kind });
        setError(false);
      } catch {
        onChange({ kind: "" });
        setError(true);
      }
    }} /></label>{error && <p role="alert">Enter valid parameters for {kind.replaceAll("_", " ")}.</p>}</>;
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

function NodeConnections({ node, wires, definitions, disabled, remove }: {
  node: WorkflowGraphNode; wires: WorkflowWire[]; definitions: WorkflowDefinitions; disabled: boolean; remove: (wire: WorkflowWire) => void;
}) {
  const connected = wires.filter(({ from, to }) => from.node === node.id || to.node === node.id);
  if (!connected.length) return null;
  return <section aria-label="Node connections" className="pipeline-stack">
    <h3>Connections</h3>
    <ul className="workflow-connections">{connected.map((wire) => {
      const name = `${wire.from.node}.${wire.from.port} → ${wire.to.node}.${wire.to.port}`;
      return <li key={wireKey(wire)}><span>{name}{incompatibility(definitions, wire) && <strong className="block">Incompatible connection</strong>}</span><Button type="button" variant="ghost" disabled={disabled}
        onClick={() => remove(wire)} aria-label={`Remove ${name}`}><Trash2 size={14} /></Button></li>;
    })}</ul>
  </section>;
}
