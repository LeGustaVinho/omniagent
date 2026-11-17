// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ConversationTemplateEditor } from "@/conversation-template-editor";
import { PipelineAuthoringEditor } from "@/workflow-authoring";
import { WorkflowComparisons } from "@/workflow-comparison";
import type { WorkflowDefinition, WorkflowGraph } from "@/workflow-graph-model";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@xyflow/react", async (load) => ({
  ...await load<typeof import("@xyflow/react")>(),
  ReactFlow: () => null, Background: () => null, Controls: () => null,
}));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const shared: ModelOption = {
  selection: { provider: "codex", profileId: "qualified", modelId: "shared" },
  name: "Shared Codex model", selectable: true, unavailableReason: null, catalogVersion: 1,
  rating: { intelligenceTenths: 915, priceScore: 72, apiReferenceCents: 125 },
};
const other: ModelOption = {
  ...shared,
  selection: { ...shared.selection, modelId: "other" }, name: "Other Codex model",
  rating: { intelligenceTenths: 763, priceScore: 47, apiReferenceCents: 250 },
};
const props = {
  checkout: exampleCheckout, simulation: null, startSimulation: vi.fn(), openWorkspace: vi.fn(),
  openActivity: vi.fn(), openChanges: vi.fn(),
};
const definitions: WorkflowDefinition[] = [
  { kind: "start", title: "Start", revision: 1, configuration: "none", executor: "bind_input", effect: "none",
    inputs: [], outputs: [{ name: "start", value: { kind: "control" } }] },
  { kind: "plan", title: "Plan", revision: 1, configuration: "agent_role", executor: "plan_agent", effect: "invoke_model",
    inputs: [{ name: "ready", value: { kind: "control" } }], outputs: [] },
];
const graph: WorkflowGraph = {
  name: "Pinned planner",
  nodes: [
    { id: "start", kind: "start", configuration: { kind: "none" } },
    { id: "plan", kind: "plan", configuration: { kind: "agent_role", role: "planner" } },
  ],
  wires: [{ from: { node: "start", port: "start" }, to: { node: "plan", port: "ready" } }],
};
const commit = "a".repeat(40);
const templateVersion = { revision: "template-v1", ordinal: 1, baseRevision: null };

function mount(component: React.ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{component}</QueryClientProvider>);
}

async function assertPickerMeters(user: ReturnType<typeof userEvent.setup>, role: "menuitemradio" | "menuitemcheckbox") {
  const model = await screen.findByRole(role, { name: shared.name });
  await user.hover(model);
  const meters = within(await screen.findByLabelText(`Ratings for ${shared.name}`));
  expect(meters.getByRole("meter", { name: "Intelligence" })).toHaveProperty("value", 91.5);
  expect(meters.getByRole("meter", { name: "Price score" })).toHaveProperty("value", 72);
  expect(meters.getByText("API reference $1.25 / 1M tokens")).toBeTruthy();
  return model;
}

