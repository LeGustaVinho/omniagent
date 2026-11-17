// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { exampleCheckout } from "@/workbench-contract";
import { WorkflowPackageEditor } from "@/workflow-authoring-package";
import { WorkflowReusableDerivation } from "@/workflow-reusable";
import type { WorkflowDefinition, WorkflowGraph, WorkflowWire } from "@/workflow-graph-model";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@xyflow/react", async (original) => ({
  ...await original<typeof import("@xyflow/react")>(), ReactFlow: () => null, Background: () => null, Controls: () => null,
}));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const projectId = exampleCheckout.projectId;
const control = { kind: "control" } as const;
const checks = { kind: "data", artifact: "checks" } as const;
const start: WorkflowDefinition = {
  kind: "start", revision: 1, title: "Start", configuration: "none", executor: "bind_input", effect: "none",
  inputs: [], outputs: [{ name: "start", value: control }],
};
const verify: WorkflowDefinition = {
  kind: "verify_candidate", revision: 1, title: "Verify candidate", configuration: "required_checks", executor: "verify",
  effect: "read_checkout", inputs: [{ name: "ready", value: control }], outputs: [{ name: "checks", value: checks }],
};
const edge: WorkflowWire = { from: { node: "start", port: "start" }, to: { node: "verify", port: "ready" } };
const graph: WorkflowGraph = {
  name: "Pipeline",
  nodes: [{ id: "start", kind: "start", configuration: { kind: "none" } },
    { id: "verify", kind: "verify_candidate", configuration: { kind: "required_checks", names: ["Build"] } }],
  wires: [edge],
};
const draft = {
  id: "draft", revision: "saved", conversationId: null, request: null, publication: null,
  package: { name: "Package", members: [{ key: "pipeline", name: "Pipeline", body: { kind: "pipeline", graph } }] },
};

function mount(child: ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}>{child}</QueryClientProvider>);
}

it("derives a preset and a typed subflow selection from the immutable package revision", async () => {
  const source = { type: "package_revision" as const, draftId: draft.id, revision: draft.revision, memberKey: "pipeline" };
  const result = { ...draft, revision: "new-revision" };
  const output = { node: "verify", port: "checks" };
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "derive_workflow_reusable") return result;
    if (command === "describe_workflow_reusable_boundary") return {
      inputs: [{ wire: edge, value: control }], outputs: [{ endpoint: output, value: checks, consumedOutsideSelection: false }],
    };
    throw Error(command);
  });
  const saved = vi.fn();
  mount(<WorkflowReusableDerivation projectId={projectId} graph={graph} source={source} disabled={false} onSaved={saved} />);
  const user = userEvent.setup();
  await user.click(screen.getByText("Save a reusable node"));
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Reusable verification" } });
  await user.selectOptions(screen.getByLabelText("Configured node"), "verify");
  await user.click(screen.getByRole("checkbox", { name: "names" }));
  await user.click(screen.getByRole("button", { name: "Save as preset" }));
  expect(invoke).toHaveBeenCalledWith("derive_workflow_reusable", { projectId, source, request: {
    name: "Reusable verification", key: "reusable-verification", destination: "project",
    selection: { type: "preset", nodeId: "verify", editableFields: ["names"] },
  } });
  await waitFor(() => expect(saved).toHaveBeenCalledWith(result));
  await user.selectOptions(screen.getByLabelText("Reusable type"), "subflow");
  await user.selectOptions(screen.getByLabelText("Subflow source"), "subset");
  await user.click(screen.getByRole("checkbox", { name: "verify" }));
  await user.selectOptions(screen.getByLabelText("Destination"), "library");
  fireEvent.change(await screen.findByLabelText("Public input name for start.start"), { target: { value: "ready" } });
  fireEvent.change(screen.getByLabelText("Public output name for verify.checks"), { target: { value: "verified" } });
  await user.click(screen.getByRole("button", { name: "Save as subflow" }));
  expect(invoke).toHaveBeenCalledWith("describe_workflow_reusable_boundary", { projectId, source, nodeIds: ["verify"] });
  expect(invoke).toHaveBeenCalledWith("derive_workflow_reusable", { projectId, source, request: {
    name: "Reusable verification", key: "reusable-verification", destination: "library",
    selection: { type: "subflow", nodeIds: ["verify"], inputs: { ready: [edge] }, outputs: { verified: output } },
  } });
});

