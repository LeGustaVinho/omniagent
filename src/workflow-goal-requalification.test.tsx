// @vitest-environment jsdom

import { QueryClient, QueryClientProvider, useMutation } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { WorkflowGoalRequalification } from "@/workflow-goal-requalification";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("requires a new review when the selected release changes and submits the exact reviewed continuation", async () => {
  const selected = { provider: "codex" as const, profileId: "profile", modelId: "selected-model" };
  const input = { projectId: "project", runId: "workflow", nodeId: "writer", unitId: "new-unit", selection: selected, effort: null };
  const oldScope = { checkoutId: "registered-worktree", taskId: "task", unitId: "earlier-unit", planRevision: "earlier-plan",
    releaseId: "old-release", provider: "codex", profileId: "profile", model: "selected-model" };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "preview_workflow_goal_requalification") {
      const { input: { inputs: { release } } } = args as { input: { inputs: { release: string } } };
      return { priorRevisionId: "earlier-goal", priorCheckpointRunId: "question-run", priorScope: oldScope,
        replacementScope: { ...oldScope, planRevision: "narrower-plan", releaseId: release },
        preservesWorktreeCheckpoint: true, turnsAlreadyUsed: 2, reviewToken: `${release}-reviewed` } as never;
    }
    if (command === "submit_workflow_goal_requalification") return { id: "replacement-job" } as never;
    throw new Error(`Unexpected command ${command}`);
  });
  const openActivity = vi.fn();
  function Fixture({ release }: { release: string }) {
    const action = useMutation({ mutationFn: (operation: () => Promise<unknown>) => operation() });
    return <WorkflowGoalRequalification input={{ ...input, inputs: { release } }}
      actions={{ execute: (operation) => action.mutate(operation), busy: action.isPending }} openActivity={openActivity} />;
  }
  const client = new QueryClient();
  const view = (release: string) => <QueryClientProvider client={client}><Fixture release={release} /></QueryClientProvider>;
  const rendered = render(view("new-release"));
  expect(invoke).not.toHaveBeenCalled();
  const user = userEvent.setup();
  const preview = () => user.click(screen.getByRole("button", { name: "Review Goal continuation" }));
  await preview();
  expect(within(await screen.findByRole("region", { name: "Previous scope" })).getByText("old-release")).toBeTruthy();
  expect(within(screen.getByRole("region", { name: "New approved scope" })).getByText("new-release")).toBeTruthy();
  expect(screen.getByText(/2 turns already used/)).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Open previous checkpoint" }));
  expect(openActivity).toHaveBeenCalledWith("question-run");
  rendered.rerender(view("revised-release"));
  expect(screen.queryByRole("button", { name: "Continue on approved scope" })).toBeNull();
  expect(invoke).not.toHaveBeenCalledWith("submit_workflow_goal_requalification", expect.anything());
  await preview();
  await user.click(await screen.findByRole("button", { name: "Continue on approved scope" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("submit_workflow_goal_requalification", {
    input: { ...input, inputs: { release: "revised-release" } }, reviewToken: "revised-release-reviewed",
  }));
});
