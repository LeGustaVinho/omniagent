// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useRef } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { ConversationAttachments, useConversationAttachments } from "@/conversation-attachments";

const native = vi.hoisted(() => ({ listen: vi.fn(), onDragDropEvent: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/webview", () => ({ getCurrentWebview: () => native }));

afterEach(() => {
  cleanup();
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  vi.restoreAllMocks();
  vi.resetAllMocks();
});

function Composer() {
  const ref = useRef<HTMLFormElement>(null);
  const attachments = useConversationAttachments("conversation", true, true, ref);
  return <form ref={ref} aria-label="Composer">
    <ConversationAttachments attachments={attachments} checkoutId="conversation-checkout" disabled={false} />
  </form>;
}

function mount() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}><Composer /></QueryClientProvider>);
}

it("keeps native picker and drop files in one recoverable draft and removes by its current revision", async () => {
  Object.defineProperty(window, "__TAURI_INTERNALS__", { value: {}, configurable: true });
  let drop: ((event: { payload: { id: string; x: number; y: number } }) => void) | undefined;
  native.listen.mockImplementation(async (_name, callback) => { drop = callback; return vi.fn(); });
  native.onDragDropEvent.mockResolvedValue(vi.fn());
  const first = { id: "one", source: { kind: "checkout", relativePath: "docs/note.md" }, delivery: null };
  const second = { id: "two", source: { kind: "selected_file", path: "C:\\share\\screenshot.png" }, delivery: null };
  const selected = { revision: "r1", files: [first] };
  const dropped = { revision: "r2", files: [first, second] };
  let saved: { revision: string | null; files: (typeof first | typeof second)[] } = { revision: null, files: [] };
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "pick_conversation_attachments") saved = selected;
    if (command === "accept_conversation_attachment_drop") saved = dropped;
    if (command === "edit_conversation_attachment_draft") saved = { revision: "r3", files: [second] };
    return saved;
  });
  const user = userEvent.setup();
  const view = mount();
  vi.spyOn(screen.getByRole("form", { name: "Composer" }), "getBoundingClientRect")
    .mockReturnValue(new DOMRect(20, 20, 180, 100));
  await waitFor(() => expect(screen.getByRole("button", { name: "Attach files" })).not.toHaveProperty("disabled", true));
  await user.click(screen.getByRole("button", { name: "Attach files" }));
  expect(await screen.findByText("note.md · Checkout")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("pick_conversation_attachments", { conversationId: "conversation" });
  drop?.({ payload: { id: "outside-target", x: 1, y: 1 } });
  expect(invoke).not.toHaveBeenCalledWith("accept_conversation_attachment_drop", expect.anything());
  drop?.({ payload: { id: "native-receipt", x: 30, y: 30 } });
  expect(await screen.findByText("screenshot.png · Selected external file")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("accept_conversation_attachment_drop", { conversationId: "conversation", dropId: "native-receipt" });
  view.unmount();
  mount();
  const files = within(await screen.findByRole("list", { name: "Draft files" }));
  expect(files.getAllByRole("listitem")).toHaveLength(2);
  fireEvent.contextMenu(files.getByText("screenshot.png · Selected external file"));
  expect(screen.queryByRole("menuitem", { name: "Show source in File Explorer" })).toBeNull();
  await user.click(screen.getByRole("menuitem", { name: "Copy original path" }));
  expect(await navigator.clipboard.readText()).toBe(second.source.path);

  const note = files.getByText("note.md · Checkout");
  fireEvent.contextMenu(note);
  await user.click(screen.getByRole("menuitem", { name: "Show source in File Explorer" }));
  expect(invoke).toHaveBeenCalledWith("open_checkout_item_in_shell", {
    checkoutId: "conversation-checkout", relativePath: "docs/note.md", action: "reveal",
  });
  fireEvent.contextMenu(note);
  await user.click(screen.getByRole("menuitem", { name: "Remove from draft" }));
  expect(invoke).toHaveBeenCalledWith("edit_conversation_attachment_draft", {
    conversationId: "conversation", expectedRevision: "r2", fileId: "one", edit: { kind: "remove" },
  });
  await waitFor(() => expect(files.getAllByRole("listitem")).toHaveLength(1));
});
