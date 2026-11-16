// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ProjectWorkModes, TaskWorkSettings } from "@/project-agile-settings";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const scope = { projectId: "project", taskId: "work" };
const original = { title: "Refine the update", priority: 2, acceptanceCriteria: ["Keep behavior"], prerequisites: [], blockers: [] };
const first = { id: "first", revision: 1, position: 0, body: "Review the output", checked: false };
const second = { id: "second", revision: 1, position: 1, body: "Verify public methods", checked: false };
const release = { workflowRunId: "delivery", checkoutId: "approved-worktree", planRevision: "plan-r1", releaseId: "release-r1", unitId: "open-unit" };
const selection = { provider: "codex", profileId: "profile", modelId: "model" } as const;

function setup(kind: "task" | "subtask", locked = false) {
  let fields = original;
  let projectMode = "track_progress";
  let projectBranch = "independent";
  let overrideMode: string | null = null;
  let overrideStrategy: string | null = null;
  const openQueueJob = vi.fn();
  vi.mocked(invoke).mockImplementation(async (name, input) => {
    const args = input as { mode?: string; strategy?: string; expected?: typeof original; next?: typeof original };
    switch (name) {
      case "project_work_fields": return fields;
      case "update_project_work_fields": {
        expect(args.expected).toEqual(fields);
        fields = args.next!;
        return fields;
      }
      case "project_checklist_mode": return projectMode;
      case "set_project_checklist_mode": projectMode = args.mode!; return;
      case "project_branch_strategy": return projectBranch;
      case "set_project_branch_strategy": projectBranch = args.strategy!; return projectBranch;
      case "task_checklist_mode_settings": return { overrideMode, effectiveMode: overrideMode ?? projectMode };
      case "set_task_checklist_mode": overrideMode = args.mode ?? null; return;
      case "task_branch_strategy": return { overrideStrategy, effectiveStrategy: overrideStrategy ?? projectBranch, locked };
      case "set_task_branch_strategy": overrideStrategy = args.strategy ?? null; return;
      case "project_checklist_dispatch": return release;
      case "project_task_assignments": return [{ role: "implementer", ...selection }];
      case "queue_project_checklist_work": return { id: "queued-work" };
      default: throw new Error(`Unexpected ${name}`);
    }
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}>
    <ProjectWorkModes projectId={scope.projectId} />
    <TaskWorkSettings scope={scope} workKind={kind} candidates={[{ id: "review", title: "Previous task" }]}
      items={[first, second]} openQueueJob={openQueueJob} />
  </QueryClientProvider>);
  return { user: userEvent.setup(), openQueueJob };
}

it("persists direct fields, project defaults, and the pre-provisioning Task branch override", async () => {
  const { user } = setup("task");
  await user.click(screen.getByText("Project work modes"));
  const projectMode = await screen.findByRole<HTMLSelectElement>("combobox", { name: "Project checklist mode" });
  await waitFor(() => expect(projectMode.disabled).toBe(false));
  await user.selectOptions(projectMode, "run_by_item");
  await user.selectOptions(screen.getByRole("combobox", { name: "Project branch strategy" }), "shared_branch_per_task");
  expect(invoke).toHaveBeenCalledWith("set_project_checklist_mode", { projectId: scope.projectId, mode: "run_by_item" });
  expect(invoke).toHaveBeenCalledWith("set_project_branch_strategy", { projectId: scope.projectId, strategy: "shared_branch_per_task" });

  await user.click(screen.getByText("Work fields and execution"));
  const title = await screen.findByRole("textbox", { name: "Work title" });
  await user.clear(title);
  await user.type(title, "Update the integration");
  await user.selectOptions(screen.getByRole("listbox", { name: "Prerequisites" }), "review");
  await user.click(screen.getByRole("button", { name: "Save work fields" }));
  expect(invoke).toHaveBeenCalledWith("update_project_work_fields", { ...scope, expected: original,
    next: { ...original, title: "Update the integration", prerequisites: ["review"] } });
  const override = screen.getByRole<HTMLSelectElement>("combobox", { name: "Task branch strategy override" });
  await waitFor(() => expect(override.disabled).toBe(false));
  await user.selectOptions(override, "independent");
  expect(invoke).toHaveBeenCalledWith("set_task_branch_strategy", { ...scope, strategy: "independent" });
});

it("inherits branch strategy on Sub-Tasks and queues both checklist modes against the same approved release", async () => {
  const { user, openQueueJob } = setup("subtask", true);
  await user.click(screen.getByText("Work fields and execution"));
  const track = await screen.findByRole<HTMLButtonElement>("button", { name: "Queue checklist progress" });
  await waitFor(() => expect(track.disabled).toBe(false));
  expect(screen.queryByRole("combobox", { name: "Task branch strategy override" })).toBeNull();
  expect(await screen.findByText(/Locked after provisioning/)).toBeTruthy();
  await user.click(track);
  expect(invoke).toHaveBeenCalledWith("queue_project_checklist_work", { ...scope, expectedReleaseId: release.releaseId, expectedUnitId: release.unitId, selection, itemId: null });
  const job = await screen.findByRole("button", { name: "Open queued job" });
  await user.click(job);
  expect(openQueueJob).toHaveBeenCalledWith("queued-work");

  await user.selectOptions(screen.getByRole("combobox", { name: "Checklist mode override" }), "run_by_item");
  expect(invoke).toHaveBeenCalledWith("set_task_checklist_mode", { ...scope, mode: "run_by_item" });
  await user.selectOptions(await screen.findByRole("combobox", { name: "Checklist item to run" }), second.id);
  await user.click(screen.getByRole("button", { name: "Queue selected checklist item" }));
  expect(invoke).toHaveBeenCalledWith("queue_project_checklist_work", { ...scope, expectedReleaseId: release.releaseId, expectedUnitId: release.unitId, selection, itemId: second.id });
});