it("persists the same shared model through template, stage, and ordered comparison pickers", async () => {
  const user = userEvent.setup();
  let template: { name: string; model: ModelSelection } | undefined;
  let stage = { id: "first-draft-revision", graph,
    definitions: Object.fromEntries(definitions.map((node) => [node.kind, node])) };
  let comparison: unknown[] = [];
  let inventory = [shared, other];
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    switch (command) {
      case "conversation_models":
        return (input as { provider?: string })?.provider === "codex" ? inventory.map((option) => ({ option, efforts: [] })) : [];
      case "list_conversation_templates":
        return template ? [{ templateId: "template", name: template.name, version: templateVersion }] : [];
      case "conversation_template_profile":
        return { cliVersion: "0.169.0", transport: "codex_app_server",
          toolScopes: [{ mode: "read_only", blocker: null }], capabilities: [] };
      case "save_conversation_template":
        template = (input as { template: NonNullable<typeof template> }).template;
        return { status: "saved", detail: { revision: { templateId: "template", template, version: templateVersion },
          history: [templateVersion] } };
      case "conversation_template_revision":
        return { revision: { templateId: "template", template, version: templateVersion }, history: [templateVersion] };
      case "workflow_node_definitions": return definitions;
      case "inspect_workflow_authoring": return { id: "draft", conversationId: "chat", base: null, head: stage,
        request: { jobId: "job", runId: null, state: "saved", diagnostic: null } };
      case "save_workflow_authoring_draft":
        stage = { ...stage, id: "persisted-stage-revision", graph: (input as { graph: WorkflowGraph }).graph };
        return { id: "draft", conversationId: "chat", base: null, head: stage,
          request: { jobId: "job", runId: null, state: "saved", diagnostic: null } };
      case "comparison_source_commit": return commit;
      case "model_comparisons": return comparison;
      case "start_model_comparison": {
        const { selections } = input as { selections: ModelSelection[] };
        const group = { id: "comparison", commit, tree: "tree", prompt: "Shared prompt",
          candidates: selections.map((selection, index) => {
            const option = inventory.find((item) => item.selection.modelId === selection.modelId)!;
            return { selection, jobId: `job-${index}`, state: "queued",
              modelReference: { name: option.name, rating: option.rating, catalogVersion: option.catalogVersion } };
          }) };
        comparison = [group];
        return group;
      }
      default: throw new Error(command);
    }
  });
  let view = mount(<ConversationTemplateEditor checkout={props.checkout} />);
  await user.click(await screen.findByRole("button", { name: "New conversation template" }));
  await user.type(screen.getByRole("textbox", { name: "Name" }), "Saved review role");
  await user.type(screen.getByRole("textbox", { name: "Instructions" }), "Review this exact input");
  await user.type(screen.getByRole("textbox", { name: "Completion contract" }), "Record findings");
  await user.click(await screen.findByRole("button", { name: /^Default CLI and model ·/ }));
  await user.click(await assertPickerMeters(user, "menuitemradio"));
  await user.click(await screen.findByRole("button", { name: "Save template revision" }));
  await screen.findByRole("heading", { name: "Edit conversation template" });
  expect(template?.model).toEqual(shared.selection);
  view.unmount();

  view = mount(<PipelineAuthoringEditor draftId="draft" {...props} />);
  await user.selectOptions(await screen.findByRole("combobox", { name: "Inspect node" }), "plan");
  await user.click(await screen.findByRole("button", { name: /^Stage model override ·/ }));
  await user.click(await assertPickerMeters(user, "menuitemradio"));
  await user.type(screen.getByRole("textbox", { name: "Agent role" }), " exact");
  await user.click(screen.getByRole("button", { name: "Save draft revision" }));
  await screen.findByText("persisted-stage-revision");
  expect(stage.graph.nodes[1]?.configuration).toEqual({ kind: "agent_role", role: "planner exact", model: shared.selection });
  view.unmount();
  view = mount(<PipelineAuthoringEditor draftId="draft" {...props} />);
  await user.selectOptions(await screen.findByRole("combobox", { name: "Inspect node" }), "plan");
  expect(await screen.findByRole("button", { name: `Stage model override · ${shared.name}` })).toBeTruthy();
  view.unmount();

  view = mount(<WorkflowComparisons {...props} />);
  await user.type(screen.getByRole("textbox", { name: "Shared request" }), "Shared prompt");
  await user.click(await screen.findByRole("button", { name: /^Comparison candidates ·/ }));
  await user.click(await screen.findByRole("menuitemcheckbox", { name: other.name }));
  await user.click(await assertPickerMeters(user, "menuitemcheckbox"));
  await user.keyboard("{Escape}");
  const candidates = within(screen.getByRole("list", { name: "Candidate order" })).getAllByRole("listitem");
  expect(candidates.map((candidate) => candidate.querySelector("strong")?.textContent)).toEqual([other.name, shared.name]);
  expect(within(candidates[1]!).getByText("API reference $1.25 / 1M tokens")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Start comparison" }));
  await screen.findByRole("list", { name: "Comparison results" });
  expect(invoke).toHaveBeenCalledWith("start_model_comparison", {
    projectId: props.checkout.projectId, checkoutId: props.checkout.checkoutId, commit,
    prompt: "Shared prompt", selections: [other.selection, shared.selection],
  });
  view.unmount();
  inventory = [];
  mount(<WorkflowComparisons {...props} />);
  const restored = within(await screen.findByRole("list", { name: "Comparison results" })).getAllByRole("listitem");
  expect(restored.map((candidate) => within(candidate).getByRole("heading").textContent)).toEqual([other.name, shared.name]);
  const selected = within(restored[1]!);
  expect(selected.getByRole("meter", { name: "Intelligence" })).toHaveProperty("value", 91.5);
  expect(selected.getByRole("meter", { name: "Price score" })).toHaveProperty("value", 72);
  expect(selected.getByText("API reference $1.25 / 1M tokens")).toBeTruthy();
});
