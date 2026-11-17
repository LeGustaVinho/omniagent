// @vitest-environment jsdom

import type { ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ModelOption } from "@/bindings/ModelOption";
import PipelinesWorkspace from "@/pipelines-workspace";
import { BuilderAuthoring, PipelineAuthoringEditor } from "@/workflow-authoring";
import { WorkflowPackageEditor } from "@/workflow-authoring-package";
import type { WorkflowPublication } from "@/workflow-publication";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
beforeEach(() => vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} }));
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.resetAllMocks(); });

const selection = { provider: "claude", profileId: "desktop-claude", modelId: "qualified-model" } as const;
const option: ModelOption = { selection, name: "Qualified Claude", selectable: true, unavailableReason: null, catalogVersion: 1, rating: null };
const checkout = exampleCheckout;
const args = { projectId: checkout.projectId, checkoutId: checkout.checkoutId, selection, effort: "high", brief: "Preserve existing references." };
const result = {
  id: "linked-draft", revision: "draft-revision", conversationId: "editor-source-chat", publication: null,
  package: { name: "Editor package", members: [] }, request: { state: "queued", runId: null, jobId: "editor-job", diagnostic: null },
};

function mock(extra: (command: string, input: unknown) => unknown) {
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    if (command === "conversation_models") return (input as { provider: string }).provider === "claude"
      ? [{ option, efforts: ["high"], defaultEffort: null }] : [];
    if (command === "start_workflow_editor_authoring") return result;
    return extra(command, input);
  });
}

function mount(child: ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{child}</QueryClientProvider>);
}

function navigation() {
  return { checkout, simulation: null, startSimulation: vi.fn(), openWorkspace: vi.fn(), openChanges: vi.fn(), openActivity: vi.fn(), openQueueJob: vi.fn() };
}

