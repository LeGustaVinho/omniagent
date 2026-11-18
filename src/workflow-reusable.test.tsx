// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { MouseEvent, ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { testCheckout } from "@/test-fixtures/checkout";
import { WorkflowPackageEditor } from "@/workflow-authoring-package";
import { WorkflowReusableDerivation } from "@/workflow-reusable";
import type { WorkflowDefinition, WorkflowGraph, WorkflowWire } from "@/workflow-graph-model";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@xyflow/react", async (original) => {
  type CanvasNode = { id: string; selected: boolean };
  return {
    ...await original<typeof import("@xyflow/react")>(), Background: () => null, Controls: () => null,
    ReactFlow: ({ nodes, onNodesChange, onNodeContextMenu }: {
      nodes: CanvasNode[];
      onNodesChange?: (changes: { type: "select"; id: string; selected: boolean }[]) => void;
      onNodeContextMenu?: (event: MouseEvent, node: CanvasNode) => void;
    }) => <div>{nodes.map((node) => <button key={node.id} type="button" aria-label={`Select graph node ${node.id}`}
      aria-pressed={node.selected} onClick={() => onNodesChange?.([{ type: "select", id: node.id, selected: !node.selected }])}
      onContextMenu={(event) => onNodeContextMenu?.(event, node)} />)}</div>,
  };
});
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const projectId = testCheckout.projectId;
const control = { kind: "control" } as const;
const checks = { kind: "data", artifact: "checks" } as const;
const start: WorkflowDefinition = {
  kind: "start", revision: 1, title: "Start", configuration: "none", executor: "bind_input", effect: "none",
  inputs: [], outputs: [{ name: "start", value: control }],
};
const verify: WorkflowDefinition = {
  kind: "agent", revision: 1, title: "Verify candidate", configuration: "agent_role", executor: "read_only_agent",
  effect: "read_checkout", inputs: [{ name: "ready", value: control }], outputs: [{ name: "checks", value: checks }],
};
const edge: WorkflowWire = { from: { node: "start", port: "start" }, to: { node: "verify", port: "ready" } };
const graph: WorkflowGraph = {
  name: "Pipeline",
  nodes: [{ id: "start", kind: "start", configuration: { kind: "none" } },
    { id: "verify", kind: "agent", configuration: { kind: "agent_role", role: "Build" } }],
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
  await user.click(screen.getByRole("checkbox", { name: "role" }));
  await user.click(screen.getByRole("button", { name: "Save as preset" }));
  expect(invoke).toHaveBeenCalledWith("derive_workflow_reusable", { projectId, source, request: {
    name: "Reusable verification", key: "reusable-verification", destination: "project",
    selection: { type: "preset", nodeId: "verify", editableFields: ["role"] },
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

it("opens the saved preset and typed subflow review from the clicked canvas node and selection", async () => {
  const connected = { ...graph, nodes: [...graph.nodes, { ...graph.nodes[1]!, id: "verify-again" }] };
  const sourceDraft = { ...draft, package: { ...draft.package, members: [{ ...draft.package.members[0]!,
    body: { kind: "pipeline", graph: connected } }] } };
  const output = { node: "verify-again", port: "checks" };
  const source = { type: "package_revision", draftId: draft.id, revision: draft.revision, memberKey: "pipeline" };
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "inspect_workflow_package") return sourceDraft;
    if (command === "conversation_models") return [];
    if (command === "workflow_node_definitions") return [start, verify];
    if (command === "preview_workflow_package_revision") return { revisions: [], members: [{ key: "pipeline", graphDefinitions: {} }] };
    if (command === "describe_workflow_reusable_boundary") return {
      inputs: [{ wire: edge, value: control }], outputs: [{ endpoint: output, value: checks, consumedOutsideSelection: true }],
    };
    if (command === "derive_workflow_reusable") return { ...sourceDraft, revision: "derived" };
    throw Error(command);
  });
  mount(<WorkflowPackageEditor draftId={draft.id} checkout={testCheckout}
    openActivity={vi.fn()} openChanges={vi.fn()} openWorkspace={vi.fn()} />);
  const user = userEvent.setup();
  const first = await screen.findByRole("button", { name: "Select graph node verify" });
  const second = screen.getByRole("button", { name: "Select graph node verify-again" });
  await user.click(first);
  fireEvent.contextMenu(second);
  await user.click(await screen.findByRole("menuitem", { name: "Save node as preset…" }));
  const preset = within(await screen.findByRole("dialog", { name: "Save node as preset" }));
  expect(preset.getByRole<HTMLSelectElement>("combobox", { name: "Configured node" }).value).toBe("verify-again");
  await user.click(preset.getByRole("button", { name: "Close" }));

  await user.click(second);
  fireEvent.contextMenu(first);
  await user.click(await screen.findByRole("menuitem", { name: "Extract 2 selected nodes as subflow…" }));
  const subflow = within(await screen.findByRole("dialog", { name: "Extract subflow" }));
  expect(subflow.getByRole<HTMLInputElement>("checkbox", { name: "verify" }).checked).toBe(true);
  expect(subflow.getByRole<HTMLInputElement>("checkbox", { name: "verify-again" }).checked).toBe(true);
  await subflow.findByLabelText("Public input name for start.start");
  await user.type(subflow.getByLabelText("Name", { exact: true }), "Verification");
  await user.click(subflow.getByRole("button", { name: "Save as subflow" }));
  expect(invoke).toHaveBeenCalledWith("describe_workflow_reusable_boundary", { projectId, source, nodeIds: ["verify", "verify-again"] });
  expect(invoke).toHaveBeenCalledWith("derive_workflow_reusable", { projectId, source, request: {
    name: "Verification", key: "verification", destination: "project",
    selection: { type: "subflow", nodeIds: ["verify", "verify-again"], inputs: { start: [edge] }, outputs: { checks: output } },
  } });
});

it("edits pinned reusable nodes, saves an editable override, and opens a child with the same graph inspector", async () => {
  const presetVersion = { kind: "preset", id: "preset", revision: "preset-r1", destination: "library" };
  const subflowVersion = { kind: "subflow", id: "subflow", revision: "subflow-r1", destination: "library" };
  const preset = {
    artifact: { version: presetVersion, name: "Test preset", blockers: [], references: {},
      body: { kind: "preset", configuration: graph.nodes[1]?.configuration, editableFields: ["role"] } },
    definition: { ...verify, title: "Test preset" }, graphDefinitions: {}, editableOverrides: ["role"],
  };
  const subflow = {
    artifact: { version: subflowVersion, name: "Review subflow", blockers: [], references: {}, body: { kind: "subflow", graph } },
    definition: { ...verify, kind: "subflow", configuration: "none", title: "Review subflow" },
    graphDefinitions: { start, verify }, editableOverrides: [],
  };
  const member = draft.package.members[0]!;
  const opened = { ...draft, package: { ...draft.package, members: [{
    ...member, body: { ...member.body, graph: { ...graph, nodes: [...graph.nodes,
      { id: "reusable-verify-candidate", kind: "agent", configuration: { kind: "preset_overrides", fields: {} } },
      { id: "reusable-subflow", kind: "subflow", configuration: { kind: "none" } },
    ] } },
    references: { "reusable-verify-candidate": { source: "published", version: presetVersion },
      "reusable-subflow": { source: "published", version: subflowVersion } },
  }] } };
  vi.mocked(invoke).mockImplementation(async (name, payload) => {
    switch (name) {
      case "inspect_workflow_package": return opened;
      case "conversation_models": return [];
      case "preview_workflow_package_revision": return { revision: draft.revision, revisions: [draft.revision],
        members: [{ key: "pipeline", graphDefinitions: { start, verify } }] };
      case "workflow_node_definitions": return [start, verify];
      case "workflow_package_catalog": return [preset.artifact, subflow.artifact];
      case "describe_workflow_reusable_node": return (payload as { version: typeof presetVersion }).version.kind === "preset" ? preset : subflow;
      case "read_published_workflow_package_artifact": return { ...subflow.artifact, definitions: subflow.graphDefinitions };
      case "save_workflow_package": return { ...draft, revision: "saved-next", package: (payload as { package: typeof draft.package }).package };
      default: throw Error(name);
    }
  });
  mount(<WorkflowPackageEditor draftId="draft" checkout={testCheckout}
    openActivity={vi.fn()} openChanges={vi.fn()} openWorkspace={vi.fn()} />);
  const user = userEvent.setup();
  const parent = await screen.findByRole("region", { name: "Pipeline graph editor" });
  const inspect = within(parent).getByLabelText("Inspect node");
  await user.selectOptions(inspect, await within(inspect).findByRole("option", { name: /Test preset · reusable/ }));
  const names = within(parent).getByLabelText(/Agent role/);
  expect((names as HTMLInputElement).value).toBe("Build");
  fireEvent.change(names, { target: { value: "Lint" } });
  const pinnedNode = "reusable-verify-candidate";
  fireEvent.contextMenu(screen.getByRole("button", { name: `Select graph node ${pinnedNode}` }));
  await user.click(await screen.findByRole("menuitem", { name: "Duplicate node" }));
  expect(screen.getByRole("button", { name: `Select graph node ${pinnedNode}-2` })).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Undo graph edit" }));
  expect(screen.queryByRole("button", { name: `Select graph node ${pinnedNode}-2` })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Redo graph edit" }));
  expect(screen.getByRole("button", { name: `Select graph node ${pinnedNode}-2` })).toBeTruthy();
  await user.selectOptions(inspect, await within(inspect).findByRole("option", { name: /Review subflow · reusable/ }));
  await user.click(within(parent).getByRole("button", { name: "Open pinned Function" }));
  const child = await screen.findByRole("region", { name: "Artifact preview" });
  expect(within(child).getByText("subflow-r1")).toBeTruthy();
  const childInspect = within(child).getByLabelText("Inspect node");
  await user.selectOptions(childInspect, "verify");
  expect((within(child).getByLabelText(/Agent role/) as HTMLInputElement).disabled).toBe(true);
  await user.click(screen.getByRole("button", { name: "Voltar" }));
  await user.click(screen.getByRole("button", { name: "Save draft revision" }));
  expect(await screen.findByText("saved-next")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("save_workflow_package", expect.objectContaining({
    expectedRevision: "saved",
    package: { ...draft.package, members: [{ ...draft.package.members[0], body: { kind: "pipeline", graph: {
      ...graph, nodes: [...graph.nodes, { id: "reusable-verify-candidate", kind: "agent",
        configuration: { kind: "preset_overrides", fields: { role: "Lint" } } },
      { id: "reusable-subflow", kind: "subflow", configuration: { kind: "none" } },
      { id: "reusable-verify-candidate-2", kind: "agent",
        configuration: { kind: "preset_overrides", fields: { role: "Lint" } } }],
    } }, references: {
      "reusable-verify-candidate": { source: "published", version: presetVersion },
      "reusable-verify-candidate-2": { source: "published", version: presetVersion },
      "reusable-subflow": { source: "published", version: subflowVersion },
    } }] },
  }));
});
