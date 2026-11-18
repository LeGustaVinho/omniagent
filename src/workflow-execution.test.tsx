// @vitest-environment jsdom

import { useState, type ComponentType } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { Job } from "@/bindings/Job";
import PipelinesWorkspace from "@/pipelines-workspace";
import { WorkflowExecutionAgent } from "@/workflow-execution-agent";
import { WorkflowDiffStage } from "@/workflow-execution-diff";
import { WorkflowNodeEvidence } from "@/workflow-execution-evidence";
import { executionCanvasGraph, needsExecutionRefresh, presentation, wiredExecutionInputs, type ExecutionJob, type ExecutionOutput,
  type ExecutionPinnedNode, type WorkflowExecutionSnapshot } from "@/workflow-execution-model";
import { testCheckout } from "@/test-fixtures/checkout";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@xyflow/react", async (original) => {
  type CanvasNode = { id: string; data: unknown; ariaLabel: string };
  return {
    ...await original<typeof import("@xyflow/react")>(),
    Handle: ({ id }: { id: string }) => <span data-port={id} />,
    Controls: () => null,
    Background: () => null,
    ReactFlow: ({ nodes, nodeTypes, onNodeClick, onNodeContextMenu }: {
      nodes: CanvasNode[]; nodeTypes: { execution: ComponentType<CanvasNode> }; onNodeClick: (event: unknown, node: CanvasNode) => void;
      onNodeContextMenu?: (event: React.MouseEvent, node: CanvasNode) => void;
    }) => <div>{nodes.map((node) => <button type="button" key={node.id} aria-label={node.ariaLabel} onClick={() => onNodeClick(undefined, node)}
      onContextMenu={(event) => onNodeContextMenu?.(event, node)}>
      <nodeTypes.execution {...node} />
    </button>)}</div>,
  };
});

afterEach(() => { cleanup(); vi.resetAllMocks(); });

const projectId = testCheckout.projectId;
const runId = "workflow-original";
const version = { id: "pipeline", revision: "immutable-original-graph" };
const logical = { planRevision: "selected-plan", taskId: "linked-task", unit: { id: "logical-one", ordinal: 1, purpose: "First delivery" } };
const control = (name: string) => ({ name, value: { kind: "control" as const } });
const port = (name: string, artifact: string) => ({ name, value: { kind: "data" as const, artifact } });
function node(id: string, title: string, inputs: ExecutionPinnedNode["definition"]["inputs"] = [], outputs: ExecutionPinnedNode["definition"]["outputs"] = []): ExecutionPinnedNode {
  return { instance: { id, kind: id, configuration: { kind: "none" } }, revision: `${id}-locked`,
    definition: { title, kind: id, revision: 1, inputs, outputs, configuration: "none", executor: "fixture", effect: "none" } };
}
function job(nodeId: string, attempts: ExecutionJob["attempts"], kind: Job["kind"] = "agent"): ExecutionJob {
  const last = attempts.at(-1);
  return { nodeId, unitId: null, inputs: nodeId === "planner" ? { context: "original-markdown" } : {}, attempts,
    job: { id: `job-${nodeId}`, projectId, checkoutId: testCheckout.checkoutId, taskId: null, worktreeOwnerTaskId: null, origin: { type: "pipeline" },
      kind, pipeline: { pipelineRunId: runId, nodeInstanceId: nodeId, planRevision: null, release: null }, context: null,
      mode: "read_only", priority: 0, queueState: "queued", queuedRecovery: null, downstreamWait: null,
      latestAttempt: last ? { ...last, cancelRequested: false, recovery: last.previousRunId ? "retry" : null } : null } };
}
const output = (nodeInstanceId: string, id: string, port: string, kind: string, evidence: unknown, unitId: string | null = null): ExecutionOutput => ({
  current: true, inputReceipts: [], artifact: { id, nodeInstanceId, port, value: { kind, evidence }, unitId, workflowRunId: runId },
});

it("refreshes a conversation run between job completion and the End result", () => {
  const agent = node("conversation_agent", "Conversation agent");
  const completed = job(agent.instance.id, [{ runId: "native-attempt", ordinal: 1, previousRunId: null,
    conversationId: "chat", status: "succeeded" }]);
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, projectId, version, planRevision: null },
    definition: { version, name: "Direct conversation", nodes: [agent], wires: [] },
    units: [], unitNodeIds: [], jobs: [completed], outputs: [], control: [], conversationTurnActive: true,
  };
  expect(needsExecutionRefresh(snapshot)).toBe(true);
  expect(needsExecutionRefresh({ ...snapshot, conversationTurnActive: false })).toBe(false);
});

it("shows a Function as one live graph node with its published result and internal graph", async () => {
  const start = node("start", "Start", [], [control("start")]);
  const inner = node("summarize::execute", "AI Node Agent", [control("ready")],
    [control("completed"), port("result", "json_object")]);
  const end = node("end", "End", [control("ready"), port("result", "json_object")]);
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, projectId, version, planRevision: null },
    definition: {
      version, name: "Conversation workflow", nodes: [start, inner, end], wires: [
        { from: { node: "start", port: "start" }, to: { node: inner.instance.id, port: "ready" } },
        { from: { node: inner.instance.id, port: "completed" }, to: { node: "end", port: "ready" } },
        { from: { node: inner.instance.id, port: "result" }, to: { node: "end", port: "result" } },
      ],
      subflows: [{
        path: "summarize", name: "Summarize request",
        version: { id: "summary", revision: "published-summary", kind: "subflow", destination: "project" },
        nodes: [inner.instance.id],
        inputs: { begin: [{ node: inner.instance.id, port: "ready" }] },
        outputs: {
          completed: { node: inner.instance.id, port: "completed" },
          reply: { node: inner.instance.id, port: "result" },
        },
      }],
    },
    units: [], unitNodeIds: [], jobs: [],
    control: [{ nodeId: inner.instance.id, state: "completed", selectedPorts: ["completed"] }],
    outputs: [output(inner.instance.id, "summary-artifact", "result", "json_object", { reply: "A structured summary" })],
  };
  expect(executionCanvasGraph(snapshot.definition).wires).toEqual([
    { from: { node: "start", port: "start" }, to: { node: "summarize", port: "begin" } },
    { from: { node: "summarize", port: "completed" }, to: { node: "end", port: "ready" } },
    { from: { node: "summarize", port: "reply" }, to: { node: "end", port: "result" } },
  ]);
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "list_repository_deliveries") return [snapshot.run];
    if (command === "inspect_workflow_execution") return snapshot;
    throw new Error(command);
  });
  render(<QueryClientProvider client={new QueryClient()}><PipelinesWorkspace checkout={testCheckout}
    target={`run:${runId}`} openWorkspace={vi.fn()}
    openActivity={vi.fn()} openChanges={vi.fn()} /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Summarize request · Completed" }));
  expect(screen.queryByRole("button", { name: "AI Node Agent · Completed" })).toBeNull();
  expect(within(screen.getByRole("region", { name: "Result on reply" })).getByText(/A structured summary/)).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Open internal graph" }));
  expect(screen.getByRole("button", { name: "AI Node Agent · Completed" })).toBeTruthy();
});

