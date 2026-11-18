import type { WorkflowDefinition, WorkflowEndpoint, WorkflowGraph, WorkflowNodeConfiguration, WorkflowPort } from "@/workflow-graph-model";
import type { ReusableReference } from "@/workflow-reusable";
import type { JsonSchema } from "@/workflow-schema-model";

type PromptInput = { kind: string; target?: string };
type PromptOutput = { kind: string; source?: string };
export type PromptContract = {
  text: string;
  executor: string;
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
export const defaultTypedPrompt = (): PromptContract => ({
  text: "", executor: "agent", inputs: {}, outputs: {}, routes: [], delegates: [], schemaContract: true,
});

export function promptNodeDefinition(name: string, base: WorkflowDefinition,
  schemas?: { input?: JsonSchema; output?: JsonSchema }, write = false): WorkflowDefinition {
  const control = (name: string): WorkflowPort => ({ name, value: { kind: "control" } });
  const data = (name: string, artifact: string): WorkflowPort => ({ name, value: { kind: "data", artifact } });
  return {
    ...base, title: name,
    inputs: [control("ready"), data("input", "json_object"), ...(write ? [
      data("release", "release"), data("worktree", "worktree"), data("plan", "plan"),
    ] : [])],
    outputs: [control("completed"), control("failed"), data("result", "json_object")],
    inputSchemas: schemas?.input ? { input: schemas.input } : undefined,
    outputSchemas: schemas?.output ? { result: schemas.output } : undefined,
  };
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

