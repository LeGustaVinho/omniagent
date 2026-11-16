// @vitest-environment jsdom

import type { DndContext, DragEndEvent } from "@dnd-kit/core";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import { afterEach, expect, it, vi } from "vitest";
import type { Hierarchy, Item, WorkflowStatus } from "@/project-agile-model";
import { ProjectTasksWorkspace } from "@/project-tasks";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const gestures = vi.hoisted(() => ({ drop: undefined as ((event: DragEndEvent) => void) | undefined }));
vi.mock("@dnd-kit/core", async (original) => ({
  ...(await original<typeof import("@dnd-kit/core")>()),
  DndContext({ children, onDragEnd }: ComponentProps<typeof DndContext>) {
    gestures.drop = onDragEnd;
    return <>{children}</>;
  },
}));
afterEach(() => { cleanup(); gestures.drop = undefined; vi.resetAllMocks(); });

const { projectId } = exampleCheckout;
const epic = { id: "release", title: "Release epic" };
const parent = { id: "docs", title: "Documentation" };
const child = { id: "review", title: "Proofread samples" };
const standalone = { id: "logging", title: "Improve logging" };
const blocked = { id: "waiting", title: "Publish guide" };
const gitWaiting = { id: "git-waiting", title: "Verify generated manifest" };
const commit = { branch: "codex/old-logging-branch", sha: "e".repeat(40), captureRunId: "history-attempt", purpose: "Record useful logs" };
const question = { runId: "subtask-question-run", conversationId: "subtask-chat", requestId: 17, state: "live", answerRecorded: false };
const acceptedDiff = "diff --git a/logger.rs b/logger.rs\n+Log the request context";

