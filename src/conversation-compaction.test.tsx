// @vitest-environment jsdom

import { useState } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { ConversationCompactionControl, useConversationCompaction } from "@/conversation-compaction";
import type { ModelSelection } from "@/bindings/ModelSelection";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const conversationId = "conversation";
const contextId = "previous-context";
const pendingText = "Preserve this pending input.";
const selection: ModelSelection = { provider: "codex", profileId: "qualified-profile", modelId: "qualified-model" };
const settings = { automatic: true, thresholdPercent: 80 };
const initial = {
  contextId, settings, eligible: true, providerCompactionObserved: true, activeSummary: null,
  native: { eligible: true, reason: null, request: null },
  estimate: { historyTokens: 1800, pendingTokens: 30, availableTokens: 2000, projectedHistoryTokens: 800, overBudget: false, automaticNeeded: true },
  request: null,
};
const request = { artifactId: "summary-revision", sourceContextId: contextId, jobId: "summary-job", detail: null };

function mount() {
  const send = vi.fn();
  function Composer() {
    const [draft, setDraft] = useState(pendingText);
    const compaction = useConversationCompaction({
      conversationId, selection, effort: null, mode: "read_only", pendingText: draft, enabled: true,
    });
    return <>
      <ConversationCompactionControl compaction={compaction} contextId={contextId} />
      <label>Message draft<textarea value={draft} onChange={(event) => setDraft(event.currentTarget.value)} /></label>
      <button type="button" disabled={compaction.checkingSend} onClick={async () => {
        const destination = await compaction.beforeSend(contextId, draft);
        if (destination) send(destination, draft);
      }}>Send</button>
    </>;
  }
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><Composer /></QueryClientProvider>);
  return { user: userEvent.setup(), send };
}

it("starts before send and submits the unchanged draft against the activated context", async () => {
  let queued = false;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "conversation_compaction_state": return queued ? {
        ...initial, contextId: "compacted-context", eligible: false,
        estimate: { ...initial.estimate, historyTokens: 800, automaticNeeded: false },
        request: { ...request, status: "activated" },
      } : initial;
      case "request_conversation_compaction":
        expect(args).toEqual({ conversationId, selection, effort: null, mode: "read_only",
          expectedContextId: contextId, pendingText, automatic: true });
        queued = true;
        return { ...initial, request: { ...request, status: "queued" } };
      default: throw new Error(command);
    }
  });
  const { user, send } = mount();
  await user.click(screen.getByRole("button", { name: "Send" }));
  expect(send).not.toHaveBeenCalled();
  await waitFor(() => expect(send).toHaveBeenCalledExactlyOnceWith("compacted-context", pendingText), { timeout: 3000 });
});

it("shows project settings and keeps an oversized draft when the summary fails", async () => {
  let threshold = settings.thresholdPercent;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "conversation_compaction_state": return { ...initial, settings: { ...settings, thresholdPercent: threshold },
        estimate: { ...initial.estimate, overBudget: true } };
      case "set_conversation_compaction_settings":
        expect(args).toEqual({ conversationId, automatic: true, thresholdPercent: 70 });
        threshold = 70;
        return { ...settings, thresholdPercent: threshold };
      case "request_conversation_compaction":
        return { ...initial, estimate: { ...initial.estimate, overBudget: true },
          request: { ...request, status: "failed", detail: "The summary did not reduce the reviewed source context." } };
      default: throw new Error(command);
    }
  });
  const { user, send } = mount();
  await user.click(screen.getByText("Compact context"));
  expect(await screen.findByText(/Provider-managed compaction/)).toBeTruthy();
  expect(screen.getByText(/Estimated after compaction/)).toBeTruthy();
  const thresholdInput = screen.getByRole<HTMLInputElement>("spinbutton", { name: "Automatic threshold (%)" });
  expect(thresholdInput.value).toBe("80");
  await user.clear(thresholdInput);
  await user.type(thresholdInput, "70");
  await user.tab();
  await waitFor(() => expect(threshold).toBe(70));
  await user.click(screen.getByRole("button", { name: "Send" }));
  expect(await screen.findByText(/Shorten the draft, remove an attachment or reference/)).toBeTruthy();
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Message draft" }).value).toBe(pendingText);
  expect(send).not.toHaveBeenCalled();
});

it("offers the native route independently and renders its provider boundary and usage", async () => {
  let finish: (value: unknown) => void = () => { throw new Error("Compaction was not started"); };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "conversation_compaction_state": return initial;
      case "request_native_conversation_compaction":
        expect(args).toEqual({
          conversationId, selection, effort: null, mode: "read_only", expectedContextId: contextId, pendingText,
        });
        return new Promise((resolve) => { finish = resolve; });
      default: throw new Error(command);
    }
  });
  const { user, send } = mount();
  await user.click(screen.getByText("Compact context"));
  const native = await screen.findByRole<HTMLButtonElement>("button", { name: "Compact Codex session" });
  const managed = screen.getByRole<HTMLButtonElement>("button", { name: "Summarize with OmniAgent" });
  expect(native.disabled).toBe(false);
  expect(managed.disabled).toBe(false);
  await user.click(native);
  expect(await screen.findByText("Codex is compacting its session.")).toBeTruthy();
  expect(managed.disabled).toBe(true);
  finish({
    ...initial, native: { ...initial.native, request: {
      runId: "provider-run", sourceContextId: contextId, status: "succeeded", detail: null,
      boundary: { turnId: "provider-turn", itemId: "provider-item" },
      usage: { input: 123, output: 15, cachedInput: null, reasoningOutput: null },
    } },
  });
  expect(await screen.findByText("Codex confirmed session compaction.")).toBeTruthy();
  expect(screen.getByText("provider-turn")).toBeTruthy();
  expect(screen.getByText("provider-item")).toBeTruthy();
  expect(screen.getByText(/Provider usage.*123 input.*15 output/)).toBeTruthy();
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Message draft" }).value).toBe(pendingText);
  expect(send).not.toHaveBeenCalled();
});
