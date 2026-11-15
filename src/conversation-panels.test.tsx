// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import type { Conversation } from "@/bindings/Conversation";
import { ConversationSidebar } from "@/conversation-panels";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

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
