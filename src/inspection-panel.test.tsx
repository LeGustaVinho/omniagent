// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { RunRecord } from "@/bindings/RunRecord";
import { RunInspector } from "@/inspection-panel";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("opens recorded run pages through the same Activity tabs as Simulate and loads exact evidence on selection", async () => {
  const conversationId = "recorded-conversation";
  const run: RunRecord = {
    id: "recorded-run", projectId: exampleCheckout.projectId, checkoutId: exampleCheckout.checkoutId,
    turnId: "recorded-turn", status: "succeeded",
  };
  vi.mocked(invoke).mockImplementation(async (command) => {
    switch (command) {
      case "inspect_conversation_run": return [run, conversationId];
      case "conversation_events": return { cursor: "0", more: false, events: [] };
      case "saved_conversation_questions": case "conversation_approvals": return [];
      case "conversation_checklist_progress": case "downstream_wait": return null;
      case "conversation_input": return {
        delivery: "submitted", turnId: run.turnId, frames: ["Exact recorded request"],
        attachments: [], removedAttachmentCount: 0, instructions: [],
      };
      case "run_orchestration": return { job: { taskId: null, ownerTaskId: null } };
      case "attempt_worktree_evidence": return {
        kind: "saved",
        artifact: {
          revision: "frozen-candidate", committedPatchBase64: "",
          start: { starting: { head: "baseline", changes: null }, worktree: { ownerTaskId: "task", branch: "work" } },
          candidate: { head: "saved-head", changes: null },
        },
      };
      default: throw new Error(command);
    }
  });
  const props = {
    checkout: exampleCheckout, runId: run.id, conversationId, onClose: vi.fn(), startSimulation: vi.fn(),
    openWorkspace: vi.fn(), openChanges: vi.fn(), openActivity: vi.fn(),
  };
  const result = render(<RunInspector {...props} simulation={{ scenario: "delivery", delivery: "verified", epoch: 1, advanceDelivery: vi.fn() }} />);
  const sampleTabs = screen.getAllByRole("tab").map((element) => element.textContent);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  result.rerender(<QueryClientProvider client={client}><RunInspector {...props} simulation={null} /></QueryClientProvider>);

  const activity = within(await screen.findByRole("complementary", { name: "Conversation activity" }));
  expect(activity.getAllByRole("tab").map((element) => element.textContent)).toEqual(sampleTabs);
  expect(activity.getByRole("tab", { name: "Overview", selected: true })).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "conversation_input")).toBe(false);
  await userEvent.click(activity.getByRole("tab", { name: "Exact input" }));
  expect(await activity.findByText("Exact recorded request")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("conversation_input", { conversationId, runId: run.id });

  await userEvent.click(activity.getByRole("tab", { name: "Approvals" }));
  expect(await activity.findByText("No approval requests recorded.")).toBeTruthy();
  expect(activity.queryByText("Exact recorded request")).toBeNull();
  expect(invoke).toHaveBeenCalledWith("conversation_approvals", { conversationId, runId: run.id });

  await userEvent.click(activity.getByRole("tab", { name: "Artifacts" }));
  await userEvent.click(await activity.findByText("Saved candidate for this attempt"));
  await userEvent.click(await activity.findByRole("button", { name: "Open saved diff in Git" }));
  expect(props.openWorkspace).toHaveBeenCalledExactlyOnceWith("git", "attempt:recorded-run");
});
