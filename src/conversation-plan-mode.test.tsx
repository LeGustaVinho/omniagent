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
      case "submit_conversation_continuation": started = true; return { status: "queued", job };
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
  expect(invoke).toHaveBeenCalledWith("submit_conversation_continuation", {
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

it("sends Approved write and carries the selected mode through a continuation review", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const conversationId = "direct-chat";
  const selection = { provider: "codex", profileId: "writer", modelId: "coding-model" };
  const text = "Implement the reviewed change";
  const contextId = "saved-history";
  const job = { kind: "agent", id: "write-job", queueState: "queued", mode: "approved_write" };
  const blockers = [{ type: "approval", approvalId: "write-approval" }];
  let queued = false;
  vi.mocked(invoke).mockImplementation(async (command) => {
    switch (command) {
      case "conversation_draft": return text;
      case "conversation_history": return { contextId, messages: [] };
      case "conversation_models": return [{ option: { name: "Coding model", selection, selectable: true }, efforts: [] }];
      case "latest_conversation_job": return queued ? { job, blockers, recoveryActions: [] } : null;
      case "latest_conversation_run": return null;
      case "pending_conversation_instructions": return queued ? [{
        id: job.id, revisionId: "queued-message", text, checkoutId: "managed-worktree", contextId,
        provider: selection.provider, model: selection.modelId, mode: job.mode, attachments: [], delivery: "queued",
        editable: true, runId: null, recordedContext: false, blockers,
      }] : [];
      case "conversation_side_requests":
      case "saved_conversation_questions": return [];
      case "composer_suggestion_mode": return "off";
      case "save_conversation_draft": return;
      case "submit_conversation_continuation": return {
        status: "review_required", previewId: "reviewed-write", contextId, reason: "profile_changed",
        settings: { projectDefault: "same_tab", conversationOverride: null, effective: "same_tab" }, seed: [],
      };
      case "confirm_conversation_continuation":
        queued = true;
        return { conversation: { id: conversationId, checkoutId: "managed-worktree" }, job };
      default: throw new Error(`Unexpected command ${command}`);
    }
  });

  const props = {
    params: { conversationId, title: "Direct conversation" },
    api: { isVisible: true, renderer: "always", onDidVisibilityChange: () => ({ dispose() {} }) },
  } as unknown as ComponentProps<typeof ConversationPanel>;
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><ConversationPanel {...props} /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Coding model" }));
  const modes = screen.getByRole<HTMLSelectElement>("combobox", { name: "Conversation mode" });
  expect(modes.value).toBe("read_only");
  await user.selectOptions(modes, within(modes).getByRole("option", { name: "Approved write" }));
  await user.click(screen.getByRole("button", { name: "Send" }));
  const scope = { conversationId, selection, effort: null, mode: "approved_write", text };
  expect(invoke).toHaveBeenCalledWith("submit_conversation_continuation", { ...scope, expectedContextId: contextId });

  const review = within(await screen.findByRole("dialog", { name: "Choose how to continue" }));
  await user.click(review.getByRole("button", { name: "Continue and send" }));
  expect(invoke).toHaveBeenCalledWith("confirm_conversation_continuation", {
    ...scope, previewId: "reviewed-write", choice: "same_tab",
  });
  expect(await screen.findByText("Waiting for approval write-approval.")).toBeTruthy();
});
