// @vitest-environment jsdom

import type { DndContext, DragEndEvent } from "@dnd-kit/core";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import { afterEach, expect, it, vi } from "vitest";
import type { JobInspection } from "@/bindings/JobInspection";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { AgentTree } from "@/conversation-children";
import type { VerifiedMerge } from "@/delivery-merge-model";
import type { Delivery, Hierarchy, Item, WorkflowStatus } from "@/project-agile-model";
import { ProjectTasksWorkspace } from "@/project-tasks";
import type { TaskParticipantAttempt } from "@/task-participants";
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
const commit = {
  id: "unit-commit", projectId, taskId: standalone.id,
  branch: "codex/old-logging-branch", sha: "e".repeat(40), captureRunId: "history-attempt", purpose: "Record useful logs",
};
const merge: VerifiedMerge = {
  id: "recorded-merge", projectId, taskId: standalone.id, workflowRunId: "workflow", unitId: "logging-unit",
  commitEvidenceId: commit.id, commitEvidenceIds: [commit.id], sourceBranch: commit.branch, sourceSha: commit.sha,
  destinationBranch: "refs/heads/former-release", previousDestinationSha: "a".repeat(40), integrationSha: "f".repeat(40),
  verifiedAt: "2026-11-16T12:30:00Z", source: "external",
};
const question = { runId: "subtask-question-run", conversationId: "subtask-chat", requestId: 17, state: "live", answerRecorded: false };
const acceptedDiff = "diff --git a/logger.rs b/logger.rs\n+Log the request context";

type ActivityFixture = {
  attempts: Record<string, TaskParticipantAttempt[]>;
  jobs?: JobInspection[];
  tree?: AgentTree;
  models?: ModelOption[];
  assignment?: ModelSelection;
};