it("opens a legacy run link in Runs and navigates between the parent backlog and task run", async () => {
  const backlog = node("delivery", "Run backlog");
  const sourceCommit = "b".repeat(40);
  const childId = "pinned-child-run";
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, projectId, version, planRevision: null },
    definition: { version, name: "Backlog workflow", nodes: [backlog], wires: [] },
    units: [], unitNodeIds: [], jobs: [], outputs: [], control: [],
    backlogExecutions: [{ nodeId: backlog.instance.id, backlogArtifactId: "backlog-revision", reason: "Waiting for a verified merge.",
      state: "pending", tasks: [{ key: "DOC-2", taskId: "task-two", workflowRunId: childId }] }],
  };
  const child: WorkflowExecutionSnapshot = { ...snapshot, run: { ...snapshot.run, id: childId }, backlogExecutions: [],
    launch: { sourceCommit, origin: { kind: "backlog", parentRunId: runId, nodeId: backlog.instance.id, workId: "task-two" } } };
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    if (command === "list_repository_deliveries") return [snapshot.run, child.run];
    if (command === "inspect_workflow_execution") return (input as { runId: string }).runId === childId ? child : snapshot;
    throw new Error(command);
  });
  const openWorkspace = vi.fn();
  function Harness() {
    const [target, setTarget] = useState(runId);
    return <PipelinesWorkspace checkout={testCheckout} target={target}
      openWorkspace={(workspace, next) => { openWorkspace(workspace, next); setTarget(next ?? ""); }}
      openActivity={vi.fn()} openChanges={vi.fn()} />;
  }
  render(<QueryClientProvider client={new QueryClient()}><Harness /></QueryClientProvider>);
  expect(await screen.findByText("Waiting for a verified merge.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Runs" }).getAttribute("aria-current")).toBe("page");
  expect(screen.queryByRole("button", { name: "Open delivery controls" })).toBeNull();
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Open run for DOC-2" }));
  expect(openWorkspace).toHaveBeenLastCalledWith("pipelines", `run:${childId}`);
  expect(await screen.findByTitle(sourceCommit)).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Open parent backlog" }));
  expect(openWorkspace).toHaveBeenLastCalledWith("pipelines", `run:${runId}?node=delivery`);
  expect(needsExecutionRefresh(snapshot)).toBeTruthy();
  expect(needsExecutionRefresh(child)).toBeFalsy();
});

it("keeps wait text scoped to its unit and resolves the original artifact forwarded through Repeat until", () => {
  const repeat = node("repeat", "Repeat until", [], [port("approved", "text")]);
  const next = node("next", "Verify", [port("result", "text")]);
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, projectId, version, planRevision: logical.planRevision },
    definition: { version, name: "Accepted iteration", nodes: [repeat, next], wires: [
      { from: { node: repeat.instance.id, port: "approved" }, to: { node: next.instance.id, port: "result" } },
    ] },
    units: [logical], unitNodeIds: [next.instance.id], jobs: [],
    outputs: [output("review", "first-accepted", "answer", "text", "Approved")],
    control: [{ nodeId: repeat.instance.id, planRevision: logical.planRevision, state: "completed", selectedPorts: ["completed"],
      forwardedOutputs: { approved: { input: "review", artifactId: "first-accepted" } } }],
    testPhaseWaits: [{ nodeId: next.instance.id, unitId: "other-unit", reason: "Another unit's check" },
      { nodeId: next.instance.id, unitId: logical.unit.id, reason: null }],
  };
  expect(wiredExecutionInputs(snapshot, next, logical)).toEqual({ result: "first-accepted" });
  expect(presentation(snapshot, next, logical)).toEqual({ status: "waiting", reason: "Waiting for the registered test check" });
});

it("opens the real Builder from the Runs empty state", async () => {
  vi.mocked(invoke).mockResolvedValue([]);
  const openWorkspace = vi.fn();
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <PipelinesWorkspace checkout={testCheckout} target="runs"
      openWorkspace={openWorkspace} openActivity={vi.fn()} openChanges={vi.fn()} />
  </QueryClientProvider>);
  const runs = within(screen.getByRole("region", { name: "Workflow runs" }));
  await runs.findByRole("heading", { name: "No runs yet" });
  await userEvent.setup().click(runs.getByRole("button", { name: "Open builder" }));
  expect(openWorkspace).toHaveBeenCalledWith("pipelines", "drafts");
});

it("navigates from the context of the clicked saved run", async () => {
  vi.mocked(invoke).mockResolvedValue([
    { id: "older-execution", projectId, planRevision: null, version: { ...version, revision: "earlier" } },
    { id: runId, projectId, planRevision: "exact-plan", version },
  ]);
  const openWorkspace = vi.fn();
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <PipelinesWorkspace checkout={testCheckout} target="runs"
      openWorkspace={openWorkspace} openActivity={vi.fn()} openChanges={vi.fn()} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  const list = within(await screen.findByRole("region", { name: "Saved workflow runs" }));
  fireEvent.contextMenu(list.getByRole("button", { name: /^workflow-original/ }));
  await user.click(await screen.findByRole("menuitem", { name: "Copy plan revision" }));
  expect(await navigator.clipboard.readText()).toBe("exact-plan");
  fireEvent.contextMenu(list.getByRole("button", { name: /^older-execution/ }));
  await user.click(await screen.findByRole("menuitem", { name: "Open execution graph" }));
  expect(openWorkspace).toHaveBeenCalledExactlyOnceWith("pipelines", "run:older-execution");
});

