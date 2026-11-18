// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { BuilderAuthoring, PipelineAuthoringEditor } from "@/workflow-authoring";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

beforeEach(() => vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} }));
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.resetAllMocks(); });

const selection = { provider: "codex", profileId: "local", modelId: "authoring-model" } as const;
const version = { id: "base", revision: "published-revision" };
const checkout = exampleCheckout;
const pipelineBase = { ...version, kind: "pipeline", destination: "project" };
const catalog = [
  { version: pipelineBase, name: "Published review" },
  { version: { id: "prefab", revision: "prefab-revision", kind: "subflow", destination: "library" }, name: "Saved prefab", nodePrefab: true },
  { version: { id: "function", revision: "function-revision", kind: "subflow", destination: "library" }, name: "Saved function" },
  { version: { id: "role", revision: "role-revision", kind: "role", destination: "project" }, name: "Saved role" },
  { version: { id: "schema", revision: "schema-revision", kind: "schema", destination: "library" }, name: "Saved schema" },
  { version: { id: "prompt", revision: "prompt-revision", kind: "subflow", destination: "project" }, name: "Legacy Prompt", prompt: true },
];
const model = { option: { selection, name: "Authoring model", selectable: true, rating: null, unavailableReason: null }, efforts: ["low"] };
const graph = { name: "Review pipeline", nodes: [], wires: [] };
const draft = {
  id: "draft-id", conversationId: "source-chat", base: version,
  head: { id: "selected-revision", runId: "run-1", graph },
  request: { jobId: "job-1", runId: "run-1", state: "saved", diagnostic: null },
};

async function chooseModel(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("button", { name: "Authoring model · Choose model" }));
  await user.click(await screen.findByRole("menuitemradio", { name: "Authoring model" }));
  await user.selectOptions(screen.getByRole("combobox", { name: "Authoring effort" }), "low");
}

it("starts an authoring request directly in the worktree and opens its visual pipeline draft", async () => {
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    if (command === "workflow_package_catalog") return catalog;
    if (command === "conversation_models") return (input as { provider: string }).provider === "codex" ? [model] : [];
    if (command === "start_workflow_editor_authoring") return { id: "pipeline-draft", conversationId: null };
    throw new Error(`Unexpected ${command}`);
  });
  const open = vi.fn();
  const user = userEvent.setup();
  render(<QueryClientProvider client={new QueryClient()}><BuilderAuthoring checkout={checkout} openWorkspace={open} /></QueryClientProvider>);
  await user.click(screen.getByRole("button", { name: "Create or edit with AI" }));
  expect(screen.getByRole("combobox", { name: "Output" })).toHaveProperty("value", "pipeline");
  expect(screen.queryByRole("combobox", { name: "Source conversation" })).toBeNull();
  await chooseModel(user);
  await screen.findByRole("option", { name: /Published review/ });
  await user.selectOptions(screen.getByRole("combobox", { name: "Starting point" }), screen.getByRole("option", { name: /Published review/ }));
  await user.type(screen.getByRole("textbox", { name: "Authoring request" }), "Add verification steps");
  const start = screen.getByRole("button", { name: "Start authoring request" });
  expect(start).toHaveProperty("disabled", false);
  await user.click(start);
  expect(invoke).toHaveBeenCalledWith("start_workflow_editor_authoring", {
    projectId: checkout.projectId, checkoutId: checkout.checkoutId, output: "pipeline", selection, effort: "low",
    brief: "Add verification steps", base: pipelineBase,
  });
  expect(invoke).not.toHaveBeenCalledWith("list_conversations", expect.anything());
  await waitFor(() => expect(open).toHaveBeenCalledWith("pipelines", "authoring:package:pipeline-draft"));
});

it.each([
  ["node_prefab", 1],
  ["function", 2],
  ["role", 3],
  ["schema", 4],
  ["package", 4],
] as const)("can select %s authoring and retain the exact artifact base", async (output, index) => {
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    if (command === "workflow_package_catalog") return catalog;
    if (command === "conversation_models") return (input as { provider: string }).provider === "codex" ? [model] : [];
    if (command === "start_workflow_editor_authoring") return { id: "package-draft" };
    throw new Error(command);
  });
  const open = vi.fn();
  const user = userEvent.setup();
  render(<QueryClientProvider client={new QueryClient()}><BuilderAuthoring checkout={checkout} openWorkspace={open} /></QueryClientProvider>);
  await user.click(screen.getByRole("button", { name: "Create or edit with AI" }));
  await user.selectOptions(screen.getByRole("combobox", { name: "Output" }), output);
  expect(screen.queryByRole("option", { name: /Legacy Prompt/ })).toBeNull();
  if (output !== "package") expect(screen.queryByRole("option", { name: /Published review/ })).toBeNull();
  await chooseModel(user);
  const item = catalog[index]!;
  await user.selectOptions(screen.getByRole("combobox", { name: "Starting point" }),
    await screen.findByRole("option", { name: new RegExp(item.name) }));
  await user.type(screen.getByRole("textbox", { name: "Authoring request" }), "Propose a related role and preset.");
  await user.click(screen.getByRole("button", { name: "Start authoring request" }));
  expect(invoke).toHaveBeenCalledWith("start_workflow_editor_authoring", {
    projectId: checkout.projectId, checkoutId: checkout.checkoutId, output, selection, effort: "low",
    brief: "Propose a related role and preset.", base: item.version,
  });
  await waitFor(() => expect(open).toHaveBeenCalledWith("pipelines", "authoring:package:package-draft"));
});

it("retains fresh package authoring when no project conversation exists", async () => {
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    if (command === "workflow_package_catalog") return [];
    if (command === "conversation_models") return (input as { provider: string }).provider === "codex" ? [model] : [];
    if (command === "start_workflow_editor_authoring") return { id: "fresh-package" };
    throw new Error(command);
  });
  const open = vi.fn();
  const user = userEvent.setup();
  render(<QueryClientProvider client={new QueryClient()}><BuilderAuthoring checkout={checkout} openWorkspace={open} /></QueryClientProvider>);
  await user.click(screen.getByRole("button", { name: "Create or edit with AI" }));
  await user.selectOptions(screen.getByRole("combobox", { name: "Output" }), "package");
  await chooseModel(user);
  await user.type(screen.getByRole("textbox", { name: "Authoring request" }), "Define the review process");
  await user.click(screen.getByRole("button", { name: "Start authoring request" }));
  expect(invoke).toHaveBeenCalledWith("start_workflow_editor_authoring", {
    projectId: checkout.projectId, checkoutId: checkout.checkoutId, output: "package", selection, effort: "low", brief: "Define the review process", base: null,
  });
  await waitFor(() => expect(open).toHaveBeenCalledWith("pipelines", "authoring:package:fresh-package"));
});

it("routes feedback with the loaded draft revision and links the source conversation", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "inspect_workflow_authoring") return draft;
    if (command === "workflow_node_definitions") return [];
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
