// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationDeliveries, type ConversationInput } from "@/conversation-deliveries";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("reviews the exact uncertain revision in the receiving run and keeps earlier deliveries ordered", async () => {
  const client = new QueryClient();
  const initial = {
    instructionId: "initial", revisionId: "revision-1", text: "Original prompt", mode: "standalone",
    status: "submitted", providerSessionId: "native-session", providerTurnId: "native-turn",
    receipt: { kind: "turn_started", eventSequence: "17" }, steeringFrame: null, review: null,
  } as const;
  const input: ConversationInput = {
    delivery: "submitted", turnId: "turn-id", frames: ["original wire"], attachments: [], removedAttachmentCount: 0,
    instructions: [initial, {
      ...initial, instructionId: "uncertain", revisionId: "revision-2", text: "Uncertain follow-up",
      mode: "steering", status: "unconfirmed", receipt: null, steeringFrame: "follow-up wire",
    }],
  };
  const user = userEvent.setup();
  render(<QueryClientProvider client={client}>
    <ConversationDeliveries input={input} conversationId="chat-id" runId="run-id" live={false} />
  </QueryClientProvider>);
  expect(screen.getByText("original wire")).toBeTruthy();
  const rows = within(screen.getByRole("list", { name: "Instruction deliveries" })).getAllByRole("listitem");
  expect(rows.map((row) => row.textContent)).toEqual([
    expect.stringContaining("Original prompt"), expect.stringContaining("Uncertain follow-up"),
  ]);
  const [original, uncertain] = rows;
  if (!original || !uncertain) throw new Error("Missing instruction delivery rows");
  expect(within(original).getByText(/Provider confirmed turn start/)).toBeTruthy();
  await user.click(within(uncertain).getByRole("button", { name: "Confirm undelivered" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("resolve_uncertain_steering", {
    conversationId: "chat-id", runId: "run-id", instructionId: "uncertain", revisionId: "revision-2", confirmedUndelivered: true,
  }));
});

it("shows the retained attachments and requests native bytes by original ordinal after a deletion", async () => {
  const client = new QueryClient();
  const metadata = {
    name: "notes.txt", mimeType: "text/plain", source: { kind: "checkout" as const, relativePath: "docs/notes.txt" },
    sourceSha256: "f".repeat(64), sourceSize: 128, messageRevisionId: "revision-before-edit", reportedReads: [],
  };
  vi.mocked(invoke).mockResolvedValue(null);
  const user = userEvent.setup();
  render(<QueryClientProvider client={client}>
    <ConversationDeliveries input={{
      delivery: "submitted", turnId: "turn-id", frames: [], instructions: [], removedAttachmentCount: 1, attachments: [
        { ...metadata, ordinal: 0, transport: { method: "excerpt", start: 11, end: 20, text: "café ☕" } },
        { ...metadata, ordinal: 2, transport: { method: "reference", runnerPath: "C:\\checkout\\before-rename.txt", uri: null },
          reportedReads: [{ eventSequence: "42", path: "before-rename.txt", tool: "Read" }] },
        { ...metadata, ordinal: 3, transport: { method: "native", encoding: "acp_resource", runnerPath: null, resourceUri: "file:///original" } },
      ],
    }} conversationId="chat-id" runId="run-id" live={false} checkoutId="original-run-checkout" />
  </QueryClientProvider>);
  expect(screen.getByText("Exact input is unavailable after attachment history was deleted.")).toBeTruthy();
  await user.click(screen.getByText("Saved attachments", { selector: "summary" }));
  const list = within(screen.getByRole("list", { name: "Saved attachments" }));
  expect(list.getByText("UTF-8 bytes 11–20")).toBeTruthy();
  expect(list.getByText("café ☕")).toBeTruthy();
  expect(list.getByText("Runner path C:\\checkout\\before-rename.txt")).toBeTruthy();
  expect(within(list.getByRole("list", { name: "CLI-reported reads" })).getByText(
    "CLI reported read via Read · before-rename.txt · Event 42",
  )).toBeTruthy();
  fireEvent.contextMenu(list.getByText("notes.txt · Excerpt"));
  await user.click(screen.getByRole("menuitem", { name: "Copy saved excerpt" }));
  expect(await navigator.clipboard.readText()).toBe("café ☕");
  fireEvent.contextMenu(list.getByText("notes.txt · File reference"));
  await user.click(screen.getByRole("menuitem", { name: "Show source in File Explorer" }));
  expect(invoke).toHaveBeenCalledWith("open_checkout_item_in_shell", {
    checkoutId: "original-run-checkout", relativePath: "docs/notes.txt", action: "reveal",
  });
  fireEvent.contextMenu(list.getByText("notes.txt · Native content"));
  await user.click(screen.getByRole("menuitem", { name: "Download saved bytes" }));
  expect(invoke).toHaveBeenCalledWith("conversation_attachment_content", { conversationId: "chat-id", runId: "run-id", index: 3 });
});

