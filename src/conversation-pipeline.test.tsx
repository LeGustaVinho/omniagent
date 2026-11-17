// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, renderHook, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { ConversationPanel, ConversationSidebar } from "@/conversation-panels";
import { directConversationChoice, repositoryDeliveryChoice } from "@/conversation-pipeline-picker";
import { ConversationPipelineTurnStatus, useConversationPipelineTurns } from "@/conversation-pipeline-turns";
import { RunInspectorNavigation } from "@/run-inspector-navigation";
import { useWorkspace } from "@/workspace-state";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => {
  cleanup();
  vi.resetAllMocks();
  vi.unstubAllGlobals();
  useWorkspace.getState().selectCheckout(null);
  localStorage.clear();
});

const workflow = { id: "review", revision: "abc12345pinned" };
const published = { kind: "published", workflow } as const;
const checkout = { projectId: "project", checkoutId: "checkout", repositoryRoot: "C:/fixture", checkoutRoot: "C:/fixture" };

function client() {
  return new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
}

test("reports when the chosen workflow branch skips an explicitly targeted slash action", () => {
  render(<ConversationPipelineTurnStatus attached turn={{ requestId: "turn", text: "Review this change", state: "succeeded",
    actionOutcome: { nodeId: "review", label: "Review changes", state: "skipped" } }} />);
  expect(screen.getByText("Pipeline completed", { exact: false })).toBeTruthy();
  expect(screen.getByText("The pipeline skipped Review changes. Its slash action was not run.")).toBeTruthy();
});

test("reopened conversations use the last accepted pipeline inputs and default model", async () => {
  const selection = { provider: "codex", modelId: "recorded-model", profileId: "profile" };
  const savedInputs = { documentRevisionId: "markdown-revision", values: { priority: "high" } };
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "conversation_pipeline_schema") return { fields: [], requiresDocument: true, requiresModel: true };
    if (command === "conversation_pipeline_turns") return [
      { requestId: "accepted", state: "succeeded", inputs: savedInputs, selection, effort: "high" },
      { requestId: "rejected", state: "failed", inputs: { values: {} } },
    ];
    throw new Error(command);
  });
  const queryClient = client();
  const { result } = renderHook(() => useConversationPipelineTurns("reopened", true, async () => {}), {
    wrapper: ({ children }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>,
  });
  await waitFor(() => expect(result.current.inputs).toEqual(savedInputs));
  expect(result.current.defaultModel).toMatchObject({ selection, effort: "high" });
  expect(result.current.setupNeeded).toBe(false);
});

test("requires a conversation pipeline choice and creates the exact published revision", async () => {
  const conversation = { id: "first", checkoutId: checkout.checkoutId, title: "Conversation 1",
    pipeline: { ...published, name: "Code review" } };
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "list_conversations") return [];
    if (command === "conversation_pipeline_options") return [
      directConversationChoice, repositoryDeliveryChoice, { name: "Code review", pipeline: published },
    ];
    if (command === "create_conversation") return conversation;
    throw new Error(command);
  });
  const onOpen = vi.fn();
  const user = userEvent.setup();
  render(<QueryClientProvider client={client()}><ConversationSidebar checkoutId={checkout.checkoutId} onOpen={onOpen} /></QueryClientProvider>);
  const create = screen.getByRole<HTMLButtonElement>("button", { name: "New conversation" });
  await waitFor(() => expect(create.disabled).toBe(false));
  await user.click(create);
  const dialog = within(await screen.findByRole("dialog", { name: "Choose a conversation pipeline" }));
  expect(dialog.getByRole<HTMLButtonElement>("button", { name: "Create conversation" }).disabled).toBe(true);
  await user.click(await dialog.findByRole("radio", { name: /Code review/ }));
  await user.click(dialog.getByRole("button", { name: "Create conversation" }));
  await waitFor(() => expect(onOpen).toHaveBeenCalledOnce());
  expect(invoke).toHaveBeenCalledWith("create_conversation", { checkoutId: checkout.checkoutId, title: "Conversation 1", pipeline: published });
});

