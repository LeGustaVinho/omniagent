// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { exampleCheckout } from "@/workbench-contract";
import { WorkflowPackageEditor } from "@/workflow-authoring-package";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@xyflow/react", async (original) => ({ ...await original<typeof import("@xyflow/react")>(),
  ReactFlow: () => null, Background: () => null, Controls: () => null,
}));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("reviews an exact prior package revision and simulates the chosen member without publishing", async () => {
  const projectId = exampleCheckout.projectId;
  const reference = { destination: "library", kind: "subflow", id: "checker", revision: "pinned-subflow" };
  const member = {
    key: "pipeline", name: "Delivery pipeline", kind: "pipeline", base: null,
    changes: [{ path: "/body/graph/nodes/check", change: "added", after: { kind: "command" }, before: null }],
    changesTruncated: false, dependencies: { check: { source: "published", version: reference } },
    affectedUses: [], diagnostics: [], requirements: ["Command check needs a local registration."],
    simulatable: true, sampleInputs: ["document"], sampleNodes: [{ nodeId: "check::command", title: "Command", outputs: ["result"] }],
  };
  const request = { projectId, draftId: "draft", revision: "older" };
  vi.mocked(invoke).mockImplementation(async (name, payload) => {
    switch (name) {
      case "inspect_workflow_package": return { id: "draft", revision: "head", conversationId: null, request: null, publication: null,
        package: { name: "Delivery package", members: [{ key: "pipeline", name: "Delivery pipeline", body: { kind: "pipeline" } }] } };
      case "conversation_models": return [];
      case "preview_workflow_package_revision": return { revision: (payload as { revision: string }).revision, revisions: ["head", "older"], members: [member] };
      case "simulate_workflow_package_revision": return { revision: "older", memberKey: "pipeline", inputs: { document: { relativePath: "sample.md" } },
        steps: [{ nodeId: "check::command", title: "Command", state: "waiting", inputs: [], outputs: {}, reason: "Waiting for sample approval." }],
        subflows: [{ path: "check", name: "Reusable checker", revision: reference.revision, nodes: ["check::command"],
          outputs: { result: { state: "waiting", from: { node: "check::command", port: "result" } } } }], outputs: {} };
      default: throw Error(`Unexpected ${name}`);
    }
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <WorkflowPackageEditor draftId="draft" checkout={exampleCheckout} simulation={null}
      startSimulation={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} openWorkspace={vi.fn()} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Review changes and simulate" }));
  await user.selectOptions(await screen.findByRole("combobox", { name: "Review saved revision" }), await screen.findByRole("option", { name: "older" }));
  expect(await screen.findByText(member.requirements[0]!)).toBeTruthy();
  await user.click(screen.getByText("Graph changes (1)"));
  expect(screen.getByText("/body/graph/nodes/check")).toBeTruthy();
  const outcomes = { "check::command": { state: "waiting", reason: "Sample operator response" } };
  fireEvent.change(screen.getByRole("textbox", { name: "Sample outcomes" }), { target: { value: JSON.stringify(outcomes) } });
  await user.click(screen.getByRole("button", { name: "Simulate" }));
  expect(invoke).toHaveBeenCalledWith("preview_workflow_package_revision", request);
  expect(invoke).toHaveBeenCalledWith("simulate_workflow_package_revision", { ...request, memberKey: "pipeline",
    scenario: { inputs: { document: { relativePath: "sample.md" } }, outcomes } });
  const trace = within(await screen.findByRole("region", { name: "Simulation trace" }));
  await user.click(trace.getByText(/Reusable checker/));
  expect(trace.getByText(reference.revision)).toBeTruthy();
  expect(trace.getByText("Waiting for sample approval.")).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.some(([name]) => name === "save_workflow_package" || name === "publish_reviewed_workflow_package")).toBe(false);
});

it("edits a package member graph through the shared editor and saves the same package", async () => {
  const graph = { name: "Delivery", nodes: [{ id: "start", kind: "start", configuration: { kind: "none" } }], wires: [] };
  const member = { key: "workflow", name: "Delivery", body: { kind: "pipeline", graph },
    references: { tool: { source: "parameter", name: "Check command", kind: "command" } } };
  const draft = { id: "draft", revision: "saved", conversationId: null, request: null, publication: null,
    package: { name: "Authoring package", members: [member] } };
  const start = { title: "Start", kind: "start", revision: 1, configuration: "none", executor: "bind_input", effect: "none",
    inputs: [], outputs: [{ name: "start", value: { kind: "control" } }] };
  const parallel = { title: "Parallel", kind: "parallel", revision: 1, configuration: "none", executor: "fan_out", effect: "none",
    inputs: [{ name: "ready", value: { kind: "control" } }], outputs: [{ name: "branches", value: { kind: "control" } }] };
  vi.mocked(invoke).mockImplementation(async (name, payload) => {
    switch (name) {
      case "inspect_workflow_package": return draft;
      case "conversation_models": return [];
      case "preview_workflow_package_revision": return { revision: draft.revision, revisions: [draft.revision],
        members: [{ key: member.key, graphDefinitions: { start } }] };
      case "workflow_graph_registry": return [start, parallel];
      case "save_workflow_package": return { ...draft, revision: "edited", package: (payload as { package: typeof draft.package }).package };
      default: throw Error(`Unexpected ${name}`);
    }
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <WorkflowPackageEditor draftId="draft" checkout={exampleCheckout} simulation={null}
      startSimulation={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} openWorkspace={vi.fn()} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByText("Edit package"));
  await user.click(screen.getByText("Edit member graph"));
  await user.click(await screen.findByRole("button", { name: "Add node" }));
  await user.click(within(screen.getByRole("region", { name: "Node catalog" })).getByRole("button", { name: "Parallel" }));
  await user.click(screen.getByRole("button", { name: "Save package revision" }));
  expect(invoke).toHaveBeenCalledWith("save_workflow_package", {
    projectId: exampleCheckout.projectId, draftId: draft.id, expectedRevision: draft.revision,
    package: { name: draft.package.name, members: [{ ...member, body: { ...member.body, graph: {
      ...graph, nodes: [...graph.nodes, { id: "parallel", kind: "parallel", configuration: { kind: "none" } }],
    } } }] },
  });
});
