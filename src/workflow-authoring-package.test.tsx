// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { PipelineAuthoringEditor } from "@/workflow-authoring";
import { WorkflowPackageList } from "@/workflow-authoring-package";
import { exampleCheckout, type WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const version = { destination: "library", kind: "command", id: "local-command", revision: "published-v1" };
const artifact = {
  version, name: "Repository checker",
  body: { kind: "command", binding: { source: "required", name: "Local checker" }, arguments: [], input: {}, output: "json" },
  references: {}, blockers: ["Register and bind the Command executable or script Local checker."],
};
const pipeline = {
  key: "pipeline", name: "Pipeline", destination: "project", body: { kind: "pipeline", graph: {
    name: "Pipeline", nodes: [{ id: "start", kind: "start", configuration: { kind: "none" } }], wires: [],
  } },
};
const initial = {
  id: "draft", revision: "first-review", conversationId: null, request: null, publication: null,
  package: { name: "Repository package", members: [pipeline] },
};
const published = {
  id: "receipt", reviewedRevision: "second-review", originatingRunId: "author-run",
  members: [
    { key: "pipeline", previous: null, artifact: {
      name: "Pipeline", version: { destination: "project", kind: "pipeline", id: "different-pipeline", revision: "result-v1" },
      body: pipeline.body, references: {}, blockers: [],
    } },
    { key: "command-local-command", previous: version, artifact: { ...artifact, version: { ...version, revision: "published-v2" } } },
  ],
};

function mount({ stale = false, publication = null, openWorkspace = vi.fn() }: {
  stale?: boolean; publication?: typeof published | null; openWorkspace?: WorkspaceProps["openWorkspace"];
} = {}) {
  let draft: { package: { name: string; members: object[] }; revision: string; publication: unknown } = { ...initial, publication };
  const note = { id: "saved-brief", relativePath: "brief.md", sha256: "a".repeat(64) };
  vi.mocked(invoke).mockImplementation(async (name, input) => {
    switch (name) {
      case "inspect_workflow_package": return draft;
      case "conversation_models": return [];
      case "workflow_package_catalog": return [artifact];
      case "read_published_workflow_package_artifact": return artifact;
      case "workflow_authoring_bases": return [
        { name: "Earlier version", version: { id: "different-pipeline", revision: "earlier" } },
        { name: "Pipeline", version: { id: "different-pipeline", revision: "result-v1" } },
      ];
      case "list_repository_markdown_revisions": return [note];
      case "read_repository_markdown": return { revision: note, markdown: "Saved brief" };
      case "launch_workflow_manual": return { id: "published-run" };
      case "save_workflow_package": {
        const { package: saved } = input as { package: typeof draft.package };
        draft = { ...draft, package: saved, revision: published.reviewedRevision };
        return draft;
      }
      case "publish_reviewed_workflow_package":
        if (stale) return { status: "stale_base", member: "command-local-command", current: { ...artifact, version: { ...version, revision: "updated-elsewhere" } } };
        draft = { ...draft, publication: published };
        return { status: "published", publication: published };
      default: throw new Error(name);
    }
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <PipelineAuthoringEditor checkout={exampleCheckout} draftId="package:draft" simulation={null}
      openWorkspace={openWorkspace} openActivity={vi.fn()} openChanges={vi.fn()} startSimulation={vi.fn()} />
  </QueryClientProvider>);
  return userEvent.setup();
}

async function addPublishedRevision(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByText("Edit package"));
  await user.click(screen.getByText("Published artifacts"));
  const candidate = await screen.findByRole("option", { name: /Repository checker/ });
  await user.selectOptions(screen.getByRole("combobox", { name: "Published artifact" }), candidate);
  await user.click(screen.getByRole("button", { name: "Edit this revision in the package" }));
  expect(invoke).toHaveBeenCalledWith("read_published_workflow_package_artifact", { projectId: exampleCheckout.projectId, version });
  const text = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Package document" });
  await waitFor(() => expect(JSON.parse(text.value).members[1]?.base).toEqual(version));
  await user.click(screen.getByRole("button", { name: "Save package revision" }));
  expect(invoke).toHaveBeenCalledWith("save_workflow_package", expect.objectContaining({
    projectId: exampleCheckout.projectId, draftId: "draft", expectedRevision: "first-review",
    package: expect.objectContaining({ members: [pipeline, expect.objectContaining({ base: version })] }),
  }));
  await screen.findByText(published.reviewedRevision);
}

it("opens an editable project draft from the selected factory template revision", async () => {
  const openWorkspace = vi.fn();
  vi.mocked(invoke).mockImplementation(async (name) => {
    switch (name) {
      case "list_workflow_packages": return [];
      case "workflow_factory_templates": return [
        { id: "task_delivery", name: "Task delivery", revision: 1, members: ["Task delivery"] },
      ];
      case "create_workflow_factory_draft": return initial;
      default: throw new Error(name);
    }
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <WorkflowPackageList checkout={exampleCheckout} openWorkspace={openWorkspace} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(screen.getByText("Start from a template"));
  await user.click(await screen.findByRole("button", { name: "Create Task delivery draft" }));
  expect(invoke).toHaveBeenCalledWith("create_workflow_factory_draft", {
    projectId: exampleCheckout.projectId, template: "task_delivery", revision: 1,
  });
  await waitFor(() => expect(openWorkspace).toHaveBeenCalledWith("pipelines", "authoring:package:draft"));
  expect(invoke).not.toHaveBeenCalledWith("publish_reviewed_workflow_package", expect.anything());
});

it("opens exact published bases, confirms one saved revision and shows every package result", async () => {
  const user = mount();
  await addPublishedRevision(user);
  await user.click(screen.getByRole("button", { name: "Review package publication" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Publish this package revision" }));
  expect(dialog.getByText(published.reviewedRevision)).toBeTruthy();
  expect(dialog.getByText("Repository checker · library")).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "publish_reviewed_workflow_package")).toBe(false);
  await user.click(dialog.getByRole("button", { name: "Confirm package publication" }));
  expect(invoke).toHaveBeenCalledWith("publish_reviewed_workflow_package", {
    projectId: exampleCheckout.projectId, draftId: "draft", reviewedRevision: published.reviewedRevision,
  });
  const receipt = within(await screen.findByRole("region", { name: "Package publication receipt" }));
  expect(receipt.getByText("result-v1")).toBeTruthy();
  expect(receipt.getByText("published-v2")).toBeTruthy();
  expect(receipt.getByText(artifact.blockers[0]!)).toBeTruthy();
});

it("preserves the whole draft and displays the current published revision on conflict", async () => {
  const user = mount({ stale: true });
  await addPublishedRevision(user);
  await user.click(screen.getByRole("button", { name: "Review package publication" }));
  await user.click(screen.getByRole("button", { name: "Confirm package publication" }));
  const conflict = within(await screen.findByRole("region", { name: "Current published package artifact" }));
  expect(conflict.getByText("updated-elsewhere")).toBeTruthy();
  expect(screen.getByText(published.reviewedRevision)).toBeTruthy();
  expect(screen.getByRole("list", { name: "Saved package members" }).textContent).toContain("Repository checker");
});

it("runs the exact pipeline from its package receipt and opens its execution graph", async () => {
  const openWorkspace = vi.fn();
  const user = mount({ publication: published, openWorkspace });
  await user.click(await screen.findByRole("button", { name: "Run published workflow" }));
  const version = await screen.findByRole<HTMLSelectElement>("combobox", { name: "Published workflow revision" });
  expect(version.selectedOptions[0]?.textContent).toContain("Pipeline · result-v1");
  await user.click(await screen.findByText("Read saved Markdown"));
  expect(screen.getByText("Saved brief")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Start workflow" }));
  expect(invoke).toHaveBeenCalledWith("launch_workflow_manual", {
    request: {
      projectId: exampleCheckout.projectId,
      workflow: { id: "different-pipeline", revision: "result-v1" },
      source: { checkoutId: exampleCheckout.checkoutId, revisionId: "saved-brief" },
      workId: null,
      requestId: expect.any(String),
    },
  });
  await waitFor(() => expect(openWorkspace).toHaveBeenCalledWith("pipelines", "run:published-run"));
});
