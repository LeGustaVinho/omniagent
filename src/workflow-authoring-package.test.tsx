// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ModelOption } from "@/bindings/ModelOption";
import { PipelineAuthoringEditor } from "@/workflow-authoring";
import { PipelineBuilderLanding } from "@/workflow-authoring-package";
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

function mount({ stale = false, publication = null, openWorkspace = vi.fn(), members = initial.package.members, stageModels = [] }: {
  stale?: boolean; publication?: typeof published | null; openWorkspace?: WorkspaceProps["openWorkspace"];
  members?: object[]; stageModels?: { option: Pick<ModelOption, "name" | "selectable" | "selection" | "rating"> }[];
} = {}) {
  let draft: { package: { name: string; members: object[] }; revision: string; publication: unknown } =
    { ...initial, package: { ...initial.package, members }, publication };
  const note = { id: "saved-brief", relativePath: "brief.md", sha256: "a".repeat(64) };
  vi.mocked(invoke).mockImplementation(async (name, input) => {
    switch (name) {
      case "inspect_workflow_package": return draft;
      case "conversation_models": return stageModels.filter(({ option }) => option.selection.provider === (input as { provider: string }).provider);
      case "workflow_package_catalog": return [artifact];
      case "preview_workflow_package_revision": return {
        revision: (input as { revision: string }).revision, revisions: [draft.revision, initial.revision],
        members: [
          { key: "pipeline", name: "Pipeline", kind: "pipeline", changes: [], dependencies: {}, affectedUses: [], diagnostics: [], requirements: [] },
          { key: "command-local-command", name: artifact.name, kind: artifact.version.kind,
            changes: [{ path: "/body/output", change: "changed", before: "text", after: "json" }],
            dependencies: {}, affectedUses: [], diagnostics: [], requirements: artifact.blockers },
        ],
      };
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
    <PipelineAuthoringEditor checkout={exampleCheckout} draftId="package:draft"
      openWorkspace={openWorkspace} openActivity={vi.fn()} openChanges={vi.fn()} />
  </QueryClientProvider>);
  return userEvent.setup();
}

async function addPublishedRevision(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("button", { name: "Draft artifacts" }));
  await user.click(await screen.findByText("Edit draft source"));
  await user.click(screen.getByText("Published artifacts"));
  const candidate = await screen.findByRole("option", { name: /Repository checker/ });
  await user.selectOptions(screen.getByRole("combobox", { name: "Published artifact" }), candidate);
  await user.click(screen.getByRole("button", { name: "Edit this revision in the draft" }));
  expect(invoke).toHaveBeenCalledWith("read_published_workflow_package_artifact", { projectId: exampleCheckout.projectId, version });
  const text = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Draft document" });
  await waitFor(() => expect(JSON.parse(text.value).members[1]?.base).toEqual(version));
  await user.click(screen.getByRole("button", { name: "Save draft revision" }));
  expect(invoke).toHaveBeenCalledWith("save_workflow_package", expect.objectContaining({
    projectId: exampleCheckout.projectId, draftId: "draft", expectedRevision: "first-review",
    package: expect.objectContaining({ members: [pipeline, expect.objectContaining({ base: version })] }),
  }));
  await screen.findByTitle(published.reviewedRevision);
}

it("opens exact published bases, confirms one saved revision and shows every package result", async () => {
  const user = mount();
  await addPublishedRevision(user);
  await user.click(screen.getByRole("button", { name: "Review publication" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Publish this draft revision" }));
  expect(dialog.getByText(published.reviewedRevision)).toBeTruthy();
  await user.click(dialog.getByText(/Artifacts in this publication/));
  expect(dialog.getByText("Repository checker · library")).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "publish_reviewed_workflow_package")).toBe(false);
  expect(await dialog.findByText("Field changes (1)")).toBeTruthy();
  await user.click(dialog.getByRole("button", { name: "Confirm publication" }));
  expect(invoke).toHaveBeenCalledWith("publish_reviewed_workflow_package", {
    projectId: exampleCheckout.projectId, draftId: "draft", reviewedRevision: published.reviewedRevision,
  });
  const receipt = within(await screen.findByRole("region", { name: "Publication receipt" }));
  expect(receipt.getByText("result-v1")).toBeTruthy();
  expect(receipt.getByText("published-v2")).toBeTruthy();
  expect(receipt.getByText(artifact.blockers[0]!)).toBeTruthy();
});

it("preserves the whole draft and displays the current published revision on conflict", async () => {
  const user = mount({ stale: true });
  await addPublishedRevision(user);
  await user.click(screen.getByRole("button", { name: "Review publication" }));
  await screen.findByText("Field changes (1)");
  await user.click(screen.getByRole("button", { name: "Confirm publication" }));
  await user.click(await screen.findByRole("button", { name: "Compare published base" }));
  const conflict = within(await screen.findByRole("region", { name: "Current published package artifact" }));
  expect(conflict.getByText("updated-elsewhere")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Close published version changed" }));
  expect(screen.getByTitle(published.reviewedRevision)).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Draft artifacts" }));
  expect(screen.getByRole("list", { name: "Saved draft artifacts" }).textContent).toContain("Repository checker");
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

it("pins the chosen pipeline model on unbound stages and locally edited presets through the draft", async () => {
  const selected = { provider: "codex", profileId: "desktop", modelId: "fixture-model" } as const;
  const explicit = { provider: "claude", profileId: "desktop", modelId: "existing-model" };
  const stageModels = [{ option: { name: "Fixture model", selectable: true, selection: selected, rating: null } }];
  const agent = { kind: "agent_role", role: "Reviewer", requirements: { reviewReuse: true } };
  const preset = { key: "reviewer", name: "Reviewer", destination: "library", body: {
    kind: "preset", nodeKind: "agent", editableFields: ["role"], configuration: agent,
  } };
  const pinnedRole = { id: "other-role", revision: "published-v1", destination: "library", kind: "role" };
  const boundPreset = { ...preset, key: "bound-preset", references: { "$preset": { source: "published", version: pinnedRole } } };
  const fixedPreset = { ...preset, key: "fixed-preset", body: { ...preset.body, configuration: { ...agent, model: explicit } } };
  const direct = { id: "direct", kind: "agent", configuration: agent };
  const fixed = { id: "fixed", kind: "agent", configuration: { ...agent, model: explicit } };
  const bound = { id: "bound", kind: "agent", configuration: { ...agent, requirements: { boundRole: pinnedRole } } };
  const referenced = { id: "referenced", kind: "agent", configuration: agent };
  const nodes = [...pipeline.body.graph.nodes, direct, fixed, bound, referenced];
  const root = { ...pipeline, body: { ...pipeline.body, graph: { ...pipeline.body.graph, nodes } },
    references: { referenced: { source: "draft", key: preset.key } } };
  const child = { key: "child", name: "Child", body: {
    kind: "subflow", graph: { name: "Child", nodes: [direct], wires: [] },
  } };
  const members = [root, child, preset, boundPreset, fixedPreset];
  const user = mount({ members, stageModels });
  await user.click(await screen.findByText("Configure stage models"));
  const bulk = await screen.findByRole<HTMLButtonElement>("button", { name: "Apply to 3 unconfigured stages" });
  expect(bulk.disabled).toBe(true);
  await user.click(await screen.findByRole("button", { name: "Pipeline stage model · Choose model" }));
  await user.click(await screen.findByRole("menuitemradio", { name: "Fixture model" }));
  await user.click(bulk);
  expect(screen.queryByText("Configure stage models")).toBeNull();
  await user.click(screen.getByRole("button", { name: "Edit with AI" }));
  expect(await screen.findByRole("button", { name: "Draft authoring model · Choose model" })).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Close edit with ai" }));
  expect(invoke).not.toHaveBeenCalledWith("save_workflow_package", expect.anything());
  await user.click(screen.getByRole("button", { name: "Save draft revision" }));

  const configured = { ...agent, model: selected };
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("save_workflow_package", {
    projectId: exampleCheckout.projectId, draftId: initial.id, expectedRevision: initial.revision,
    package: { name: initial.package.name, members: [
      { ...root, body: { ...root.body, graph: { ...root.body.graph,
        nodes: [...pipeline.body.graph.nodes, { ...direct, configuration: configured }, fixed, bound, referenced],
      } } },
      { ...child, body: { ...child.body, graph: { ...child.body.graph, nodes: [{ ...direct, configuration: configured }] } } },
      { ...preset, body: { ...preset.body, configuration: configured } },
      boundPreset, fixedPreset,
    ] },
  }));
  expect(agent).not.toHaveProperty("model");
  expect(invoke).not.toHaveBeenCalledWith("publish_reviewed_workflow_package", expect.anything());
});

it("starts a named pipeline directly in the visual Builder", async () => {
  const openWorkspace = vi.fn();
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "list_workflow_packages") return [
      { id: "legacy-package", name: "Old factory package", standalonePipeline: false, published: false },
      { id: "saved-draft", name: "Existing pipeline draft", standalonePipeline: true, published: false },
    ];
    if (command === "create_workflow_package") return { id: "visual-draft", revision: "new", package: (args as { package: unknown }).package };
    throw Error(command);
  });
  const projectId = exampleCheckout.projectId;
  render(<QueryClientProvider client={new QueryClient()}>
    <PipelineBuilderLanding checkout={exampleCheckout} openWorkspace={openWorkspace} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  expect(await screen.findByText("Existing pipeline draft")).toBeTruthy();
  expect(screen.queryByText("Old factory package")).toBeNull();
  expect(screen.queryByRole("button", { name: "New workflow package" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Start from a template" })).toBeNull();
  await user.click(await screen.findByRole("button", { name: "New pipeline" }));
  const dialog = within(screen.getByRole("dialog", { name: "New pipeline" }));
  await user.type(dialog.getByRole("textbox", { name: "Pipeline name" }), "Visual review");
  await user.click(dialog.getByRole("button", { name: "Create pipeline" }));
  expect(invoke).toHaveBeenCalledWith("create_workflow_package", {
    projectId, conversationId: null, package: { name: "Visual review", members: [{
      key: "pipeline", name: "Visual review", destination: "project",
      body: { kind: "pipeline", graph: { name: "Visual review", nodes: [
        { id: "start", kind: "typed_start", configuration: { kind: "typed_start", fields: [] } },
        { id: "end", kind: "end", configuration: { kind: "end" } },
      ], wires: [{ from: { node: "start", port: "start" }, to: { node: "end", port: "ready" } }] } },
    }] },
  });
  await waitFor(() => expect(openWorkspace).toHaveBeenCalledWith("pipelines", "authoring:package:visual-draft"));
});
