// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import type { SessionMode } from "@/bindings/SessionMode";
import type { WorkflowNodeConfiguration } from "@/workflow-graph-model";
import { AgentRoleFields } from "@/workflow-stage-model";
import { WorkflowPackageEditor } from "@/workflow-authoring-package";
import { testCheckout } from "@/test-fixtures/checkout";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@xyflow/react", async (original) => ({
  ...await original<typeof import("@xyflow/react")>(), ReactFlow: () => null, Background: () => null, Controls: () => null,
}));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const model = { provider: "codex", profileId: "selected-cli-profile", modelId: "selected-model" } as const;
const version = { kind: "role", destination: "library", id: "writer", revision: "writer-revision" } as const;
const requirements = { stage: { capabilities: [{ kind: "protocol", capability: "workspace_write" }] } };
const projectId = "project";
const checkoutId = "checkout";
const nodeId = "stage";

function mount(initial: WorkflowNodeConfiguration, nodeKind: string, mode: SessionMode, pinned = true) {
  function Stage() {
    const [configuration, setConfiguration] = useState(initial);
    return <><AgentRoleFields projectId={projectId} checkoutId={checkoutId} nodeId={nodeId} nodeKind={nodeKind}
      goalRole={pinned ? { version } : undefined} mode={mode} configuration={configuration} disabled={false} onChange={setConfiguration} />
      <output aria-label="Saved stage configuration">{JSON.stringify(configuration)}</output></>;
  }
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <Stage />
  </QueryClientProvider>);
}

function savedStage() {
  return JSON.parse(screen.getByLabelText("Saved stage configuration").textContent ?? "{}");
}

const pin = { stageRevision: "node-revision", configurationRevision: "goal-revision", role: "Builder", roleRevision: version };

