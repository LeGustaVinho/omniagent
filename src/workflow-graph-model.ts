export type WorkflowEndpoint = { node: string; port: string };
export type WorkflowWire = { from: WorkflowEndpoint; to: WorkflowEndpoint };
export type WorkflowNodeConfiguration = { kind: string; [field: string]: unknown };
export type WorkflowGraphNode = { id: string; kind: string; configuration: WorkflowNodeConfiguration };
export type WorkflowGraph = { name: string; nodes: WorkflowGraphNode[]; wires: WorkflowWire[] };
export type WorkflowPortType = { kind: "control" } | { kind: "data"; artifact: string };
export type WorkflowPort = { name: string; value: WorkflowPortType };

export type WorkflowDefinition = {
  kind: string;
  revision: number;
  title: string;
  configuration: string;
  executor: string;
  effect: string;
  inputs: WorkflowPort[];
  outputs: WorkflowPort[];
};

export type WorkflowDefinitions = Record<string, WorkflowDefinition>;
export type WorkflowLayout = Record<string, { x: number; y: number }>;
export type WorkflowCanvasEdit = { layout: WorkflowLayout; updateDefinitions: Record<string, number> };

export function defaultConfiguration(kind: string): WorkflowNodeConfiguration {
  switch (kind) {
    case "agent_role": return { kind, role: "" };
    case "required_checks": return { kind, names: [] };
    case "command": return { kind, registrationId: "", registrationRevision: "", arguments: [], input: {}, output: "text" };
    default: return { kind };
  }
}

export function portType(value: WorkflowPortType) {
  return value.kind === "control" ? "Control" : value.artifact.replaceAll("_", " ");
}

export function wireKey({ from, to }: WorkflowWire) {
  return JSON.stringify([from.node, from.port, to.node, to.port]);
}

export function outputPort(definitions: WorkflowDefinitions, endpoint: WorkflowEndpoint) {
  return definitions[endpoint.node]?.outputs.find(({ name }) => name === endpoint.port);
}

function inputPort(definitions: WorkflowDefinitions, endpoint: WorkflowEndpoint) {
  return definitions[endpoint.node]?.inputs.find(({ name }) => name === endpoint.port);
}

function sameType(a: WorkflowPortType, b: WorkflowPortType) {
  return a.kind === b.kind && (a.kind === "control" || (b.kind === "data" && a.artifact === b.artifact));
}

export function incompatibility(definitions: WorkflowDefinitions, { from, to }: WorkflowWire) {
  const output = outputPort(definitions, from);
  const input = inputPort(definitions, to);
  if (!output) return `Output ${from.node}.${from.port} is unavailable.`;
  if (!input) return `Input ${to.node}.${to.port} is unavailable.`;
  return sameType(output.value, input.value) ? null
    : `${from.node}.${from.port} (${portType(output.value)}) cannot connect to ${to.node}.${to.port} (${portType(input.value)}).`;
}

export function connectionProblem(graph: WorkflowGraph, definitions: WorkflowDefinitions, wire: WorkflowWire) {
  const incompatible = incompatibility(definitions, wire);
  if (incompatible) return incompatible;
  if (graph.wires.some(({ to }) => to.node === wire.to.node && to.port === wire.to.port)) {
    return `Input ${wire.to.node}.${wire.to.port} accepts one connection.`;
  }
  const pending = [wire.to.node];
  const visited = new Set<string>();
  while (pending.length) {
    const node = pending.pop()!;
    if (node === wire.from.node) return "This connection would create a cycle.";
    if (visited.has(node)) continue;
    visited.add(node);
    graph.wires.filter(({ from }) => from.node === node).forEach(({ to }) => pending.push(to.node));
  }
  return null;
}

export function configurationProblem(configuration: WorkflowNodeConfiguration, definition: WorkflowDefinition) {
  if (configuration.kind !== definition.configuration) return "Review the parameters for this node version.";
  switch (configuration.kind) {
    case "agent_role": return typeof configuration.role === "string" && configuration.role.trim() ? null : "Agent role is required.";
    case "required_checks": return Array.isArray(configuration.names) && configuration.names.every((name) => typeof name === "string" && name.trim())
      ? null : "Check names must contain text.";
    case "command":
      if (!configuration.registrationId || !configuration.registrationRevision) return "A registered command revision is required.";
      return configuration.input && typeof configuration.input === "object" && !Array.isArray(configuration.input)
        ? null : "Enter a JSON object for input.";
    default: return null;
  }
}

export function graphProblems(graph: WorkflowGraph, definitions: WorkflowDefinitions) {
  const problems: string[] = [];
  if (!graph.name.trim()) problems.push("Enter a pipeline name.");
  if (graph.nodes.filter(({ kind }) => kind === "start").length !== 1) problems.push("The pipeline needs one Start node.");
  for (const node of graph.nodes) {
    const definition = definitions[node.id];
    if (!definition) {
      problems.push(`Node ${node.id} has no available definition.`);
      continue;
    }
    const configuration = configurationProblem(node.configuration, definition);
    if (configuration) problems.push(`${definition.title} · ${node.id} · ${configuration}`);
    for (const port of definition.inputs) {
      if (!graph.wires.some(({ to }) => to.node === node.id && to.port === port.name)) {
        problems.push(`${node.id}.${port.name} (${portType(port.value)}) requires a connection.`);
      }
    }
  }
  for (const wire of graph.wires) {
    const issue = incompatibility(definitions, wire);
    if (issue) problems.push(issue);
  }
  return problems;
}

export function changedPorts(node: string, previous: WorkflowDefinition, next: WorkflowDefinition, wires: WorkflowWire[]) {
  return (["inputs", "outputs"] as const).flatMap((direction) => {
    const current = new Map(previous[direction].map(({ name, value }) => [name, value]));
    const replacement = new Map(next[direction].map(({ name, value }) => [name, value]));
    return [...new Set([...current.keys(), ...replacement.keys()])].flatMap((name) => {
      const before = current.get(name);
      const after = replacement.get(name);
      if (before && after && sameType(before, after)) return [];
      const affected = wires.filter((wire) => {
        const endpoint = direction === "inputs" ? wire.to : wire.from;
        return endpoint.node === node && endpoint.port === name;
      });
      return [{ direction, name, before: before && portType(before), after: after && portType(after), affected }];
    });
  });
}

export function initialNodePositions(graph: WorkflowGraph): WorkflowLayout {
  const remaining = new Map(graph.nodes.map(({ id }) => [id, new Set<string>()]));
  const next = new Map(graph.nodes.map(({ id }) => [id, new Set<string>()]));
  for (const { from, to } of graph.wires) {
    remaining.get(to.node)?.add(from.node);
    next.get(from.node)?.add(to.node);
  }
  const queue = graph.nodes.filter(({ id }) => remaining.get(id)?.size === 0).map(({ id }) => id);
  const depth = new Map<string, number>();
  for (const id of queue) {
    for (const downstream of next.get(id) ?? []) {
      depth.set(downstream, Math.max(depth.get(downstream) ?? 0, (depth.get(id) ?? 0) + 1));
      const dependencies = remaining.get(downstream);
      if (dependencies?.delete(id) && dependencies.size === 0) queue.push(downstream);
    }
  }
  const rows = new Map<number, number>();
  return Object.fromEntries(graph.nodes.map(({ id }) => {
    const column = depth.get(id) ?? 0;
    const row = rows.get(column) ?? 0;
    rows.set(column, row + 1);
    return [id, { x: column * 300, y: row * 220 }];
  }));
}
