// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { WorkflowApproval } from "@/workflow-approval";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const ipc = vi.mocked(invoke);
const scope = { projectId: "repository", runId: "delivery" };
const submission = { ...scope, nodeId: "approval-stage", inputs: { plan: "reviewed-plan", task: "linked-work" } };
const facts = { taskId: "task", sourceCheckoutId: "source-checkout", planRevision: "reviewed-plan", unitCount: 2 };
const human = { revision: null, settings: { mode: "human" }, facts, outcome: "human_required", allowsHuman: true, decisionArtifactId: null };

function approvalView() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const execute = (operation: () => Promise<unknown>) => { void operation(); };
  return render(<QueryClientProvider client={client}>
    <WorkflowApproval scope={scope} nodeId={submission.nodeId} planId={facts.planRevision}
      taskArtifactId={submission.inputs.task} actions={{ execute, busy: false }} />
  </QueryClientProvider>);
}

beforeEach(() => { ipc.mockReset(); });
afterEach(cleanup);

it("keeps human approval as the default and sends the pinned scope", async () => {
  ipc.mockImplementation(async (name) => {
    if (name === "preview_workflow_approval") return human;
    if (name === "approve_workflow_implementation") return { id: "approval" };
    throw new Error(name);
  });
  approvalView();
  fireEvent.click(await screen.findByRole("button", { name: "Approve implementation" }));
  expect(ipc).toHaveBeenCalledWith("approve_workflow_implementation", { submission });
});

it("configures a typed unit limit over the displayed checkout and task and explains the waiting condition", async () => {
  const settings = { mode: "automatic", taskId: facts.taskId, sourceCheckoutId: facts.sourceCheckoutId, maximumUnits: 1, ifUnmet: "wait" };
  ipc.mockImplementation(async (name) => {
    if (name === "preview_workflow_approval") return human;
    if (name === "set_workflow_approval_rule") return { ...human, revision: "saved-rule", settings, outcome: "unit_limit_exceeded", allowsHuman: false };
    throw new Error(name);
  });
  approvalView();
  fireEvent.click(await screen.findByText("Implementation approval settings"));
  fireEvent.change(screen.getByLabelText("Decision mode"), { target: { value: "automatic" } });
  expect(screen.getByText(facts.sourceCheckoutId)).toBeTruthy();
  expect(screen.getByText(facts.planRevision)).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Maximum logical units"), { target: { value: "1" } });
  fireEvent.click(screen.getByRole("button", { name: "Save approval settings" }));
  expect(ipc).toHaveBeenCalledWith("set_workflow_approval_rule", { submission, expectedRevision: null, settings });
  expect(await screen.findByText("The reviewed plan has 2 logical units. The rule permits 1.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Approve implementation" })).toBeNull();
});
