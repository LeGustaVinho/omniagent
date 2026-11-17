// @vitest-environment jsdom

import type { ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { WorkflowExecutionHost } from "@/workflow-execution-host";
import type { ExecutionOutput, ExecutionPinnedNode, WorkflowExecutionSnapshot } from "@/workflow-execution-model";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const projectId = "active-project";
const runId = "active-run";
const planRevision = "exact-plan";
const version = { id: "workflow", revision: "pinned-version" };
const control = (name: string) => ({ name, value: { kind: "control" as const } });
const data = (name: string, artifact = name) => ({ name, value: { kind: "data" as const, artifact } });
type Ports = ExecutionPinnedNode["definition"]["inputs"];

function node(id: string, kind: string, inputs: Ports, outputs: Ports) {
  const definition = { kind, title: id, revision: 1, inputs, outputs, configuration: "none", executor: "fixture", effect: "none" };
  return { instance: { id, kind, configuration: { kind: "none" } }, revision: id, definition };
}

function output(nodeInstanceId: string, port: string, id: string, current = true, evidence: unknown = {}): ExecutionOutput {
  return { current, inputReceipts: [], artifact: { id, port, nodeInstanceId, workflowRunId: runId, unitId: null, value: { kind: port, evidence } } };
}

const planner = node("design", "plan", [], [data("plan")]);
const review = node("assess", "review_plan", [], [control("reviewed"), data("review")]);
const link = node("prioritize", "link_work", [control("ready"), data("plan"), data("review")], [data("task")]);
const approval = node("operator-decision", "approve_implementation", [], [control("approved"), data("decision", "approval")]);
const prepare = node("provision", "prepare_worktree", [control("ready"), data("task"), data("decision", "approval")], [data("worktree"), data("release")]);
function wire(from: ExecutionPinnedNode, port: string, to: ExecutionPinnedNode, input: string) {
  return { from: { node: from.instance.id, port }, to: { node: to.instance.id, port: input } };
}
function snapshot(): WorkflowExecutionSnapshot {
  return {
    run: { id: runId, projectId, planRevision, version },
    definition: { version, name: "Editable repair workflow", nodes: [planner, review, link, approval, prepare], wires: [
      wire(planner, "plan", link, "plan"), wire(review, "review", link, "review"), wire(review, "reviewed", link, "ready"),
      wire(link, "task", prepare, "task"), wire(approval, "decision", prepare, "decision"), wire(approval, "approved", prepare, "ready"),
    ] },
    outputs: [output("design", "plan", planRevision), output("assess", "review", "review-on-selected-plan"),
      output("assess", "review", "review-on-earlier-plan", false)],
    unitNodeIds: [], units: [], jobs: [], control: [],
  };
}

function setup(initial: WorkflowExecutionSnapshot, active: ExecutionPinnedNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const queryKey = ["repository-delivery", projectId, "execution", runId];
  client.setQueryData(queryKey, initial);
  function Provider({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  }
  const view = render(<WorkflowExecutionHost snapshot={initial} node={active} />, { wrapper: Provider });
  return { ...view, client, queryKey };
}

it("links the Agile work selected at launch using the exact wired review and custom node ID", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "repository_delivery_tasks") return [{ id: "linked-at-launch", title: "Repair failing checks" }];
    if (command === "execute_repository_delivery_host") return {};
    throw new Error(command);
  });
  const { client, queryKey } = setup({ ...snapshot(), launch: { origin: { kind: "rule", workId: "linked-at-launch" } } }, link);
  expect(await screen.findByText("Selected at launch · Repair failing checks")).toBeTruthy();
  expect(screen.queryByLabelText("Existing Task or Sub-Task")).toBeNull();
  expect(screen.queryByRole("button", { name: "Create and link Task" })).toBeNull();
  await userEvent.setup().click(screen.getByRole("button", { name: "Link work" }));
  expect(invoke).toHaveBeenCalledWith("execute_repository_delivery_host", {
    projectId, runId, nodeId: "prioritize", inputs: { plan: planRevision, review: "review-on-selected-plan" },
    action: { action: "link_existing_task", taskId: "linked-at-launch" },
  });
  await waitFor(() => expect(client.getQueryState(queryKey)?.isInvalidated).toBe(true));
});

it("lets an editor launch create a Task or link existing work", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "repository_delivery_tasks") return [{ id: "existing-subtask", title: "Existing Sub-Task" }];
    if (command === "execute_repository_delivery_host") return {};
    throw new Error(command);
  });
  setup({ ...snapshot(), launch: { origin: { kind: "editor" } } }, link);
  const user = userEvent.setup();
  await user.selectOptions(screen.getByLabelText("Existing Task or Sub-Task"), await screen.findByRole("option", { name: "Existing Sub-Task" }));
  await user.click(screen.getByRole("button", { name: "Link work" }));
  const create = screen.getByRole<HTMLButtonElement>("button", { name: "Create and link Task" });
  expect(create.disabled).toBe(true);
  await user.type(screen.getByLabelText("New Task title"), "  New follow-up  ");
  await user.click(create);
  const scope = { projectId, runId, nodeId: "prioritize", inputs: { plan: planRevision, review: "review-on-selected-plan" } };
  expect(invoke).toHaveBeenCalledWith("execute_repository_delivery_host", {
    ...scope, action: { action: "link_existing_task", taskId: "existing-subtask" },
  });
  expect(invoke).toHaveBeenCalledWith("execute_repository_delivery_host", {
    ...scope, action: { action: "create_task", title: "New follow-up" },
  });
});

it("prepares from the current approval and work on the selected branch", async () => {
  vi.mocked(invoke).mockResolvedValue({});
  const state = snapshot();
  state.outputs.push(output("prioritize", "task", "linked-task"), output("operator-decision", "decision", "earlier-decision", false));
  const { rerender } = setup(state, prepare);
  const prepareButton = () => screen.queryByRole("button", { name: "Prepare managed worktree" });
  expect(prepareButton()).toBeNull();
  const approved = { ...state, outputs: [...state.outputs, output("operator-decision", "decision", "current-decision")] };
  rerender(<WorkflowExecutionHost snapshot={{ ...approved, control: [
    { nodeId: "provision", planRevision, state: "skipped", selectedPorts: [] },
  ] }} node={prepare} />);
  expect(prepareButton()).toBeNull();
  const staleRelease = output("provision", "release", "stale-release", true, { decisionArtifactId: "earlier-decision" });
  rerender(<WorkflowExecutionHost snapshot={{ ...approved, outputs: [...approved.outputs, staleRelease] }} node={prepare} />);
  await userEvent.setup().click(screen.getByRole("button", { name: "Prepare managed worktree" }));
  expect(invoke).toHaveBeenCalledWith("execute_repository_delivery_host", {
    projectId, runId, nodeId: "provision", inputs: { task: "linked-task", decision: "current-decision" },
    action: { action: "prepare_worktree" },
  });
  const release = output("provision", "release", "current-release", true, { decisionArtifactId: "current-decision" });
  rerender(<WorkflowExecutionHost snapshot={{ ...approved, outputs: [...approved.outputs, release] }} node={prepare} />);
  expect(prepareButton()).toBeNull();
});
