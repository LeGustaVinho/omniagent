export type WorkflowEndpoint = { node: string; port: string };
export type WorkflowWire = { from: WorkflowEndpoint; to: WorkflowEndpoint };
export type WorkflowNodeConfiguration = { kind: string; [field: string]: unknown };
export type WorkflowGraphNode = { id: string; kind: string; configuration: WorkflowNodeConfiguration };
export type WorkflowGraph = { name: string; nodes: WorkflowGraphNode[]; wires: WorkflowWire[] };
export type WorkflowPortType = { kind: "control" } | { kind: "data"; artifact: string } | { kind: "evidence" };
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
export type WorkflowCanvasEdit = {
  layout: WorkflowLayout;
  updateDefinitions: Record<string, number>;
  definitionCopies?: Record<string, { sourceId: string; revision: number }>;
};
export type WorkflowReuseSelection = { type: "preset"; nodeId: string } | { type: "subflow"; nodeIds: string[] };

export type WorkflowScalar = string | number | boolean | null;
export type WorkflowPredicate = { op: "exists"; pointer: string }
  | { op: "equals"; pointer: string; value: WorkflowScalar }
  | { op: "candidate_checks_passed" };

export type SetFieldOperation = { op: "set"; path: string; value: { source: "literal"; value: unknown } | { source: "input"; pointer: string } }
  | { op: "rename"; from: string; path: string };

export const MAX_ITERATION_ITEMS = 32;
export const ITERATION_PORT_FIELDS = [
  ["readyInput", "Subflow ready input"],
  ["itemInput", "Subflow item input"],
  ["completionOutput", "Subflow completion output"],
  ["resultOutput", "Subflow result output"],
] as const;

export const START_VALUE_TYPES = ["text", "number", "boolean", "null", "object", "array"] as const;
export type WorkflowStartField = { name: string; valueType: typeof START_VALUE_TYPES[number]; required: boolean };

export function isStartNode(kind: string) {
  return kind === "start" || kind === "typed_start";
}

export function isPresetEligible({ kind, configuration }: WorkflowGraphNode) {
  return !isStartNode(kind) && configuration.kind !== "preset_overrides"
    && !["subflow", "repeat_until", "for_each", "for_each_begin", "for_each_gate", "for_each_collect"].includes(kind);
}

export function defaultConfiguration(kind: string): WorkflowNodeConfiguration {
  switch (kind) {
    case "agent_role": return { kind, role: "" };
    case "required_checks": return { kind, names: [] };
    case "condition": return { kind, predicate: { op: "exists", pointer: "/evidence" } };
    case "command": return { kind, registrationId: "", registrationRevision: "", arguments: [], input: {}, output: "text" };
    case "mcp_tool": return { kind, serverId: "", serverRevision: "", toolName: "", contractRevision: "", arguments: {}, argumentMappings: {}, destinationPointer: null };
    case "build_list": return { kind, source: "values", values: [] };
    case "for_each": return { kind, maxItems: 8, readyInput: "ready", itemInput: "item", completionOutput: "completed", resultOutput: "result" };
    case "repeat_until": return {
      kind, maxIterations: 3, readyInput: "ready", completionOutput: "completed", evidenceOutput: "result",
      until: { op: "exists", pointer: "/evidence" },
    };
    case "set_fields": return { kind, mode: "build", operations: [] };
    case "filter_map_list": return { kind, predicate: null, mappings: [] };
    case "build_text": return { kind, template: "" };
    case "compare_models": return { kind, selections: [] };
    case "request_input": return { kind, message: "Input requested", fields: [{ id: "answer", label: "Answer", required: true, input: { kind: "text" } }] };
    case "approval": return { kind, message: "Does this stage have your approval?" };
    case "typed_start": return { kind, fields: [] };
    case "find_work": return { kind, query: { kind: "work", result: "list" } };
    case "update_progress": return { kind, action: { kind: "checklist", checked: true } };
    case "manage_work": return { kind, action: { kind: "link" } };
    case "load_work_context": return { kind, mode: "work" };
    default: return { kind };
  }
}