test("keeps the draft after a rejected pipeline turn, reuses its request id and links a waiting run", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  useWorkspace.getState().selectCheckout(checkout);
  let draft = "Review the release";
  let attempted = 0;
  const turns: unknown[] = [];
  vi.mocked(invoke).mockImplementation(async (command, raw) => {
    const args = raw as { text?: string; request?: Record<string, unknown> } | undefined;
    switch (command) {
      case "checkout_conversation": return { id: "chat", checkoutId: checkout.checkoutId, title: "Review", contextId: "context",
        pipeline: { ...published, name: "Code review" } };
      case "conversation_draft": return draft;
      case "save_conversation_draft": draft = args?.text ?? ""; return;
      case "conversation_history": return { contextId: "context", messages: [] };
      case "conversation_pipeline_schema": return {
        fields: [{ name: "priority", valueType: "text", required: true }, { name: "message", valueType: "text", required: true, conversationSource: "message" }],
        requiresDocument: true, requiresWork: false, requiresEvent: false, requiresModel: false,
      };
      case "conversation_pipeline_turns": return turns.slice();
      case "list_repository_markdown_revisions": return [{ id: "note", relativePath: "Release.md", sha256: "pinned-sha" }];
      case "project_work_hierarchy": return { work: [] };
      case "enqueue_conversation_pipeline_turn": {
        if (!attempted++) throw new Error("Revision changed during validation");
        const row = { ...args?.request, state: "running", runId: "workflow-run", waiting: { nodeId: "approval", label: "Approve changes", kind: "approval" },
          artifacts: [{ id: "plan-1", nodeId: "prepare plan", port: "document", kind: "plan" }] };
        turns.push(row);
        draft = "";
        return row;
      }
      case "latest_conversation_run":
      case "latest_conversation_job":
      case "conversation_goal_overview":
      case "checkout_model_defaults": return null;
      case "conversation_models":
      case "pending_conversation_instructions":
      case "conversation_instruction_deliveries":
      case "pending_conversation_inputs":
      case "saved_conversation_questions": return [];
      default: throw new Error(command);
    }
  });
  const openWorkspace = vi.fn();
  const props = { params: { conversationId: "chat", title: "Review" },
    api: { isVisible: true, renderer: "always", onDidVisibilityChange: () => ({ dispose() {} }) },
  } as unknown as ComponentProps<typeof ConversationPanel>;
  render(<QueryClientProvider client={client()}>
    <RunInspectorNavigation.Provider value={{ openActivity: vi.fn(), openWorkspace }}>
      <ConversationPanel {...props} />
    </RunInspectorNavigation.Provider>
  </QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Choose inputs" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Pipeline inputs" }));
  expect(dialog.queryByRole("textbox", { name: "message" })).toBeNull();
  await dialog.findByRole("option", { name: /Release\.md/ });
  await user.selectOptions(dialog.getByRole("combobox", { name: "Saved Markdown revision" }), "note");
  await user.type(dialog.getByRole("textbox", { name: "priority" }), "high");
  await user.click(dialog.getByRole("button", { name: "Send with these inputs" }));
  expect(await screen.findByText(/Revision changed during validation/)).toBeTruthy();
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Message draft" }).value).toBe("Review the release");
  await user.click(screen.getByRole("button", { name: "Run pipeline" }));
  await user.click(await screen.findByRole("button", { name: "Open waiting step" }));
  expect(openWorkspace).toHaveBeenCalledWith("pipelines", "run:workflow-run?node=approval");
  await user.click(screen.getByRole("button", { name: "document · plan" }));
  expect(openWorkspace).toHaveBeenCalledWith("pipelines", "run:workflow-run?node=prepare+plan&artifact=plan-1");
  const requests = vi.mocked(invoke).mock.calls.filter(([command]) => command === "enqueue_conversation_pipeline_turn")
    .map(([, args]) => (args as { request: Record<string, unknown> }).request);
  expect(requests).toHaveLength(2);
  expect(requests[0]?.requestId).toBe(requests[1]?.requestId);
  expect(requests[0]?.inputs).toMatchObject({ documentRevisionId: "note", values: { priority: "high" } });
  expect(invoke).not.toHaveBeenCalledWith("submit_conversation_continuation", expect.anything());
});
