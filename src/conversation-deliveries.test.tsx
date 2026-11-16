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
    delivery: "submitted", turnId: "turn-id", frames: ["original wire"],
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
