// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import {
  cloneTurnResponseEvidence, ConversationResponseEvidence, newTurnResponseEvidence, recordResponseEvidence,
} from "@/conversation-response-evidence";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const parent = { sessionId: "native-session", turnId: "native-turn", itemId: null, parentItemId: null };

it("shows only an explicit final response as final and keeps the other observed items", async () => {
  vi.mocked(invoke).mockResolvedValue([]);
  const original = newTurnResponseEvidence();
  const progress = { ...parent, itemId: "progress" };
  const response = { ...parent, itemId: "response" };
  recordResponseEvidence(original, { type: "assistant_delta", text: "Checking " }, progress);
  recordResponseEvidence(original, { type: "assistant_delta", text: "inputs" }, progress);
  const responses = cloneTurnResponseEvidence(original);
  recordResponseEvidence(responses, { type: "assistant_message", text: "Checked the input" }, progress);
  recordResponseEvidence(responses, { type: "assistant_delta", text: "Done" }, response);
  recordResponseEvidence(responses, { type: "assistant_message", text: "Done." }, response);
  recordResponseEvidence(responses, { type: "assistant_final", text: "Done." }, response);
  recordResponseEvidence(responses, { type: "assistant_delta", text: "Child text" }, { ...parent, parentItemId: "child" });
  // Pagination can resume from the earlier projection without mutating that projection.
  expect(original.items[0]?.text).toBe("Checking inputs");

  render(<QueryClientProvider client={new QueryClient()}>
    <ConversationResponseEvidence conversationId="chat" runId="run" live={false}
      evidence={{ responses, reasoning: [{ text: "Reasoned through the result", summary: true }] }} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(screen.getByText("Final responses"));
  const final = within(screen.getByRole("list", { name: "Final responses" }));
  expect(final.getByText("Done.")).toBeTruthy();
  expect(final.queryByText("Checked the input")).toBeNull();
  await user.click(screen.getByText("Provisional and recorded responses"));
  const provisional = within(screen.getByRole("list", { name: "Provisional and recorded responses" }));
  expect(provisional.getByText("Checked the input")).toBeTruthy();
  expect(provisional.queryByText("Done.")).toBeNull();
  expect(screen.queryByText("Child text")).toBeNull();
  await user.click(screen.getByText("Reported reasoning"));
  const reasoning = within(screen.getByRole("list", { name: "Reported reasoning" }));
  expect(reasoning.getByText("Provider reasoning summary")).toBeTruthy();
});

it("shows reasoning token-only reports and distinguishes a submitted decision from uncertain delivery", async () => {
  vi.mocked(invoke).mockResolvedValue([
    {
      eventSequence: "17", operation: "shell", description: "Run the fixture",
      location: parent, decision: "deny", delivery: "submitted",
    },
    {
      eventSequence: "19", operation: "filesystem", description: null,
      location: { ...parent, parentItemId: "nested" }, decision: "allow_once", delivery: "unconfirmed",
    },
  ]);
  const responses = newTurnResponseEvidence();
  recordResponseEvidence(responses, { type: "assistant_delta", text: "Stopped after this fragment" }, parent);
  render(<QueryClientProvider client={new QueryClient()}>
    <ConversationResponseEvidence conversationId="owner" runId="attempt" live={false}
      evidence={{ responses, reasoning: [], reasoningTokens: 12, usage: { input: 0, output: null, cached_input: null } }} />
  </QueryClientProvider>);
  expect(screen.queryByText("Final responses")).toBeNull();
  expect(screen.getByText("Reasoning text unavailable.")).toBeTruthy();
  const usage = within(screen.getByRole("region", { name: "Reported usage" }));
  expect(usage.getByText("Reasoning tokens")).toBeTruthy();
  expect(usage.getByText("12")).toBeTruthy();
  expect(usage.getByText("0")).toBeTruthy();
  await userEvent.click(screen.getByText("Approvals"));
  const approvals = within(await screen.findByRole("list", { name: "Approval history" }));
  expect(approvals.getByText("Decision submitted · Deny · Event 17")).toBeTruthy();
  expect(approvals.getByText("Delivery unconfirmed · Allow once · Event 19")).toBeTruthy();
  expect(approvals.getByText("filesystem · Child tool request")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("conversation_approvals", { conversationId: "owner", runId: "attempt" });
});
