import { schemaMismatch, type JsonSchema } from "@/workflow-schema-model";

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
  inputSchemas?: Record<string, JsonSchema>;
  outputSchemas?: Record<string, JsonSchema>;
};

export type WorkflowDefinitions = Record<string, WorkflowDefinition>;
export type WorkflowLayout = Record<string, { x: number; y: number }>;

export function readableGraphViewport(positions: readonly WorkflowLayout[string][], entry = positions[0]) {
  if (!entry || positions.length < 2) return;
  const horizontal = positions.map(({ x }) => x);
  if (positions.length < 16 && Math.max(...horizontal) - Math.min(...horizontal) < 1900) return;
  const zoom = 0.86;
  return { x: 56 - entry.x * zoom, y: 64 - entry.y * zoom, zoom };
}
export type WorkflowCanvasEdit = {
  layout: WorkflowLayout;
  updateDefinitions: Record<string, number>;
  definitionCopies?: Record<string, { sourceId: string; revision: number }>;
};
export type WorkflowReuseSelection = { type: "preset"; nodeId: string } | { type: "subflow"; nodeIds: string[] };

export type WorkflowScalar = string | number | boolean | null;
export type WorkflowPredicate = { op: "exists"; pointer: string }
  | { op: "equals"; pointer: string; value: WorkflowScalar };

const MAX_ITERATION_ITEMS = 32;

export const START_VALUE_TYPES = ["text", "number", "boolean", "null", "object", "array"] as const;
export const CONVERSATION_START_INPUT_TYPES = { message: "text", history: "array" } as const;
export type ConversationStartSource = keyof typeof CONVERSATION_START_INPUT_TYPES;
export type WorkflowStartField = {
  name: string;
  valueType: typeof START_VALUE_TYPES[number];
  required: boolean;
  conversationSource?: ConversationStartSource;
};

export function isStartNode(kind: string) {
  return kind === "start" || kind === "typed_start";
}

const workflowNodeKinds = new Set([
  "start", "typed_start", "end", "agent", "subflow", "fork", "join_results", "wait", "loop", "human",
]);

export function isSupportedWorkflowNode(kind: string) {
  return workflowNodeKinds.has(kind);
}

export function isPresetEligible({ kind, configuration }: WorkflowGraphNode) {
  return isSupportedWorkflowNode(kind) && !isStartNode(kind) && kind !== "end" && kind !== "subflow" && kind !== "loop"
    && configuration.kind !== "preset_overrides";
}