function testWorkspace(seed = true) {
  const states: Record<string, WorkflowStatus> = { [parent.id]: "ready", [child.id]: "blocked", [standalone.id]: "completed",
    [blocked.id]: "blocked", [gitWaiting.id]: "in_review" };
  const hierarchy: Hierarchy = seed ? { epics: [epic], work: [
    { task: parent, placement: { kind: "task", epicId: epic.id } },
    { task: child, placement: { kind: "subtask", parentTaskId: parent.id } },
    { task: standalone, placement: { kind: "task", epicId: null } },
    { task: blocked, placement: { kind: "task", epicId: null } },
    { task: gitWaiting, placement: { kind: "task", epicId: null } },
  ] } : { epics: [], work: [] };
  const checklists: Record<string, Item[]> = seed ? { [parent.id]: [
    { id: "editorial-review", revision: 5, position: 0, body: "Editorial review", checked: false },
  ] } : {};
  const evidence = { id: "delivery", recordedAt: "2026-11-16T12:00:00Z", evidence: {
    kind: "verified_commit", completionSha: commit.sha, workflowRunId: "workflow", unitEvidenceIds: ["unit-commit"],
  } };
  vi.mocked(invoke).mockImplementation(async (name, args) => {
    const scope = args as { projectId: string; taskId: string; evidenceId?: string; expectedState?: WorkflowStatus;
      nextState?: WorkflowStatus; next?: Item; title: string; body: string; placement: { kind: "epic" } | Hierarchy["work"][number]["placement"] };
    const checklist = checklists[scope?.taskId] ?? [];
    switch (name) {
      case "project_work_hierarchy": return { epics: [...hierarchy.epics], work: [...hierarchy.work] };
      case "create_workspace_work": {
        const task = { id: scope.title.toLocaleLowerCase().replaceAll(" ", "-"), title: scope.title };
        if (scope.placement.kind === "epic") hierarchy.epics.push(task);
        else hierarchy.work.push({ task, placement: scope.placement });
        states[task.id] = "backlog";
        return task.id;
      }
      case "project_epic_delivery_state": return "backlog";
      case "project_task_delivery_state": return {
        state: states[scope.taskId], questionBlocker: scope.taskId === child.id ? question : null,
        prerequisiteBlockers: scope.taskId === blocked.id ? [{ taskId: parent.id, title: parent.title }] : [],
        checklistResolved: checklist.every(({ checked }) => checked), hasDirectWork: checklist.length > 0,
        pendingRepositoryChanges: false, activeWorkflowRunId: scope.taskId === gitWaiting.id ? "pending-workflow" : null,
        waitingForCommit: scope.taskId === gitWaiting.id,
        completion: scope.taskId === standalone.id ? evidence : null,
      };
      case "project_task_checklist": return { items: checklist };
      case "project_task_deliveries": return scope.taskId === standalone.id ? [evidence] : [];
      case "project_task_runs": case "project_task_goals": case "project_task_assignments": case "conversation_models": return [];
      case "verified_delivery_commit": expect(args).toEqual({ projectId, taskId: standalone.id, evidenceId: "unit-commit" }); return commit;
      case "verified_delivery_patch": expect(args).toEqual({ projectId, taskId: standalone.id, evidenceId: "unit-commit" }); return { patchBase64: btoa(acceptedDiff) };
      case "revise_project_checklist_item": {
        expect(scope.taskId).toBe(parent.id);
        const previous = checklist.find(({ id }) => id === scope.next?.id);
        if (!previous || !scope.next) throw new Error("Checklist item missing");
        expect(scope.next.revision).toBe(previous.revision);
        const updated = { ...scope.next, revision: previous.revision + 1 };
        checklists[scope.taskId] = [updated];
        return updated;
      }
      case "append_project_checklist_item": {
        const item = { id: `item-${checklist.length}`, revision: 1, position: checklist.length, body: scope.body, checked: false };
        checklists[scope.taskId] = [...checklist, item];
        return item;
      }
      case "transition_project_work": {
        if (scope.expectedState !== states[scope.taskId]) throw new Error("The work state changed. Refresh its current state and try again.");
        states[scope.taskId] = scope.nextState!;
        return { state: scope.nextState, checklistResolved: checklist.every(({ checked }) => checked), hasDirectWork: true };
      }
      default: throw new Error(`Unexpected ${name}`);
    }
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const openActivity = vi.fn();
  const openWorkspace = vi.fn();
  render(<QueryClientProvider client={client}><ProjectTasksWorkspace checkout={exampleCheckout} simulation={null}
    startSimulation={vi.fn()} openWorkspace={openWorkspace} openActivity={openActivity} openChanges={vi.fn()} /></QueryClientProvider>);
  const user = userEvent.setup();
  return { states, openActivity, openWorkspace, user };
}

function dropWork(taskId: string, next: WorkflowStatus) {
  expect(gestures.drop).toBeDefined();
  act(() => gestures.drop?.({ active: { id: taskId }, over: { id: next } } as DragEndEvent));
}

it("shares hierarchy, checklist rows, filters, and saved Git evidence between board and table", async () => {
  const { user, openActivity, openWorkspace } = testWorkspace();
  const ready = within(screen.getByRole("region", { name: "Ready" }));
  await ready.findByRole("button", { name: parent.title });
  expect(await ready.findByText("0/1 checked")).toBeTruthy();
  expect(screen.queryByRole("button", { name: epic.title })).toBeNull();

  await user.click(await screen.findByRole("button", { name: `Open saved branch ${commit.branch}` }));
  const savedEvidence = within(await screen.findByRole("region", { name: "Recorded Git evidence" }));
  expect(savedEvidence.getByText(commit.sha)).toBeTruthy();
  expect((await savedEvidence.findByLabelText("Accepted diff")).textContent).toContain("Log the request context");
  await user.click(savedEvidence.getByRole("button", { name: "Open saved Git diff" }));
  expect(openWorkspace).toHaveBeenCalledWith("git", `attempt:${commit.captureRunId}`);

  await user.selectOptions(screen.getByRole("combobox", { name: "Kanban level" }), "subtask");
  const waiting = within(screen.getByRole("region", { name: "Blocked" }));
  await waiting.findByRole("button", { name: child.title });
  expect(waiting.getByText(/Waiting for input/)).toBeTruthy();
  await user.click(waiting.getByRole("button", { name: "Answer question" }));
  expect(openActivity).toHaveBeenCalledWith(question.runId, question.conversationId, question.requestId);
  await user.selectOptions(screen.getByRole("combobox", { name: "Kanban level" }), "epic");
  expect(await screen.findByRole("button", { name: epic.title })).toBeTruthy();

  await user.click(screen.getByRole("button", { name: "Table" }));
  const table = within(screen.getByRole("table", { name: "Agile table" }));
  expect(table.getByRole("columnheader", { name: "Branch" })).toBeTruthy();
  expect(table.getByRole("columnheader", { name: "Commit" })).toBeTruthy();
  expect(table.getByRole("button", { name: /Improve logging/ })).toBeTruthy();
  await user.click(table.getByRole("button", { name: `Expand ${epic.title}` }));
  await user.click(table.getByRole("button", { name: `Expand ${parent.title}` }));
  expect(table.getByRole("button", { name: /Proofread samples/ })).toBeTruthy();
  await user.click(table.getByRole("checkbox", { name: "Editorial review" }));
  await table.findByRole("checkbox", { name: "Editorial review", checked: true });
  expect(await table.findByText("1/1 checked")).toBeTruthy();

  await user.click(table.getByRole("button", { name: `Open verified commit ${commit.sha}` }));
  await user.click(savedEvidence.getByRole("button", { name: "Open implementation attempt" }));
  expect(openActivity).toHaveBeenCalledWith(commit.captureRunId);
  await user.selectOptions(screen.getByRole("combobox", { name: "Filter by state" }), "completed");
  expect(table.queryByRole("button", { name: /Documentation/ })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Kanban" }));
  await user.selectOptions(screen.getByRole("combobox", { name: "Kanban level" }), "task");
  expect(within(screen.getByRole("region", { name: "Completed" })).getByRole("button", { name: standalone.title })).toBeTruthy();
  expect(within(screen.getByRole("region", { name: "Ready" })).queryByRole("button", { name: parent.title })).toBeNull();
});

it("routes board moves and the detail control through the same guarded transition", async () => {
  const { user, states } = testWorkspace();
  await user.click(await within(screen.getByRole("region", { name: "Ready" })).findByRole("button", { name: parent.title }));
  const detail = within(screen.getByRole("region", { name: parent.title }));
  dropWork(parent.id, "in_progress");
  await within(screen.getByRole("region", { name: "In Progress" })).findByRole("button", { name: parent.title });
  expect(invoke).toHaveBeenCalledWith("transition_project_work", { projectId, taskId: parent.id, expectedState: "ready", nextState: "in_progress" });
  await waitFor(() => expect(detail.getByRole("status").textContent).toBe("In Progress"));

  dropWork(blocked.id, "ready");
  expect(invoke).not.toHaveBeenCalledWith("transition_project_work", expect.objectContaining({ taskId: blocked.id }));
  await user.click(within(screen.getByRole("region", { name: "Blocked" })).getByRole("button", { name: blocked.title }));
  const dependency = within(screen.getByRole("region", { name: "Task dependency blockers" }));
  expect(dependency.getByRole("button", { name: parent.title })).toBeTruthy();

  await user.click(dependency.getByRole("button", { name: parent.title }));
  states[parent.id] = "in_review";
  await user.selectOptions(await screen.findByRole("combobox", { name: "Move work to" }), "ready");
  expect(await screen.findByText(/The work state changed/)).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("transition_project_work", { projectId, taskId: parent.id, expectedState: "in_progress", nextState: "ready" });
  await within(screen.getByRole("region", { name: "In Review" })).findByRole("button", { name: parent.title });
  await user.click(screen.getByRole("button", { name: "Table" }));
  const table = within(screen.getByRole("table", { name: "Agile table" }));
  await user.click(table.getByRole("button", { name: `Expand ${epic.title}` }));
  const row = table.getByText(parent.title, { selector: "strong" }).closest("tr")!;
  expect(within(row).getByText("In Review")).toBeTruthy();
  const gitWaitingRow = table.getByText(gitWaiting.title, { selector: "strong" }).closest("tr")!;
  expect(within(gitWaitingRow).getByText("Waiting for commit")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Kanban" }));
  const inReview = within(screen.getByRole("region", { name: "In Review" }));
  expect(inReview.getByText("Waiting for commit")).toBeTruthy();
  dropWork(gitWaiting.id, "completed");
  expect(invoke).not.toHaveBeenCalledWith("transition_project_work", expect.objectContaining({ taskId: gitWaiting.id }));
});
