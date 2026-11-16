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

test("sends selected IDs, an explicit false toggle and a custom choice with their native types", async () => {
  render(<PendingInput conversationId="chat" runId="run" onResolved={vi.fn()} input={{ data: {
    type: "question",
    request_id: "native-question",
    normalized: {
      message: "Configure the review.",
      actions: ["accept", "decline", "cancel"],
      fields: [
        {
          id: "formats", label: "Formats", description: null, default: null, required: true, secret: false,
          input: { kind: "choice", multiple: true, allowCustom: false, minSelections: 2, maxSelections: 2, options: [
            { id: "format-markdown", label: "Markdown" }, { id: "format-json", label: "JSON" },
          ] },
        },
        {
          id: "install", label: "Install", description: null, default: true, required: true, secret: false,
          input: { kind: "boolean" },
        },
        {
          id: "plan", label: "Plan size", description: null, default: null, required: true, secret: false,
          input: { kind: "choice", multiple: false, allowCustom: false, options: [
            { id: "01", label: "Small" }, { id: "02", label: "Expanded" },
          ] },
        },
        {
          id: "destination", label: "Destination", description: null, default: null, required: true, secret: false,
          input: { kind: "choice", multiple: true, allowCustom: true, options: [{ id: "usual", label: "Usual workspace" }] },
        },
      ],
    },
  } }} />);
  await userEvent.click(screen.getByRole("button", { name: "Reply" }));
  expect(screen.getByRole("alert").textContent).toContain("Choose a value for Formats");
  expect(invoke).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole("checkbox", { name: "Markdown" }));
  await userEvent.click(screen.getByRole("button", { name: "Reply" }));
  expect(screen.getByRole("alert").textContent).toContain("Choose between 2 and 2 options for Formats");
  await userEvent.click(screen.getByRole("checkbox", { name: "JSON" }));
  await userEvent.click(screen.getByRole("radio", { name: "Expanded" }));
  await userEvent.click(screen.getByRole("switch", { name: "Install" }));
  await userEvent.click(screen.getByRole("checkbox", { name: "Usual workspace" }));
  await userEvent.type(screen.getByRole("textbox", { name: "Other Destination" }), "Local workspace");
  expect(screen.getByRole<HTMLInputElement>("checkbox", { name: "Usual workspace" }).checked).toBe(false);
  await userEvent.dblClick(screen.getByRole("button", { name: "Reply" }));
  expect(invoke).toHaveBeenCalledExactlyOnceWith("answer_conversation_question", {
    conversationId: "chat", runId: "run", requestId: "native-question",
    answer: { action: "accept", values: { formats: ["format-markdown", "format-json"], install: false, plan: "02", destination: "Local workspace" } },
  });
});

test.each([
  { label: "Decline", action: "decline", savedReason: undefined, command: "answer_conversation_question" },
  { label: "Cancel question", action: "cancel", savedReason: "The live reply deadline expired.", command: "answer_saved_conversation_question" },
])("routes $action once with an empty field payload", async ({ label, action, savedReason, command }) => {
  render(<PendingInput conversationId="chat" runId="run" onResolved={vi.fn()} savedReason={savedReason} input={{ data: {
    type: "question", request_id: 8,
    normalized: {
      message: "Provide context", actions: ["accept", "decline", "cancel"], fields: [{
        id: "note", label: "Note", description: null, default: "draft text", required: true, secret: false,
        input: { kind: "text", minLength: null, maxLength: null },
      }],
    },
  } }} />);
  await userEvent.dblClick(screen.getByRole("button", { name: label }));
  expect(invoke).toHaveBeenCalledExactlyOnceWith(command, {
    conversationId: "chat", runId: "run", requestId: 8, answer: { action, values: {} },
  });
});
