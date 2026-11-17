// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ProjectTasksWorkspace } from "@/project-tasks";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

it("records manual completion after resolving the checklist and preserves history after reopening", async () => {
  const { projectId } = exampleCheckout;
  const task = { id: "release-notes", title: "Publish release notes" };
  const epic = { id: "release", title: "Release" };
  const taskArgs = { projectId, taskId: task.id };
  const epicArgs = { projectId, epicId: epic.id };
  const completion = {
    id: "manual-delivery",
    recordedAt: "2026-11-16T10:00:00Z",
    evidence: { kind: "manual", note: null },
  };
  let item = { id: "review", revision: 3, position: 0, body: "Review the draft", checked: false };
  const revisedItem = { ...item, checked: true };
  let completed = false;
  let deliveries: typeof completion[] = [];
  const historyReads = () => vi.mocked(invoke).mock.calls.filter(([command]) => command === "project_task_deliveries").length;

  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "project_work_hierarchy":
        expect(args).toEqual({ projectId });
        return { epics: [epic], work: [{ task, placement: { kind: "task", epicId: epic.id } }] };
      case "project_task_delivery_state":
        expect(args).toEqual(taskArgs);
        return {
          state: completed ? "completed" : "ready", hasDirectWork: true, waitingForCommit: false,
          checklistResolved: item.checked, pendingRepositoryChanges: false, activeWorkflowRunId: null,
        };
      case "project_task_checklist":
        expect(args).toEqual(taskArgs);
        return { items: [item] };
      case "project_task_deliveries":
        expect(args).toEqual(taskArgs);
        return deliveries;
      case "project_task_runs":
        expect(args).toEqual(taskArgs);
        return [];
      case "verified_work_delivery_units": return [];
      case "revise_project_checklist_item":
        expect(args).toEqual({ ...taskArgs, next: revisedItem });
        item = { ...revisedItem, revision: revisedItem.revision + 1 };
        return item;
      case "record_manual_task_delivery":
        expect(args).toEqual({ ...taskArgs, note: null });
        expect(item.checked).toBe(true);
        completed = true;
        deliveries = [completion];
        return completion;
      case "reopen_task_delivery":
        expect(args).toEqual(taskArgs);
        completed = false;
        return;
      case "project_epic_delivery_state":
        expect(args).toEqual(epicArgs);
        return completed ? "completed" : "backlog";
      default:
        throw new Error(`Unexpected command ${command}`);
    }
  });

  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const user = userEvent.setup();
  render(<QueryClientProvider client={client}>
    <ProjectTasksWorkspace checkout={exampleCheckout} simulation={null}
      startSimulation={vi.fn()} openWorkspace={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} />
  </QueryClientProvider>);

  await user.click(await screen.findByRole("button", { name: task.title }));
  let details = within(screen.getByRole("region", { name: task.title }));
  await details.findByText("Ready");
  const checkbox = await details.findByRole("checkbox", { name: item.body, checked: false });
  expect(details.queryByRole("button", { name: "Record manual completion" })).toBeNull();
  await user.click(checkbox);
  await details.findByRole("checkbox", { name: item.body, checked: true });
  const complete = await details.findByRole<HTMLButtonElement>("button", { name: "Record manual completion" });
  await waitFor(() => expect(complete.disabled).toBe(false));
  await user.click(complete);
  await details.findByText("Completed");
  await user.click(details.getByRole("button", { name: "Work history" }));
  const originalDelivery = await within(await details.findByRole("region", { name: "Delivery history" }))
    .findByText(/^Manual completion ·/);

  await user.selectOptions(screen.getByRole("combobox", { name: "Kanban level" }), "epic");
  await user.click(await screen.findByRole("button", { name: epic.title }));
  await within(screen.getByRole("region", { name: epic.title })).findByText("Completed");
  await user.selectOptions(screen.getByRole("combobox", { name: "Kanban level" }), "task");
  await user.click(await screen.findByRole("button", { name: task.title }));
  details = within(screen.getByRole("region", { name: task.title }));
  await user.click(details.getByRole("button", { name: "Work history" }));
  await within(details.getByRole("region", { name: "Delivery history" })).findByText(/^Manual completion ·/);
  await user.click(details.getByRole("button", { name: "Overview" }));
  const readsBeforeReopening = historyReads();
  const reopen = await details.findByRole<HTMLButtonElement>("button", { name: "Reopen work" });
  await waitFor(() => expect(reopen.disabled).toBe(false));
  await user.click(reopen);
  await details.findByText("Ready");
  await waitFor(() => expect(historyReads()).toBeGreaterThan(readsBeforeReopening));
  expect(details.queryByRole("button", { name: "Reopen work" })).toBeNull();
  await user.click(details.getByRole("button", { name: "Work history" }));
  const retainedDelivery = within(details.getByRole("region", { name: "Delivery history" }))
    .getByText(/^Manual completion ·/);
  expect(retainedDelivery.textContent).toBe(originalDelivery.textContent);

  await user.selectOptions(screen.getByRole("combobox", { name: "Kanban level" }), "epic");
  await user.click(await screen.findByRole("button", { name: epic.title }));
  await within(screen.getByRole("region", { name: epic.title })).findByText("Backlog");
});