export function portType(value: WorkflowPortType) {
  if (value.kind === "data") return value.artifact.replaceAll("_", " ");
  return value.kind === "evidence" ? "Structured evidence" : "Control";
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
  return a.kind === b.kind && (a.kind !== "data" || (b.kind === "data" && a.artifact === b.artifact));
}

export function incompatibility(definitions: WorkflowDefinitions, { from, to }: WorkflowWire) {
  const output = outputPort(definitions, from);
  const input = inputPort(definitions, to);
  if (!output) return `Output ${from.node}.${from.port} is unavailable.`;
  if (!input) return `Input ${to.node}.${to.port} is unavailable.`;
  return sameType(output.value, input.value) || (output.value.kind === "data" && input.value.kind === "evidence") ? null
    : `${from.node}.${from.port} (${portType(output.value)}) cannot connect to ${to.node}.${to.port} (${portType(input.value)}).`;
}

function wireProblem(graph: WorkflowGraph, definitions: WorkflowDefinitions, wire: WorkflowWire) {
  const config = graph.nodes.find(({ id }) => id === wire.from.node)?.configuration;
  if (wire.from.port === "event" && config?.kind === "typed_start" && Array.isArray(config.fields)
    && config.fields.some((field) => field?.required === true)) {
    return "Make Start fields optional to connect the event port.";
  }
  return incompatibility(definitions, wire);
}

