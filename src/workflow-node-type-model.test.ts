import { expect, it } from "vitest";
import type { WorkflowGraphNode } from "@/workflow-graph-model";
import { defaultTypedPrompt } from "@/workflow-prompt-model";
import type { ReusableReference, ReusableVersion } from "@/workflow-reusable";
import { nativeWorkflowNodeAppearance, publishedWorkflowNodeVersions, resolveWorkflowNodeTypes, workflowNodeVersionKey,
  type WorkflowNodeSource } from "@/workflow-node-type-model";

const node = (id: string, kind: string): WorkflowGraphNode => ({ id, kind, configuration: { kind: "none" } });
const call = node("call", "subflow");
const draft = (key: string): ReusableReference => ({ source: "draft", key });
const published = (version: ReusableVersion): ReusableReference => ({ source: "published", version });
const fn = (nodes: WorkflowGraphNode[], references: Record<string, ReusableReference> = {}): WorkflowNodeSource =>
  ({ body: { kind: "subflow", graph: { name: "Function", nodes, wires: [] } }, references });
const prefab: WorkflowNodeSource = { body: { kind: "prompt", prompt: { ...defaultTypedPrompt(), nodePrefab: true } } };

it("uses the pinned executor for agents and keeps native nodes that do not use AI distinct", () => {
  const writer = { kind: "agent", title: "Writer", revision: 3, configuration: "agent_role", inputs: [], outputs: [],
    executor: "write_agent", effect: "write_repository" };
  expect(nativeWorkflowNodeAppearance("agent", writer)).toEqual({ type: "ai", usesAI: true });
  for (const kind of ["start", "wait", "human", "fork"]) {
    expect(nativeWorkflowNodeAppearance(kind)).toEqual({ type: "no-ai", usesAI: false });
  }
  expect(nativeWorkflowNodeAppearance("unavailable")).toMatchObject({ type: "unknown" });
});

it("preserves the reusable type while resolving AI use through nested drafts and pinned revisions", () => {
  const version = { id: "reusable", kind: "subflow", revision: "original", destination: "library" } as const;
  const newVersion = { ...version, revision: "new" };
  const sources = new Map([
    [workflowNodeVersionKey(version), fn([node("wait", "wait")])],
    [workflowNodeVersionKey(newVersion), prefab],
  ]);
  const drafts = new Map([
    ["root", fn([call], { call: draft("nested") })],
    ["nested", fn([call, node("missing", "loop")], { call: published(newVersion) })],
    ["plain-agent", { body: { kind: "prompt", prompt: defaultTypedPrompt() } }],
    ["cycle", fn([call], { call: draft("cycle") })],
  ] satisfies [string, WorkflowNodeSource][]);
  const types = resolveWorkflowNodeTypes(sources, drafts);
  expect(types.node(call, undefined, draft("root"))).toEqual({ type: "function", usesAI: true });
  expect(types.node(call, undefined, draft("plain-agent"))).toEqual({ type: "ai", usesAI: true });
  expect(types.node(call, undefined, published(version))).toEqual({ type: "function", usesAI: false });
  expect(types.node(call, undefined, published(newVersion))).toEqual({ type: "prefab", usesAI: true });
  expect(types.node(node("loop", "loop"), undefined, draft("nested"))).toEqual({ type: "loop", usesAI: true });
  expect(types.node(call, undefined, draft("cycle"))).toEqual({ type: "function", usesAI: undefined });
  expect(types.node(call, undefined, published({ ...version, revision: "missing" }))).toEqual({ type: "reusable", usesAI: undefined });
  expect(publishedWorkflowNodeVersions(fn([call]).body.graph!, { call: draft("root") }, drafts)).toEqual([newVersion]);
});
