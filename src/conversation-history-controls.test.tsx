// @vitest-environment jsdom

import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import type { ConversationHistory } from "@/bindings/ConversationHistory";
import {
  ConversationHistoryControls, ConversationMessageHistoryActions, type ConversationMessageHistoryAction,
} from "@/conversation-history-controls";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const conversationId = "first-chat";
const first = { messageId: "message-1", revisionId: "revision-1", role: "user", text: "Original request" } as const;
const answer = { messageId: "message-2", revisionId: "revision-2", role: "assistant", text: "First answer" } as const;
const later = { messageId: "message-3", revisionId: "revision-3", role: "user", text: "Later request" } as const;
const original: ConversationHistory = { contextId: "original-context", messages: [first, answer, later] };
const selection = { provider: "codex", profileId: "fixture", modelId: "codex-test" } as const;

function setup() {
  let active = original;
  const previous: { contextId: string; supersededBy: string; action: string; messageCount: number; replacedAt: string }[] = [];
  const archived = new Map<string, ConversationHistory>();
  const opened = vi.fn();
  const fork = { id: "new-chat", contextId: "new-context", title: "Scoped chat (fork)", checkoutId: "same-checkout" };
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    const args = input as { throughRevision?: string; contextId?: string; title?: string };
    switch (command) {
      case "conversation_history": return active;
      case "conversation_queue_history_policy": return { projectDefault: "review", conversationOverride: null };
      case "conversation_history_inspection": return { previous: [...previous], fork: null, retainedQuestions: [{ runId: "pinned-run", requestId: 17 }] };
      case "conversation_next_prompt_instructions": return { sources: [{ path: "D:/repo/AGENTS.md", selection: "expected" }], unresolved: [] };
      case "conversation_history_revision": return archived.get(args.contextId!);
      case "fork_conversation": case "fork_conversation_with_message_edit": case "fork_retained_conversation": return { ...fork, title: args.title };
      case "rewind_conversation_history": {
        const revision = args.throughRevision;
        const nextContextId = `context-${previous.length + 1}`;
        archived.set(active.contextId, active);
        previous.unshift({ contextId: active.contextId, supersededBy: nextContextId,
          action: revision ? "rewind" : "clear", messageCount: active.messages.length, replacedAt: "2026-11-16T10:00:00Z" });
        active = { contextId: nextContextId, messages: revision ? active.messages.slice(0, active.messages.findIndex(({ revisionId }) => revisionId === revision) + 1) : [] };
        return active.contextId;
      }
      default: throw new Error(`Unexpected ${command}`);
    }
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  function Harness({ busy }: { busy: boolean }) {
    const [requestedAction, setAction] = useState<ConversationMessageHistoryAction>();
    const history = useQuery({ queryKey: ["conversation", conversationId, "messages"], queryFn: () => invoke<ConversationHistory>("conversation_history", { conversationId }) });
    return <>
      <ConversationHistoryControls conversationId={conversationId} history={history.data} title="Scoped chat" disabled={busy}
        mode="plan" selection={selection} openFork={opened} requestedAction={requestedAction} onRequestedActionClose={() => setAction(undefined)} />
      {history.data?.messages.map((message) => <section key={message.revisionId} aria-label={message.text}>
        <ConversationMessageHistoryActions message={message} disabled={busy} onAction={setAction} />
      </section>)}
    </>;
  }
  const view = render(<QueryClientProvider client={client}><Harness busy={false} /></QueryClientProvider>);
  return { client, opened, user: userEvent.setup(), updateBusy: (busy: boolean) => view.rerender(<QueryClientProvider client={client}><Harness busy={busy} /></QueryClientProvider>) };
}