it("adds pinned reusable nodes, saves an editable override, and opens a child with the same graph inspector", async () => {
  const presetVersion = { kind: "preset", id: "preset", revision: "preset-r1", destination: "library" };
  const subflowVersion = { kind: "subflow", id: "subflow", revision: "subflow-r1", destination: "library" };
  const preset = {
    artifact: { version: presetVersion, name: "Test preset", blockers: [], references: {},
      body: { kind: "preset", configuration: graph.nodes[1]?.configuration, editableFields: ["names"] } },
    definition: { ...verify, title: "Test preset" }, graphDefinitions: {}, editableOverrides: ["names"],
  };
  const subflow = {
    artifact: { version: subflowVersion, name: "Review subflow", blockers: [], references: {}, body: { kind: "subflow", graph } },
    definition: { ...verify, kind: "subflow", configuration: "none", title: "Review subflow" },
    graphDefinitions: { start, verify }, editableOverrides: [],
  };
  vi.mocked(invoke).mockImplementation(async (name, payload) => {
    switch (name) {
      case "inspect_workflow_package": return draft;
      case "conversation_models": return [];
      case "preview_workflow_package_revision": return { revision: draft.revision, revisions: [draft.revision],
        members: [{ key: "pipeline", graphDefinitions: { start, verify } }] };
      case "workflow_graph_registry": return [start, verify];
      case "workflow_package_catalog": return [preset.artifact, subflow.artifact];
      case "describe_workflow_reusable_node": return (payload as { version: typeof presetVersion }).version.kind === "preset" ? preset : subflow;
      case "save_workflow_package": return { ...draft, revision: "saved-next", package: (payload as { package: typeof draft.package }).package };
      default: throw Error(name);
    }
  });
  mount(<WorkflowPackageEditor draftId="draft" checkout={exampleCheckout} simulation={null}
    startSimulation={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} openWorkspace={vi.fn()} />);
  const user = userEvent.setup();
  await user.click(await screen.findByText("Add a reusable node"));
  const pick = screen.getByLabelText("Published reusable node");
  await user.selectOptions(pick, await screen.findByRole("option", { name: /Test preset ·/ }));
  await user.click(screen.getByRole("button", { name: "Add pinned node" }));
  const parent = screen.getByRole("region", { name: "Pipeline graph editor" });
  const inspect = within(parent).getByLabelText("Inspect node");
  await user.selectOptions(inspect, await within(inspect).findByRole("option", { name: /Test preset · reusable/ }));
  const names = within(parent).getByLabelText(/Required checks/);
  expect((names as HTMLTextAreaElement).value).toBe("Build");
  fireEvent.change(names, { target: { value: "Lint" } });
  await user.selectOptions(pick, screen.getByRole("option", { name: /Review subflow ·/ }));
  await user.click(screen.getByRole("button", { name: "Add pinned node" }));
  await user.selectOptions(inspect, await within(inspect).findByRole("option", { name: /Review subflow · reusable/ }));
  await user.click(within(parent).getByRole("button", { name: "Open pinned subflow" }));
  const child = await screen.findByRole("region", { name: "Pinned subflow Review subflow" });
  expect(within(child).getByText("subflow-r1")).toBeTruthy();
  const childInspect = within(child).getByLabelText("Inspect node");
  await user.selectOptions(childInspect, "verify");
  expect((within(child).getByLabelText(/Required checks/) as HTMLTextAreaElement).disabled).toBe(true);
  await user.click(screen.getByRole("button", { name: "Close pinned subflow" }));
  await user.click(screen.getByRole("button", { name: "Save package revision" }));
  expect(invoke).toHaveBeenCalledWith("save_workflow_package", expect.objectContaining({
    expectedRevision: "saved",
    package: { ...draft.package, members: [{ ...draft.package.members[0], body: { kind: "pipeline", graph: {
      ...graph, nodes: [...graph.nodes, { id: "reusable-verify-candidate", kind: "verify_candidate",
        configuration: { kind: "preset_overrides", fields: { names: ["Lint"] } } },
      { id: "reusable-subflow", kind: "subflow", configuration: { kind: "none" } }],
    } }, references: {
      "reusable-verify-candidate": { source: "published", version: presetVersion },
      "reusable-subflow": { source: "published", version: subflowVersion },
    } }] },
  }));
});