it.each(["native", "excerpt", "reference"] as const)("reuses a saved %s from the clicked attempt in the chosen conversation draft", async (method) => {
  const checkoutId = "destination-worktree";
  const conversationId = "source-conversation";
  const runId = "run-id";
  const destinationConversationId = "destination-conversation";
  const attachment = {
    ordinal: 4, name: "release.txt", mimeType: "text/plain", source: { kind: "checkout", relativePath: "obsolete/release.txt" },
    sourceSha256: "a".repeat(64), sourceSize: 90, messageRevisionId: "source-revision", reportedReads: [],
    transport: method === "native" ? { method, encoding: "acp_resource", runnerPath: null, resourceUri: null }
      : method === "excerpt" ? { method, start: 27, end: 48, text: "exact saved paragraph" }
        : { method, runnerPath: "D:/old-runner/release.txt", uri: "file:///D:/old-runner/release.txt" },
  } satisfies ConversationInput["attachments"][number];
  const input: ConversationInput = { removedAttachmentCount: 2, delivery: "submitted", turnId: "turn", frames: [], instructions: [], attachments: [attachment] };
  const open = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  let retained = true;
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "list_conversations") return [{ id: destinationConversationId, title: "Release follow-up" }];
    if (command === "conversation_input") return retained ? input : { ...input, attachments: [] };
    if (command === "conversation_attachment_content") return btoa("bytes that reached the original provider\r\n");
    if (command === "reuse_saved_conversation_attachment" || command === "append_checkout_context_to_conversation") return;
    throw new Error(command);
  });
  const user = userEvent.setup();
  render(<QueryClientProvider client={client}>
    <ConversationDeliveries input={input} conversationId={conversationId} runId={runId} live={false} checkoutId="origin-worktree"
      draftDestination={{ checkoutId, open }} />
  </QueryClientProvider>);
  await user.click(screen.getByText("Saved attachments", { selector: "summary" }));
  if (method === "native") {
    fireEvent.contextMenu(screen.getByText(/^release\.txt ·/));
    await user.click(screen.getByRole("menuitem", { name: "Preview saved attachment" }));
    const preview = within(await screen.findByRole("dialog", { name: "Saved attachment preview" }));
    await preview.findByText("bytes that reached the original provider");
    expect(invoke).toHaveBeenCalledWith("conversation_attachment_content", { conversationId, runId, index: 4 });
    await user.click(preview.getByRole("button", { name: "Close dialog" }));
  }
  fireEvent.contextMenu(screen.getByText(/^release\.txt ·/));
  await user.click(screen.getByRole("menuitem", { name: method === "native" ? "Reuse saved attachment…"
    : method === "excerpt" ? "Add saved excerpt to draft…" : "Add saved reference to draft…" }));
  const dialog = within(await screen.findByRole("dialog", { name: method === "native" ? "Reuse saved attachment" : "Add checkout context" }));
  expect(dialog.getByText(/Run run-id · Attachment 5 · Message revision source-revision/)).toBeTruthy();
  await dialog.findByRole("option", { name: "Release follow-up" });
  expect(invoke).toHaveBeenCalledWith("list_conversations", { checkoutId });
  expect(open).not.toHaveBeenCalled();
  await user.selectOptions(dialog.getByLabelText("Conversation"), destinationConversationId);
  if (method === "reference") {
    retained = false;
    await user.click(dialog.getByRole("button", { name: "Add to draft" }));
    expect((await dialog.findByRole("alert")).textContent).toContain("This saved attachment changed or was removed");
    expect(invoke).not.toHaveBeenCalledWith("append_checkout_context_to_conversation", expect.anything());
    retained = true;
  }
  await user.click(dialog.getByRole("button", { name: "Add to draft" }));
  await waitFor(() => expect(open).toHaveBeenCalledExactlyOnceWith(destinationConversationId));
  if (method === "native") {
    expect(invoke).toHaveBeenCalledWith("reuse_saved_conversation_attachment", { request: { sourceConversationId: conversationId, runId,
      ordinal: 4, messageRevisionId: attachment.messageRevisionId, sourceSha256: attachment.sourceSha256, destinationConversationId } });
  } else {
    expect(invoke).toHaveBeenCalledWith("conversation_input", { conversationId, runId });
    expect(invoke).toHaveBeenCalledWith("append_checkout_context_to_conversation", {
      checkoutId, conversationId: destinationConversationId, context: expect.stringContaining(method === "excerpt" ? "exact saved paragraph"
        : "Recorded runner path: D:/old-runner/release.txt\nRecorded URI: file:///D:/old-runner/release.txt"),
    });
  }
  expect(invoke).not.toHaveBeenCalledWith("add_checkout_files_to_conversation", expect.anything());
  expect(invoke).not.toHaveBeenCalledWith("start_conversation_turn", expect.anything());
});
