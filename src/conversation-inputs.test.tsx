// @vitest-environment jsdom
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { PendingInput } from "@/conversation-inputs";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve()) }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

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

test("validates a required multi-choice question and sends provider option IDs with false Boolean values", async () => {
  render(<PendingInput conversationId="chat" runId="run" onResolved={vi.fn()} input={{ data: {
    type: "question",
    request_id: "native-question",
    normalized: {
      message: "Configure the review.",
      actions: ["accept", "decline"],
      fields: [
        {
          id: "formats", label: "Formats", description: null, default: null, required: true, secret: false,
          input: { kind: "choice", multiple: true, allowCustom: true, options: [
            { id: "format-markdown", label: "Markdown" }, { id: "format-json", label: "JSON" },
          ] },
        },
        {
          id: "install", label: "Install", description: null, default: false, required: true, secret: false,
          input: { kind: "boolean" },
        },
      ],
    },
  } }} />);
  await userEvent.click(screen.getByRole("button", { name: "Reply" }));
  expect(screen.getByRole("alert").textContent).toContain("Choose a value for Formats");
  expect(invoke).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole("checkbox", { name: "Markdown" }));
  await userEvent.type(screen.getByRole("textbox", { name: "Other Formats" }), "XML");
  await userEvent.click(screen.getByRole("checkbox", { name: "JSON" }));
  await userEvent.click(screen.getByRole("button", { name: "Reply" }));
  expect(invoke).toHaveBeenCalledExactlyOnceWith("answer_conversation_question", {
    conversationId: "chat", runId: "run", requestId: "native-question",
    answer: { action: "accept", values: { formats: ["format-markdown", "XML", "format-json"], install: false } },
  });
});
