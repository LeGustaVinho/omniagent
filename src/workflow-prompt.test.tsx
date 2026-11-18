// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { WorkflowPackageEditor } from "@/workflow-authoring-package";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@xyflow/react", async (original) => ({
  ...await original<typeof import("@xyflow/react")>(), ReactFlow: () => null, Background: () => null, Controls: () => null,
}));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const start = {
  title: "Start", kind: "start", revision: 1, configuration: "none", executor: "bind_input", effect: "none",
  inputs: [], outputs: [{ name: "start", value: { kind: "control" } }],
};
const subflow = {
  title: "Subflow", kind: "subflow", revision: 1, configuration: "none", executor: "expand_subflow", effect: "none",
  inputs: [], outputs: [],
};
const role = { key: "reviewer", name: "Reviewer", body: { kind: "role" } };
const pipeline = { key: "pipeline", name: "Delivery", body: { kind: "pipeline", graph: {
  name: "Delivery", nodes: [{ id: "start", kind: "start", configuration: { kind: "none" } }], wires: [],
} } };
const draft = { id: "draft", revision: "current", conversationId: null, request: null, publication: null,
  package: { name: "Delivery package", members: [pipeline, role] } };
const scope = { projectId: exampleCheckout.projectId, draftId: draft.id };
const model = { modelId: "fixture", profileId: "desktop", provider: "codex" };
const proposal = { inputs: {}, outputs: { summary: { kind: "rendered_text" } }, routes: ["review", "deliver"] };

const savedPrompt = {
  name: "Saved decision Prompt", prompt: true, blockers: [],
  version: { kind: "subflow", destination: "project", id: "prompt-id", revision: "prompt-v1" },
  body: { kind: "prompt", prompt: { ...proposal, text: "Evaluate carefully.", executor: "agent", delegates: [] } },
  references: { role: { source: "published", version: { kind: "role", destination: "project", id: "role-id", revision: "role-v1" } } },
};

