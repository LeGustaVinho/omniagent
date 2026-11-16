// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import { afterEach, expect, it, vi } from "vitest";
import {
  ConversationContinuationDialog, ConversationContinuationSettings, type ConversationContinuationReview,
} from "@/conversation-continuation";
import { ConversationPanel } from "@/conversation-panels";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.resetAllMocks();
});

const review: ConversationContinuationReview = {
  status: "review_required",
  previewId: "protected-preview",
  contextId: "saved-revision",
  reason: "profile_changed",
  settings: { projectDefault: "same_tab", conversationOverride: null, effective: "same_tab" },
  seed: [
    { revisionId: "user", role: "user", text: "Earlier revision\nwith a line break.",
      attachments: [{ name: "brief.md", sha256: "frozen-digest", size: 22, delivery: { kind: "excerpt", start: 0, end: 6 },
        excerpt: "Frozen", referenceTarget: null }] },
    { revisionId: "assistant", role: "assistant", text: "Saved assistant answer.", attachments: [] },
  ],
};

it("defaults to same-tab continuation, previews the saved seed, and excludes it for a fresh conversation", async () => {
  const user = userEvent.setup();
  const onConfirm = vi.fn();
  const onClose = vi.fn();
  render(<ConversationContinuationDialog review={review} pending={false} onConfirm={onConfirm} onClose={onClose} />);
  const dialog = within(screen.getByRole("dialog", { name: "Choose how to continue" }));
  expect((dialog.getByRole("radio", { name: "Continue in this tab" }) as HTMLInputElement).checked).toBe(true);
  const seed = within(dialog.getByRole("region", { name: "History seed" }));
  expect(seed.getByText("Earlier revision with a line break.").textContent).toBe(review.seed[0]?.text);
  expect(seed.getByText("Saved assistant answer.")).toBeTruthy();
  expect(seed.getByText("brief.md · excerpt · 22 bytes")).toBeTruthy();
  expect(seed.getByText("Frozen")).toBeTruthy();
  await user.click(dialog.getByRole("radio", { name: "Start a fresh conversation" }));
  expect(dialog.queryByRole("region", { name: "History seed" })).toBeNull();
  await user.click(dialog.getByRole("radio", { name: "Fork with context" }));
  expect(dialog.getByRole("region", { name: "History seed" })).toBeTruthy();
  expect(onConfirm).not.toHaveBeenCalled();
  await user.click(dialog.getByRole("button", { name: "Continue and send" }));
  expect(onConfirm).toHaveBeenCalledExactlyOnceWith("fork_with_context");
  await user.click(dialog.getByRole("button", { name: "Cancel" }));
  expect(onClose).toHaveBeenCalledOnce();
});

it("saves project defaults and lets a conversation restore inheritance", async () => {
  const user = userEvent.setup();
  let settings = review.settings;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "conversation_continuation_settings") return settings;
    if (command !== "set_conversation_continuation_policy") throw new Error(`Unexpected IPC ${command}`);
    const { level, choice } = args as { level: "project" | "conversation"; choice: typeof settings.conversationOverride };
    const projectDefault = level === "project" && choice ? choice : settings.projectDefault;
    const conversationOverride = level === "conversation" ? choice : settings.conversationOverride;
    settings = { projectDefault, conversationOverride, effective: conversationOverride ?? projectDefault };
    return settings;
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><ConversationContinuationSettings conversationId="original-chat" /></QueryClientProvider>);
  await user.click(screen.getByRole("button", { name: "Continuation" }));
  const popover = within(await screen.findByRole("dialog", { name: "Continuation settings" }));
  const project = await popover.findByRole<HTMLSelectElement>("combobox", { name: "Project default" });
  const conversation = popover.getByRole<HTMLSelectElement>("combobox", { name: "Conversation override" });
  await user.selectOptions(project, "fork_with_context");
  await waitFor(() => expect(project.value).toBe("fork_with_context"));
  await user.selectOptions(conversation, "fresh");
  await waitFor(() => expect(conversation.value).toBe("fresh"));
  await user.selectOptions(conversation, "");
  await waitFor(() => expect(conversation.value).toBe(""));
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "set_conversation_continuation_policy")).toEqual([
    ["set_conversation_continuation_policy", { conversationId: "original-chat", level: "project", choice: "fork_with_context" }],
    ["set_conversation_continuation_policy", { conversationId: "original-chat", level: "conversation", choice: "fresh" }],
    ["set_conversation_continuation_policy", { conversationId: "original-chat", level: "conversation", choice: null }],
  ]);
});

it("holds the explicit send until confirmation, then opens the returned fork by conversation ID", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const selection = { provider: "codex", profileId: "changed-profile", modelId: "changed-model" };
  const text = "Send from my protected draft";
  const pending = { id: "forked-chat", title: "Saved conversation (fork)", contextId: "fork-seed", checkoutId: "checkout" };
  vi.mocked(invoke).mockImplementation(async (command) => {
    switch (command) {
      case "conversation_draft": return text;
      case "conversation_history": return { contextId: review.contextId, messages: [] };
      case "conversation_models":
        return [{ option: { name: "Changed model", selection, selectable: true, rating: null }, efforts: [] }];
      case "composer_suggestion_mode": return "off";
      case "conversation_goal_overview":
      case "latest_conversation_job":
      case "latest_conversation_run": return null;
      case "pending_conversation_instructions": return [];
      case "save_conversation_draft": return;
      case "submit_conversation_continuation": return review;
      case "confirm_conversation_continuation": return { conversation: pending, job: { id: "queued-job" } };
      default: throw new Error(`Unexpected IPC ${command}`);
    }
  });
  const addPanel = vi.fn();
  const props = {
    params: { conversationId: "source-chat", title: "Saved conversation" },
    api: { isVisible: true, renderer: "always", onDidVisibilityChange: () => ({ dispose() {} }) },
    containerApi: { getPanel: () => undefined, addPanel },
  } as unknown as ComponentProps<typeof ConversationPanel>;
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><ConversationPanel {...props} /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Model · Choose model" }));
  await user.click(screen.getByRole("menuitemradio", { name: "Changed model" }));
  const send = screen.getByRole("button", { name: "Send" });
  await user.click(send);
  let dialog = within(await screen.findByRole("dialog", { name: "Choose how to continue" }));
  expect(invoke).toHaveBeenCalledWith("submit_conversation_continuation", {
    conversationId: "source-chat", expectedContextId: review.contextId, selection, effort: null, mode: "read_only", text,
  });
  expect(invoke).not.toHaveBeenCalledWith("start_conversation_turn", expect.anything());
  expect(invoke).not.toHaveBeenCalledWith("confirm_conversation_continuation", expect.anything());
  await user.click(dialog.getByRole("button", { name: "Cancel" }));
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Message draft" }).value).toBe(text);
  expect(addPanel).not.toHaveBeenCalled();

  await user.click(send);
  dialog = within(await screen.findByRole("dialog", { name: "Choose how to continue" }));
  await user.click(dialog.getByRole("radio", { name: "Fork with context" }));
  await user.click(dialog.getByRole("button", { name: "Continue and send" }));
  expect(invoke).toHaveBeenCalledWith("confirm_conversation_continuation", {
    conversationId: "source-chat", previewId: review.previewId, choice: "fork_with_context", selection,
    effort: null, mode: "read_only", text,
  });
  await waitFor(() => expect(addPanel).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
    id: `conversation:${pending.id}`, params: { conversationId: pending.id, title: pending.title },
  })));
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Message draft" }).value).toBe("");
});