export function connectionProblem(graph: WorkflowGraph, definitions: WorkflowDefinitions, wire: WorkflowWire) {
  const incompatible = wireProblem(graph, definitions, wire);
  if (incompatible) return incompatible;
  const target = definitions[wire.to.node]?.kind;
  const multiple = (target === "join" && wire.to.port === "branches") ||
    (target === "summarize_result" && wire.to.port === "evidence") ||
    (target === "for_each_collect" && (wire.to.port === "finished" || wire.to.port === "result"));
  if (graph.wires.some((existing) => wireKey(existing) === wireKey(wire))) return "These ports are already connected.";
  if (!multiple && graph.wires.some(({ to }) => to.node === wire.to.node && to.port === wire.to.port)) {
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

function validPointer(value: unknown, maxLength: number, allowRoot: boolean): value is string {
  return typeof value === "string" && (value.startsWith("/") || (allowRoot && value === "")) &&
    new TextEncoder().encode(value).length <= maxLength && !/~(?![01])/.test(value);
}

function predicateProblem(input: unknown, allowChecks = true) {
  const predicate = input as { op?: unknown; pointer?: unknown; value?: unknown } | undefined;
  if (allowChecks && predicate?.op === "candidate_checks_passed") return null;
  if (!predicate || !validPointer(predicate.pointer, 1024, true)) return "Enter a valid field path.";
  if (predicate.op === "exists") return null;
  const { value } = predicate;
  if (predicate.op !== "equals" || !(value === null || typeof value === "string" || typeof value === "boolean" ||
    (typeof value === "number" && Number.isFinite(value)))) return "Choose an equality value with a matching type.";
  return new TextEncoder().encode(JSON.stringify(value)).length <= 4096 ? null : "The equality value is too long.";
}

function namedPort(value: unknown) {
  return typeof value === "string" && Boolean(value.trim());
}

function validIterationLimit(limit: unknown) {
  return typeof limit === "number" && Number.isInteger(limit) && limit >= 1 && limit <= MAX_ITERATION_ITEMS;
}

function setOperations(configuration: WorkflowNodeConfiguration): SetFieldOperation[] {
  return Array.isArray(configuration.operations) ? configuration.operations : [];
}

export function configurationProblem(configuration: WorkflowNodeConfiguration, definition: WorkflowDefinition) {
  if (configuration.kind !== definition.configuration) return "Review the parameters for this node version.";
  switch (configuration.kind) {
    case "agent_role": return typeof configuration.role === "string" && configuration.role.trim() ? null : "Agent role is required.";
    case "required_checks": return Array.isArray(configuration.names) && configuration.names.every((name) => typeof name === "string" && name.trim())
      ? null : "Check names must contain text.";
    case "condition": return predicateProblem(configuration.predicate);
    case "command":
      if (!configuration.registrationId || !configuration.registrationRevision) return "A registered command revision is required.";
      return configuration.input && typeof configuration.input === "object" && !Array.isArray(configuration.input)
        ? null : "Enter a JSON object for input.";
    case "mcp_tool":
      if (!configuration.serverId || !configuration.serverRevision || !configuration.toolName || !configuration.contractRevision) return "Select a pinned MCP server and tool.";
      if (configuration.argumentMappings === null) return "Enter a JSON object of argument names and JSON Pointers.";
      return configuration.arguments && typeof configuration.arguments === "object" && !Array.isArray(configuration.arguments) ? null : "Enter a JSON object for the mapped arguments.";
    case "build_list": {
      if (configuration.source === "values") {
        return Array.isArray(configuration.values) && configuration.values.length <= MAX_ITERATION_ITEMS
          ? null : `Add at most ${MAX_ITERATION_ITEMS} items.`;
      }
      const pointer = configuration.pointer;
      return configuration.source === "evidence" && validPointer(pointer, 2048, false) ? null : "Enter the path to a list in the source artifact.";
    }
    case "for_each": {
      const { maxItems, readyInput, itemInput, completionOutput, resultOutput } = configuration;
      if (!validIterationLimit(maxItems)) {
        return `Enter a maximum between 1 and ${MAX_ITERATION_ITEMS} items.`;
      }
      return ITERATION_PORT_FIELDS.every(([field]) => namedPort(configuration[field])) &&
        readyInput !== itemInput && completionOutput !== resultOutput ? null : "Choose distinct input and output ports on the pinned subflow.";
    }
    case "repeat_until": {
      const { maxIterations, readyInput, completionOutput, evidenceOutput, until } = configuration;
      if (!validIterationLimit(maxIterations)) return `Enter a maximum between 1 and ${MAX_ITERATION_ITEMS} iterations.`;
      if (![readyInput, completionOutput, evidenceOutput].every(namedPort)) return "Select the required subflow ports.";
      const feedback = configuration.feedback as { input?: unknown; output?: unknown } | undefined;
      if (feedback && (!namedPort(feedback.input) || !namedPort(feedback.output) || feedback.input === readyInput)) {
        return "Choose matching feedback ports.";
      }
      return predicateProblem(until);
    }
    case "set_fields":
      return (configuration.mode === "build" || configuration.mode === "update") && Array.isArray(configuration.operations) &&
        setOperations(configuration).every((operation) => validPointer(operation?.path, 1024, false) &&
          (operation.op === "rename" ? validPointer(operation.from, 1024, false) && operation.from !== operation.path
            : operation.op === "set" && (operation.value?.source === "literal" ||
              (operation.value?.source === "input" && validPointer(operation.value.pointer, 1024, true)))))
        ? null : "Review the source and destination of each field operation.";
    case "filter_map_list":
      return (configuration.predicate === null ? null : predicateProblem(configuration.predicate, false)) ??
        (Array.isArray(configuration.mappings) && configuration.mappings.every((mapping) =>
          validPointer(mapping?.from, 1024, true) && validPointer(mapping?.path, 1024, false)) ? null : "Review the field mappings.");
    case "build_text": return typeof configuration.template === "string" ? null : "Enter a text template.";
    case "compare_models": {
      const selections = Array.isArray(configuration.selections) ? configuration.selections : [];
      const keys = selections.flatMap((entry) => entry && typeof entry.provider === "string" && typeof entry.profileId === "string"
        && typeof entry.modelId === "string" ? [JSON.stringify([entry.provider, entry.profileId, entry.modelId])] : []);
      return keys.length === selections.length && keys.length >= 2 && new Set(keys).size === keys.length ? null : "Select at least two distinct models.";
    }
    case "typed_start": {
      const fields = Array.isArray(configuration.fields) ? configuration.fields : null;
      return fields?.every((field) => typeof field?.name === "string" && /^[A-Za-z_][A-Za-z0-9_-]*$/.test(field.name) &&
        START_VALUE_TYPES.includes(field.valueType) && typeof field.required === "boolean") &&
        new Set(fields.map(({ name }) => name)).size === fields.length ? null : "Use unique input names starting with a letter or underscore.";
    }
    case "request_input":
      return typeof configuration.message === "string" && configuration.message.trim() && Array.isArray(configuration.fields) && configuration.fields.length
        ? null : "Enter a question and add at least one answer field.";
    case "approval": return typeof configuration.message === "string" && configuration.message.trim() ? null : "Describe what needs approval.";
    default: return null;
  }
}

export function graphProblems(graph: WorkflowGraph, definitions: WorkflowDefinitions) {
  const problems: string[] = [];
  if (!graph.name.trim()) problems.push("Enter a pipeline name.");
  if (graph.nodes.filter(({ kind }) => isStartNode(kind)).length !== 1) problems.push("The pipeline needs one Start node.");
  for (const node of graph.nodes) {
    const definition = definitions[node.id];
    if (!definition) {
      problems.push(`Node ${node.id} has no available definition.`);
      continue;
    }
    const configuration = configurationProblem(node.configuration, definition);
    if (configuration) problems.push(`${definition.title} · ${node.id} · ${configuration}`);
    for (const port of definition.inputs) {
      const wired = graph.wires.some(({ to }) => to.node === node.id && to.port === port.name);
      if (node.configuration.kind === "mcp_tool" && port.name === "arguments") {
        const mappings = node.configuration.argumentMappings;
        if (wired !== Boolean(mappings && Object.keys(mappings).length)) {
          problems.push(`${node.id}.arguments requires a connection and argument mappings together.`);
        }
      } else if (node.configuration.kind === "build_list" && port.name === "value") {
        if (wired !== (node.configuration.source === "evidence")) {
          problems.push(`${node.id}.value requires a connection in source artifact mode.`);
        }
      } else if (node.kind === "set_fields" && port.name === "source") {
        const needsSource = node.configuration.mode === "update" || setOperations(node.configuration)
          .some((operation) => operation.op === "set" && operation.value?.source === "input");
        if (needsSource && !wired) problems.push(`${node.id}.source requires a connection for the selected operations.`);
      } else if (((node.kind === "approval" || node.kind === "summarize_result") && port.name === "evidence") ||
        (node.kind === "build_text" && port.name === "values") ||
        (node.kind === "agent" && port.name === "comparison")) {
        continue;
      } else if (node.kind === "manage_work" && (port.name === "task" || port.name === "title")) {
        const action = node.configuration.action as { kind?: string } | undefined;
        const required = port.name === "task" ? action?.kind !== "create_task" : action?.kind !== "link";
        if (required && !wired) problems.push(`${node.id}.${port.name} requires a connection for the selected work action.`);
      } else if (node.kind === "load_work_context" && (port.name === "task" || port.name === "document")) {
        const required = port.name === "task" ? node.configuration.mode !== "markdown" : node.configuration.mode !== "work";
        if (required && !wired) problems.push(`${node.id}.${port.name} requires a connection for the selected context source.`);
      } else if (!wired) {
        problems.push(`${node.id}.${port.name} (${portType(port.value)}) requires a connection.`);
      }
    }
  }
  for (const wire of graph.wires) {
    const issue = wireProblem(graph, definitions, wire);
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

export function initialNodePositions(graph: { nodes: readonly { id: string }[]; wires: readonly WorkflowWire[] }): WorkflowLayout {
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