it.each(["checkbox", "context menu"])("hands saved responses from the %s to the exact plan review before queueing", async (entry) => {
  const plan = node("planner", "Plan", [], [port("plan", "plan")]);
  const review = node("review_plan", "Plan review", [port("plan", "plan")], [port("review", "review")]);
  const planRevision = "saved-plan";
  const origin = "plan-attempt";
  const sourceCheckoutId = "original-comparison-checkout";
  const reviewer = { provider: "codex", profileId: "reviewer-profile", modelId: "reviewer-model" } as const;
  const planJob = job(plan.instance.id, [{ runId: origin, previousRunId: null, conversationId: null, status: "succeeded", ordinal: 1 }]);
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, version, projectId, planRevision },
    definition: { version, name: "Authored workflow", nodes: [plan, review], wires: [
      { from: { node: plan.instance.id, port: "plan" }, to: { node: review.instance.id, port: "plan" } },
    ] },
    outputs: [output(plan.instance.id, planRevision, "plan", "plan", { run_id: origin, event_sequence: 2 })],
    unitNodeIds: [], units: [], control: [],
    jobs: [{ ...planJob, job: { ...planJob.job, checkoutId: sourceCheckoutId } }],
    rolePins: [{
      nodeId: review.instance.id, nodeRevision: review.revision, requirementsOnly: false,
      version: { destination: "project", kind: "role", id: "reviewer", revision: "reviewer-r1" },
      role: { name: "Reviewer", model: reviewer, effort: null, toolScope: "read_only", reviewPolicy: "cross_review" },
    }],
  };
  const wrongCheckout = { ...snapshot, run: { ...snapshot.run, id: "foreign-checkout-run" },
    jobs: snapshot.jobs.map((row) => ({ ...row, job: { ...row.job, checkoutId: testCheckout.checkoutId } })) };
  const commit = "a".repeat(40);
  const group = {
    id: "review-comparison", sourceCheckoutId, commit, tree: "worktree",
    prompt: "Review the exact document", candidates: [
      { selection: reviewer, modelReference: { name: "Source candidate", rating: null },
        jobId: "source-job", runId: "source-result", dispatchId: "source-dispatch", state: "succeeded" },
      { selection: reviewer, modelReference: { name: "Other candidate", rating: null },
        jobId: "other-job", runId: "other-result", dispatchId: "other-dispatch", state: "succeeded" },
      { selection: reviewer, modelReference: { name: "Failed candidate", rating: null },
        jobId: "failed-job", runId: "failed-run", dispatchId: null, state: "failed" },
    ],
  };
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    switch (command) {
      case "conversation_models": return (input as { provider: string }).provider === "codex"
        ? [{ option: { selection: reviewer, name: "Reviewer model", selectable: true, rating: null } }] : [];
      case "comparison_source_commit": return commit;
      case "model_comparisons": return [group];
      case "model_comparison_outcomes": return ["source", "other"].map((id) => ({
        jobId: `${id}-job`, runId: `${id}-result`, response: `Saved ${id} response`, reasoning: [], failure: null,
        reportedModel: null, durationSeconds: null, inputTokens: null, outputTokens: null, cachedInputTokens: null, reasoningTokens: null,
      }));
      case "model_comparison_environments": return [];
      case "list_repository_deliveries": return [wrongCheckout.run, snapshot.run];
      case "inspect_workflow_execution": return (input as { runId: string }).runId === runId ? snapshot : wrongCheckout;
      case "submit_repository_delivery_agent": return job(review.instance.id, []).job;
      default: throw new Error(command);
    }
  });
  function Harness() {
    const [target, setTarget] = useState("compare");
    return <PipelinesWorkspace checkout={testCheckout} target={target}
      openWorkspace={(_workspace, next) => setTarget(next ?? "")} openActivity={vi.fn()} openChanges={vi.fn()} />;
  }
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <Harness />
  </QueryClientProvider>);
  const user = userEvent.setup();
  const results = within(await screen.findByRole("list", { name: "Comparison results" }));
  const [sourceChoice, otherChoice] = await results.findAllByRole<HTMLInputElement>("checkbox", { name: "Use this result" });
  if (entry === "context menu") {
    await user.click(otherChoice!);
    fireEvent.contextMenu(results.getByRole("heading", { name: "Failed candidate" }));
    expect(screen.queryByRole("menuitem", { name: "Use this result in pipeline…" })).toBeNull();
    await user.keyboard("{Escape}");
    fireEvent.contextMenu(results.getByRole("heading", { name: "Source candidate" }));
    await user.click(await screen.findByRole("menuitem", { name: "Use this result in pipeline…" }));
    expect(otherChoice!.checked).toBe(true);
    expect(sourceChoice!.checked).toBe(false);
  } else {
    await user.click(sourceChoice!);
    await user.click(screen.getByRole("button", { name: "Use in pipeline" }));
  }
  const destination = within(screen.getByRole("region", { name: "Comparison destination" }));
  const runs = await destination.findByRole("combobox", { name: "Workflow run" });
  await user.selectOptions(runs, wrongCheckout.run.id);
  expect(await destination.findByText(/no eligible plan-review stage/)).toBeTruthy();
  expect(destination.getByRole<HTMLButtonElement>("button", { name: "Open plan review" }).disabled).toBe(true);
  await user.selectOptions(runs, runId);
  await user.selectOptions(await destination.findByRole("combobox", { name: "Plan-review stage" }), review.instance.id);
  await user.click(destination.getByRole("button", { name: "Open plan review" }));
  const inspector = within(await screen.findByRole("complementary", { name: "Execution node evidence" }));
  expect((await inspector.findByRole<HTMLInputElement>("checkbox", { name: /source-result/ })).checked).toBe(true);
  expect(inspector.getByRole<HTMLInputElement>("checkbox", { name: /other-result/ }).checked).toBe(false);
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "submit_repository_delivery_agent")).toBe(false);
  const queue = inspector.getByRole<HTMLButtonElement>("button", { name: "Queue plan review" });
  await waitFor(() => expect(queue.disabled).toBe(false));
  await user.click(queue);
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("submit_repository_delivery_agent", { input: {
    projectId, runId, nodeId: review.instance.id, unitId: null, inputs: { plan: planRevision }, selection: reviewer, effort: null,
    comparison: { groupId: group.id, runIds: ["source-result"] },
  } }));
});

