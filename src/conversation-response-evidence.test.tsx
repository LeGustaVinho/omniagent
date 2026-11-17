// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { RunRecord } from "@/bindings/RunRecord";
import type { ChildLocation } from "@/conversation-children";
import {
  cloneTurnResponseEvidence, ConversationResponseEvidence, newTurnResponseEvidence, ObservedConversationResponse, recordResponseEvidence,
  type ResponseObservation,
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
  fireEvent.contextMenu(final.getByText("Done."));
  await user.click(screen.getByRole("menuitem", { name: "Copy recorded response" }));
  expect(await navigator.clipboard.readText()).toBe("Done.");
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

const archivedRun: RunRecord = { id: "recorded-run", projectId: "project", checkoutId: "source-checkout", turnId: "app-turn", status: "failed" };
const responseLocation = { ...parent, itemId: "source-item" };
const responseText = "Ignored first line\r\nVerified check α\r\nSecond check\r\nIgnored last line";
const responseEvent: ResponseObservation = { type: "assistant_final", text: responseText };
const precedingResponse: ResponseObservation = { type: "assistant_message", text: "The preliminary observation" };
const excerpt = "check α\r\nSecond";
const destination = { id: "draft", checkoutId: "review-checkout", title: "Review draft", contextId: "saved-context" };

async function openResponse(run = archivedRun, event = responseEvent, location: ChildLocation = responseLocation) {
  const responses = newTurnResponseEvidence();
  recordResponseEvidence(responses, precedingResponse, { ...parent, itemId: "preliminary" });
  recordResponseEvidence(responses, event, location);
  const open = vi.fn();
  render(<QueryClientProvider client={new QueryClient()}>
    <ObservedConversationResponse evidence={{ responses, reasoning: [] }} draftSource={{ conversationId: "source-chat", run,
      destination: { checkoutId: destination.checkoutId, open } }} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  const group = event.type === "assistant_final" ? "Final responses" : "Provisional and recorded responses";
  await user.click(screen.getByText(group));
  const text = within(screen.getByRole("list", { name: group })).getByText(/Verified check α/);
  const selected = document.createRange();
  const start = responseText.indexOf(excerpt);
  selected.setStart(text.firstChild!, start);
  selected.setEnd(text.firstChild!, start + excerpt.length);
  document.getSelection()?.removeAllRanges();
  document.getSelection()?.addRange(selected);
  fireEvent.contextMenu(text);
  return { user, open };
}

it.each([false, true])("verifies the exact archived source before adding a response excerpt (changed source: %s)", async (changed) => {
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "list_conversations": return [destination];
      case "inspect_conversation_run": return [archivedRun, "source-chat"];
      case "conversation_events": return args && "after" in args && args.after === "0" ? {
        cursor: "128", more: true, events: [
          { data: precedingResponse, location: { ...parent, itemId: "preliminary" } },
          { data: responseEvent, location: { ...responseLocation, parentItemId: "different-child" } },
        ],
      } : { cursor: "132", more: false, events: [
        { data: responseEvent, location: changed ? { ...responseLocation, itemId: "different-item" } : responseLocation },
      ] };
      case "append_checkout_context_to_conversation": return;
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  const { user, open } = await openResponse();
  await user.click(await screen.findByRole("menuitem", { name: "Add response excerpt to conversation…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Add checkout context" }));
  const reference = "Provider-reported final response · Conversation source-chat · Run recorded-run (failed) · Source worktree source-checkout · Turn app-turn"
    + " · Provider session native-session · Provider turn native-turn · Provider item source-item · Response item 2 · Lines 2–3";
  expect(dialog.getByText(reference)).toBeTruthy();
  expect(dialog.getByLabelText("Selected text").textContent).toBe(excerpt);
  expect(invoke).not.toHaveBeenCalledWith("conversation_events", expect.anything());
  await user.selectOptions(dialog.getByRole("combobox", { name: "Conversation" }), await dialog.findByRole("option", { name: destination.title }));
  await user.click(dialog.getByRole("button", { name: "Add to draft" }));
  if (changed) {
    expect((await dialog.findByRole("alert")).textContent).toContain("The recorded response is unavailable or changed");
    expect(invoke).not.toHaveBeenCalledWith("append_checkout_context_to_conversation", expect.anything());
    expect(open).not.toHaveBeenCalled();
  } else {
    await waitFor(() => expect(open).toHaveBeenCalledExactlyOnceWith(destination.id));
    expect(invoke).toHaveBeenCalledWith("append_checkout_context_to_conversation", {
      checkoutId: destination.checkoutId, conversationId: destination.id, context: `Source: ${reference}\n\n\`\`\`markdown\n${excerpt}\n\`\`\``,
    });
  }
  expect(invoke).toHaveBeenCalledWith("inspect_conversation_run", { projectId: archivedRun.projectId, conversationId: "source-chat", runId: archivedRun.id });
  expect(invoke).toHaveBeenCalledWith("conversation_events", { conversationId: "source-chat", runId: archivedRun.id, after: "128" });
  expect(invoke).not.toHaveBeenCalledWith("start_conversation_turn", expect.anything());
});

it("keeps the copy menu on a live response without offering the draft action", async () => {
  const { user, open } = await openResponse({ ...archivedRun, status: "running" });
  expect(screen.queryByRole("menuitem", { name: "Add response excerpt to conversation…" })).toBeNull();
  await user.click(screen.getByRole("menuitem", { name: "Copy recorded response" }));
  expect(await navigator.clipboard.readText()).toBe(responseText);
  expect(invoke).not.toHaveBeenCalled();
  expect(open).not.toHaveBeenCalled();
});

it("keeps ended provisional text and identifies missing provider IDs without inventing them", async () => {
  vi.mocked(invoke).mockResolvedValue([destination]);
  const { user, open } = await openResponse({ ...archivedRun, status: "interrupted" }, { ...responseEvent, type: "assistant_delta" },
    { sessionId: null, turnId: null, itemId: null, parentItemId: null });
  await user.click(await screen.findByRole("menuitem", { name: "Add response excerpt to conversation…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Add checkout context" }));
  expect(dialog.getByText(/Streamed provisional text.*Turn app-turn.*Provider turn ID unavailable.*Provider item ID unavailable.*Response item 2.*Lines 2–3/)).toBeTruthy();
  expect(dialog.getByLabelText("Selected text").textContent).toBe(excerpt);
  expect(await dialog.findByRole("option", { name: destination.title })).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("list_conversations", { checkoutId: destination.checkoutId });
  expect(invoke).not.toHaveBeenCalledWith("append_checkout_context_to_conversation", expect.anything());
  expect(open).not.toHaveBeenCalled();
});
