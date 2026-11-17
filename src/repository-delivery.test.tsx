// @vitest-environment jsdom

import type { ComponentProps } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { Job } from "@/bindings/Job";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelPicker } from "@/components/model-picker";
import type { Delivery, WorkflowState } from "@/project-agile-model";
import { RepositoryDeliveryWorkspace } from "@/repository-delivery";
import type { DeliveryJob, DeliverySnapshot } from "@/repository-delivery-model";
import type { WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/components/model-picker", () => ({
  ModelPicker: ({ label, options, selection, disabled }: ComponentProps<typeof ModelPicker>) => (
    <div aria-label={label}>
      {options.map((option) => <button key={option.selection.modelId} type="button" disabled={disabled || !option.selectable}
        onClick={() => { if (selection.mode === "single") selection.onChange(option.selection); }}>
        {option.name}
      </button>)}
    </div>
  ),
}));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

it.each([false, true])("starts delivery from saved Markdown and queues planning after reopening (project default: %s)", async (useDefault) => {
  const checkout = {
    projectId: "project", checkoutId: "source-checkout", checkoutRoot: "C:/fixture/repo", repositoryRoot: "C:/fixture/repo",
  };
  const props: WorkspaceProps = {
    checkout, simulation: null, startSimulation: vi.fn(), openWorkspace: vi.fn(), openActivity: vi.fn(), openChanges: vi.fn(),
  };
  const previous = { id: "previous-delivery", planRevision: null };
  const run = { id: "new-delivery", planRevision: null };
  const first = { id: "revision-original", relativePath: "docs/original.md", sha256: "a".repeat(64) };
  const selected = { id: "revision-selected", relativePath: "docs/release.md", sha256: "b".repeat(64) };
  const model: ModelOption = {
    name: "Fixture planning model", selectable: true, unavailableReason: null, rating: null, catalogVersion: 1,
    selection: { provider: "codex", profileId: "planning-profile", modelId: "planning-model" },
  };
  const inputs = { context: "loaded-context" };
  const queuedJob: Job = {
    id: "planning-job", projectId: checkout.projectId, checkoutId: checkout.checkoutId, taskId: null,
    kind: "agent", origin: { type: "pipeline" }, worktreeOwnerTaskId: null, context: null, mode: "plan",
    pipeline: { pipelineRunId: run.id, nodeInstanceId: "plan", planRevision: null, release: null },
    priority: 0, queueState: "queued", latestAttempt: null, queuedRecovery: null, downstreamWait: null,
  };
  const snapshot: DeliverySnapshot = {
    run, jobs: [], artifacts: [
      { id: "start-document", nodeInstanceId: "start", port: "document", unitId: null,
        value: { kind: "markdown", evidence: { checkoutId: checkout.checkoutId, ...selected } } },
      { id: inputs.context, nodeInstanceId: "context", port: "context", unitId: null,
        value: { kind: "markdown", evidence: { checkoutId: checkout.checkoutId, ...selected } } },
    ],
  };
  let started = false;
  let jobs: DeliveryJob[] = [];
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "list_repository_markdown_revisions": return [first, selected];
      case "read_repository_markdown": return args && "revisionId" in args && args.revisionId === selected.id ?
        { revision: selected, markdown: "# Deliver the selected release" } : { revision: first, markdown: "# Archived document" };
      case "list_repository_deliveries": return started ? [previous, run] : [previous];
      case "start_repository_delivery":
        expect(args).toEqual({ projectId: checkout.projectId, checkoutId: checkout.checkoutId, revisionId: selected.id });
        started = true;
        return run;
      case "inspect_repository_delivery": return args && "runId" in args && args.runId === run.id ? { ...snapshot, jobs } :
        { run: previous, jobs: [], artifacts: [] };
      case "project_model_defaults":
        expect(args).toEqual({ projectId: checkout.projectId });
        return { conversation: null, planning: useDefault ? model.selection : null, implementation: null };
      case "conversation_models": return args && "provider" in args && args.provider === "codex" ? [{ option: model }] : [];
      case "repository_delivery_tasks": return [];
      case "submit_repository_delivery_agent":
        jobs = [{ nodeId: "plan", inputs, unitId: null, job: {
          ...queuedJob, latestAttempt: { ordinal: 1, runId: "planning-attempt", conversationId: "planning-conversation",
            status: "running", cancelRequested: false, previousRunId: null, recovery: null },
        } }];
        return queuedJob;
      default: throw new Error(`Unexpected command ${command}`);
    }
  });

  const mount = () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
    return render(<QueryClientProvider client={client}><RepositoryDeliveryWorkspace {...props} /></QueryClientProvider>);
  };
  const user = userEvent.setup();
  const initial = mount();
  const revisions = await screen.findByRole("combobox", { name: "Saved source revision" });
  await user.selectOptions(revisions, selected.id);
  await user.click(await screen.findByText("Read saved Markdown"));
  expect(await screen.findByText("# Deliver the selected release")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Start repository delivery" }));
  await screen.findByRole("button", { name: "Queue planning" });
  initial.unmount();

  mount();
  const runs = await screen.findByRole("combobox", { name: "Saved delivery" });
  await user.selectOptions(runs, run.id);
  const planning = await screen.findByRole("region", { name: "Planning" });
  expect(within(planning).getByText("Plan Mode")).toBeTruthy();
  const queue = within(planning).getByRole<HTMLButtonElement>("button", { name: "Queue planning" });
  if (useDefault) {
    expect(await within(planning).findByText("Model from project defaults")).toBeTruthy();
  } else {
    expect(queue.disabled).toBe(true);
    await user.click(await within(planning).findByRole("button", { name: model.name }));
  }
  expect(queue.disabled).toBe(false);
  await user.click(queue);

  await user.click(await screen.findByRole("button", { name: "Open planning attempt" }));
  expect(invoke).toHaveBeenCalledWith("submit_repository_delivery_agent", { input: {
    projectId: checkout.projectId, runId: run.id, nodeId: "plan", unitId: null, inputs,
    selection: model.selection, effort: null,
  } });
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "start_repository_delivery")).toHaveLength(1);
  expect(props.openActivity).toHaveBeenCalledWith("planning-attempt", "planning-conversation");
});

