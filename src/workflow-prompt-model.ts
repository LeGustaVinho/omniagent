import type { WorkflowDefinition, WorkflowEndpoint, WorkflowGraph, WorkflowNodeConfiguration, WorkflowPort } from "@/workflow-graph-model";
import type { ReusableReference } from "@/workflow-reusable";
import type { JsonSchema } from "@/workflow-schema-model";

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
  schemaContract?: boolean;
  nodePrefab?: boolean;
  roleOverrides?: Record<string, unknown>;
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
export const PROMPT_OUTPUTS = ["json_object", "json_list", "rendered_text"] as const;
export const defaultPromptContract = (): PromptContract => ({
  text: "", executor: "agent", inputs: {}, outputs: {}, routes: ["concluido"], delegates: [],
});

export const defaultTypedPrompt = (): PromptContract => ({
  ...defaultPromptContract(), routes: [], schemaContract: true,
});

export function hasPromptDecision(prompt: PromptContract) {
  return prompt.executor === "agent" && (prompt.routes.length !== 1 || prompt.routes[0] !== "concluido"
    || !!prompt.delegates.length || Object.values(prompt.outputs).some(({ source }) => !source) || !!prompt.outputSchema);
}

export function promptNodeDefinition(name: string, prompt: PromptContract, base: WorkflowDefinition,
  schemas?: { input?: JsonSchema; output?: JsonSchema }, write = false): WorkflowDefinition {
  const control = (name: string): WorkflowPort => ({ name, value: { kind: "control" } });
  const data = (name: string, artifact: string): WorkflowPort => ({ name, value: { kind: "data", artifact } });
  if (prompt.schemaContract) return {
    ...base, title: name,
    inputs: [control("ready"), data("input", "json_object"), ...(write ? [
      data("release", "release"), data("worktree", "worktree"), data("plan", "plan"),
    ] : [])],
    outputs: [control("completed"), control("failed"), data("result", "json_object")],
    inputSchemas: schemas?.input ? { input: schemas.input } : undefined,
    outputSchemas: schemas?.output ? { result: schemas.output } : undefined,
  };
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

