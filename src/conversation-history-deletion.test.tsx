// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationHistoryDeletion } from "@/conversation-history-deletion";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const target = { kind: "message", conversationId: "chat", messageRevisionId: "prompt-r2" };
const emptyImpact = {
  revision: "selected-token", messageRevisionIds: ["prompt-r2"], artifactIds: [],
  dispatchIds: [], referenceIds: [], attemptDiffIds: [], candidateIds: [],
  sideRequestIds: [],
  nativeSessions: 1, retainedRuns: 1, affectedJobs: 0, unsettledRunIds: [],
};

function show() {
  const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
  const onDeleted = vi.fn();
  render(<QueryClientProvider client={client}>
    <ConversationHistoryDeletion conversationId="chat" history={{ messages: [
      { messageId: "message", revisionId: "prompt-r2", role: "user", text: "Prompt text" },
    ] }} attemptDiffs={[{ runId: "saved-run", label: "Recorded attempt" }]} onDeleted={onDeleted} />
  </QueryClientProvider>);
  return onDeleted;
}

it("requires a fresh preview when deletion scope changes and confirms the exact reviewed revision", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(emptyImpact)
    .mockResolvedValueOnce({ ...emptyImpact, revision: "cascade-token", artifactIds: ["summary-1"], dispatchIds: ["dispatch-1"] })
    .mockResolvedValueOnce(emptyImpact);
  const onDeleted = show();
  fireEvent.change(screen.getByLabelText("History item"), { target: { value: "message:prompt-r2" } });
  fireEvent.click(screen.getByRole("button", { name: "Review deletion impact" }));
  expect(await screen.findByRole("button", { name: "Confirm deletion" })).toBeTruthy();
  expect(invoke).toHaveBeenLastCalledWith("preview_conversation_history_deletion", { target, mode: "selected" });
  fireEvent.change(screen.getByLabelText("Scope"), { target: { value: "with_derived_copies" } });
  expect(screen.queryByRole("button", { name: "Confirm deletion" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Review deletion impact" }));
  expect(await screen.findByText("summary-1")).toBeTruthy();
  expect(screen.getByText("dispatch-1")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Confirm deletion" }));
  await waitFor(() => expect(invoke).toHaveBeenLastCalledWith("confirm_conversation_history_deletion", {
    target, mode: "with_derived_copies", expectedRevision: "cascade-token",
  }));
  expect(await screen.findByText("History payload deleted.")).toBeTruthy();
  expect(onDeleted).toHaveBeenCalledOnce();
});

it("disables confirmation for unsettled attempts and discards a rejected preview token", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce({ ...emptyImpact, unsettledRunIds: ["live-run"] })
    .mockResolvedValueOnce(emptyImpact)
    .mockRejectedValueOnce("History deletion impact changed. Review it again.");
  show();
  fireEvent.change(screen.getByLabelText("History item"), { target: { value: "diff:saved-run" } });
  fireEvent.click(screen.getByRole("button", { name: "Review deletion impact" }));
  const button = await screen.findByRole<HTMLButtonElement>("button", { name: "Confirm deletion" });
  expect(button.disabled).toBe(true);
  expect(invoke).toHaveBeenLastCalledWith("preview_conversation_history_deletion", {
    target: { kind: "attempt_diff", conversationId: "chat", runId: "saved-run" }, mode: "selected",
  });
  fireEvent.click(screen.getByRole("button", { name: "Review deletion impact" }));
  const fresh = await screen.findByRole<HTMLButtonElement>("button", { name: "Confirm deletion" });
  expect(fresh.disabled).toBe(false);
  fireEvent.click(fresh);
  expect((await screen.findByRole("alert")).textContent).toContain("impact changed");
  expect(screen.queryByRole("button", { name: "Confirm deletion" })).toBeNull();
});