it("queues an authored planner with the pinned role model and exact wired context", async () => {
  const source = node("frozen-context", "Saved context", [], [control("loaded"), port("context", "markdown")]);
  const plan = node("design-stage", "Design plan", [control("ready"), port("context", "markdown")], [port("plan", "plan")]);
  plan.instance.kind = "plan";
  const model = { provider: "codex", profileId: "planner-profile", modelId: "planner-model" } as const;
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, version, projectId, planRevision: null },
    definition: { version, name: "Authored workflow", nodes: [source, plan], wires: [
      { from: { node: source.instance.id, port: "loaded" }, to: { node: plan.instance.id, port: "ready" } },
      { from: { node: source.instance.id, port: "context" }, to: { node: plan.instance.id, port: "context" } },
    ] },
    outputs: [
      output(source.instance.id, "selected-document", "context", "markdown", {}),
      { ...output(source.instance.id, "superseded-document", "context", "markdown", {}), current: false },
    ],
    rolePins: [{
      nodeId: plan.instance.id, nodeRevision: plan.revision,
      version: { destination: "project", kind: "role", id: "planner", revision: "planner-r1" }, requirementsOnly: false,
      role: { name: "Planner", model, effort: "high", toolScope: "plan", reviewPolicy: "independent" },
    }],
    unitNodeIds: [], units: [], jobs: [], control: [],
  };
  vi.mocked(invoke).mockImplementation(async (name, input) => {
    if (name === "conversation_models") return (input as { provider: string }).provider === "codex"
      ? [{ option: { selection: model, name: "Planner profile", selectable: true, rating: null } }] : [];
    if (name === "submit_repository_delivery_agent") return job(plan.instance.id, []).job;
    throw new Error(name);
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <WorkflowExecutionAgent snapshot={snapshot} node={plan} openActivity={vi.fn()} />
  </QueryClientProvider>);
  const queue = screen.getByRole<HTMLButtonElement>("button", { name: "Queue design plan" });
  await waitFor(() => expect(queue.disabled).toBe(false));
  await userEvent.setup().click(queue);
  expect(invoke).toHaveBeenCalledWith("submit_repository_delivery_agent", {
    input: { projectId, runId, nodeId: plan.instance.id, unitId: null, inputs: { context: "selected-document" }, selection: model, effort: "high" },
  });
});

it("queues a generic Agent with its exact prompt and an unconnected optional comparison port", async () => {
  const source = node("source", "Compose request", [], [control("completed"), port("text", "rendered_text")]);
  const agent = node("agent", "Summarize request", [control("ready"), port("prompt", "rendered_text"), port("comparison", "comparison_results")]);
  const model = { provider: "codex", profileId: "reader-profile", modelId: "reader-model" } as const;
  agent.instance.configuration = { kind: "agent_role", role: "Summarizer", model };
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, version, projectId, planRevision: null },
    definition: { version, name: "Authored Agent", nodes: [source, agent], wires: [
      { from: { node: source.instance.id, port: "completed" }, to: { node: agent.instance.id, port: "ready" } },
      { from: { node: source.instance.id, port: "text" }, to: { node: agent.instance.id, port: "prompt" } },
    ] },
    outputs: [output(source.instance.id, "selected-prompt", "text", "rendered_text", "Saved request")],
    unitNodeIds: [], units: [], jobs: [], control: [],
  };
  vi.mocked(invoke).mockImplementation(async (name, input) => {
    if (name === "conversation_models") return (input as { provider: string }).provider === "codex"
      ? [{ option: { selection: model, name: "Reader profile", selectable: true, rating: null } }] : [];
    if (name === "submit_repository_delivery_agent") return job(agent.instance.id, []).job;
    throw new Error(name);
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <WorkflowExecutionAgent snapshot={snapshot} node={agent} openActivity={vi.fn()} />
  </QueryClientProvider>);
  const queue = screen.getByRole<HTMLButtonElement>("button", { name: "Queue summarize request" });
  await waitFor(() => expect(queue.disabled).toBe(false));
  await userEvent.setup().click(queue);
  expect(invoke).toHaveBeenCalledWith("submit_repository_delivery_agent", {
    input: { projectId, runId, nodeId: agent.instance.id, unitId: null, inputs: { prompt: "selected-prompt" }, selection: model, effort: null },
  });
});

it("records a generic Goal criterion against its saved continuation checkpoint", async () => {
  const source = node("source", "Compose request", [], [port("text", "rendered_text")]);
  const agent = node("agent", "Review request", [port("prompt", "rendered_text")], [port("response", "agent_response")]);
  agent.instance.configuration = { kind: "agent_role", role: "Reviewer", requirements: {
    goal: { objective: "Review the changes", criteria: [{ text: "Assess the rollout", evidence: "operator_review" }], maxTurns: 3 },
  } };
  const attempt = { runId: "initial-agent-turn", ordinal: 1, previousRunId: null, conversationId: "goal-chat", status: "succeeded" as const };
  const recorded = job(agent.instance.id, [attempt]);
  recorded.inputs = { prompt: "selected-prompt" };
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, version, projectId, planRevision: null },
    definition: { version, name: "Authored Goal", nodes: [source, agent], wires: [
      { from: { node: source.instance.id, port: "text" }, to: { node: agent.instance.id, port: "prompt" } },
    ] },
    outputs: [output(source.instance.id, "selected-prompt", "text", "rendered_text", "Saved request")],
    unitNodeIds: [], units: [], jobs: [recorded], control: [],
  };
  const checkpoint = "continuation-review-turn";
  const evidence = { kind: "operator_review", approved: true, note: null };
  let verified = false;
  const verification = () => ({ runId: checkpoint, status: verified ? "completed" : "verifying", criteria: [
    { ordinal: 1, text: "Assess the rollout", status: verified ? "passed" : "pending", evidence: verified ? evidence : null },
  ] });
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "run_orchestration") {
      expect(args).toEqual({ projectId, runId: attempt.runId, conversationId: attempt.conversationId });
      return { goal: { controlState: "active", checkpoint: { runId: checkpoint, nextAction: "review" } } };
    }
    if (command === "goal_verification") {
      expect(args).toEqual({ projectId, runId: checkpoint });
      return verification();
    }
    if (command === "list_goal_criterion_evidence") return [];
    if (command === "record_goal_criterion_evidence") {
      expect(args).toEqual({ projectId, runId: checkpoint, ordinal: 1, evidence });
      verified = true;
      return verification();
    }
    throw new Error(`Unexpected ${command}`);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const refresh = vi.spyOn(client, "invalidateQueries");
  render(<QueryClientProvider client={client}><WorkflowExecutionAgent snapshot={snapshot} node={agent} openActivity={vi.fn()} /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.selectOptions(await screen.findByLabelText("Evidence for criterion 1"), "approve");
  await user.click(screen.getByRole("button", { name: "Record evidence" }));
  expect(await screen.findByText("Verified and complete")).toBeTruthy();
  expect(refresh).toHaveBeenCalledWith({ queryKey: ["repository-delivery", projectId] });
});

