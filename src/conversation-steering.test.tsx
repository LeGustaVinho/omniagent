// @vitest-environment jsdom

import type { ComponentProps } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationPanel } from "@/conversation-panels";
import type { InstructionDeliveryEvidence } from "@/conversation-instructions";
import type { MessageRevision } from "@/bindings/MessageRevision";
import type { PendingInstruction } from "@/bindings/PendingInstruction";
import { RunInspectorNavigation } from "@/run-inspector-navigation";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
  vi.unstubAllGlobals();
});

const conversationId = "conversation";
const runId = "active-turn";
const contextId = "active-context";
const initial = "Keep the working tree clean";
const models = {
  codex: { provider: "codex", profileId: "steering-profile", modelId: "qualified-model" },
  claude: { provider: "claude", profileId: "queue-profile", modelId: "queue-model" },
} as const;

function mount({ recorded = [], messages = [] }: { recorded?: InstructionDeliveryEvidence[]; messages?: MessageRevision[] } = {}) {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const openActivity = vi.fn();
  let draft = initial;
  let instruction: PendingInstruction | undefined;
  let deliveries = recorded;
  vi.mocked(invoke).mockImplementation(async (command, raw) => {
    const params = raw as { provider?: keyof typeof models; selection?: typeof models[keyof typeof models]; text?: string;
      instructionId?: string; revisionId?: string; runId?: string; confirmedUndelivered?: boolean };
    switch (command) {
      case "conversation_draft": return draft;
      case "save_conversation_draft": draft = params.text ?? ""; return;
      case "latest_conversation_run": return { id: runId, status: "running", projectId: "project", checkoutId: "checkout" };
      case "latest_conversation_job":
      case "conversation_goal_overview": return null;
      case "conversation_history": return { contextId, messages };
      case "conversation_models": {
        const provider = params.provider ?? "codex";
        if (!models[provider]) return [];
        return [{ option: { name: provider === "codex" ? "Qualified model" : "Queue model", selection: models[provider], selectable: true }, efforts: [] }];
      }
      case "composer_suggestion_mode": return "off";
      case "saved_conversation_questions":
      case "pending_conversation_inputs": return [];
      case "conversation_events": return { cursor: "0", events: [], more: false };
      case "pending_conversation_instructions": return instruction ? [instruction] : [];
      case "conversation_instruction_deliveries": return deliveries;
      case "preview_conversation_steering": return params.selection?.provider === "codex";
      case "submit_conversation_continuation": {
        expect(params.text).toBe(draft);
        instruction = { id: "queued-instruction", revisionId: "submitted-revision", text: draft, checkoutId: "checkout",
          contextId, provider: params.selection?.provider ?? "codex", model: params.selection?.modelId ?? null,
          mode: "read_only", attachments: [], delivery: "queued", editable: true, runId: null, recordedContext: false, blockers: [] };
        draft = "";
        return { status: "queued", job: { id: instruction.id } };
      }
      case "steer_queued_conversation_instruction": {
        if (!instruction) throw new Error("No queued instruction");
        deliveries = [{ instructionId: instruction.id, revisionId: instruction.revisionId,
          runId, route: "steering", state: "accepted", canResolve: false }];
        instruction = undefined;
        return true;
      }
      case "revise_pending_instruction":
        if (!instruction || !params.text) throw new Error("No pending instruction draft");
        instruction = { ...instruction, text: params.text, revisionId: "revised-instruction" };
        return instruction;
      case "remove_pending_instruction": instruction = undefined; return;
      case "resolve_uncertain_steering":
        deliveries = deliveries.map((delivery) => delivery.instructionId === params.instructionId
          && delivery.revisionId === params.revisionId && delivery.runId === params.runId
          ? { ...delivery, state: params.confirmedUndelivered ? "undelivered" : "discarded", canResolve: false } : delivery);
        return;
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const props = {
    params: { conversationId, title: "Steering conversation" },
    api: { isVisible: true, renderer: "always", onDidVisibilityChange: () => ({ dispose() {} }) },
  } as unknown as ComponentProps<typeof ConversationPanel>;
  render(<QueryClientProvider client={client}><RunInspectorNavigation.Provider value={{ openActivity, activeRunId: undefined }}>
    <ConversationPanel {...props} />
  </RunInspectorNavigation.Provider></QueryClientProvider>);
  const user = userEvent.setup();
  async function choose(provider: keyof typeof models) {
    await user.click(await screen.findByRole("button", { name: "Model · Choose model" }));
    await user.click(await screen.findByRole("menuitemradio", { name: provider === "codex" ? "Qualified model" : "Queue model" }));
  }
  return { user, choose, openActivity };
}

it("offers an active route beside the queue and links acknowledgement to the receiving turn", async () => {
  const { user, choose, openActivity } = mount();
  await choose("codex");
  expect(screen.getByRole("button", { name: "Queue for next turn" })).toBeTruthy();
  await user.click(await screen.findByRole("button", { name: "Send to active turn" }));
  expect(invoke).toHaveBeenCalledWith("preview_conversation_steering", {
    conversationId, expectedContextId: contextId, runId, selection: models.codex, effort: null, mode: "read_only",
  });
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("steer_queued_conversation_instruction", {
    conversationId, runId, instructionId: "queued-instruction",
  }));
  expect(screen.queryByRole("list", { name: "Recorded instruction deliveries" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Instruction delivery history" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Instruction delivery history" }));
  expect(await dialog.findByText("Delivered")).toBeTruthy();
  expect(dialog.getByText("Active turn")).toBeTruthy();
  await user.click(dialog.getByRole("button", { name: "Open receiving turn" }));
  await waitFor(() => expect(openActivity).toHaveBeenCalledWith(runId, conversationId));
  expect(screen.queryByRole("dialog", { name: "Instruction delivery history" })).toBeNull();
});

it("identifies exact revisions and resolves an uncertain delivery from the history dialog", async () => {
  const unresolved = { instructionId: "uncertain", revisionId: "saved-revision", runId: "attempt-a", route: "steering",
    state: "unconfirmed", canResolve: true } as const;
  const { user } = mount({
    recorded: [unresolved, { instructionId: "delivered", revisionId: "unloaded-revision", runId: "attempt-b", route: "standalone",
      state: "accepted", canResolve: false }],
    messages: [{ messageId: "message", revisionId: unresolved.revisionId, role: "user", text: "Check the queued instruction" }],
  });
  const entry = screen.getByRole("button", { name: "Instruction delivery history" });
  await waitFor(() => expect(entry.getAttribute("aria-description")).toBe("1 awaiting review"));
  await user.click(entry);
  const dialog = within(await screen.findByRole("dialog", { name: "Instruction delivery history" }));
  expect(await dialog.findByText("Check the queued instruction")).toBeTruthy();
  expect(dialog.getByText("Separate run")).toBeTruthy();
  expect(dialog.getByTitle("unloaded-revision").textContent).toBe("Instruction unloaded");
  await user.click(dialog.getByRole("button", { name: "Confirm undelivered and queue" }));
  expect(invoke).toHaveBeenCalledWith("resolve_uncertain_steering", {
    conversationId, instructionId: unresolved.instructionId, revisionId: unresolved.revisionId,
    runId: unresolved.runId, confirmedUndelivered: true,
  });
  expect(await dialog.findByText("Confirmed undelivered")).toBeTruthy();
  await user.keyboard("{Escape}");
  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("button", { name: "Instruction delivery history" })));
});

