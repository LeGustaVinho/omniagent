// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationAuthoring, PipelineAuthoringEditor } from "@/workflow-authoring";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => { cleanup(); vi.resetAllMocks(); });

const selection = { provider: "codex", profileId: "local", modelId: "authoring-model" } as const;
const version = { id: "base", revision: "published-revision" };
const graph = { name: "Review pipeline", nodes: [], wires: [] };
const draft = {
  id: "draft-id", conversationId: "source-chat", base: version,
  head: { id: "selected-revision", runId: "run-1", graph },
  request: { jobId: "job-1", runId: "run-1", state: "saved", diagnostic: null },
};

it("submits an explicit authoring request for the selected protected base and opens its editor link", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "workflow_authoring_bases") return [{ version, name: "Published review" }];
    if (command === "conversation_workflow_authoring") return [];
    if (command === "start_workflow_authoring") return draft;
    throw new Error(`Unexpected ${command}`);
  });
  const open = vi.fn();
  const user = userEvent.setup();
  render(<QueryClientProvider client={new QueryClient()}><ConversationAuthoring projectId="project" conversationId="source-chat"
    selectedModel={selection} effort="low" onOpenDraft={open} /></QueryClientProvider>);
  await user.click(screen.getByRole("button", { name: "Create or edit with AI" }));
  await screen.findByRole("option", { name: /Published review/ });
  await user.selectOptions(screen.getByRole("combobox", { name: "Starting point" }), screen.getByRole("option", { name: /Published review/ }));
  await user.type(screen.getByRole("textbox", { name: "Authoring request" }), "Add verification steps");
  await user.click(screen.getByRole("button", { name: "Start authoring request" }));
  expect(invoke).toHaveBeenCalledWith("start_workflow_authoring", {
    conversationId: "source-chat", selection, effort: "low", brief: "Add verification steps", base: version,
  });
  await user.click(await screen.findByRole("button", { name: "Open draft" }));
  expect(open).toHaveBeenCalledWith("draft-id");
});

it("routes feedback with the loaded draft revision and links the source conversation", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "inspect_workflow_authoring") return draft;
    if (command === "conversation_models") return [{ option: { selection, name: "Authoring model", selectable: true, rating: null }, efforts: [] }];
    if (command === "request_workflow_authoring_changes") return { ...draft, request: { jobId: "job-2", runId: null, state: "queued", diagnostic: null } };
    throw new Error(`Unexpected ${command}`);
  });
  const open = vi.fn();
  const user = userEvent.setup();
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <PipelineAuthoringEditor draftId={draft.id} checkout={exampleCheckout} simulation={null} startSimulation={vi.fn()}
      openWorkspace={open} openActivity={vi.fn()} openChanges={vi.fn()} />
  </QueryClientProvider>);
  await user.click(await screen.findByRole("button", { name: "Open source conversation" }));
  expect(open).toHaveBeenCalledWith("conversations", "source-chat");
  await user.click(await screen.findByRole("button", { name: /Authoring model · Choose model/ }));
  await user.click(await screen.findByRole("menuitemradio", { name: /Authoring model/ }));
  await user.type(screen.getByRole("textbox", { name: "Feedback" }), "Keep the checkout pinned");
  const request = screen.getByRole("button", { name: "Request changes" });
  await waitFor(() => expect(request).toHaveProperty("disabled", false));
  await user.click(request);
  expect(invoke).toHaveBeenCalledWith("request_workflow_authoring_changes", {
    projectId: exampleCheckout.projectId, draftId: draft.id, selectedRevision: draft.head.id,
    selection, effort: null, feedback: "Keep the checkout pinned",
  });
});