it("reads only the recorded agent response and drops it on history invalidation", async () => {
  const agent = node("agent", "Summarize request");
  const artifact = output(agent.instance.id, "agent-output", "response", "agent_response", { runId: "recorded-agent-run" });
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, version, projectId, planRevision: null },
    definition: { version, name: "Recorded agent", nodes: [agent], wires: [] },
    outputs: [artifact], unitNodeIds: [], units: [], jobs: [], control: [],
  };
  let deleted = false;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    expect(command).toBe("workflow_agent_response");
    expect(args).toEqual({ projectId, runId, artifactId: "agent-output" });
    if (deleted) throw new Error("The recorded response is unavailable.");
    return "Original agent text";
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const navigation = { checkout: testCheckout, openWorkspace: vi.fn(), openActivity: vi.fn() };
  render(<QueryClientProvider client={client}>
    <WorkflowNodeEvidence snapshot={snapshot} node={agent} navigation={navigation} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  expect(invoke).not.toHaveBeenCalled();
  await user.click(screen.getByText("Read recorded agent response"));
  expect(await screen.findByText("Original agent text")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Open original agent response" }));
  expect(navigation.openActivity).toHaveBeenCalledWith("recorded-agent-run");
  deleted = true;
  await client.invalidateQueries({ queryKey: ["conversation"] });
  expect(await screen.findByRole("alert")).toBeTruthy();
  expect(screen.queryByText("Original agent text")).toBeNull();
});

it("sends a selected Git candidate with its exact base, tree and pinned document input", async () => {
  const source = node("start", "Pinned source", [], [control("start"), port("document", "markdown")]);
  const read = node("capture-diff", "Read Git", [control("ready"), port("document", "markdown")]);
  read.instance.kind = read.definition.kind = "read_git";
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, version, projectId, planRevision: null },
    definition: { version, name: "Review diff", nodes: [source, read], wires: [
      { from: { node: source.instance.id, port: "start" }, to: { node: read.instance.id, port: "ready" } },
      { from: { node: source.instance.id, port: "document" }, to: { node: read.instance.id, port: "document" } },
    ] },
    outputs: [output("start", "pinned-doc", "document", "markdown", {})],
    unitNodeIds: [], units: [], jobs: [], control: [],
  };
  const scope = { projectId, runId, nodeId: read.instance.id };
  vi.mocked(invoke).mockImplementation(async (name) => {
    if (name === "workflow_diff_candidates") return [
      { id: "frozen-selection", baseCommit: "frozen-base", tree: "frozen-tree", branch: "managed/feature", target: { taskId: "target-task" } },
    ];
    if (name === "execute_workflow_diff_stage") return {};
    throw new Error(name);
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <WorkflowDiffStage snapshot={snapshot} node={read} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  const apply = screen.getByRole<HTMLButtonElement>("button", { name: "Read selected Git diff" });
  expect(apply.disabled).toBe(true);
  await user.selectOptions(screen.getByLabelText("Saved Git candidate"), await screen.findByRole("option", { name: /managed\/feature/ }));
  expect(screen.getByText("frozen-base")).toBeTruthy();
  expect(screen.getByText("frozen-tree")).toBeTruthy();
  await user.click(apply);
  expect(invoke).toHaveBeenCalledWith("workflow_diff_candidates", scope);
  expect(invoke).toHaveBeenCalledWith("execute_workflow_diff_stage", {
    ...scope, inputs: { document: "pinned-doc" },
    action: { action: "read_git", taskId: "target-task", candidateId: "frozen-selection", expectedBase: "frozen-base", expectedTree: "frozen-tree" },
  });
});