it("saves the writer goal and checks the actual model and role pin without writing the resolved pin into the package", async () => {
  let preview = 0;
  vi.mocked(invoke).mockImplementation(async (name) => {
    if (name === "conversation_models") return [{ option: { selection: model, name: "Fixture model", selectable: true, rating: null } }];
    if (name === "preview_workflow_stage_goal") return {
      pin,
      route: preview++ === 0 ? { route: "native_candidate", evidenceId: "matched-qualification" }
        : { route: "blocked", reason: "The effective Goal route cannot enforce this token budget" },
    };
    throw Error(name);
  });
  mount({ kind: "agent_role", role: "Builder", model, requirements }, "implement", "approved_write");
  const user = userEvent.setup();
  await user.click(screen.getByText("Goal Mode"));
  await user.click(screen.getByRole("checkbox", { name: "Use Goal Mode for this stage" }));
  const button = screen.getByRole<HTMLButtonElement>("button", { name: "Check Goal route" });
  expect(button.disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("Goal objective"), { target: { value: "Deliver the pinned change" } });
  fireEvent.change(screen.getByLabelText("Criterion 1"), { target: { value: "The candidate passes Build" } });
  await user.selectOptions(screen.getByLabelText("Evidence for criterion 1"), "candidate_gate");
  expect(button.disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("Time limit in seconds"), { target: { value: "300" } });
  const goal = {
    objective: "Deliver the pinned change",
    criteria: [{ text: "The candidate passes Build", evidence: "candidate_gate" }],
    maxSeconds: 300,
  };
  expect(savedStage()).toEqual({ kind: "agent_role", role: "Builder", model, requirements: { ...requirements, goal } });
  await user.click(button);
  expect(await screen.findByText("Qualified native Goal candidate")).toBeTruthy();
  expect(screen.getByText("matched-qualification")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("preview_workflow_stage_goal", {
    projectId, checkoutId, selection: model, node: { id: nodeId, kind: "implement",
      configuration: { kind: "agent_role", role: "Builder", model, requirements: { ...requirements, goal, boundRole: version } } },
  });
  fireEvent.change(screen.getByLabelText("Token budget"), { target: { value: "1000" } });
  expect(screen.queryByText("matched-qualification")).toBeNull();
  await user.click(button);
  expect(await screen.findByText(/Goal Mode blocked.*cannot enforce this token budget/)).toBeTruthy();
});

it("uses the saved role model to preview managed continuation and preserves the remaining requirements when disabled", async () => {
  vi.mocked(invoke).mockImplementation(async (name) => {
    if (name === "conversation_models") return [];
    if (name === "read_published_workflow_package_artifact") return {
      name: "Pinned reviewer", body: { kind: "role", template: { model } },
    };
    if (name === "preview_workflow_stage_goal") return {
      pin: { ...pin, role: "Reviewer" },
      route: { route: "managed", reason: "The scheduler enforces the requested turn limit" },
    };
    throw Error(name);
  });
  const goal = { objective: "Review the result", criteria: [{ text: "Confirm evidence", evidence: "operator_review" }], maxTurns: 2 };
  mount({ kind: "agent_role", role: "Reviewer", requirements: { ...requirements, goal } }, "review_plan", "read_only");
  const user = userEvent.setup();
  await user.click(screen.getByText("Goal Mode · enabled"));
  expect(screen.queryByRole("option", { name: "Candidate gate result" })).toBeNull();
  expect(screen.getByText(/Goal role · Reviewer/)).toBeTruthy();
  const button = screen.getByRole<HTMLButtonElement>("button", { name: "Check Goal route" });
  await waitFor(() => expect(button.disabled).toBe(false));
  await user.click(button);
  expect(await screen.findByText(/OmniAgent-managed continuation.*requested turn limit/)).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("read_published_workflow_package_artifact", { projectId, version });
  expect(invoke).toHaveBeenCalledWith("preview_workflow_stage_goal", { projectId, checkoutId, selection: model,
    node: { id: nodeId, kind: "review_plan", configuration: { kind: "agent_role", role: "Reviewer",
      requirements: { ...requirements, goal, boundRole: version } } } });
  await user.click(screen.getByRole("checkbox", { name: "Use Goal Mode for this stage" }));
  expect(savedStage()).toEqual({ kind: "agent_role", role: "Reviewer", requirements });
  expect(screen.queryByText(/OmniAgent-managed continuation/)).toBeNull();
});

it("shows why the isolated candidate reviewer cannot opt in", async () => {
  vi.mocked(invoke).mockResolvedValue([]);
  mount({ kind: "agent_role", role: "Reviewer" }, "review_candidate", "read_only", false);
  const user = userEvent.setup();
  await user.click(screen.getByText("Goal Mode"));
  expect(screen.getByRole<HTMLInputElement>("checkbox", { name: "Use Goal Mode for this stage" }).disabled).toBe(true);
  expect(screen.getByRole("alert").textContent).toContain("isolated checkout without the Goal bridge");
});

it("keeps the authored package reference when the graph editor saves the Goal settings", async () => {
  const definition = {
    kind: "agent", revision: 1, title: "Implement", configuration: "agent_role",
    executor: "write_agent", effect: "write_repository", inputs: [], outputs: [],
  };
  const authored = { source: "published", version };
  const goal = { objective: "Deliver", criteria: [{ text: "Build passed", evidence: "candidate_gate" }], maxSeconds: 300 };
  const configuration = { kind: "agent_role", role: "Builder", model, requirements: { ...requirements, goal } };
  const draft = { id: "package", revision: "saved", conversationId: null, request: null, publication: null,
    package: { name: "Package", members: [{ key: "flow", name: "Delivery flow",
      body: { kind: "pipeline", graph: { name: "Pipeline",
        nodes: [{ id: nodeId, kind: definition.kind, configuration }], wires: [] } },
      references: { [nodeId]: authored } }] } };
  vi.mocked(invoke).mockImplementation(async (name, payload) => {
    switch (name) {
      case "inspect_workflow_package": return draft;
      case "conversation_models": return [];
      case "workflow_node_definitions": return [definition];
      case "preview_workflow_package_revision": return { revision: draft.revision, revisions: [draft.revision],
        members: [{ key: "flow", graphDefinitions: { [nodeId]: definition } }] };
      case "preview_workflow_stage_goal": return { pin, route: { route: "native_candidate", evidenceId: "published-role-evidence" } };
      case "save_workflow_package": return { ...draft, revision: "new", package: (payload as { package: typeof draft.package }).package };
      default: throw Error(name);
    }
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <WorkflowPackageEditor draftId="package" checkout={testCheckout}
      openActivity={vi.fn()} openChanges={vi.fn()} openWorkspace={vi.fn()} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByText("Goal Mode · enabled"));
  fireEvent.change(screen.getByLabelText("Goal objective"), { target: { value: "Deliver the change" } });
  await user.click(screen.getByRole("button", { name: "Check Goal route" }));
  expect(await screen.findByText("published-role-evidence")).toBeTruthy();
  const nextConfiguration = { ...configuration, requirements: { ...requirements, goal: { ...goal, objective: "Deliver the change" } } };
  const nextNode = { id: nodeId, kind: definition.kind, configuration: nextConfiguration };
  expect(invoke).toHaveBeenCalledWith("preview_workflow_stage_goal", {
    projectId: testCheckout.projectId, checkoutId: testCheckout.checkoutId, selection: model,
    node: { ...nextNode, configuration: { ...nextConfiguration, requirements: { ...nextConfiguration.requirements, boundRole: version } } },
  });
  await user.click(screen.getByRole("button", { name: "Save draft revision" }));
  const member = draft.package.members[0]!;
  expect(invoke).toHaveBeenCalledWith("save_workflow_package", {
    projectId: testCheckout.projectId, draftId: draft.id, expectedRevision: draft.revision,
    package: { ...draft.package, members: [{ ...member, body: { ...member.body, graph: { ...member.body.graph, nodes: [nextNode] } } }] },
  });
});