it("rewinds inclusively, clears a new context, and inspects or forks the preserved revision", async () => {
  const { user, opened } = setup();
  const turn = within(await screen.findByRole("region", { name: answer.text }));
  await user.click(turn.getByRole("button", { name: "Rewind here" }));
  const rewind = within(screen.getByRole("dialog", { name: "Conversation history" }));
  expect((rewind.getByRole("combobox", { name: "Keep messages through" }) as HTMLSelectElement).value).toBe(answer.revisionId);
  expect(await rewind.findByText(/AGENTS\.md/)).toBeTruthy();
  expect(rewind.getByText("Planning assistant · Plan Mode")).toBeTruthy();
  expect(await rewind.findByText(/Pinned workflow questions retained.*1/)).toBeTruthy();
  await user.click(rewind.getByRole("button", { name: "Confirm rewind" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(invoke).toHaveBeenCalledWith("rewind_conversation_history", { conversationId, baseId: original.contextId, throughRevision: answer.revisionId });
  expect(screen.queryByRole("region", { name: later.text })).toBeNull();

  await user.click(screen.getByRole("button", { name: "History" }));
  await user.click(screen.getByRole("button", { name: "Confirm clear" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(invoke).toHaveBeenCalledWith("rewind_conversation_history", { conversationId, baseId: "context-1", throughRevision: null });

  await user.click(screen.getByRole("button", { name: "History" }));
  await user.click(screen.getByText("Inspect earlier context"));
  await user.click(await screen.findByRole("button", { name: /Before rewind · 3 messages/ }));
  const saved = (await screen.findByText(later.text)).closest("li")!;
  expect(saved).toBeTruthy();
  await user.click(within(saved).getByRole("button", { name: "Fork from saved message" }));
  await user.click(screen.getByRole("button", { name: "Create fork" }));
  await waitFor(() => expect(opened).toHaveBeenCalledOnce());
  expect(invoke).toHaveBeenCalledWith("fork_retained_conversation", { conversationId, expectedContextId: "context-2", sourceContextId: original.contextId,
    throughRevision: later.revisionId, title: "Scoped chat (fork)" });
});

it("opens a head fork and an edited-message fork through the conversation tab callback", async () => {
  const { user, opened } = setup();
  await screen.findByRole("region", { name: first.text });
  await user.click(screen.getByRole("button", { name: "History" }));
  await user.selectOptions(screen.getByRole("combobox", { name: "Action" }), "fork");
  await user.click(screen.getByRole("button", { name: "Create fork" }));
  await waitFor(() => expect(opened).toHaveBeenCalledWith(expect.objectContaining({ id: "new-chat" })));
  expect(invoke).toHaveBeenCalledWith("fork_conversation", { conversationId, expectedContextId: original.contextId, throughRevision: null, title: "Scoped chat (fork)" });

  await user.click(within(screen.getByRole("region", { name: first.text })).getByRole("button", { name: "Edit in a fork" }));
  const input = screen.getByRole("textbox", { name: "Edit user message" });
  expect((input as HTMLTextAreaElement).value).toBe(first.text);
  await user.clear(input);
  await user.type(input, "Alternative request");
  await user.click(screen.getByRole("button", { name: "Create fork with edit" }));
  await waitFor(() => expect(opened).toHaveBeenCalledTimes(2));
  expect(invoke).toHaveBeenCalledWith("fork_conversation_with_message_edit", { conversationId, expectedContextId: original.contextId,
    throughRevision: first.revisionId, title: "Scoped chat (fork)", text: "Alternative request" });
  expect(screen.getByRole("region", { name: first.text })).toBeTruthy();
});

it("disables a reviewed action if the active turn starts or the saved context changes", async () => {
  const { user, client, updateBusy } = setup();
  await screen.findByRole("region", { name: first.text });
  await user.click(screen.getByRole("button", { name: "History" }));
  updateBusy(true);
  expect((screen.getByRole("button", { name: "Confirm clear" }) as HTMLButtonElement).disabled).toBe(true);
  expect(screen.getByText(/Wait for the active turn/)).toBeTruthy();
  updateBusy(false);
  client.setQueryData(["conversation", conversationId, "messages"], { ...original, contextId: "changed" });
  expect(await screen.findByText(/The conversation context changed/)).toBeTruthy();
  expect((screen.getByRole("button", { name: "Confirm clear" }) as HTMLButtonElement).disabled).toBe(true);
  expect(invoke).not.toHaveBeenCalledWith("rewind_conversation_history", expect.anything());
});
