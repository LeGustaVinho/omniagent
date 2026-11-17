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

const sampleNode = {
  nodeId: "prompt::execute", title: "Agent", outputs: ["response"],
  prompt: { title: "Evaluate candidate", routes: ["review", "deliver"], outputs: { summary: "rendered_text" },
    delegates: [{ alias: "tests", required: false }, { alias: "correctness", required: true }] },
};

function mountPromptEditor(sampleNodes = [sampleNode]) {
  vi.mocked(invoke).mockImplementation(async (name, payload) => {
    switch (name) {
      case "inspect_workflow_package": return draft;
      case "workflow_graph_registry": return [start, subflow];
      case "workflow_package_catalog": return [savedPrompt];
      case "describe_workflow_reusable_node": return {
        artifact: savedPrompt, definition: subflow, graphDefinitions: {}, editableOverrides: [],
      };
      case "conversation_models": return (payload as { provider: string }).provider === "codex"
        ? [{ option: { name: "Fixture model", selectable: true, selection: model, rating: null } }] : [];
      case "preview_workflow_package_revision": return { revision: draft.revision, revisions: [draft.revision], members: [
        { key: pipeline.key, name: pipeline.name, kind: "pipeline", changes: [], diagnostics: [], requirements: [],
          dependencies: {}, affectedUses: [], simulatable: true, sampleInputs: [], sampleNodes, graphDefinitions: { start } },
      ] };
      case "request_prompt_contract_suggestion": return "fixture-job";
      case "prompt_contract_suggestion": return { state: "ready", proposal };
      case "save_workflow_package": return draft;
      case "simulate_workflow_package_revision": return { revision: draft.revision, memberKey: pipeline.key, inputs: {}, steps: [], subflows: [], outputs: {} };
      default: throw Error(`Unexpected ${name}`);
    }
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <WorkflowPackageEditor draftId={draft.id} checkout={exampleCheckout} simulation={null}
      startSimulation={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} openWorkspace={vi.fn()} />
  </QueryClientProvider>);
  return userEvent.setup();
}

it("creates a versioned Prompt member and applies proposed ports only to the matching card snapshot", async () => {
  const user = mountPromptEditor([]);
  await user.click(await screen.findByRole("button", { name: "Add node" }));
  await user.click(screen.getByRole("button", { name: "Add Prompt" }));
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

  await user.click(screen.getByRole("button", { name: "Save package revision" }));
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

it("simulates alternate routes and optional invited Roles by the matching native sample ID", async () => {
  const user = mountPromptEditor();
  await user.click(await screen.findByRole("button", { name: "Review changes and simulate" }));
  await user.click(await screen.findByText("Prompt responses"));
  const route = screen.getByRole("combobox", { name: "Sample route for prompt::execute" });
  await user.selectOptions(route, "review");
  await user.click(screen.getByRole("checkbox", { name: "Call tests" }));
  await user.type(screen.getByRole("textbox", { name: "summary" }), "Sample evidence");
  await user.click(screen.getByRole("button", { name: "Simulate" }));
  await screen.findByRole("region", { name: "Simulation trace" });
  const sample = (route: string) => ({ ...scope, revision: draft.revision, memberKey: pipeline.key, scenario: { inputs: {},
    outcomes: { "prompt::execute": { state: "completed", outputs: { response: {
      route, reason: "Sample route selection", outputs: { summary: "Sample evidence" },
      delegations: [{ role: "tests", focus: "Review the saved subject." }],
    } } } },
  } });
  expect(invoke).toHaveBeenCalledWith("simulate_workflow_package_revision", sample("review"));
  await user.selectOptions(route, "deliver");
  await user.click(screen.getByRole("button", { name: "Simulate" }));
  expect(invoke).toHaveBeenCalledWith("simulate_workflow_package_revision", sample("deliver"));
});

it("adds an exact published Prompt revision and shows its read-only contract", async () => {
  const user = mountPromptEditor([]);
  await user.click(await screen.findByRole("button", { name: "Add a reusable node" }));
  await user.selectOptions(await screen.findByRole("combobox", { name: "Published reusable node" }),
    JSON.stringify(savedPrompt.version));
  await user.click(screen.getByRole("button", { name: "Add pinned node" }));
  await user.selectOptions(screen.getByRole("combobox", { name: "Inspect node" }), "reusable-subflow");
  const prompt = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Prompt" });
  expect(prompt.value).toBe("Evaluate carefully.");
  expect(prompt.disabled).toBe(true);
  await user.click(screen.getByRole("button", { name: "Save package revision" }));
  expect(invoke).toHaveBeenCalledWith("save_workflow_package", expect.objectContaining({
    package: expect.objectContaining({ members: [
      expect.objectContaining({ ...pipeline, body: expect.objectContaining({ graph: expect.objectContaining({
        nodes: [...pipeline.body.graph.nodes, { id: "reusable-subflow", kind: "subflow", configuration: { kind: "none" } }],
      }) }),
      references: { "reusable-subflow": { source: "published", version: savedPrompt.version } } }),
      role,
    ] }),
  }));
});
