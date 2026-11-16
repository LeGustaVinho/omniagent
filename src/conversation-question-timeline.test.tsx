// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationQuestionTimeline } from "@/conversation-question-timeline";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("shows the saved choice IDs and distinct reply, decline, and cancel actions", async () => {
  const scope = { conversationId: "chat", runId: "plan-attempt" };
  const item = { provider: "antigravity", route: "native", reason: null, continuationRunId: null };
  vi.mocked(invoke).mockResolvedValue([
    { ...item, sequence: "100", origin: { runId: scope.runId, requestId: 6 }, message: "Review inputs", state: "submitted",
      reply: { action: "accept", fields: [
        { id: "tests", label: "Tests", value: ["api-test", "ui-test"] },
        { id: "enable", label: "Enable", value: false },
        { id: "secret", label: "Secret", value: null },
      ] } },
    { ...item, sequence: "101", origin: { runId: scope.runId, requestId: "6" }, message: "Extra change", state: "submitted",
      reply: { action: "decline", fields: [] } },
    { ...item, sequence: "102", origin: { runId: scope.runId, requestId: 7 }, message: "Additional question", state: "recorded",
      reason: "provider_expired", reply: { action: "cancel", fields: [] } },
  ]);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><ConversationQuestionTimeline {...scope} live={false} /></QueryClientProvider>);
  const timeline = within(await screen.findByRole("region", { name: "Question timeline" }));
  expect(invoke).toHaveBeenCalledWith("conversation_question_timeline", scope);
  expect(timeline.getByText("api-test")).toBeTruthy();
  expect(timeline.getByText("ui-test")).toBeTruthy();
  expect(timeline.getByText("No")).toBeTruthy();
  expect(timeline.getByText("Protected answer")).toBeTruthy();
  expect(timeline.getByText("Replied · Submitted to the CLI")).toBeTruthy();
  expect(timeline.getByText("Declined · Submitted to the CLI")).toBeTruthy();
  expect(timeline.getByText("Cancelled · Answer recorded")).toBeTruthy();
  expect(timeline.getByText("The provider closed the live request.")).toBeTruthy();
});