it.each([true, false])("keeps the saved workflow's completion after reopening the work (second delivery: %s)", async (secondDelivered) => {
  const checkout = {
    projectId: "project", checkoutId: "checkout", checkoutRoot: "C:/fixture/repo", repositoryRoot: "C:/fixture/repo",
  };
  const task = { projectId: checkout.projectId, taskId: "one-task" };
  const first = { id: "older-workflow", planRevision: "first-plan" };
  const second = { id: "later-workflow", planRevision: "second-plan" };
  const firstSha = "1".repeat(40);
  const secondSha = "2".repeat(40);
  const snapshot = (run: typeof first): DeliverySnapshot => ({
    run, jobs: [], hostOutputs: { work: "linked-task" }, artifacts: [
      { id: "linked-task", nodeInstanceId: "work", port: "task", unitId: null, value: { kind: "task", evidence: task } },
    ],
  });
  const receipt = (runId: string, sha: string): Delivery => ({
    id: `receipt-${runId}`, recordedAt: "2026-11-16T10:00:00Z",
    evidence: { kind: "verified_commit", workflowRunId: runId, completionSha: sha, unitEvidenceIds: [`unit-${runId}`] },
  });
  const firstReceipt = receipt(first.id, firstSha);
  const secondReceipt = receipt(second.id, secondSha);
  const taskState: WorkflowState = {
    state: secondDelivered ? "completed" : "ready", activeWorkflowRunId: secondDelivered ? second.id : null,
    completion: secondDelivered ? secondReceipt : null, hasDirectWork: true,
    waitingForCommit: false, checklistResolved: true, pendingRepositoryChanges: false, questionBlocker: null,
  };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "list_repository_markdown_revisions": return [];
      case "list_repository_deliveries": return secondDelivered ? [second, first] : [first];
      case "inspect_repository_delivery":
        return snapshot(args && "runId" in args && args.runId === second.id ? second : first);
      case "project_task_delivery_state":
        expect(args).toEqual(task);
        return taskState;
      case "project_task_deliveries":
        expect(args).toEqual(task);
        return secondDelivered ? [firstReceipt, secondReceipt] : [firstReceipt];
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}>
    <RepositoryDeliveryWorkspace checkout={checkout} target={first.id} simulation={null}
      startSimulation={vi.fn()} openWorkspace={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} />
  </QueryClientProvider>);
  const archived = within(await screen.findByRole("region", { name: "Delivery summary" }));
  expect(archived.getByRole("status").textContent).toBe("Saved delivery · Verified commit");
  expect(archived.getByText(firstSha)).toBeTruthy();
  expect(archived.queryByText(secondSha)).toBeNull();
  expect(archived.queryByRole("button", { name: "Reopen work" })).toBeNull();
  expect(archived.queryByRole("button", { name: "Confirm restored worktree" })).toBeNull();

  if (secondDelivered) {
    const user = userEvent.setup();
    await user.selectOptions(screen.getByRole("combobox", { name: "Saved delivery" }), second.id);
    expect(await screen.findByText(secondSha)).toBeTruthy();
    const live = within(screen.getByRole("region", { name: "Delivery summary" }));
    expect(live.getByRole("status").textContent).toBe("Task · completed");
    expect(live.getByRole("button", { name: "Reopen work" })).toBeTruthy();
    expect(live.queryByText(firstSha)).toBeNull();
  }
});

