// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ModelOption } from "@/bindings/ModelOption";
import { DeliveryAgent } from "@/repository-delivery-agent";
import { TaskAssignment } from "@/task-assignment";
import { TaskParticipants, type TaskParticipantAttempt } from "@/task-participants";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("saves a planned model with a shared rating and preserves the previous participant", async () => {
  const scope = { projectId: "project", taskId: "task" };
  const original = { provider: "codex", profileId: "profile", modelId: "gpt-6.1-sol" } as const;
  const replacement: ModelOption = {
    selection: { provider: "claude", profileId: "claude-profile", modelId: "claude-sonnet-5-5" },
    name: "Claude Sonnet 5.5", selectable: true, unavailableReason: null, catalogVersion: 1,
    rating: { intelligenceTenths: 739, priceScore: 65, apiReferenceCents: 400 },
  };
  const originalOption: ModelOption = {
    ...replacement, selection: original, name: "GPT‑6.1 Sol",
    rating: { intelligenceTenths: 738, priceScore: 65, apiReferenceCents: 400 },
  };
  const prior: TaskParticipantAttempt = {
    runId: "original", conversationId: "conversation", ordinal: 1, role: { kind: "workflow", value: "implementer" },
    ...original, requestedModel: original.modelId, reportedModel: "gpt-6.1-sol-recorded", status: "succeeded",
  };
  const assignment = { role: "implementer", ...original };
  const workflow = { projectId: scope.projectId, runId: "delivery" };
  const inputs = { release: "approved-release" };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "project_task_assignments":
        expect(args).toEqual(scope);
        return [assignment];
      case "project_task_runs":
        expect(args).toEqual(scope);
        return [prior];
      case "conversation_models":
        if (args && "provider" in args && args.provider === "claude") return [{ option: replacement }];
        if (args && "provider" in args && args.provider === "antigravity") return [];
        expect(args).toEqual({ provider: "codex" });
        return [{ option: originalOption }];
      case "set_project_task_assignment":
        if (args && "selection" in args && args.selection === null) {
          expect(args).toEqual({ ...scope, role: "implementer", selection: null });
          return [];
        }
        expect(args).toEqual({ ...scope, role: "implementer", selection: replacement.selection });
        return [{ role: "implementer", ...replacement.selection }];
      case "submit_repository_delivery_agent":
        expect(args).toEqual({ input: {
          ...workflow, nodeId: "implement", unitId: "unit", inputs, selection: replacement.selection, effort: null,
        } });
        return;
      default:
        throw new Error(`Unexpected command ${command}`);
    }
  });

  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const user = userEvent.setup();
  const openActivity = vi.fn();
  render(<QueryClientProvider client={client}>
    <TaskAssignment {...scope} />
    <TaskParticipants {...scope} openActivity={openActivity} />
    <DeliveryAgent scope={workflow} nodeId="implement" unitId="unit" taskId={scope.taskId}
      inputs={inputs} label="Implementation" actions={{ execute: (operation) => { void operation(); }, busy: false }}
      openActivity={openActivity} />
  </QueryClientProvider>);

  await user.click(screen.getByText("Planned model assignments"));
  await screen.findByRole("group", { name: "Reference rating for GPT‑6.1 Sol" });
  await user.selectOptions(screen.getByRole("combobox", { name: "Agent provider" }), "claude");
  await user.click(await screen.findByRole("button", { name: "Assignment model · Choose model" }));
  const menu = await screen.findByRole("menu", { name: "Assignment model" });
  expect(within(menu).getByRole("meter", { name: "Intelligence" })).toBeTruthy();
  expect(within(menu).getByText("API reference $4.00 / 1M tokens")).toBeTruthy();
  await user.click(within(menu).getByRole("menuitemradio", { name: "Claude Sonnet 5.5" }));
  expect(await screen.findByText(/Saved assignment · Claude Code · claude-sonnet-5-5/)).toBeTruthy();
  expect(screen.getByText("Model availability · Reported by CLI")).toBeTruthy();
  expect(screen.getByText("Verification status · Per run")).toBeTruthy();
  expect(within(screen.getByRole("group", { name: "Reference rating for Claude Sonnet 5.5" }))
    .getByText("73.9/100")).toBeTruthy();

  const delivery = within(screen.getByRole("region", { name: "Implementation" }));
  expect(delivery.getByRole("button", { name: "Implementation model · Claude Sonnet 5.5" })).toBeTruthy();
  expect(delivery.getByText("Model from task assignment")).toBeTruthy();
  await user.click(delivery.getByRole("button", { name: "Queue implementation" }));
  expect(invoke).toHaveBeenCalledWith("submit_repository_delivery_agent", expect.any(Object));

  const saved = within(screen.getByRole("region", { name: "Participant history" }));
  expect(saved.getByText("gpt-6.1-sol-recorded")).toBeTruthy();
  await user.click(saved.getByRole("button", { name: "Open attempt 1" }));
  expect(openActivity).toHaveBeenCalledWith("original", "conversation");
  await user.click(screen.getByRole("button", { name: "Clear assignment" }));
  await delivery.findByRole("button", { name: "Implementation model · Choose model" });
  expect(saved.getByText("gpt-6.1-sol-recorded")).toBeTruthy();
});