it("shows real pinned stages, failure and dependency evidence, and opens the exact attempt and Git or Agile records", async () => {
  const start = node("start", "Pinned source", [], [control("start"), port("document", "markdown")]);
  const plan = node("planner", "Historical plan", [port("context", "markdown")], [port("plan", "plan")]);
  const review = node("review", "Review pinned plan", [port("plan", "plan")]);
  const approval = node("approval", "Saved plan approval", [], [port("decision", "approval")]);
  approval.instance.kind = "approve_implementation";
  const commit = node("commit", "Verified Git", [], [port("git", "git")]);
  const jobs = [
    job("planner", [{ runId: "attempt-first", ordinal: 1, previousRunId: null, conversationId: "original-conversation", status: "failed" },
      { runId: "attempt-retry", ordinal: 2, previousRunId: "attempt-first", conversationId: "retry-conversation", status: "failed" }]),
    job("queued", [], "command"),
    job("running", [{ runId: "command-now", ordinal: 1, previousRunId: null, conversationId: null, status: "running" }], "command"),
    job("command", [{ runId: "command-saved", ordinal: 1, previousRunId: null, conversationId: null, status: "succeeded" }], "command"),
    job("command-failed", [{ runId: "command-invalid", ordinal: 1, previousRunId: null, conversationId: null, status: "failed" }], "command"),
    job("question", [{ runId: "pending-question", ordinal: 1, previousRunId: null, conversationId: "questions", status: "waiting_for_input" }]),
  ];
  const task = { taskId: "linked-task", projectId };
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, version, planRevision: "selected-plan", projectId },
    launch: { origin: { kind: "rule", workId: "linked-subtask" },
      checklistItem: { itemId: "trigger-item", revision: 4, checked: true } },
    definition: { version, name: "Recorded pipeline title",
      subflows: [{ path: "quality", name: "Pinned quality checks", version: {
        id: "quality-flow", revision: "quality-revision", destination: "project", kind: "subflow",
      }, nodes: ["command"], outputs: { report: { node: "command", port: "result" } } }],
      nodes: [start, plan, review, approval, commit,
      node("queued", "Queued command"), node("running", "Active command"), node("question", "User question"),
      node("command", "Saved command", [], [port("result", "command_result")]),
      node("command-failed", "Invalid command", [], [control("completed"), control("failed"), port("result", "command_result")]),
      node("remediation", "Remediation", [control("ready")]), node("unselected", "Success branch", [control("ready")])],
      wires: [{ from: { node: "start", port: "document" }, to: { node: "planner", port: "context" } },
        { from: { node: "planner", port: "plan" }, to: { node: "review", port: "plan" } },
        { from: { node: "command-failed", port: "failed" }, to: { node: "remediation", port: "ready" } },
        { from: { node: "command-failed", port: "completed" }, to: { node: "unselected", port: "ready" } }] },
    rolePins: [{
      nodeId: review.instance.id, nodeRevision: review.revision,
      version: { destination: "library", kind: "role", id: "reviewer-role", revision: "saved-role-revision" },
      requirementsOnly: false,
      role: { name: "Historical reviewer", model: { provider: "codex", profileId: "reviewer", modelId: "old-model" },
        effort: null, toolScope: "read_only", reviewPolicy: "independent" },
    }],
    unitNodeIds: ["commit"], units: [logical], jobs,
    control: [
      { nodeId: "command-failed", planRevision: "selected-plan", state: "failed", selectedPorts: ["failed"],
        reason: "The recorded command failure selected the error branch", sourceRunId: "command-invalid" },
      { nodeId: "unselected", planRevision: "selected-plan", state: "skipped", selectedPorts: [],
        reason: "No execution reaches this stage through the selected branches",
        dependencies: [{ from: { node: "command-failed", port: "completed" }, toPort: "ready", selected: false }] },
      { nodeId: "review", planRevision: "older-plan", state: "skipped", selectedPorts: [], reason: "Earlier revision outcome" },
    ],
    outputs: [
      output("start", "original-markdown", "document", "markdown", { relativePath: "old-brief.md", id: "frozen-source" }),
      { ...output("approval", "approval-receipt", "decision", "approval", { task, source: "automatic_rule", planRevision: "selected-plan", ruleEvidence: { revision: "rule-reviewed" } }),
        inputReceipts: [{ executionKey: "selected-plan", revision: "pinned-approval-input", inputs: { context: "original-markdown" } }] },
      output("commit", "git-receipt", "git", "git", { id: "verified-commit", task }, "logical-one"),
      output("command", "command-receipt", "result", "command_result", { runId: "command-saved", output: { format: "json", value: { count: 2 } } }),
    ],
  };
  let approvalPreview = { revision: "rule-reviewed", settings: { mode: "automatic", taskId: task.taskId,
    sourceCheckoutId: testCheckout.checkoutId, maximumUnits: 2, ifUnmet: "wait" },
    facts: { planRevision: "selected-plan", taskId: task.taskId, sourceCheckoutId: testCheckout.checkoutId, unitCount: 2 },
    outcome: "conditions_met", allowsHuman: false, decisionArtifactId: "approval-receipt" as string | null };
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    switch (command) {
      case "workflow_trigger_rules": return [];
      case "workflow_trigger_activations": return [{ ruleId: "trigger-rule", ruleName: "Review", ruleRevision: 1,
        eventId: "trigger-event", workId: "linked-subtask", runId, outcome: "launched", recordedAt: "2026-11-16T00:00:00Z" }];
      case "list_repository_deliveries": return [snapshot.run, { ...snapshot.run, id: "newer-run", version: { id: "pipeline", revision: "new-edited-draft" } }];
      case "inspect_workflow_execution": expect(input).toEqual({ projectId, runId }); return snapshot;
      case "workflow_approval_stage":
        expect(input).toEqual({ projectId, runId, nodeId: "approval" });
        return { status: "ready", planId: "selected-plan", taskArtifactId: "linked-task-artifact",
          source: approvalPreview.decisionArtifactId ? "automatic_rule" : null, preview: approvalPreview };
      case "preview_workflow_approval":
        expect(input).toEqual({ submission: { projectId, runId, nodeId: "approval",
          inputs: { plan: "selected-plan", task: "linked-task-artifact" } } });
        return approvalPreview;
      case "set_workflow_approval_rule":
        expect(input).toEqual({
          submission: { projectId, runId, nodeId: "approval", inputs: { plan: "selected-plan", task: "linked-task-artifact" } },
          expectedRevision: "rule-reviewed", settings: { ...approvalPreview.settings, maximumUnits: 1 },
        });
        approvalPreview = { ...approvalPreview, revision: "updated-rule", decisionArtifactId: null,
          settings: { ...approvalPreview.settings, maximumUnits: 1 }, outcome: "unit_limit_exceeded" };
        return approvalPreview;
      case "inspect_workflow_execution_job": {
        const matched = jobs.find(({ job }) => job.id === (input as { jobId: string }).jobId)!;
        return { job: matched.job, blockers: [], recoveryActions: matched.nodeId === "planner" ? ["retry"] : [] };
      }
      case "job_failure": return { checkoutRoot: "D:\\frozen-checkout", logsAvailable: true, savedDiffAvailable: false,
        observation: { cause: "invalid_result", phase: "result_validation", process: null } };
      case "verified_delivery_commit": return { id: "verified-commit", projectId, taskId: "linked-task", branch: "archived-branch", sha: "fixed-sha", captureRunId: "frozen-git-attempt" };
      default: throw new Error(`Unexpected ${command}`);
    }
  });
  const props = { checkout: testCheckout,
    openWorkspace: vi.fn(), openActivity: vi.fn(), openChanges: vi.fn(), openQueueJob: vi.fn() };
  function NavigablePipelines() {
    const [target, setTarget] = useState<string | undefined>("automations");
    return <PipelinesWorkspace {...props} target={target} openWorkspace={(area, next) => {
      props.openWorkspace(area, next);
      if (area === "pipelines" && target === "automations") setTarget(next);
    }} />;
  }
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}><NavigablePipelines /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Open execution" }));
  expect(await screen.findByRole("heading", { name: "Recorded pipeline title" })).toBeTruthy();
  expect(screen.getByText("linked-subtask")).toBeTruthy();
  expect(screen.getByText("trigger-item")).toBeTruthy();
  expect(screen.getByText(/Revision 4 · Checked/)).toBeTruthy();
  const graph = within(screen.getByLabelText("Workflow execution graph"));
  expect(graph.getByRole("button", { name: "Pinned source · Completed" })).toBeTruthy();
  expect(graph.getByRole("button", { name: "Queued command · Queued" })).toBeTruthy();
  expect(graph.getByRole("button", { name: "Active command · Running" })).toBeTruthy();
  expect(graph.getByRole("button", { name: "User question · Waiting" })).toBeTruthy();
  const inspector = within(screen.getByRole("complementary", { name: "Execution node evidence" }));
  await user.click(graph.getByRole("button", { name: "Review pinned plan · Waiting" }));
  const role = within(inspector.getByRole("region", { name: "Pinned role template" }));
  expect(role.getByText(/Historical reviewer/)).toBeTruthy();
  expect(role.getByText("saved-role-revision")).toBeTruthy();
  expect(inspector.getByText("plan needs planner.plan")).toBeTruthy();
  await user.click(inspector.getByText(/Earlier control decision · skipped/));
  expect(inspector.getByText("Earlier revision outcome")).toBeTruthy();
  fireEvent.contextMenu(graph.getByRole("button", { name: "Historical plan · Failed" }));
  const attemptActions = await screen.findByRole("menuitem", { name: "Inspect attempt" });
  attemptActions.focus();
  await user.keyboard("{ArrowRight}");
  await user.click(await screen.findByRole("menuitem", { name: "Attempt 2 · attempt-retry" }));
  expect(props.openActivity).toHaveBeenCalledWith("attempt-retry", "retry-conversation");
  await user.click(graph.getByRole("button", { name: "Historical plan · Failed" }));
  expect(await inspector.findByText(/Permitted recovery · Retry saved input/)).toBeTruthy();
  await waitFor(() => expect(inspector.getAllByText("The structured result failed validation.")).toHaveLength(2));
  await user.click(inspector.getByText("Job inputs"));
  expect(inspector.getByText("original-markdown")).toBeTruthy();
  const history = within(inspector.getByRole("list", { name: "Attempt history" }));
  await user.click(within(history.getAllByRole("listitem")[0]!).getByRole("button", { name: "Open prompts and activity" }));
  expect(props.openActivity).toHaveBeenCalledWith("attempt-first", "original-conversation");
  await user.click(inspector.getByRole("button", { name: "Open queue job" }));
  expect(props.openQueueJob).toHaveBeenCalledWith("job-planner");
  await user.click(graph.getByRole("button", { name: "Saved plan approval · Completed" }));
  expect(await inspector.findByText("Automatic implementation approval recorded.")).toBeTruthy();
  expect(inspector.getByText("rule-reviewed")).toBeTruthy();
  await user.click(inspector.getByText("Implementation approval settings"));
  const limit = inspector.getByRole("spinbutton", { name: "Maximum logical units" });
  await user.clear(limit);
  await user.type(limit, "1");
  await user.click(inspector.getByRole("button", { name: "Save approval settings" }));
  expect(await inspector.findByText("The reviewed plan has 2 logical units. The rule permits 1.")).toBeTruthy();
  await user.click(inspector.getByRole("button", { name: "Open Agile work" }));
  expect(props.openWorkspace).toHaveBeenCalledWith("tasks", "linked-task");
  await user.click(graph.getByRole("button", { name: "Verified Git · Completed" }));
  expect(await inspector.findByText("fixed-sha")).toBeTruthy();
  await user.click(inspector.getByRole("button", { name: "Open saved diff" }));
  expect(props.openWorkspace).toHaveBeenCalledWith("git", "attempt:frozen-git-attempt");
  await user.click(graph.getByRole("button", { name: "Saved command · Completed" }));
  expect(inspector.getByText("command-receipt")).toBeTruthy();
  await user.click(inspector.getByRole("button", { name: "Open command output and logs" }));
  expect(props.openActivity).toHaveBeenCalledWith("command-saved");
  await user.click(graph.getByRole("button", { name: "Invalid command · Failed" }));
  expect(await inspector.findByText("No recovery action is currently available.")).toBeTruthy();
  expect(await inspector.findByText("The structured result failed validation.")).toBeTruthy();
  await user.click(inspector.getByRole("button", { name: "Open command logs" }));
  expect(props.openActivity).toHaveBeenCalledWith("command-invalid", undefined);
  expect(inspector.getByText("Selected ports · failed")).toBeTruthy();
  await user.click(inspector.getByRole("button", { name: "Open triggering attempt" }));
  expect(props.openActivity).toHaveBeenLastCalledWith("command-invalid", undefined);
  await user.click(graph.getByRole("button", { name: "Remediation · Waiting" }));
  expect(inspector.queryByLabelText("Waiting for dependencies")).toBeNull();
  await user.click(graph.getByRole("button", { name: "Success branch · Skipped" }));
  expect(inspector.getByText("No execution reaches this stage through the selected branches")).toBeTruthy();
  expect(inspector.getByText("ready · command-failed.completed · Skipped input")).toBeTruthy();
  await user.selectOptions(screen.getByRole("combobox", { name: "Pinned subflow" }), "quality");
  expect(graph.queryByRole("button", { name: "Pinned source · Completed" })).toBeNull();
  expect(graph.getByRole("button", { name: "Saved command · Completed" })).toBeTruthy();
  await user.click(screen.getByText("Pinned subflow outputs"));
  expect(screen.getByText("quality-revision")).toBeTruthy();
  await user.click(inspector.getByRole("button", { name: "Open command output and logs" }));
  expect(props.openActivity).toHaveBeenLastCalledWith("command-saved");
  await user.selectOptions(screen.getByRole("combobox", { name: "Saved workflow run" }), "newer-run");
  expect(props.openWorkspace).toHaveBeenCalledWith("pipelines", "run:newer-run");
});