async function submit(user: ReturnType<typeof userEvent.setup>, revision: string) {
  await user.click(await screen.findByRole("button", { name: "Assist with AI" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Assist with AI" }));
  expect(dialog.getByText(revision)).toBeTruthy();
  expect(dialog.getByRole("button", { name: "Start authoring request" })).toHaveProperty("disabled", true);
  await user.click(dialog.getByRole("button", { name: /Authoring model · Choose model/ }));
  await user.click(await screen.findByRole("menuitemradio", { name: /Qualified Claude/ }));
  await user.selectOptions(dialog.getByRole("combobox", { name: "Authoring effort" }), "high");
  await user.type(dialog.getByRole("textbox", { name: "Authoring request" }), args.brief);
  await user.click(dialog.getByRole("button", { name: "Start authoring request" }));
}

it("uses the role revision selected in the live editor and links the resulting draft and chat in both directions", async () => {
  const current = { revision: "latest-role", ordinal: 2, baseRevision: "older-role" };
  const older = { revision: "older-role", ordinal: 1, baseRevision: null };
  const templateId = "saved-role";
  mock((command, input) => {
    switch (command) {
      case "list_conversation_templates": return [{ templateId, name: "Reviewer", version: current }];
      case "conversation_template_revision": return { history: [current, older], revision: { templateId,
        version: (input as { revision: string }).revision === older.revision ? older : current,
        template: { name: "Reviewer", role: "agent", model: selection, effort: null, instructions: "Review the work.", toolScope: "read_only",
          completionContract: "Finish review", reviewPolicy: "human_approval" } } };
      case "conversation_template_profile": return { cliVersion: "fixture", transport: "claude", toolScopes: [], capabilities: [] };
      case "inspect_workflow_package": return result;
      case "list_conversations": return [{ id: result.conversationId, checkoutId: checkout.checkoutId, title: "Editor source" }];
      case "workflow_authoring_bases": case "conversation_workflow_authoring": return [];
      case "list_workflow_packages": {
        expect(input).toEqual({ projectId: checkout.projectId, conversationId: result.conversationId });
        return [{ id: result.id, revision: result.revision, name: result.package.name, memberCount: 0, published: false }];
      }
      default: throw Error(command);
    }
  });
  const props = navigation();
  const user = userEvent.setup();
  const editor = mount(<PipelinesWorkspace {...props} target="templates" />);
  await user.selectOptions(await screen.findByRole("combobox", { name: "Saved revision" }), older.revision);
  await submit(user, older.revision);
  expect(invoke).toHaveBeenCalledWith("start_workflow_editor_authoring", { ...args,
    base: { id: templateId, revision: older.revision, destination: "project", kind: "role" } });
  await waitFor(() => expect(props.openWorkspace).toHaveBeenCalledWith("pipelines", `authoring:package:${result.id}`));
  editor.unmount();
  const draft = mount(<WorkflowPackageEditor {...props} draftId={result.id} />);
  await user.click(await screen.findByRole("button", { name: "Package details" }));
  await user.click(await screen.findByRole("button", { name: "Open source conversation" }));
  expect(props.openWorkspace).toHaveBeenCalledWith("conversations", result.conversationId);
  await user.click(screen.getByRole("button", { name: "Open queued authoring job" }));
  expect(props.openQueueJob).toHaveBeenCalledWith(result.request.jobId);
  draft.unmount();
  mount(<BuilderAuthoring checkout={checkout} openWorkspace={props.openWorkspace} />);
  await user.click(screen.getByRole("button", { name: "Create or edit with AI" }));
  await user.selectOptions(screen.getByRole("combobox", { name: "Source conversation" }), await screen.findByRole("option", { name: "Editor source" }));
  props.openWorkspace.mockClear();
  await user.click(await screen.findByRole("button", { name: "Open linked package" }));
  expect(props.openWorkspace).toHaveBeenCalledWith("pipelines", `authoring:package:${result.id}`);
});

it("preserves the selected published pipeline and reusable artifact revisions", async () => {
  const workflow = { id: "published-graph", revision: "exact-pipeline" };
  const reusable = { id: "local-node", revision: "exact-reusable", kind: "preset", destination: "library" };
  const publication: WorkflowPublication = { id: "pipeline-publication", workflow, base: null, draftId: "graph",
    reviewedRevisionId: "review", originatingRequestId: null, originatingRunId: "run", decisionSource: "desktop_user", confirmedAt: 1600000000 };
  mock((command) => {
    switch (command) {
      case "inspect_workflow_authoring": return { id: "graph", conversationId: "chat", base: null,
        head: { id: "review", graph: { name: "Pipeline", nodes: [], wires: [] } }, request: { state: "saved", runId: "run" },
        publication };
      case "workflow_graph_registry": return [];
      case "inspect_workflow_package": return { ...result, request: null };
      case "workflow_package_catalog": return [{ version: reusable, name: "Saved reusable node", blockers: [] }];
      default: throw Error(command);
    }
  });
  const props = navigation();
  const user = userEvent.setup();
  const pipeline = mount(<PipelineAuthoringEditor {...props} draftId="graph" />);
  await submit(user, workflow.revision);
  expect(invoke).toHaveBeenCalledWith("start_workflow_editor_authoring", { ...args,
    base: { ...workflow, kind: "pipeline", destination: "project" } });
  await waitFor(() => expect(props.openWorkspace).toHaveBeenCalledWith("pipelines", `authoring:package:${result.id}`));
  pipeline.unmount();

  mount(<WorkflowPackageEditor {...props} draftId="new-package" />);
  await user.click(await screen.findByRole("button", { name: "Package details" }));
  await user.click(await screen.findByText("Edit package"));
  await user.click(screen.getByText("Published artifacts"));
  await user.selectOptions(screen.getByRole("combobox", { name: "Published artifact" }),
    await screen.findByRole("option", { name: /Saved reusable node/ }));
  await submit(user, reusable.revision);
  expect(invoke).toHaveBeenCalledWith("start_workflow_editor_authoring", { ...args, base: reusable });
});
