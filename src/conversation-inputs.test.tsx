// @vitest-environment jsdom
import { invoke } from "@tauri-apps/api/core";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, test, vi } from "vitest";
import { PendingInput } from "@/conversation-inputs";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve()) }));

test("routes a permission decision to the original conversation and run", async () => {
  const resolved = vi.fn();
  render(<PendingInput conversationId="chat" runId="run" onResolved={resolved}
    input={{ data: { type: "approval", request_id: 7, operation: "read_file", description: "Read one document" } }} />);
  await userEvent.click(screen.getByRole("button", { name: "Deny" }));
  expect(invoke).toHaveBeenCalledWith("approve_conversation_input", {
    conversationId: "chat", runId: "run", requestId: 7, decision: "deny",
  });
  await waitFor(() => expect(resolved).toHaveBeenCalledOnce());
});
