// @vitest-environment jsdom

import type { ComponentProps } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationPanel } from "@/conversation-panels";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
  vi.unstubAllGlobals();
});

it("keeps the selected profile and the saved draft when a side question has no qualified route", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const conversationId = "selected-profile-chat";
  const selection = { provider: "antigravity", profileId: "selected-side-profile", modelId: "selected-side-model" };
  const submitted = "/btw Is there a side answer?";
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "conversation_draft": return submitted;
      case "conversation_history": return { contextId: "committed-context", messages: [] };
      case "conversation_models": return (args as { provider?: string } | undefined)?.provider === "antigravity"
        ? [{ option: { name: "Selected side model", selection, selectable: true, rating: null }, efforts: [] }]
        : [];
      case "conversation_goal_overview":
      case "latest_conversation_job":
      case "latest_conversation_run": return null;
      case "pending_conversation_instructions":
      case "conversation_side_requests": return [];
      case "composer_suggestion_mode": return "off";
      case "enqueue_conversation_side_request": return {
        status: "unavailable", provider: selection.provider, model: selection.modelId, reason: "tool_free_fresh_route_unavailable",
      };
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  const panel = {
    params: { conversationId, title: "Saved conversation" },
    api: { isVisible: true, renderer: "always", onDidVisibilityChange: () => ({ dispose() {} }) },
  } as unknown as ComponentProps<typeof ConversationPanel>;
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><ConversationPanel {...panel} /></QueryClientProvider>);
  const user = userEvent.setup();
  const provider = screen.getByRole<HTMLSelectElement>("combobox", { name: "Provider" });
  await user.selectOptions(provider, selection.provider);
  await user.click(await screen.findByRole("button", { name: "Model · Choose model" }));
  await user.click(await screen.findByRole("menuitemradio", { name: "Selected side model" }));
  await user.click(screen.getByRole("button", { name: "Ask side question" }));

  const side = within(await screen.findByRole("complementary", { name: "Side questions" }));
  expect(await side.findByText("Antigravity · selected-side-model · This profile has no qualified tool-free route for a side question.")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("enqueue_conversation_side_request", {
    conversationId, expectedContextId: "committed-context", selection, effort: null, mode: "read_only",
    purpose: "question", question: "Is there a side answer?",
  });
  expect(invoke).not.toHaveBeenCalledWith("submit_conversation_continuation", expect.anything());
  expect(invoke).not.toHaveBeenCalledWith("save_conversation_draft", expect.anything());
  expect(provider.value).toBe(selection.provider);
  expect(screen.getByRole("button", { name: "Model · Selected side model" })).toBeTruthy();
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Message draft" }).value).toBe(submitted);
});
