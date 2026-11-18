// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { exampleCheckout } from "@/workbench-contract";
import { WorkflowPackageEditor } from "@/workflow-authoring-package";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@xyflow/react", async (original) => ({ ...await original<typeof import("@xyflow/react")>(),
  ReactFlow: () => null, Background: () => null, Controls: () => null,
}));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("reviews changes and pinned dependencies from an exact prior package revision", async () => {
  const projectId = exampleCheckout.projectId;
  const reference = { destination: "library", kind: "subflow", id: "checker", revision: "pinned-subflow" };
  const member = {
    key: "pipeline", name: "Delivery pipeline", kind: "pipeline", base: null,
    changes: [{ path: "/body/graph/nodes/check", change: "added", after: { kind: "command" }, before: null }],
    changesTruncated: false, dependencies: { check: { source: "published", version: reference } },
    affectedUses: [], diagnostics: [], requirements: ["Command check needs a local registration."],
  };
  const request = { projectId, draftId: "draft", revision: "older" };
  vi.mocked(invoke).mockImplementation(async (name, payload) => {
    switch (name) {
      case "inspect_workflow_package": return { id: "draft", revision: "head", conversationId: null, request: null, publication: null,
        package: { name: "Delivery package", members: [{ key: "pipeline", name: "Delivery pipeline", body: { kind: "pipeline" } }] } };
      case "conversation_models": return [];
      case "preview_workflow_package_revision": return { revision: (payload as { revision: string }).revision, revisions: ["head", "older"], members: [member] };
      default: throw Error(`Unexpected ${name}`);
    }
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <WorkflowPackageEditor draftId="draft" checkout={exampleCheckout}
      openActivity={vi.fn()} openChanges={vi.fn()} openWorkspace={vi.fn()} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Review changes" }));
  await user.selectOptions(await screen.findByRole("combobox", { name: "Review saved revision" }), await screen.findByRole("option", { name: "older" }));
  expect(await screen.findByText(member.requirements[0]!)).toBeTruthy();
  await user.click(screen.getByText("Graph changes (1)"));
  expect(screen.getByText("/body/graph/nodes/check")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("preview_workflow_package_revision", request);
  expect(screen.getByText(reference.revision)).toBeTruthy();
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
      case "workflow_node_definitions": return [start, parallel];
      case "save_workflow_package": return { ...draft, revision: "edited", package: (payload as { package: typeof draft.package }).package };
      default: throw Error(`Unexpected ${name}`);
    }
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <WorkflowPackageEditor draftId="draft" checkout={exampleCheckout}
      openActivity={vi.fn()} openChanges={vi.fn()} openWorkspace={vi.fn()} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  await user.clear(await screen.findByRole("textbox", { name: "Pipeline name" }));
  await user.type(screen.getByRole("textbox", { name: "Pipeline name" }), "Edited delivery");
  await user.click(screen.getByRole("button", { name: "Save draft revision" }));
  expect(invoke).toHaveBeenCalledWith("save_workflow_package", {
    projectId: exampleCheckout.projectId, draftId: draft.id, expectedRevision: draft.revision,
    package: { name: draft.package.name, members: [{ ...member, body: { ...member.body, graph: {
      ...graph, name: "Edited delivery",
    } } }] },
  });
});
