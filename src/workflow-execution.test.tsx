// @vitest-environment jsdom

import type { ComponentType } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { Job } from "@/bindings/Job";
import PipelinesWorkspace from "@/pipelines-workspace";
import type { ExecutionJob, ExecutionOutput, ExecutionPinnedNode, WorkflowExecutionSnapshot } from "@/workflow-execution-model";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@xyflow/react", async (original) => {
  type CanvasNode = { id: string; data: unknown; ariaLabel: string };
  return {
    ...await original<typeof import("@xyflow/react")>(),
    Handle: ({ id }: { id: string }) => <span data-port={id} />,
    Controls: () => null,
    Background: () => null,
    ReactFlow: ({ nodes, nodeTypes, onNodeClick }: {
      nodes: CanvasNode[]; nodeTypes: { execution: ComponentType<CanvasNode> }; onNodeClick: (event: unknown, node: CanvasNode) => void;
    }) => <div>{nodes.map((node) => <button type="button" key={node.id} aria-label={node.ariaLabel} onClick={() => onNodeClick(undefined, node)}>
      <nodeTypes.execution {...node} />
    </button>)}</div>,
  };
});

afterEach(() => { cleanup(); vi.resetAllMocks(); });

const projectId = exampleCheckout.projectId;
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
    job: { id: `job-${nodeId}`, projectId, checkoutId: exampleCheckout.checkoutId, taskId: null, worktreeOwnerTaskId: null, origin: { type: "pipeline" },
      kind, pipeline: { pipelineRunId: runId, nodeInstanceId: nodeId, planRevision: null, release: null }, context: null,
      mode: "read_only", priority: 0, queueState: "queued", queuedRecovery: null, downstreamWait: null,
      latestAttempt: last ? { ...last, cancelRequested: false, recovery: last.previousRunId ? "retry" : null } : null } };
}
const output = (nodeInstanceId: string, id: string, port: string, kind: string, evidence: unknown, unitId: string | null = null): ExecutionOutput => ({
  current: true, inputReceipts: [], artifact: { id, nodeInstanceId, port, value: { kind, evidence }, unitId, workflowRunId: runId },
});

it("shows real pinned stages, failure and dependency evidence, and opens the exact attempt and Git or Agile records", async () => {
  const start = node("start", "Pinned source", [], [control("start"), port("document", "markdown")]);
  const plan = node("planner", "Historical plan", [port("context", "markdown")], [port("plan", "plan")]);
  const review = node("review", "Review pinned plan", [port("plan", "plan")]);
  const approval = node("approval", "Saved plan approval", [], [port("decision", "approval")]);
  const commit = node("commit", "Verified Git", [], [port("git", "git")]);
  const jobs = [
    job("planner", [{ runId: "attempt-first", ordinal: 1, previousRunId: null, conversationId: "original-conversation", status: "failed" },
      { runId: "attempt-retry", ordinal: 2, previousRunId: "attempt-first", conversationId: "retry-conversation", status: "failed" }]),
    job("queued", [], "command"),
    job("running", [{ runId: "command-now", ordinal: 1, previousRunId: null, conversationId: null, status: "running" }], "command"),
    job("command", [{ runId: "command-saved", ordinal: 1, previousRunId: null, conversationId: null, status: "succeeded" }], "command"),
    job("question", [{ runId: "pending-question", ordinal: 1, previousRunId: null, conversationId: "questions", status: "waiting_for_input" }]),
  ];
  const task = { taskId: "linked-task", projectId };
  const snapshot: WorkflowExecutionSnapshot = {
    run: { id: runId, version, planRevision: "selected-plan", projectId },
    definition: { version, name: "Recorded pipeline title", nodes: [start, plan, review, approval, commit,
      node("queued", "Queued command"), node("running", "Active command"), node("question", "User question"),
      node("command", "Saved command", [], [port("result", "command_result")])],
      wires: [{ from: { node: "start", port: "document" }, to: { node: "planner", port: "context" } },
        { from: { node: "planner", port: "plan" }, to: { node: "review", port: "plan" } }] },
    unitNodeIds: ["commit"], units: [logical], jobs,
    outputs: [
      output("start", "original-markdown", "document", "markdown", { relativePath: "old-brief.md", id: "frozen-source" }),
      { ...output("approval", "approval-receipt", "decision", "approval", { task, source: "automatic_rule", planRevision: "selected-plan", ruleEvidence: { revision: "rule-reviewed" } }),
        inputReceipts: [{ executionKey: "selected-plan", revision: "pinned-approval-input", inputs: { context: "original-markdown" } }] },
      output("commit", "git-receipt", "git", "git", { id: "verified-commit", task }, "logical-one"),
      output("command", "command-receipt", "result", "command_result", { runId: "command-saved", output: { format: "json", value: { count: 2 } } }),
    ],
  };
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    switch (command) {
      case "list_repository_deliveries": return [snapshot.run, { ...snapshot.run, id: "newer-run", version: { id: "pipeline", revision: "new-edited-draft" } }];
      case "inspect_workflow_execution": expect(input).toEqual({ projectId, runId }); return snapshot;
      case "inspect_workflow_execution_job": return { job: jobs[0]!.job, blockers: [], recoveryActions: ["retry"] };
      case "job_failure": return { checkoutRoot: "D:\\frozen-checkout", logsAvailable: true, savedDiffAvailable: false,
        observation: { cause: "invalid_result", phase: "result_validation", process: null } };
      case "verified_delivery_commit": return { id: "verified-commit", projectId, taskId: "linked-task", branch: "archived-branch", sha: "fixed-sha", captureRunId: "frozen-git-attempt" };
      default: throw new Error(`Unexpected ${command}`);
    }
  });
  const props = { checkout: exampleCheckout, target: `run:${runId}`, simulation: null, startSimulation: vi.fn(),
    openWorkspace: vi.fn(), openActivity: vi.fn(), openChanges: vi.fn(), openQueueJob: vi.fn() };
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}><PipelinesWorkspace {...props} /></QueryClientProvider>);
  const user = userEvent.setup();
  expect(await screen.findByRole("heading", { name: "Recorded pipeline title" })).toBeTruthy();
  const graph = within(screen.getByLabelText("Workflow execution graph"));
  expect(graph.getByRole("button", { name: "Pinned source · Completed" })).toBeTruthy();
  expect(graph.getByRole("button", { name: "Queued command · Queued" })).toBeTruthy();
  expect(graph.getByRole("button", { name: "Active command · Running" })).toBeTruthy();
  expect(graph.getByRole("button", { name: "User question · Waiting" })).toBeTruthy();
  const inspector = within(screen.getByRole("complementary", { name: "Execution node evidence" }));
  await user.click(graph.getByRole("button", { name: "Review pinned plan · Waiting" }));
  expect(inspector.getByText("plan needs planner.plan")).toBeTruthy();
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
  expect(inspector.getByText("rule-reviewed")).toBeTruthy();
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
  await user.selectOptions(screen.getByRole("combobox", { name: "Saved workflow run" }), "newer-run");
  expect(props.openWorkspace).toHaveBeenCalledWith("pipelines", "run:newer-run");
});