it("routes a prepared repair workflow to its declared iteration nodes", async () => {
  const checkout = { projectId: "project", checkoutId: "checkout", checkoutRoot: "C:/fixture/repo", repositoryRoot: "C:/fixture/repo" };
  const task = { projectId: checkout.projectId, taskId: "repair-task" };
  const run = { id: "repair-run", planRevision: "saved-plan" };
  const snapshot: DeliverySnapshot = {
    run, jobs: [], hostOutputs: { work: "linked-task", approve: "approved-plan", prepare: "prepared-worktree" },
    definition: {
      nodes: [{ instance: { id: "repair::iteration-1::implement", kind: "implement" }, definition: { title: "Implement" } }],
      wires: [],
    },
    artifacts: [
      { id: "saved-plan", nodeInstanceId: "plan", port: "plan", unitId: null,
        value: { kind: "plan", evidence: { run_id: "planner", event_sequence: 1 } } },
      { id: "linked-task", nodeInstanceId: "work", port: "task", unitId: null, value: { kind: "task", evidence: task } },
      { id: "approved-plan", nodeInstanceId: "approve", port: "decision", unitId: null,
        value: { kind: "approval", evidence: { planRevision: "saved-plan" } } },
      { id: "stage-release", nodeInstanceId: "prepare", port: "release", unitId: null,
        value: { kind: "release", evidence: { decisionArtifactId: "approved-plan", release: { id: "release", revoked: false } } } },
      { id: "prepared-worktree", nodeInstanceId: "prepare", port: "worktree", unitId: null,
        value: { kind: "worktree", evidence: { checkout_id: "managed-checkout", task } } },
    ],
  };
  vi.mocked(invoke).mockImplementation(async (command) => {
    switch (command) {
      case "list_repository_markdown_revisions":
      case "conversation_models": return [];
      case "list_repository_deliveries": return [run];
      case "inspect_repository_delivery": return snapshot;
      case "project_task_delivery_state": return { state: "in_progress", checklistResolved: true, activeWorkflowRunId: run.id };
      case "repository_delivery_units": return [{ checkoutId: "managed-checkout", taskId: task.taskId,
        unit: { id: "repair-unit", ordinal: 1, purpose: "Restore failing checks", completionCriteria: ["Checks pass"] },
        commitEvidenceId: null }];
      default: throw new Error(command);
    }
  });
  const openWorkspace = vi.fn();
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <RepositoryDeliveryWorkspace checkout={checkout} target={run.id} simulation={null} startSimulation={vi.fn()}
      openWorkspace={openWorkspace} openActivity={vi.fn()} openChanges={vi.fn()} />
  </QueryClientProvider>);
  expect(await screen.findByText("Continue the prepared logical unit in the execution graph.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Queue implementation" })).toBeNull();
  await userEvent.setup().click(screen.getByRole("button", { name: "Open execution graph" }));
  expect(openWorkspace).toHaveBeenCalledWith("pipelines", "run:repair-run");
});
