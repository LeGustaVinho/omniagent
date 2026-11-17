// @vitest-environment jsdom

import { QueryClient, QueryClientProvider, useMutation } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { Job } from "@/bindings/Job";
import type { ModelOption } from "@/bindings/ModelOption";
import { DeliveryAgent } from "@/repository-delivery-agent";
import { RunPlanHistory } from "@/run-plan-history";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

function queryClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
}

it("queues a read-only revision from the exact completed planning proposal", async () => {
  const scope = { projectId: "project", runId: "workflow" };
  const source = { sourceAttemptId: "original-planner", expectedPlanRevision: "version-one", proposalSequence: "26" };
  const job: Job = {
    id: "planner-job", projectId: scope.projectId, checkoutId: "checkout", taskId: null,
    kind: "agent", origin: { type: "pipeline" }, worktreeOwnerTaskId: null, context: null, mode: "plan",
    pipeline: { pipelineRunId: scope.runId, nodeInstanceId: "plan", planRevision: null, release: null },
    priority: 0, queueState: "queued", queuedRecovery: null, downstreamWait: null,
    latestAttempt: { ordinal: 1, runId: source.sourceAttemptId, conversationId: null, status: "succeeded",
      cancelRequested: false, previousRunId: null, recovery: null },
  };
  const option: ModelOption = {
    selection: { provider: "codex", profileId: "coder", modelId: "planning-model" },
    name: "Fixture planning model", selectable: true, unavailableReason: null, rating: null, catalogVersion: 1,
  };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "conversation_models") return (args as { provider: string }).provider === "codex" ? [{ option }] : [];
    if (command === "project_model_defaults") return { conversation: null, planning: null, implementation: null };
    if (command === "request_repository_plan_revision") return job;
    throw new Error(`Unexpected command ${command}`);
  });
  function Fixture() {
    const action = useMutation({ mutationFn: (operation: () => Promise<unknown>) => operation() });
    return <DeliveryAgent scope={scope} nodeId="plan" inputs={{ context: "pinned-document" }} job={job} label="Planning"
      planRevision={source} actions={{ execute: (operation) => action.mutate(operation), busy: action.isPending }}
      openActivity={() => {}} />;
  }

  render(<QueryClientProvider client={queryClient()}><Fixture /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Request plan revision" }));
  const submit = screen.getByRole<HTMLButtonElement>("button", { name: "Queue plan revision" });
  expect(submit.disabled).toBe(true);
  const feedback = "Require a smoke test";
  await user.type(screen.getByRole("textbox", { name: "Feedback for proposal 26" }), feedback);
  expect(submit.disabled).toBe(true);
  await user.click(await screen.findByRole("button", { name: "Planning model · Choose model" }));
  const menu = await screen.findByRole("menu");
  await user.click(within(menu).getByRole("menuitemradio", { name: option.name }));
  expect(submit.disabled).toBe(false);
  await user.click(submit);

  await screen.findByRole("button", { name: "Request plan revision" });
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command !== "conversation_models" && command !== "project_model_defaults")).toEqual([
    ["request_repository_plan_revision", { input: {
      ...scope, contextId: "pinned-document", ...source, selection: option.selection, effort: null, feedback,
    } }],
  ]);
});

it("opens frozen plan versions, revision feedback, and the prior release in the run inspector", async () => {
  const projectId = "project";
  const runId = "revision-attempt";
  const openActivity = vi.fn();
  vi.mocked(invoke).mockResolvedValue({
    request: { baseRevisionId: "version-one", feedback: "Require a smoke test" },
    versions: [
      { revisionId: "version-one", sourceRunId: "original-planner", eventSequence: "26", text: "Original frozen plan",
        current: false, approvalArtifactId: "approval-one", releaseId: "release-one", releaseRevoked: true },
      { revisionId: "version-two", sourceRunId: runId, eventSequence: "53", text: "Revised frozen plan",
        current: true, approvalArtifactId: null, releaseId: null, releaseRevoked: null },
    ],
  });
  render(<QueryClientProvider client={queryClient()}><RunPlanHistory {...{ projectId, runId, openActivity }} /></QueryClientProvider>);

  expect(invoke).not.toHaveBeenCalled();
  const user = userEvent.setup();
  await user.click(screen.getByText("Plan revision history"));
  expect(await screen.findByText("Require a smoke test")).toBeTruthy();
  const original = await screen.findByText("Proposal 26");
  const revised = screen.getByText("Proposal 53 · Selected plan");
  fireEvent.contextMenu(original);
  await user.click(screen.getByRole("menuitem", { name: "Copy plan text" }));
  expect(await navigator.clipboard.readText()).toBe("Original frozen plan");
  await user.click(original);
  expect(screen.getByText("Original frozen plan")).toBeTruthy();
  expect(screen.getByText("approval-one")).toBeTruthy();
  expect(screen.getByText("Revoked release")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Open plan attempt" }));
  expect(openActivity).toHaveBeenCalledWith("original-planner");
  await user.click(revised);
  expect(screen.getByText("Revised frozen plan")).toBeTruthy();
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("repository_plan_history", { projectId, runId }));
});
