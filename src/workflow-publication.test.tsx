// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PipelineAuthoringEditor } from "@/workflow-authoring";
import type { WorkflowDefinition, WorkflowGraph } from "@/workflow-graph-model";
import type { WorkflowPublication } from "@/workflow-publication";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("DOMMatrixReadOnly", class { m22 = 1; });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.resetAllMocks(); });

const start = {
  kind: "start", title: "Start", revision: 1, executor: "bind_input", configuration: "none", effect: "none",
  inputs: [], outputs: [{ name: "start", value: { kind: "control" } }],
} satisfies WorkflowDefinition;
const graph: WorkflowGraph = {
  name: "Reviewed pipeline", nodes: [{ id: "start", kind: "start", configuration: { kind: "none" } }], wires: [],
};
const base = { id: "workflow", revision: "original-published-revision" };
const publication: WorkflowPublication = {
  id: "durable-receipt",
  workflow: { id: "workflow", revision: "new-published-revision" },
  base,
  draftId: "protected-draft",
  reviewedRevisionId: "reviewed-revision",
  originatingRequestId: "original-request",
  originatingRunId: "original-run",
  decisionSource: "desktop_user",
  confirmedAt: 1_800_000_000,
};
const current = {
  version: { id: "workflow", revision: "other-published-revision" },
  name: "Published elsewhere",
  nodes: graph.nodes.map((instance) => ({ instance, revision: "current-node" })),
  wires: [],
};

function setup(stale = false) {
  let published: WorkflowPublication | null = null;
  vi.mocked(invoke).mockImplementation(async (command) => {
    switch (command) {
      case "workflow_node_definitions": return [start];
      case "conversation_models":
      case "workflow_authoring_bases":
      case "list_repository_markdown_revisions": return [];
      case "inspect_workflow_authoring": return {
        id: publication.draftId, conversationId: "source-chat", base, publication: published,
        head: { id: publication.reviewedRevisionId, runId: "original-run", graph, definitions: { start } },
        request: { jobId: "source-job", runId: "original-run", state: "saved", diagnostic: null },
      };
      case "publish_reviewed_workflow":
        if (stale) return { status: "stale_base", current };
        published = publication;
        return { status: "published", publication };
      default: throw new Error(command);
    }
  });
  function mount() {
    return render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <PipelineAuthoringEditor draftId={publication.draftId} checkout={exampleCheckout} simulation={null}
        openWorkspace={vi.fn()} openActivity={vi.fn()} openChanges={vi.fn()} startSimulation={vi.fn()} />
    </QueryClientProvider>);
  }
  return { user: userEvent.setup(), mount };
}

it("requires ordinary confirmation of the exact draft and base and restores the receipt when reopened", async () => {
  const { user, mount } = setup();
  const editor = mount();
  await user.click(await screen.findByRole("button", { name: "Review publication" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Publish this pipeline revision" }));
  expect(dialog.getByText(publication.reviewedRevisionId)).toBeTruthy();
  expect(dialog.getByText(base.revision)).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.some(([name]) => name === "publish_reviewed_workflow")).toBe(false);
  await user.click(dialog.getByRole("button", { name: "Cancel" }));
  expect(vi.mocked(invoke).mock.calls.some(([name]) => name === "publish_reviewed_workflow")).toBe(false);
  await user.click(screen.getByRole("button", { name: "Review publication" }));
  await user.click(screen.getByRole("button", { name: "Confirm publication" }));
  expect(invoke).toHaveBeenCalledWith("publish_reviewed_workflow", {
    projectId: exampleCheckout.projectId, draftId: publication.draftId,
    reviewedRevisionId: publication.reviewedRevisionId, expectedBase: base,
  });
  const receipt = within(await screen.findByRole("region", { name: "Publication evidence" }));
  expect(receipt.getByText(publication.workflow.revision)).toBeTruthy();
  expect(receipt.getByText(publication.id)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Save draft revision" })).toBeNull();
  editor.unmount();
  mount();
  expect(within(await screen.findByRole("region", { name: "Publication evidence" }))
    .getByText(publication.originatingRunId!)).toBeTruthy();
});

it("displays the current immutable graph on stale base and retains the reviewed editor graph", async () => {
  const { user, mount } = setup(true);
  mount();
  await user.click(await screen.findByRole("button", { name: "Review publication" }));
  await user.click(screen.getByRole("button", { name: "Confirm publication" }));
  const comparison = within(await screen.findByRole("region", { name: "Current published workflow" }));
  expect(comparison.getByText(current.version.revision)).toBeTruthy();
  await user.click(comparison.getByText("Compare the current published graph"));
  expect(comparison.getByText(/"revision": "current-node"/)).toBeTruthy();
  expect(screen.getByRole("heading", { name: graph.name })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Save draft revision" })).toBeTruthy();
  expect(screen.queryByRole("region", { name: "Publication evidence" })).toBeNull();
});