it("shows a pending decision only for the pinned node in the current plan and selected unit", async () => {
  const source = node("start", "Pinned source", [], [port("document", "markdown")]);
  const approval = node("ask", "Release decision");
  const earlier = { ...logical, planRevision: "earlier-plan", unit: { ...logical.unit, purpose: "Earlier delivery" } };
  const active = {
    requestId: "current-request", nodeId: approval.instance.id, nodeRevision: approval.revision,
    planRevision: logical.planRevision, unitId: logical.unit.id, inputs: { document: "saved-source" },
    request: { kind: "approval" as const, message: "Review the current delivery." },
  };
  let snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, version, planRevision: logical.planRevision, projectId },
    definition: { version, name: "Decision scope", nodes: [source, approval], wires: [] },
    unitNodeIds: [approval.instance.id], units: [logical, earlier], jobs: [], control: [],
    outputs: [output(source.instance.id, "saved-source", "document", "markdown", { relativePath: "brief.md" })],
    pendingDecisions: [
      { ...active, requestId: "other-definition", nodeRevision: "old-node-revision",
        request: { kind: "approval", message: "Review the superseded node." } },
      { ...active, requestId: "earlier-request", planRevision: earlier.planRevision, unitId: earlier.unit.id,
        request: { kind: "approval", message: "Review the earlier delivery." } },
      active,
    ],
  };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "list_repository_deliveries") return [snapshot.run];
    if (command === "inspect_workflow_execution") return snapshot;
    if (command === "resolve_workflow_stage_decision") {
      expect(args).toEqual({ submission: { projectId, runId, requestId: active.requestId,
        decision: { kind: "approval", approved: true } } });
      snapshot = { ...snapshot, pendingDecisions: [], control: [{ nodeId: approval.instance.id,
        planRevision: logical.planRevision, unitId: logical.unit.id, state: "completed", selectedPorts: ["approved"] }] };
      return null;
    }
    throw new Error(`Unexpected ${command}`);
  });
  const props = { checkout: testCheckout, target: `run:${runId}`,
    openWorkspace: vi.fn(), openActivity: vi.fn(), openChanges: vi.fn() };
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}><PipelinesWorkspace {...props} /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "View waiting stage" }));
  const evidence = within(screen.getByRole("complementary", { name: "Execution node evidence" }));
  expect(evidence.getByText("Review the current delivery.")).toBeTruthy();
  expect(evidence.getByText("saved-source")).toBeTruthy();
  expect(evidence.queryByText("Review the superseded node.")).toBeNull();
  await user.selectOptions(screen.getByRole("combobox", { name: "Logical unit" }), screen.getByRole("option", { name: /Earlier delivery/ }));
  expect(evidence.queryByText("Review the earlier delivery.")).toBeNull();
  expect(evidence.queryByText("Review the current delivery.")).toBeNull();
  expect(evidence.queryByRole("button", { name: "Approve" })).toBeNull();
  await user.selectOptions(screen.getByRole("combobox", { name: "Logical unit" }), screen.getByRole("option", { name: new RegExp(logical.unit.purpose) }));
  await user.click(evidence.getByRole("button", { name: "Approve" }));
  expect(await screen.findByRole("button", { name: "Release decision · Completed" })).toBeTruthy();
  expect(evidence.queryByRole("button", { name: "Approve" })).toBeNull();
});