it("keeps an incompatible selection queued and edits or removes its exact unclaimed revision", async () => {
  const { user, choose } = mount();
  await choose("claude");
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("preview_conversation_steering", expect.objectContaining({ selection: models.claude })));
  expect(screen.queryByRole("button", { name: "Send to active turn" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Queue for next turn" }));
  const pending = within(await screen.findByRole("list", { name: "Pending instructions" }));
  expect(pending.getByText(initial)).toBeTruthy();
  await user.click(pending.getByRole("button", { name: "Edit instruction" }));
  const edit = pending.getByRole("textbox", { name: "Pending instruction text" });
  await user.clear(edit);
  await user.type(edit, "Use the saved test suite");
  await user.click(pending.getByRole("button", { name: "Save instruction" }));
  expect(await pending.findByText("Use the saved test suite")).toBeTruthy();
  await user.click(pending.getByRole("button", { name: "Remove instruction" }));
  expect(invoke).toHaveBeenCalledWith("remove_pending_instruction", expect.objectContaining({ revisionId: "revised-instruction" }));
  await waitFor(() => expect(screen.queryByRole("list", { name: "Pending instructions" })).toBeNull());
  expect(invoke).not.toHaveBeenCalledWith("steer_queued_conversation_instruction", expect.anything());
});
