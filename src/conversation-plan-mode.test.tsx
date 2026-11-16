// @vitest-environment jsdom

import type { ComponentProps } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ModelPicker } from "@/components/model-picker";
import { ConversationPanel } from "@/conversation-panels";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/components/model-picker", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/components/model-picker")>()),
  ModelPicker: ({ options, selection }: ComponentProps<typeof ModelPicker>) =>
    options.map((option) => <button key={option.name} type="button"
      onClick={() => { if (selection.mode === "single") selection.onChange(option.selection); }}>
      {option.name}
    </button>),
}));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
  vi.unstubAllGlobals();
});

it("submits Plan Mode from the conversation and answers an identified planning form", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const scope = { conversationId: "planning-chat", runId: "planning-attempt" };
  const selection = { provider: "codex", profileId: "planner", modelId: "planning-model" };
  const text = "Plan the document release";
  const run = { id: scope.runId, status: "waiting_for_input", projectId: "project" };
  const job = { kind: "agent", id: "plan-job", queueState: "claimed", mode: "plan", latestAttempt: {
    runId: run.id, status: run.status, cancelRequested: false,
  } };
  const form = {
    message: "What should be delivered?", actions: ["accept"], fields: [
      { id: "format", label: "Format", required: true, input: { kind: "choice", multiple: false,
        options: [{ id: "md", label: "Markdown" }, { id: "json", label: "JSON" }] } },
      { id: "note", label: "Release note", required: true, input: { kind: "text" } },
    ],
  };
  let started = false;
  let answered = false;
  vi.mocked(invoke).mockImplementation(async (command) => {
    switch (command) {
      case "conversation_draft": return text;
      case "conversation_history": return { contextId: "context", messages: [] };
      case "conversation_models": return [{ option: { name: "Planning model", selection, selectable: true }, efforts: [] }];
      case "latest_conversation_job": return started ? { job, blockers: [], recoveryActions: [] } : null;
      case "latest_conversation_run": return started ? run : null;
      case "pending_conversation_instructions":
      case "conversation_side_requests":
      case "saved_conversation_questions": return [];
      case "composer_suggestion_mode": return "off";
      case "save_conversation_draft": return;
      case "start_conversation_turn": started = true; return job;
      case "conversation_events": return { cursor: "0", events: [], more: false };
      case "pending_conversation_inputs": return answered ? [] : [{ data: { type: "question", request_id: 42, normalized: form } }];
      case "conversation_question_timeline": return [{
        sequence: "2", origin: { runId: run.id, requestId: "42" }, provider: "claude", route: "structured_turn", parentItemId: null,
      }, {
        sequence: "1", origin: { runId: run.id, requestId: 42 }, provider: "codex", route: "native", parentItemId: "planning-child",
      }];
      case "answer_conversation_question": answered = true; return;
      default: throw new Error(`Unexpected command ${command}`);
    }
  });

  const props = {
    params: { conversationId: scope.conversationId, title: "Planning conversation" },
    api: { isVisible: true, renderer: "always", onDidVisibilityChange: () => ({ dispose() {} }) },
  } as unknown as ComponentProps<typeof ConversationPanel>;
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><ConversationPanel {...props} /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Planning model" }));
  const modes = screen.getByRole("combobox", { name: "Conversation mode" });
  await user.selectOptions(modes, within(modes).getByRole("option", { name: "Plan Mode" }));
  await user.click(screen.getByRole("button", { name: "Send" }));

  const requests = within(await screen.findByRole("region", { name: "Pending requests" }));
  expect(invoke).toHaveBeenCalledWith("start_conversation_turn", {
    conversationId: scope.conversationId, selection, effort: null, mode: "plan", text, expectedContextId: "context",
  });
  expect(requests.getByRole("status").textContent).toBe("Waiting for input");
  expect(screen.getByRole("button", { name: "Waiting for input" })).toBeTruthy();
  expect(await requests.findByText("Codex · Native form · Subagent")).toBeTruthy();
  expect(requests.getByText("planning-child")).toBeTruthy();
  await user.click(requests.getByRole("radio", { name: "Markdown" }));
  await user.type(requests.getByRole("textbox", { name: "Release note" }), "Use the saved brief");
  await user.click(requests.getByRole("button", { name: "Reply" }));
  expect(invoke).toHaveBeenCalledWith("answer_conversation_question", {
    ...scope, requestId: 42, answer: { action: "accept", values: { format: "md", note: "Use the saved brief" } },
  });
});