it("opens comparison outcomes for the exact wired input and preserves the candidate attempt link", async () => {
  const source = node("source", "Saved request", [], [port("text", "text")]);
  const compare = node("compare", "Review alternatives", [port("prompt", "text")], [port("results", "comparison_results")]);
  const stage = { nodeId: compare.instance.id, nodeRevision: compare.revision, groupId: "current-comparison",
    unitId: null, planRevision: null, inputs: { prompt: "current-prompt" }, state: "settled" as const, error: null };
  const selection = { provider: "codex", profileId: "desktop", modelId: "saved-model" };
  const candidates = [
    { selection, jobId: "compared-job", runId: "compared-run", dispatchId: "compared-dispatch", responseRevisionId: "saved-response", state: "succeeded" },
    { selection: { ...selection, modelId: "missing-response-model" }, jobId: "missing-response-job", runId: "missing-response-run",
      dispatchId: "missing-response-dispatch", responseRevisionId: null, state: "succeeded" },
  ];
  const recorded = { groupId: stage.groupId, commit: "pinned-comparison-commit", state: "partial", candidates };
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, version, planRevision: null, projectId },
    definition: { version, name: "Comparison lineage", nodes: [source, compare],
      wires: [{ from: { node: source.instance.id, port: "text" }, to: { node: compare.instance.id, port: "prompt" } }] },
    units: [], unitNodeIds: [], jobs: [], control: [], pendingDecisions: [],
    outputs: [output(source.instance.id, "current-prompt", "text", "text", "Saved prompt"),
      output(compare.instance.id, "current-results", "results", "comparison_results", recorded)],
    comparisonStages: [stage, { ...stage, groupId: "earlier-comparison", inputs: { prompt: "earlier-prompt" } },
      { ...stage, groupId: "other-unit-comparison", unitId: "other-unit", planRevision: "other-plan" }],
  };
  const groups = [{ id: stage.groupId, commit: recorded.commit, candidates: candidates.map((candidate) => ({
    ...candidate, conversationId: candidate.runId === "compared-run" ? "compared-chat" : "missing-response-chat",
  })) }];
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "list_repository_deliveries") return [snapshot.run];
    if (command === "inspect_workflow_execution") return snapshot;
    if (command === "model_comparisons") return groups;
    if (command === "model_comparison_outcomes") {
      expect(args).toEqual({ projectId, groupId: stage.groupId });
      return candidates.map(({ jobId, runId, responseRevisionId }) => ({ jobId, runId,
        response: responseRevisionId ? "Original candidate text" : "Unpinned response text",
        reasoning: [], reportedModel: "saved-model", durationSeconds: null, inputTokens: null, outputTokens: null,
        cachedInputTokens: null, reasoningTokens: null, failure: null }));
    }
    throw new Error(`Unexpected ${command}`);
  });
  const props = { checkout: testCheckout, target: `run:${runId}`,
    openWorkspace: vi.fn(), openActivity: vi.fn(), openChanges: vi.fn() };
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}><PipelinesWorkspace {...props} /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Review alternatives · Completed" }));
  const comparison = within(screen.getByRole("region", { name: "Workflow model comparison" }));
  expect(await comparison.findByText("Original candidate text")).toBeTruthy();
  expect(comparison.getByText("Partial result")).toBeTruthy();
  expect(comparison.getByText(/Response unavailable/)).toBeTruthy();
  expect(comparison.queryByText("Unpinned response text")).toBeNull();
  await user.click(within(comparison.getAllByRole("listitem")[0]!).getByRole("button", { name: "Open execution" }));
  expect(props.openActivity).toHaveBeenCalledWith("compared-run", "compared-chat");
});