function mountPromptEditor(opened: typeof draft) {
  vi.mocked(invoke).mockImplementation(async (name, payload) => {
    switch (name) {
      case "inspect_workflow_package": return opened;
      case "workflow_node_definitions": return [start, subflow];
      case "workflow_package_catalog": return [savedPrompt];
      case "describe_workflow_reusable_node": return {
        artifact: savedPrompt, definition: subflow, graphDefinitions: {}, editableOverrides: [],
      };
      case "conversation_models": return (payload as { provider: string }).provider === "codex"
        ? [{ option: { name: "Fixture model", selectable: true, selection: model, rating: null } }] : [];
      case "preview_workflow_package_revision": return { revision: draft.revision, revisions: [draft.revision], members: [
        { key: pipeline.key, name: pipeline.name, kind: "pipeline", changes: [], diagnostics: [], requirements: [],
          dependencies: {}, affectedUses: [], graphDefinitions: { start } },
      ] };
      case "request_prompt_contract_suggestion": return "fixture-job";
      case "prompt_contract_suggestion": return { state: "ready", proposal };
      case "save_workflow_package": return draft;
      default: throw Error(`Unexpected ${name}`);
    }
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <WorkflowPackageEditor draftId={draft.id} checkout={exampleCheckout}
      openActivity={vi.fn()} openChanges={vi.fn()} openWorkspace={vi.fn()} />
  </QueryClientProvider>);
  return userEvent.setup();
}

it("edits a saved Prompt and applies proposed ports only to the matching card snapshot", async () => {
  const member = { key: "prompt-card", name: "Prompt", destination: "project",
    body: { kind: "prompt", prompt: { text: "", inputs: {}, outputs: {}, routes: [], executor: "agent", delegates: [] } },
    references: { role: { source: "draft", key: role.key } } };
  const linked = { ...pipeline, body: { ...pipeline.body, graph: { ...pipeline.body.graph,
    nodes: [...pipeline.body.graph.nodes, { id: "prompt", kind: "subflow", configuration: { kind: "none" } }] } },
    references: { prompt: { source: "draft", key: member.key } } };
  const user = mountPromptEditor({ ...draft, package: { ...draft.package, members: [linked, role, member] } });
  await user.selectOptions(await screen.findByRole("combobox", { name: "Inspect node" }), "prompt");
  const prompt = screen.getByRole("textbox", { name: "Prompt" });
  await user.type(prompt, "Review candidate");
  await user.selectOptions(screen.getByRole("combobox", { name: "Role revision" }), JSON.stringify(["draft", role.key]));
  await user.click(screen.getByText("Inputs and outputs", { exact: false, selector: "summary" }));
  await user.click(screen.getByRole("button", { name: "Add input" }));
  const inputName = screen.getByRole("textbox", { name: "Input 1 name" });
  await user.clear(inputName);
  await user.type(inputName, "candidate{Enter}");
  expect(screen.getByRole("combobox", { name: "Type for candidate" })).toBeTruthy();

  async function requestSuggestion() {
    const dialog = within(screen.getByRole("dialog", { name: "Suggest contract with AI" }));
    await user.click(dialog.getByRole("button", { name: "Request suggestions" }));
    return dialog.findByRole<HTMLButtonElement>("button", { name: "Apply proposed contract" });
  }

  await user.click(screen.getByRole("button", { name: "Suggest with AI" }));
  await user.click(await screen.findByRole("button", { name: "Suggestion model · Choose model" }));
  await user.click(await screen.findByRole("menuitemradio", { name: "Fixture model" }));
  await requestSuggestion();
  expect(invoke).toHaveBeenLastCalledWith("prompt_contract_suggestion", { projectId: scope.projectId, jobId: "fixture-job" });
  async function expectStaleProposal() {
    await user.click(screen.getByRole("button", { name: "Suggest with AI" }));
    expect((await screen.findByRole<HTMLButtonElement>("button", { name: "Apply proposed contract" })).disabled).toBe(true);
    expect(screen.getByText(/This card changed after the request/)).toBeTruthy();
  }
  const closeProposal = () => user.click(screen.getByRole("button", { name: "Close suggest contract with ai" }));
  await closeProposal();
  fireEvent.change(screen.getByRole("textbox", { name: "Card title" }), { target: { value: "Candidate decision" } });
  await expectStaleProposal();
  await requestSuggestion();

  await closeProposal();
  const roleField = screen.getByRole("combobox", { name: "Role revision" });
  await user.selectOptions(roleField, "");
  await expectStaleProposal();
  await requestSuggestion();

  await closeProposal();
  await user.selectOptions(roleField, JSON.stringify(["draft", role.key]));
  fireEvent.change(prompt, { target: { value: "Review the updated candidate" } });
  await expectStaleProposal();
  await user.click(await requestSuggestion());

  await user.click(screen.getByRole("button", { name: "Remove node" }));
  expect(screen.queryByRole("textbox", { name: "Prompt" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Undo graph edit" }));
  await user.selectOptions(screen.getByRole("combobox", { name: "Inspect node" }), "prompt");
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Prompt" }).value).toBe("Review the updated candidate");

  await user.click(screen.getByRole("button", { name: "Save draft revision" }));
  expect(invoke).toHaveBeenCalledWith("save_workflow_package", expect.objectContaining({
    ...scope, expectedRevision: draft.revision, package: { name: draft.package.name, members: [
      { ...pipeline, references: { prompt: { source: "draft", key: "prompt-card" } }, body: { ...pipeline.body, graph: {
        ...pipeline.body.graph, nodes: [...pipeline.body.graph.nodes, { id: "prompt", kind: "subflow", configuration: { kind: "none" } }],
      } } },
      role,
      { key: "prompt-card", name: "Candidate decision", destination: "project", references: { role: { source: "draft", key: role.key } },
        body: { kind: "prompt", prompt: { ...proposal, text: "Review the updated candidate", executor: "agent", delegates: [] } } },
    ] },
  }));
});

it("shows a pinned published Prompt's read-only contract", async () => {
  const linked = { ...pipeline, body: { ...pipeline.body, graph: { ...pipeline.body.graph,
    nodes: [...pipeline.body.graph.nodes, { id: "reusable-subflow", kind: "subflow", configuration: { kind: "none" } }] } },
    references: { "reusable-subflow": { source: "published", version: savedPrompt.version } } };
  const user = mountPromptEditor({ ...draft, package: { ...draft.package, members: [linked, role] } });
  await user.selectOptions(await screen.findByRole("combobox", { name: "Inspect node" }), "reusable-subflow");
  const prompt = await screen.findByRole<HTMLTextAreaElement>("textbox", { name: "Prompt" });
  expect(prompt.value).toBe("Evaluate carefully.");
  expect(prompt.disabled).toBe(true);
});
