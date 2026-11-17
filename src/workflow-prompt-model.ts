import type { WorkflowDefinition, WorkflowEndpoint, WorkflowGraph, WorkflowNodeConfiguration, WorkflowPort } from "@/workflow-graph-model";
import type { ReusableReference } from "@/workflow-reusable";

export const PROMPT_EXECUTORS = {
  agent: "General instructions",
  plan: "Create a plan",
  review_plan: "Review a plan",
  implement: "Implement changes",
  review_candidate: "Review a candidate",
  review_diff_candidate: "Review a diff",
} as const;

export type PromptExecutor = keyof typeof PROMPT_EXECUTORS;
export type PromptInput = { kind: string; target?: string };
export type PromptOutput = { kind: string; source?: string };
export type PromptContract = {
  text: string;
  executor: PromptExecutor;
  inputs: Record<string, PromptInput>;
  outputs: Record<string, PromptOutput>;
  routes: string[];
  delegates: { alias: string; required: boolean }[];
  outputSchema?: Record<string, unknown>;
  configuration?: WorkflowNodeConfiguration;
};
export type PromptMember = {
  key: string;
  name: string;
  destination?: string;
  body: { kind: "prompt"; prompt: PromptContract };
  references?: Record<string, ReusableReference>;
};
export type PromptRoleChoice = { name: string; reference: ReusableReference };
export type PromptPortRename = { direction: "inputs" | "outputs"; from: string; to: string };
export type PromptExpansion = {
  workflow: { nodes: { instance: WorkflowGraph["nodes"][number]; definition: WorkflowDefinition }[]; wires: WorkflowGraph["wires"] };
  inputs: Record<string, WorkflowEndpoint>;
  outputs: Record<string, WorkflowEndpoint>;
};
export type PromptProposal = Pick<PromptContract, "inputs" | "outputs" | "routes" | "outputSchema">;
export type PromptSample = {
  title: string;
  routes: string[];
  delegates: PromptContract["delegates"];
  outputs: Record<string, string>;
};

export const PROMPT_OUTPUTS = ["json_object", "json_list", "rendered_text"] as const;
export const defaultPromptContract = (): PromptContract => ({
  text: "", executor: "agent", inputs: {}, outputs: {}, routes: ["concluido"], delegates: [],
});

export function hasPromptDecision(prompt: PromptContract) {
  return prompt.executor === "agent" && (prompt.routes.length !== 1 || prompt.routes[0] !== "concluido"
    || !!prompt.delegates.length || Object.values(prompt.outputs).some(({ source }) => !source) || !!prompt.outputSchema);
}

export function promptNodeDefinition(name: string, prompt: PromptContract, base: WorkflowDefinition): WorkflowDefinition {
  const control = (name: string): WorkflowPort => ({ name, value: { kind: "control" } });
  const data = (name: string, artifact: string): WorkflowPort => ({ name, value: { kind: "data", artifact } });
  const outcome = hasPromptDecision(prompt) ? [
    data("decision", "json_object"), data("outcome", "json_object"),
    ...(prompt.delegates.length ? [data("evidence", "evidence_set")] : []),
  ] : [];
  return {
    ...base,
    title: name,
    inputs: [control("ready"), ...Object.entries(prompt.inputs).map(([name, { kind }]) => data(name, kind))],
    outputs: [
      ...prompt.routes.map(control),
      control("falhou"),
      ...Object.entries(prompt.outputs).map(([name, { kind }]) => data(name, kind)),
      ...outcome,
    ],
  };
}

export function availablePromptArtifacts(catalog: readonly WorkflowDefinition[]) {
  const types = new Set<string>(PROMPT_OUTPUTS);
  for (const { inputs, outputs } of catalog) {
    for (const { value } of [...inputs, ...outputs]) if (value.kind === "data") types.add(value.artifact);
  }
  return [...types].sort();
}

export function promptDataOptions(ports: readonly WorkflowPort[] | undefined) {
  return ports?.flatMap<{ name: string; kind?: string }>(({ name, value }) =>
    value.kind === "data" ? [{ name, kind: value.artifact }]
      : value.kind === "evidence" ? [{ name, kind: undefined }] : []) ?? [];
}

export function initialPromptSample(kind: string): unknown {
  switch (kind) {
    case "json_list": return [];
    case "json_object": return {};
    default: return "";
  }
}

export function promptReferenceValue(reference?: ReusableReference) {
  if (!reference) return "";
  switch (reference.source) {
    case "draft": return JSON.stringify(["draft", reference.key]);
    case "parameter": return JSON.stringify(["parameter", reference.kind, reference.name]);
    case "published": {
      const { destination, kind, id, revision } = reference.version;
      return JSON.stringify(["published", destination, kind, id, revision]);
    }
  }
}

export function promptEndpointForRename(endpoint: WorkflowEndpoint, affected: ReadonlySet<string>, rename: PromptPortRename) {
  return affected.has(endpoint.node) && endpoint.port === rename.from ? { ...endpoint, port: rename.to } : endpoint;
}