export function defaultConfiguration(kind: string): WorkflowNodeConfiguration {
  switch (kind) {
    case "fork": return { kind, branches: ["first", "second"], concurrency: null };
    case "join_results": return { kind, branches: ["first", "second"], strategy: "all", quorum: null,
      countFailures: false, cancelRemaining: false, fork: null };
    case "wait": return { kind, until: { kind: "delay", seconds: 60 } };
    case "human": return { kind, interaction: "approval", message: "Does this stage have your approval?", fields: [] };
    case "loop": return { kind, maxIterations: 3, breakWhen: null, exitWhen: null, limit: "complete",
      readyInput: "ready", contextInput: "context", completionOutput: "completed", resultOutput: "result" };
    case "agent_role": return { kind, role: "" };
    case "typed_start": return { kind, fields: [] };
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
  if (!sameType(output.value, input.value) && !(output.value.kind === "data" && input.value.kind === "evidence")) {
    return `${from.node}.${from.port} (${portType(output.value)}) cannot connect to ${to.node}.${to.port} (${portType(input.value)}).`;
  }
  const target = definitions[to.node]?.inputSchemas?.[to.port];
  if (!target) return null;
  const source = definitions[from.node]?.outputSchemas?.[from.port];
  const issue = source ? schemaMismatch(source, target) : "The source needs a declared output Schema.";
  return issue ? `${from.node}.${from.port} → ${to.node}.${to.port} · ${issue}` : null;
}

export function configuredBranches(configuration: WorkflowNodeConfiguration): string[] {
  return Array.isArray(configuration.branches) ? configuration.branches.filter((name): name is string => typeof name === "string") : [];
}

function wireProblem(graph: WorkflowGraph, definitions: WorkflowDefinitions, wire: WorkflowWire) {
  const config = graph.nodes.find(({ id }) => id === wire.from.node)?.configuration;
  if (config?.kind === "typed_start" && config.promptOrMarkdown === true) {
    if (wire.from.port === "document") return "Connect the Start inputs port to read the normalized request.";
    if (wire.from.port === "event") return "This Start mode needs a request at manual launch.";
  }
  if (wire.from.port === "event" && config?.kind === "typed_start" && Array.isArray(config.fields)
    && config.fields.some((field) => field?.required === true)) {
    return "Make Start fields optional to connect the event port.";
  }
  return incompatibility(definitions, wire);
}

export function connectionProblem(graph: WorkflowGraph, definitions: WorkflowDefinitions, wire: WorkflowWire) {
  const incompatible = wireProblem(graph, definitions, wire);
  if (incompatible) return incompatible;
  if (graph.wires.some((existing) => wireKey(existing) === wireKey(wire))) return "These ports are already connected.";
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

function validPointer(value: unknown, maxLength: number, allowRoot: boolean): value is string {
  return typeof value === "string" && (value.startsWith("/") || (allowRoot && value === "")) &&
    new TextEncoder().encode(value).length <= maxLength && !/~(?![01])/.test(value);
}

function predicateProblem(input: unknown) {
  const predicate = input as { op?: unknown; pointer?: unknown; value?: unknown } | undefined;
  if (!predicate || !validPointer(predicate.pointer, 1024, true)) return "Enter a valid field path.";
  if (predicate.op === "exists") return null;
  const { value } = predicate;
  if (predicate.op !== "equals" || !(value === null || typeof value === "string" || typeof value === "boolean" ||
    (typeof value === "number" && Number.isFinite(value)))) return "Choose an equality value with a matching type.";
  return new TextEncoder().encode(JSON.stringify(value)).length <= 4096 ? null : "The equality value is too long.";
}

function validIterationLimit(limit: unknown) {
  return typeof limit === "number" && Number.isInteger(limit) && limit >= 1 && limit <= MAX_ITERATION_ITEMS;
}

export function configurationProblem(configuration: WorkflowNodeConfiguration, definition: WorkflowDefinition) {
  if (!isSupportedWorkflowNode(definition.kind)) return "This node type was removed.";
  if (definition.kind === "subflow" && configuration.kind === "prompt_overrides") return null;
  if (configuration.kind !== definition.configuration) return "Review the parameters for this node version.";
  switch (configuration.kind) {
    case "fork":
    case "join_results": {
      const branches = configuredBranches(configuration);
      if (branches.length < 2 || new Set(branches).size !== branches.length ||
        !branches.every((name) => /^[a-zA-Z0-9_-]+$/.test(name))) return "Use at least two unique branch names.";
      const bounded = configuration.kind === "fork" ? configuration.concurrency : configuration.strategy === "quorum" ? configuration.quorum : null;
      return (bounded == null && configuration.strategy !== "quorum") ||
        (typeof bounded === "number" && Number.isInteger(bounded) && bounded >= 1 && bounded <= branches.length)
        ? null : "Choose a branch count within the configured branches.";
    }
    case "wait": {
      const until = configuration.until as { kind?: string; seconds?: number; at?: string } | undefined;
      return until?.kind === "delay" && Number.isInteger(until.seconds) && (until.seconds ?? 0) > 0 ||
        until?.kind === "scheduled" && !!until.at && Number.isFinite(Date.parse(until.at))
        ? null : "Set a positive delay or scheduled time.";
    }
    case "human":
      return typeof configuration.message === "string" && configuration.message.trim()
        && (!["input", "feedback"].includes(String(configuration.interaction)) || Array.isArray(configuration.fields) && configuration.fields.length)
        ? null : "Enter a message and configure the requested fields.";
    case "loop":
      if (!validIterationLimit(configuration.maxIterations)) return "Enter an iteration limit between 1 and 32.";
      if (configuration.breakWhen != null) {
        const problem = predicateProblem(configuration.breakWhen);
        if (problem) return problem;
      }
      return configuration.exitWhen == null ? null : predicateProblem(configuration.exitWhen);
    case "agent_role": return typeof configuration.role === "string" && configuration.role.trim() ? null : "Agent role is required.";
    case "typed_start": {
      const fields = Array.isArray(configuration.fields) ? configuration.fields : null;
      if (!fields?.every((field) => typeof field?.name === "string" && /^[A-Za-z_][A-Za-z0-9_-]{0,159}$/.test(field.name)) ||
        new Set(fields.map(({ name }) => name)).size !== fields.length) return "Use unique input names starting with a letter or underscore.";
      if (configuration.promptOrMarkdown === true && fields.some(({ name }) => name === "prompt" || name === "request")) {
        return "Start reserves the prompt and request field names in this mode.";
      }
      if (!fields.every(({ valueType, required }) => START_VALUE_TYPES.includes(valueType) && typeof required === "boolean")) {
        return "Choose a valid type and required setting for every Start input.";
      }
      return fields.every(({ conversationSource, valueType }) => conversationSource == null ||
        (Object.hasOwn(CONVERSATION_START_INPUT_TYPES, conversationSource) &&
          CONVERSATION_START_INPUT_TYPES[conversationSource as ConversationStartSource] === valueType))
        ? null : "Use Text for a conversation message and Array for conversation history.";
    }
    default: return null;
  }
}

export function graphProblems(graph: WorkflowGraph, definitions: WorkflowDefinitions) {
  const problems: string[] = [];
  if (!graph.name.trim()) problems.push("Enter a pipeline name.");
  if (graph.nodes.filter(({ kind }) => isStartNode(kind)).length !== 1) problems.push("The pipeline needs one Start node.");
  const ends = graph.nodes.filter(({ kind }) => kind === "end");
  if (ends.length > 1) problems.push("Choose one End node.");
  const start = graph.nodes.find(({ kind, configuration }) => kind === "typed_start" && configuration.conversation === true);
  if (start) {
    const input = definitions[start.id]?.outputSchemas?.inputs;
    const fields = Array.isArray(start.configuration.fields) ? start.configuration.fields as WorkflowStartField[] : [];
    const receivesMessage = input ? input.properties?.message?.type === "string"
      : fields.some(({ conversationSource, valueType }) => conversationSource === "message" && valueType === "text");
    if (!receivesMessage) problems.push("A conversation pipeline needs a text message input on Start.");
    const history = input?.properties?.history;
    if (history && (history.type !== "array" || schemaMismatch({
      type: "array", items: { type: "object", additionalProperties: false, required: ["role", "text"],
        properties: { role: { type: "string" }, text: { type: "string" } } },
    }, history))) {
      problems.push("Start history must accept a list of objects with string fields role and text.");
    }
    const result = ends[0] && definitions[ends[0].id]?.outputSchemas?.result;
    if (result?.properties?.reply?.type !== "string" || !result.required?.includes("reply")) {
      problems.push("A conversation pipeline needs an End result Schema with required reply text.");
    }
  }
  for (const node of graph.nodes) {
    if (!isSupportedWorkflowNode(node.kind)) {
      problems.push(`Node ${node.id} uses a retired node type. Replace it before publishing.`);
      continue;
    }
    const definition = definitions[node.id];
    if (!definition) {
      problems.push(`Node ${node.id} has no available definition.`);
      continue;
    }
    const configuration = configurationProblem(node.configuration, definition);
    if (configuration) problems.push(`${definition.title} · ${node.id} · ${configuration}`);
    for (const port of definition.inputs) {
      const optional = (node.kind === "end" && port.name === "result" && !definition.inputSchemas?.result)
        || (node.kind === "wait" && port.name === "input")
        || (node.kind === "human" && port.name === "input" && node.configuration.interaction !== "review")
        || (node.kind === "join_results" && port.name.startsWith("result:"))
        || (node.kind === "agent" && ["comparison", "review_subject"].includes(port.name));
      if (!optional && !graph.wires.some(({ to }) => to.node === node.id && to.port === port.name)) {
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