function testWorkspace(seed = true, savedMerge?: VerifiedMerge, activity?: ActivityFixture) {
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
  const evidence: Delivery = { id: "delivery", recordedAt: "2026-11-16T12:00:00Z", evidence: {
    kind: "verified_commit", completionSha: commit.sha, workflowRunId: "workflow", unitEvidenceIds: [commit.id],
  } };
  const reviewRole = "candidate reviewer";
  let assignment = activity?.assignment;
  vi.mocked(invoke).mockImplementation(async (name, args) => {
    const scope = args as { projectId: string; taskId: string; evidenceId?: string; expectedState?: WorkflowStatus;
      nextState?: WorkflowStatus; next?: Item; title: string; body: string; placement: { kind: "epic" } | Hierarchy["work"][number]["placement"];
      provider?: ModelSelection["provider"]; selection?: ModelSelection };
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
      case "project_task_deliveries": return scope.taskId === standalone.id ? [{ ...evidence, merge: savedMerge }] : [];
      case "verified_work_delivery_units": return [];
      case "verified_delivery_merge": return null;
      case "project_task_runs": return activity?.attempts[scope.taskId] ?? [];
      case "project_task_goals": return [];
      case "project_job_queue": return { settings: { paused: false, capacity: 3 }, active: 1, jobs: activity?.jobs ?? [] };
      case "conversation_agent_tree": return activity?.tree;
      case "conversation_models": return activity?.models?.filter(({ selection }) => selection.provider === scope.provider)
        .map((option) => ({ option })) ?? [];
      case "project_task_assignments": return assignment ? [{ role: reviewRole, ...assignment }] : [];
      case "set_project_task_assignment": {
        expect(args).toMatchObject({ projectId, taskId: standalone.id, role: reviewRole });
        assignment = scope.selection;
        return [{ role: reviewRole, ...assignment }];
      }
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
  const openQueueJob = vi.fn();
  const app = render(<QueryClientProvider client={client}><ProjectTasksWorkspace checkout={exampleCheckout} simulation={null}
    startSimulation={vi.fn()} openWorkspace={openWorkspace} openActivity={openActivity} openChanges={vi.fn()}
    openQueueJob={activity?.jobs ? openQueueJob : undefined} /></QueryClientProvider>);
  const user = userEvent.setup();
  return { ...app, states, openActivity, openWorkspace, openQueueJob, user };
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

it("filters completed work by the recorded review model after changing the planned assignment", async () => {
  const original: ModelOption = {
    selection: { provider: "codex", profileId: "review-profile", modelId: "review-model" },
    name: "Review model", selectable: true, unavailableReason: null, catalogVersion: 1,
    rating: { intelligenceTenths: 732, priceScore: 51, apiReferenceCents: 220 },
  };
  const replacement: ModelOption = { ...original, name: "Replacement model",
    selection: { provider: "claude", profileId: "replacement-profile", modelId: "replacement-model" } };
  const reviewer: TaskParticipantAttempt = {
    runId: "accepted-review", conversationId: "review-conversation", ordinal: 1,
    role: { kind: "workflow", value: "candidate reviewer" }, status: "succeeded",
    provider: original.selection.provider, profileId: original.selection.profileId, requestedModel: "retired-request",
    reportedModel: original.selection.modelId,
  };
  const { user, openActivity } = testWorkspace(true, undefined, {
    attempts: { [standalone.id]: [reviewer, {
      ...reviewer, runId: "different-attempt", profileId: "other-profile", reportedModel: "retired-model",
    }], [parent.id]: [{ ...reviewer, runId: "same-model-other-profile", profileId: "other-profile" }] },
    models: [original, replacement], assignment: original.selection,
  });
  const board = within(screen.getByLabelText("Agile Kanban"));
  await user.click(await board.findByRole("button", { name: `Work history for ${standalone.title}` }));
  const detail = within(screen.getByRole("complementary", { name: "Work detail" }));
  await user.click(detail.getByText("Planned model assignments"));
  await user.selectOptions(await detail.findByRole("combobox", { name: "Workflow role" }), "candidate reviewer");
  expect(within(await detail.findByRole("group", { name: "Reference rating for Review model" })).getByText("73.2/100")).toBeTruthy();

  await user.click(screen.getByRole("button", { name: "Filter by model · All models" }));
  const filter = within(await screen.findByRole("menu", { name: "Filter by model" }));
  expect(filter.getByText("73.2/100")).toBeTruthy();
  expect(filter.getByText("Codex · review-profile · review-model")).toBeTruthy();
  await user.click(filter.getByRole("menuitemradio", { name: "Review model" }));
  expect(board.queryByRole("button", { name: parent.title })).toBeNull();

  await user.selectOptions(detail.getByRole("combobox", { name: "Agent provider" }), "claude");
  await user.click(await detail.findByRole("button", { name: "Assignment model · Choose model" }));
  await user.click(within(await screen.findByRole("menu", { name: "Assignment model" }))
    .getByRole("menuitemradio", { name: "Replacement model" }));
  expect(await detail.findByText("Saved assignment · Claude Code · replacement-model · replacement-profile")).toBeTruthy();
  expect(board.getByRole("button", { name: standalone.title })).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("set_project_task_assignment", {
    projectId, taskId: standalone.id, role: "candidate reviewer", selection: replacement.selection,
  });

  const history = within(detail.getByRole("region", { name: "Work history" }));
  const review = history.getByText("review-model").closest("li");
  if (!review) throw new Error("Missing recorded reviewer");
  await user.click(within(review).getByRole("button", { name: "Open attempt 1" }));
  expect(openActivity).toHaveBeenCalledWith(reviewer.runId, reviewer.conversationId);

  await user.click(screen.getByRole("button", { name: "Table" }));
  const table = within(screen.getByRole("table", { name: "Agile table" }));
  await user.selectOptions(screen.getByRole("combobox", { name: "Filter by state" }), "completed");
  await user.selectOptions(screen.getByRole("combobox", { name: "Filter by provider" }), "codex");
  const workButton = { name: /^Improve logging\s*Task$/ };
  expect(table.getByRole("button", workButton)).toBeTruthy();
  await user.selectOptions(screen.getByRole("combobox", { name: "Filter by agent" }), screen.getByRole("option", { name: "Codex · other-profile" }));
  expect(await screen.findByText("No matching work.")).toBeTruthy();
  await user.selectOptions(screen.getByRole("combobox", { name: "Filter by agent" }), screen.getByRole("option", { name: "Codex · review-profile" }));
  expect(table.getByRole("button", workButton)).toBeTruthy();
  await user.selectOptions(screen.getByRole("combobox", { name: "Filter by agent" }), "");
  await user.selectOptions(screen.getByRole("combobox", { name: "Filter by provider" }), "");

  await user.click(screen.getByRole("button", { name: "Filter by model · Review model" }));
  const latestFilter = within(await screen.findByRole("menu", { name: "Filter by model" }));
  expect(latestFilter.getByRole("menuitemradio", { name: "retired-model (recorded)" })).toBeTruthy();
  await user.click(latestFilter.getByRole("menuitemradio", { name: "Replacement model" }));
  expect(table.queryByRole("button", workButton)).toBeNull();
  expect(await screen.findByText("No matching work.")).toBeTruthy();
});

it("opens exact active attempts and task job approvals and keeps observed subagents with interrupted work", async () => {
  const active: TaskParticipantAttempt = {
    runId: "running-attempt", conversationId: "active-chat", ordinal: 2, role: { kind: "workflow", value: "implementer" },
    provider: "codex", profileId: "active-profile", requestedModel: "requested", reportedModel: "running-model", status: "running",
  };
  const interrupted: TaskParticipantAttempt = { ...active, runId: "interrupted-attempt", conversationId: "old-chat",
    ordinal: 1, status: "interrupted", reportedModel: "previous-model" };
  const waitingJob: JobInspection = {
    job: { id: "approved-job-target", projectId, checkoutId: exampleCheckout.checkoutId, taskId: parent.id,
      worktreeOwnerTaskId: parent.id, origin: { type: "operator" }, context: null, mode: "read_only", priority: 0,
      queueState: "queued", latestAttempt: null, queuedRecovery: null, downstreamWait: null, pipeline: null, kind: "agent" },
    blockers: [{ type: "approval", approvalId: "exact-approval" }], recoveryActions: [],
  };
  const foreignJob: JobInspection = { ...waitingJob, job: { ...waitingJob.job, id: "foreign-job", projectId: "foreign" },
    blockers: [{ type: "policy", reason: "Other project's policy" }] };
  const otherTask: JobInspection = { ...waitingJob, job: { ...waitingJob.job, id: "other-task-job", taskId: gitWaiting.id },
    blockers: [{ type: "policy", reason: "This task needs a verified profile." }] };
  const { user, openActivity, openQueueJob } = testWorkspace(true, undefined, {
    attempts: { [parent.id]: [interrupted, active] }, jobs: [waitingJob, foreignJob, otherTask],
    tree: { parent: null, attempts: [], managed: [], nativeObservations: [{ runId: interrupted.runId, event: {
      location: { sessionId: "provider-session", parentItemId: null, turnId: "previous-turn", itemId: "delegation" },
      data: { type: "child_activity", child_id: "recorded-worker", status: "completed",
        details: { parentId: null, prompt: "Review the samples", requestedModel: null, reportedModel: "child-model" } },
    } }] },
  });
  const card = within(await screen.findByRole("region", { name: `Working now for ${parent.title}` }));
  await user.click(card.getByRole("button", { name: "Codex · active-profile · running-model" }));
  expect(openActivity).toHaveBeenCalledWith(active.runId, active.conversationId);
  const ready = within(screen.getByRole("region", { name: "Ready" }));
  await user.click(await ready.findByRole("button", { name: "Review job approval" }));
  expect(openQueueJob).toHaveBeenLastCalledWith(waitingJob.job.id);
  expect(ready.queryByRole("button", { name: "Open job blocker" })).toBeNull();
  const inReview = within(screen.getByRole("region", { name: "In Review" }));
  await user.click(inReview.getByRole("button", { name: "Open job blocker" }));
  expect(openQueueJob).toHaveBeenLastCalledWith(otherTask.job.id);
  expect(screen.queryByText("Other project's policy")).toBeNull();

  await user.click(ready.getByRole("button", { name: `Work history for ${parent.title}` }));
  const detail = within(screen.getByRole("complementary", { name: "Work detail" }));
  await user.click(within(detail.getByRole("region", { name: "Task job blockers" })).getByRole("button", { name: "Review job approval" }));
  expect(openQueueJob).toHaveBeenLastCalledWith(waitingJob.job.id);
  const history = within(detail.getByRole("region", { name: "Work history" }));
  expect(history.getByText("Interrupted")).toBeTruthy();
  await user.click(history.getByText("Observed subagents"));
  await user.click(await history.findByText("Native child recorded-worker · completed"));
  expect(history.getByText("child-model")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("conversation_agent_tree", {
    runId: interrupted.runId, conversationId: interrupted.conversationId,
  });
});

it("reopens the source and integration evidence from the persisted delivery in a fresh workspace", async () => {
  const first = testWorkspace(true, merge);
  await first.user.click(screen.getByRole("button", { name: "Table" }));
  const table = within(screen.getByRole("table", { name: "Agile table" }));
  expect(await table.findByRole("button", { name: `Open saved branch ${commit.branch}` })).toBeTruthy();
  const source = table.getByRole("button", { name: `Open verified commit ${commit.sha}` });
  expect(source.title).toBe(commit.sha);
  const destination = await table.findByRole("button", { name: `Open destination branch ${merge.destinationBranch}` });
  expect(destination.textContent).toContain(merge.destinationBranch);
  const integration = table.getByRole("button", { name: `Open integration SHA ${merge.integrationSha}` });
  expect(integration.textContent).toContain(merge.integrationSha);
  await first.user.click(destination);

  const selected = within(await screen.findByRole("region", { name: "Recorded Git evidence" }));
  expect(selected.getByText(commit.sha)).toBeTruthy();
  expect(selected.getByText(merge.integrationSha)).toBeTruthy();
  expect((await selected.findByLabelText("Accepted diff")).textContent).toContain(acceptedDiff);
  first.unmount();
  vi.mocked(invoke).mockClear();

  const reopened = testWorkspace(true, merge);
  await reopened.user.click(screen.getByRole("button", { name: "Table" }));
  const reopenedTable = within(screen.getByRole("table", { name: "Agile table" }));
  await reopened.user.click(await reopenedTable.findByRole("button", { name: `Open integration SHA ${merge.integrationSha}` }));
  const reopenedEvidence = within(await screen.findByRole("region", { name: "Recorded Git evidence" }));
  expect(reopenedEvidence.getByText(commit.sha)).toBeTruthy();
  expect(reopenedEvidence.getByText(merge.destinationBranch)).toBeTruthy();
  expect(reopenedEvidence.getByText(merge.integrationSha)).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("project_task_deliveries", { projectId, taskId: standalone.id });
  expect(invoke).toHaveBeenCalledWith("verified_delivery_commit", { projectId, taskId: standalone.id, evidenceId: commit.id });
  const commands = vi.mocked(invoke).mock.calls.map(([name]) => name);
  expect(commands.filter((name) => name.startsWith("git_") || name === "live_managed_worktree")).toEqual([]);
});

it("leaves the source available when a merge receipt belongs to a different commit", async () => {
  const { user } = testWorkspace(true, { ...merge, commitEvidenceId: "another-unit", sourceSha: "b".repeat(40) });
  await user.click(screen.getByRole("button", { name: "Table" }));
  const table = within(screen.getByRole("table", { name: "Agile table" }));
  expect(await table.findByRole("button", { name: `Open verified commit ${commit.sha}` })).toBeTruthy();
  expect(table.queryByRole("button", { name: /Open destination branch/ })).toBeNull();
  expect(table.queryByRole("button", { name: /Open integration SHA/ })).toBeNull();
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

it("creates the hierarchy from the work form and keeps a direct checklist beside a subtask", async () => {
  const { user } = testWorkspace(false);
  await user.click(screen.getByText("New work"));
  const kind = screen.getByRole("combobox", { name: "Work type" });
  const titleInput = screen.getByRole("textbox", { name: "Title" });
  const create = screen.getByRole("button", { name: "Create work" });
  async function add(kindValue: string, title: string, owner?: { label: string; id: string }) {
    await user.selectOptions(kind, kindValue);
    if (owner) await user.selectOptions(screen.getByRole("combobox", { name: owner.label }), owner.id);
    await user.type(titleInput, title);
    await user.click(create);
    await screen.findByRole("region", { name: title });
  }
  await add("epic", "New ship");
  await add("task", "Prepare docs", { label: "Epic", id: "new-ship" });
  await user.type(screen.getByRole("textbox", { name: "New checklist item" }), "Check headings");
  await user.click(screen.getByRole("button", { name: "Add checklist item" }));
  await screen.findByRole("checkbox", { name: "Check headings" });
  await add("subtask", "Proofread guide", { label: "Parent Task", id: "prepare-docs" });
  await add("task", "Independent audit");
  expect(invoke).toHaveBeenCalledWith("create_workspace_work", { projectId, title: "New ship", placement: { kind: "epic" } });
  expect(invoke).toHaveBeenCalledWith("create_workspace_work", { projectId, title: "Prepare docs", placement: { kind: "task", epicId: "new-ship" } });
  expect(invoke).toHaveBeenCalledWith("create_workspace_work", { projectId, title: "Proofread guide", placement: { kind: "subtask", parentTaskId: "prepare-docs" } });
  expect(invoke).toHaveBeenCalledWith("create_workspace_work", { projectId, title: "Independent audit", placement: { kind: "task", epicId: null } });
  expect(invoke).toHaveBeenCalledWith("append_project_checklist_item", { projectId, taskId: "prepare-docs", body: "Check headings" });

  await user.click(screen.getByRole("button", { name: "Table" }));
  const table = within(screen.getByRole("table", { name: "Agile table" }));
  await user.click(table.getByRole("button", { name: "Expand New ship" }));
  await user.click(table.getByRole("button", { name: "Expand Prepare docs" }));
  expect(table.getByRole("checkbox", { name: "Check headings" })).toBeTruthy();
  expect(table.getByRole("button", { name: /Proofread guide/ })).toBeTruthy();
  expect(table.getByRole("button", { name: /Independent audit/ })).toBeTruthy();
});
