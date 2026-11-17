// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { useCheckoutConversationActions } from "@/checkout-conversation-actions";
import { ContextActions } from "@/components/ui/context-actions";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("drafts the requested agent review using only the selected files in the clicked folder", async () => {
  const checkoutId = "linked-worktree";
  const open = vi.fn();
  function Folder() {
    const assistant = useCheckoutConversationActions(checkoutId, false, open);
    return <>
      <ContextActions label="docs" groups={assistant.actions({ kind: "folder", path: "docs" })}>
        <button type="button">docs</button>
      </ContextActions>
      {assistant.dialog}
    </>;
  }
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "list_repository_source_files") return ["docs/one.md", "docs/two.md", "docs-other/unrelated.md"];
    if (command === "list_conversations") return [{ id: "existing", title: "Docs review" }];
    if (command === "append_checkout_context_to_conversation") return;
    throw new Error(command);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const user = userEvent.setup();
  render(<QueryClientProvider client={client}><Folder /></QueryClientProvider>);
  fireEvent.contextMenu(screen.getByRole("button", { name: "docs" }));
  const agent = await screen.findByRole("menuitem", { name: "Ask agent" });
  agent.focus();
  await user.keyboard("{ArrowRight}");
  await user.click(await screen.findByRole("menuitem", { name: "Review selected files…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Add checkout context" }));
  expect(dialog.getByText(/Review the selected checkout files for defects/)).toBeTruthy();
  await user.selectOptions(dialog.getByLabelText("Conversation"), "existing");
  expect(dialog.queryByRole("checkbox", { name: "docs-other/unrelated.md" })).toBeNull();
  await user.click(await dialog.findByRole("checkbox", { name: "docs/two.md" }));
  expect(invoke).not.toHaveBeenCalledWith("append_checkout_context_to_conversation", expect.anything());
  await user.click(dialog.getByRole("button", { name: "Add to draft" }));
  await waitFor(() => expect(open).toHaveBeenCalledExactlyOnceWith("existing"));
  expect(invoke).toHaveBeenCalledWith("append_checkout_context_to_conversation", { checkoutId, conversationId: "existing",
    context: "Review the selected checkout files for defects and regressions. Cite the relevant paths and lines.\n\nSource: Selected files in checkout linked-worktree\n\n```\ndocs/two.md\n```",
  });
  expect(invoke).not.toHaveBeenCalledWith("add_checkout_files_to_conversation", expect.anything());
  expect(invoke).not.toHaveBeenCalledWith("start_conversation_turn", expect.anything());
});
