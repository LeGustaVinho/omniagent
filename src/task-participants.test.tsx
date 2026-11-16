// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { TaskParticipants, type TaskParticipantAttempt } from "@/task-participants";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

it("shows active models and keeps every reassigned or retried attempt linked to its recorded conversation", async () => {
  const original: TaskParticipantAttempt = {
    runId: "original", conversationId: "first-conversation", ordinal: 1,
    role: { kind: "workflow", value: "Implementer" }, provider: "codex", profileId: "original-profile",
    requestedModel: "original-model", reportedModel: "original-observed-model", status: "cancelled",
  };
  const reassigned: TaskParticipantAttempt = {
    ...original, runId: "reassigned", conversationId: "second-conversation", provider: "claude",
    profileId: "replacement-profile", requestedModel: "replacement-model", reportedModel: null, status: "failed",
  };
  const retry: TaskParticipantAttempt = {
    ...reassigned, runId: "retry", ordinal: 2, reportedModel: "retry-observed-model", status: "waiting_for_input",
  };
  let records = [original, reassigned, retry];
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    expect(command).toBe("project_task_runs");
    expect(args).toEqual({ projectId: "project", taskId: "task" });
    return records;
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const openActivity = vi.fn();
  const user = userEvent.setup();
  render(<QueryClientProvider client={client}>
    <TaskParticipants projectId="project" taskId="task" openActivity={openActivity} />
  </QueryClientProvider>);

  const working = within(await screen.findByRole("region", { name: "Working now" }));
  expect(working.getByText("Implementer · Claude Code")).toBeTruthy();
  expect(working.getByText("replacement-model")).toBeTruthy();
  expect(working.getByText("retry-observed-model")).toBeTruthy();
  expect(working.getByText("Waiting for input")).toBeTruthy();
  await user.click(working.getByRole("button", { name: "Open attempt 2" }));
  expect(openActivity).toHaveBeenLastCalledWith("retry", "second-conversation");

  const history = within(screen.getByRole("region", { name: "Work history" }));
  expect(history.getAllByRole("listitem")).toHaveLength(2);
  expect(history.getByText("Unreported")).toBeTruthy();
  expect(history.getByText("original-model")).toBeTruthy();
  expect(history.getByText("original-observed-model")).toBeTruthy();
  const originalRow = history.getByText("Implementer · Codex").closest("li");
  if (!originalRow) throw new Error("Missing original participant row");
  await user.click(within(originalRow).getByRole("button", { name: "Open attempt 1" }));
  expect(openActivity).toHaveBeenLastCalledWith("original", "first-conversation");

  records = [original, reassigned, { ...retry, status: "succeeded" }];
  await act(async () => client.invalidateQueries({ queryKey: ["project-tasks", "project", "task"] }));
  expect(await working.findByText("No active attempts.")).toBeTruthy();
  expect(history.getAllByRole("listitem")).toHaveLength(3);
  expect(history.getByText("retry-observed-model")).toBeTruthy();
  expect(history.getAllByText("replacement-profile")).toHaveLength(2);
});
