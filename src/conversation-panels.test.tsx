// @vitest-environment jsdom
import type { ComponentProps } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import type { Conversation } from "@/bindings/Conversation";
import { ConversationPanel, ConversationSidebar } from "@/conversation-panels";
import { SideQuestionPanel } from "@/conversation-side-requests";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); vi.unstubAllGlobals(); });

test("shows the composer effort selector only for a selected model that supports it", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const models = [
    { name: "Reasoning model", modelId: "reasoning", efforts: ["medium", "high"], defaultEffort: "medium" },
    { name: "Standard model", modelId: "standard", efforts: [], defaultEffort: null },
  ].map(({ name, modelId, efforts, defaultEffort }) => ({
    option: { name, selectable: true, selection: { provider: "codex", profileId: "default", modelId } },
    efforts, defaultEffort,
  }));
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "conversation_models": return (args as { provider: string }).provider === "codex" ? models : [];
      case "conversation_draft": return "";
      case "conversation_history": return { contextId: "context", messages: [] };
      case "latest_conversation_job":
      case "latest_conversation_run": return null;
      case "pending_conversation_instructions":
      case "conversation_side_requests":
      case "saved_conversation_questions": return [];
      case "composer_suggestion_mode": return "off";
      default: throw new Error(`Unexpected command: ${command}`);
    }
  });

  const props = {
    params: { conversationId: "effort-chat", title: "Effort conversation" },
    api: { isVisible: true, renderer: "always", onDidVisibilityChange: () => ({ dispose() {} }) },
  } as unknown as ComponentProps<typeof ConversationPanel>;
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const user = userEvent.setup();
  render(<QueryClientProvider client={client}><ConversationPanel {...props} /></QueryClientProvider>);

  const modelPicker = await screen.findByRole("button", { name: "Model · Choose model" });
  expect(screen.queryByRole("combobox", { name: "Effort" })).toBeNull();
  await user.click(modelPicker);
  await user.click(screen.getByRole("menuitemradio", { name: "Reasoning model" }));
  const effort = screen.getByRole<HTMLSelectElement>("combobox", { name: "Effort" });
  expect(within(effort).getByRole("option", { name: "Default (Medium)" })).toBeTruthy();
  await user.selectOptions(effort, "high");
  expect(effort.value).toBe("high");

  await user.click(screen.getByRole("button", { name: "Model · Reasoning model" }));
  await user.click(screen.getByRole("menuitemradio", { name: "Standard model" }));
  expect(screen.queryByRole("combobox", { name: "Effort" })).toBeNull();
});

test("restores the side question and cancels its own queue attempt", async () => {
  const side = {
    question: "What did the previous turn establish?",
    purpose: "question",
    job: {
      id: "side-job", projectId: "project", queueState: "queued",
      latestAttempt: { runId: "side-run", status: "running", cancelRequested: false },
    },
    response: null,
  };
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "conversation_side_requests") return [side];
    if (command === "interrupt_queued_job_attempt") {
      side.job.latestAttempt.status = "cancelled";
      return;
    }
    throw new Error(`Unexpected command: ${command}`);
  });
  const client = new QueryClient();
  const user = userEvent.setup();
  render(<QueryClientProvider client={client}>
    <SideQuestionPanel conversationId="chat" onClose={vi.fn()} />
  </QueryClientProvider>);
  expect(await screen.findByText(side.question)).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Cancel side question" }));
  expect(await screen.findByText("Cancelled")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("interrupt_queued_job_attempt", {
    projectId: "project", jobId: "side-job", runId: "side-run",
  });
});

test("shows a completed validation receipt without projecting its fixture answer", async () => {
  vi.mocked(invoke).mockResolvedValue([{
    purpose: "validation", provider: "claude", model: "haiku", evidenceId: "verification",
    job: { id: "fixture", queueState: "queued", latestAttempt: { status: "succeeded" } },
    response: { outcome: { kind: "answered", text: "private-fixture-code" } },
  }]);
  const client = new QueryClient();
  const user = userEvent.setup();
  const onStart = vi.fn();
  render(<QueryClientProvider client={client}>
    <SideQuestionPanel conversationId="chat" validation={{ pending: false, onStart }} onClose={vi.fn()} />
  </QueryClientProvider>);
  expect(await screen.findByText("Validation passed")).toBeTruthy();
  expect(screen.queryByText("private-fixture-code")).toBeNull();
  await user.click(screen.getByRole("button", { name: "Validate side questions" }));
  expect(onStart).toHaveBeenCalledOnce();
});

test("renames the attributed conversation in its checkout and refreshes the open tab callback", async () => {
  let conversation: Conversation = {
    id: "conversation", checkoutId: "source-checkout", title: "Original title", contextId: "context",
  };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "list_conversations") return [conversation];
    if (command === "rename_conversation") {
      conversation = { ...conversation, title: (args as { title: string }).title };
      return conversation;
    }
    throw new Error(`Unexpected command: ${command}`);
  });
  const client = new QueryClient();
  const user = userEvent.setup();
  const onOpen = vi.fn();
  const onRenamed = vi.fn();
  render(<QueryClientProvider client={client}>
    <ConversationSidebar checkoutId="source-checkout" onOpen={onOpen} onRenamed={onRenamed} />
  </QueryClientProvider>);
  fireEvent.contextMenu(await screen.findByRole("button", { name: "Original title" }));
  await user.click(screen.getByRole("menuitem", { name: "Rename" }));
  const form = within(screen.getByRole("dialog", { name: "Rename conversation" }));
  const input = form.getByRole("textbox", { name: "Conversation title" });
  await user.clear(input);
  await user.type(input, "  Updated title  ");
  await user.click(form.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("button", { name: "Updated title" })).toBeTruthy();
  await waitFor(() => expect(onRenamed).toHaveBeenCalledExactlyOnceWith(conversation));
  expect(onOpen).not.toHaveBeenCalled();
  expect(invoke).toHaveBeenCalledWith("rename_conversation", {
    conversationId: "conversation", checkoutId: "source-checkout", title: "Updated title",
  });
});
