// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
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
    delivery: "submitted", turnId: "turn-id", frames: ["original wire"], attachments: [],
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

it("shows the frozen excerpt and original runner path and requests native bytes from the selected dispatch", async () => {
  const client = new QueryClient();
  const metadata = {
    name: "notes.txt", mimeType: "text/plain", source: { kind: "checkout" as const, relativePath: "docs/notes.txt" },
    sourceSha256: "f".repeat(64), sourceSize: 128, messageRevisionId: "revision-before-edit",
  };
  vi.mocked(invoke).mockResolvedValue(null);
  const user = userEvent.setup();
  render(<QueryClientProvider client={client}>
    <ConversationDeliveries input={{
      delivery: "submitted", turnId: "turn-id", frames: [], instructions: [], attachments: [
        { ...metadata, transport: { method: "excerpt", start: 11, end: 20, text: "café ☕" } },
        { ...metadata, transport: { method: "reference", runnerPath: "C:\\checkout\\before-rename.txt", uri: null } },
        { ...metadata, transport: { method: "native", encoding: "acp_resource", runnerPath: null, resourceUri: "file:///original" } },
      ],
    }} conversationId="chat-id" runId="run-id" live={false} />
  </QueryClientProvider>);
  await user.click(screen.getByText("Saved attachments", { selector: "summary" }));
  const list = within(screen.getByRole("list", { name: "Saved attachments" }));
  expect(list.getByText("UTF-8 bytes 11–20")).toBeTruthy();
  expect(list.getByText("café ☕")).toBeTruthy();
  expect(list.getByText("Runner path C:\\checkout\\before-rename.txt")).toBeTruthy();
  await user.click(list.getByRole("button", { name: "Download saved bytes" }));
  expect(invoke).toHaveBeenCalledWith("conversation_attachment_content", { conversationId: "chat-id", runId: "run-id", index: 2 });
});
