// @vitest-environment jsdom

import { useState } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { RunInspector } from "@/inspection-panel";
import { ProjectTasksWorkspace } from "@/project-tasks";
import type { WorkspaceProps } from "@/workbench-contract";
import { testCheckout } from "@/test-fixtures/checkout";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it.each([
  { state: "live", status: "waiting_for_input", command: "answer_conversation_question" },
  { state: "saved", status: "interrupted", command: "answer_saved_conversation_question" },
] as const)("opens and answers the originating agent's $state question from a blocked Task", async ({ state, status, command }) => {
  const { projectId, checkoutId } = testCheckout;
  const task = { id: "task", title: "Update the contract" };
  const origin = { runId: "child-run", conversationId: "child-conversation", requestId: "9" };
  const scoped = { runId: origin.runId, conversationId: origin.conversationId };
  const question = (requestId: string | number, message: string) => ({ data: {
    type: "question", request_id: requestId, normalized: { message, actions: ["accept"], fields: [{
      id: "include", label: "Include compatibility checks", description: null, required: true, secret: false,
      default: true, input: { kind: "boolean" },
    }] },
  } });
  const decoy = question(9, "A different provider request");
  const originating = question(origin.requestId, "Check compatibility before changing the contract?");
  vi.mocked(invoke).mockImplementation(async (name, args) => {
    switch (name) {
      case "project_work_hierarchy": return { epics: [], work: [{ task, placement: { kind: "task", epicId: null } }] };
      case "project_task_delivery_state":
        expect(args).toEqual({ projectId, taskId: task.id });
        return {
          state: "blocked", hasDirectWork: true, waitingForCommit: false, checklistResolved: true,
          pendingRepositoryChanges: false, activeWorkflowRunId: "delivery-workflow",
          questionBlocker: { ...origin, state, answerRecorded: false },
        };
      case "project_task_checklist": return { items: [] };
      case "project_task_deliveries": case "project_task_runs": case "project_task_assignments":
      case "conversation_models": case "conversation_approvals": case "conversation_question_timeline": return [];
      case "inspect_conversation_run":
        expect(args).toEqual({ projectId, ...scoped });
        return [{ id: origin.runId, projectId, checkoutId, turnId: "child-turn", status }, origin.conversationId];
      case "conversation_events": return { cursor: "0", more: false, events: [] };
      case "pending_conversation_inputs":
        expect(args).toEqual(scoped);
        return state === "live" ? [decoy, originating] : [];
      case "saved_conversation_questions":
        expect(args).toEqual(scoped);
        return state === "saved" ? [decoy, originating].map((event) => ({
          event, state: "saved", reason: "provider_expired", answerRecorded: false,
        })) : [];
      case "conversation_subagent_quota": case "attempt_subagent_audit": case "run_orchestration":
      case "conversation_dispatch_inventory": case "conversation_profile_evidence": case "job_failure":
      case "downstream_wait": case "conversation_checklist_progress": return null;
      case "answer_conversation_question": case "answer_saved_conversation_question": return undefined;
      default: throw new Error(`Unexpected command ${name}`);
    }
  });

  const opened = vi.fn();
  function WorkbenchFixture() {
    const [selection, inspect] = useState<{ runId: string; conversationId?: string; focusQuestion?: { requestId: string | number } }>();
    const openActivity: WorkspaceProps["openActivity"] = (runId, conversationId, questionRequestId) => {
      opened(runId, conversationId, questionRequestId);
      inspect({ runId, conversationId, focusQuestion: questionRequestId === undefined ? undefined : { requestId: questionRequestId } });
    };
    const props: WorkspaceProps = {
      checkout: testCheckout, openActivity,
      openWorkspace: vi.fn(), openChanges: vi.fn(),
    };
    return <>
      <ProjectTasksWorkspace {...props} />
      {selection && <RunInspector {...props} {...selection} onClose={() => inspect(undefined)} />}
    </>;
  }

  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><WorkbenchFixture /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: task.title }));
  const details = within(screen.getByRole("region", { name: task.title }));
  await details.findByText("Blocked");
  await user.click(await details.findByRole("button", { name: "Open originating question" }));
  expect(opened).toHaveBeenCalledExactlyOnceWith(origin.runId, origin.conversationId, origin.requestId);

  const text = await screen.findByText(originating.data.normalized.message);
  const form = text.closest("form")!;
  const other = screen.getByText(decoy.data.normalized.message);
  await waitFor(() => {
    const focused = document.activeElement;
    expect(form.contains(focused) || focused?.contains(form)).toBe(true);
    expect(focused?.contains(other)).toBe(false);
  });
  if (state === "saved") expect(within(form).getByText("The provider closed the live request.")).toBeTruthy();
  await user.click(within(form).getByRole("switch", { name: "Include compatibility checks" }));
  await user.click(within(form).getByRole("button", { name: "Reply" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith(command, {
    ...origin, answer: { action: "accept", values: { include: false } },
  }));
  const otherRoute = command === "answer_saved_conversation_question" ? "answer_conversation_question" : "answer_saved_conversation_question";
  expect(vi.mocked(invoke).mock.calls.some(([name]) => name === otherRoute)).toBe(false);
});
