// @vitest-environment jsdom

import type { ComponentProps } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationPanel } from "@/conversation-panels";
import { SideQuestionPanel, type SideRequest } from "@/conversation-side-requests";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
  vi.unstubAllGlobals();
});

function renderConversation(conversationId: string) {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const panel = {
    params: { conversationId, title: "Saved conversation" },
    api: { isVisible: true, renderer: "always", onDidVisibilityChange: () => ({ dispose() {} }) },
  } as unknown as ComponentProps<typeof ConversationPanel>;
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><ConversationPanel {...panel} /></QueryClientProvider>);
}

it("keeps the selected profile and the saved draft when a side question has no qualified route", async () => {
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
  renderConversation(conversationId);
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

it("completes /b with Tab and submits only the side question", async () => {
  const conversationId = "command-completion";
  const selection = { provider: "codex", profileId: "qualified-profile", modelId: "qualified-model" };
  vi.mocked(invoke).mockImplementation(async (command) => {
    switch (command) {
      case "conversation_draft": return "";
      case "conversation_history": return { contextId: "last-committed-revision", messages: [] };
      case "conversation_models":
        return [{ option: { name: "Qualified model", selection, selectable: true, rating: null }, efforts: [] }];
      case "conversation_goal_overview":
      case "latest_conversation_job":
      case "latest_conversation_run": return null;
      case "pending_conversation_instructions":
      case "conversation_side_requests": return [];
      case "composer_suggestion_mode": return "off";
      case "save_conversation_draft": return undefined;
      case "enqueue_conversation_side_request": return { status: "queued", jobId: "queued-side-question" };
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  renderConversation(conversationId);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Model · Choose model" }));
  await user.click(await screen.findByRole("menuitemradio", { name: "Qualified model" }));
  const draft = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Message draft" });
  await user.type(draft, "/b");
  expect(await screen.findByText("Ask a side question")).toBeTruthy();
  await user.tab();
  expect(document.activeElement).toBe(draft);
  expect(draft.value).toBe("/btw ");
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("save_conversation_draft", {
    conversationId, text: "/btw ",
  }));
  await user.type(draft, "What did the check print?");
  await user.click(screen.getByRole("button", { name: "Ask side question" }));
  expect(await screen.findByRole("complementary", { name: "Side questions" })).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("enqueue_conversation_side_request", {
    conversationId, expectedContextId: "last-committed-revision", selection, effort: null, mode: "read_only",
    purpose: "question", question: "What did the check print?",
  });
  expect(invoke).not.toHaveBeenCalledWith("submit_conversation_continuation", expect.anything());
});

it("shows the anchored revision, copies an answer, and scopes cancel and delete to the side job", async () => {
  const user = userEvent.setup();
  const writeText = vi.spyOn(navigator.clipboard, "writeText");
  const answered: SideRequest = {
    contextId: "committed-revision", sourceRunId: "source-run", provider: "codex", profileId: "qualified-profile",
    model: "qualified-model", effort: null, mode: "read_only", purpose: "question", question: "Saved side question",
    response: { outcome: { kind: "answered", text: "Saved side answer", route: "fresh_codex_session" } },
    evidenceId: null, evidenceCheckedAt: null,
    job: {
      id: "answered-side-job", projectId: "project", checkoutId: "checkout", taskId: null, worktreeOwnerTaskId: null,
      origin: { type: "operator" }, kind: "side", pipeline: null, context: null, mode: "read_only", priority: 0,
      queueState: "queued", queuedRecovery: null, downstreamWait: null,
      latestAttempt: {
        ordinal: 1, runId: "answer-run", conversationId: null, status: "succeeded",
        cancelRequested: false, previousRunId: null, recovery: null,
      },
    },
  };
  const active: SideRequest = {
    ...answered, question: "Active side question", response: null,
    job: {
      ...answered.job, id: "active-side-job",
      latestAttempt: {
        ordinal: 1, runId: "active-side-run", conversationId: null, status: "running",
        cancelRequested: false, previousRunId: null, recovery: null,
      },
    },
  };
  let saved = [answered, active];
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "conversation_side_requests") return saved;
    if (command === "interrupt_queued_job_attempt") return undefined;
    if (command === "delete_conversation_side_question") {
      saved = saved.filter(({ job }) => job.id !== (args as { jobId: string }).jobId);
      return undefined;
    }
    throw new Error(`Unexpected command ${command}`);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><SideQuestionPanel conversationId="conversation" onClose={vi.fn()} /></QueryClientProvider>);
  const items = await screen.findAllByRole("listitem");
  expect(items).toHaveLength(2);
  const savedItem = within(items[0]!);
  expect(savedItem.getByText("committed-revision")).toBeTruthy();
  expect(savedItem.getByText("source-run")).toBeTruthy();
  expect(savedItem.getByText("Saved side answer")).toBeTruthy();
  await user.click(savedItem.getByRole("button", { name: "Copy answer" }));
  await waitFor(() => expect(writeText).toHaveBeenCalledWith("Saved side answer"));

  const activeItem = within(items[1]!);
  expect(activeItem.getByText("Running…")).toBeTruthy();
  await user.click(activeItem.getByRole("button", { name: "Cancel side question" }));
  expect(invoke).toHaveBeenCalledWith("interrupt_queued_job_attempt", {
    projectId: "project", jobId: "active-side-job", runId: "active-side-run",
  });
  await user.click(savedItem.getByRole("button", { name: "Delete side question" }));
  await waitFor(() => expect(screen.queryByText("Saved side answer")).toBeNull());
  expect(invoke).toHaveBeenCalledWith("delete_conversation_side_question", {
    conversationId: "conversation", jobId: "answered-side-job",
  });
  expect(screen.getByText("Active side question")).toBeTruthy();
});
