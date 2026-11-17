// @vitest-environment jsdom

import type { ComponentType } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { Job } from "@/bindings/Job";
import PipelinesWorkspace from "@/pipelines-workspace";
import { WorkflowExecutionAgent } from "@/workflow-execution-agent";
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
    sourceCheckoutId: exampleCheckout.checkoutId, maximumUnits: 2, ifUnmet: "wait" },
    facts: { planRevision: "selected-plan", taskId: task.taskId, sourceCheckoutId: exampleCheckout.checkoutId, unitCount: 2 },
    outcome: "conditions_met", allowsHuman: false, decisionArtifactId: "approval-receipt" as string | null };
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    switch (command) {
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
  const props = { checkout: exampleCheckout, target: `run:${runId}`, simulation: null, startSimulation: vi.fn(),
    openWorkspace: vi.fn(), openActivity: vi.fn(), openChanges: vi.fn(), openQueueJob: vi.fn() };
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}><PipelinesWorkspace {...props} /></QueryClientProvider>);
  const user = userEvent.setup();
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
