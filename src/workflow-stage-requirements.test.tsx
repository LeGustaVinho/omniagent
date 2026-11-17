// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { useState, type ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import {
  AttemptStageRequirements, WorkflowRequirementsEditor, type RequirementConfiguration,
} from "@/workflow-stage-requirements";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
afterEach(() => { cleanup(); invoke.mockReset(); });

const model = { provider: "codex" as const, profileId: "desktop-direct:codex", modelId: "gpt-fixture" };
const skill = { kind: "skill", provider: "codex", namespace: "review@local",
  path: { kind: "checkout", path: ".agents/skills/review/SKILL.md" }, revision: "d".repeat(64), invoke: false };

function mount(child: ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{child}</QueryClientProvider>);
}

it("pins the exact role and qualified source and independently records selected skill invocation", async () => {
  invoke.mockImplementation(async (command: string) => {
    switch (command) {
      case "list_conversation_templates": return [{ templateId: "reviewer", version: { revision: "role-1", ordinal: 1 }, name: "Reviewer" }];
      case "conversation_template_revision": return { revision: { template: {} } };
      case "workflow_stage_inventory": return {
        provider: "codex", cliVersion: "1.0", notices: [],
        choices: [{ label: "Review · review@local", source: skill }],
        protocolCapabilities: ["native_suggestions"], capabilityPreview: [],
      };
      default: throw new Error(command);
    }
  });
  function Stage() {
    const [configuration, setConfiguration] = useState<RequirementConfiguration>({});
    return <>
      <WorkflowRequirementsEditor projectId="project" checkoutId="worktree" mode="plan"
        model={model} options={[]} configuration={configuration} rolePin disabled={false} onChange={setConfiguration} />
      <output aria-label="Saved body">{JSON.stringify(configuration)}</output>
    </>;
  }
  mount(<Stage />);
  const summary = screen.getByText("Role and stage requirements");
  fireEvent.click(summary);
  await screen.findByRole("option", { name: "Reviewer · Revision 1" });
  const roleTemplate = { templateId: "reviewer", revision: "role-1" };
  fireEvent.change(screen.getByLabelText("Pinned role requirements"), { target: { value: JSON.stringify(roleTemplate) } });
  fireEvent.click(screen.getByRole("button", { name: "Preview worktree inventory" }));
  await screen.findByRole("option", { name: "Review · review@local" });
  expect(invoke).toHaveBeenCalledWith("workflow_stage_inventory", {
    projectId: "project", checkoutId: "worktree", mode: "plan", model, effort: null, configuration: { roleTemplate },
  });
  fireEvent.change(screen.getByLabelText("Instruction, skill, or plugin source"), { target: { value: "0" } });
  fireEvent.click(screen.getByRole("button", { name: "Require source" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "Invoke this skill" }));
  fireEvent.change(screen.getByLabelText("Protocol capability"), { target: { value: "native_suggestions" } });
  fireEvent.click(screen.getByRole("button", { name: "Require capability" }));
  expect(JSON.parse(screen.getByLabelText("Saved body").textContent ?? "")).toEqual({
    roleTemplate,
    stage: { capabilities: [{ kind: "protocol", capability: "native_suggestions" }], sources: [{ ...skill, invoke: true }] },
  });
});

it("uses the historical attempt ID to inspect profile and failed requirements", async () => {
  invoke.mockResolvedValue({
    role: "Planner", instanceRevision: "stage-1", roleTemplate: null,
    scope: { provider: "codex", profileId: model.profileId, requestedModel: model.modelId,
      cliVersion: "1.0", checkout: { path: "C:\\worktree" } },
    decisions: [
      {kind: "capability", subject: {kind: "protocol", capability: "native_suggestions"}, blocker: null, evidenceId: "verified-1"},
      {kind: "source", source: skill, blocker: "The selected SKILL.md contents changed"},
    ],
  });
  mount(<AttemptStageRequirements projectId="p" runId="old-attempt" />);
  expect(invoke).not.toHaveBeenCalled();
  fireEvent.click(screen.getByText("Stage requirement assessment"));
  await screen.findByText("verified-1");
  expect(invoke).toHaveBeenCalledWith("workflow_stage_assessment", { projectId: "p", runId: "old-attempt" });
  expect(within(screen.getByRole("list")).getByText(/SKILL.md contents changed/)).toBeTruthy();
});

it("adds the owned Codex image operation as a role capability", () => {
  const onChange = vi.fn();
  mount(<WorkflowRequirementsEditor projectId="project" checkoutId="worktree" mode="plan" model={model}
    options={[]} configuration={{}} disabled={false} onChange={onChange} />);
  fireEvent.click(screen.getByText("Role requirements"));
  fireEvent.click(screen.getByRole("button", { name: "Require Codex image generation" }));
  expect(onChange).toHaveBeenCalledWith({ stage: { capabilities: [
    { kind: "tool", id: "codex-native/imageGeneration", version: "app-server-v2", operation: "generate_image" },
  ] } });
});

it("lists a matching profile and explains why another profile's proof is stale", async () => {
  const alternate = { provider: "claude" as const, profileId: "desktop-direct:claude", modelId: "claude-fixture" };
  const additional = Array.from({ length: 7 }, (_, index) => ({
    provider: "antigravity" as const, profileId: `extra-profile-${index}`, modelId: "comparison-model",
  }));
  const models = [model, alternate, ...additional];
  const configuration: RequirementConfiguration = { stage: { capabilities: [{ kind: "protocol", capability: "reasoning" }] } };
  invoke.mockResolvedValueOnce([
    { selection: model, attemptId: "verified-attempt", checkedAt: "2026-11-16T20:00:00Z", blockers: [] },
    { selection: alternate, attemptId: "previous-attempt", checkedAt: "2026-11-15T20:00:00Z",
      blockers: ["Reasoning verification has been superseded."] },
  ]).mockResolvedValueOnce([
    { selection: models[8], attemptId: null, checkedAt: null, blockers: ["No saved assessment for the ninth profile."] },
  ]);
  mount(<WorkflowRequirementsEditor projectId="project" checkoutId="worktree" mode="read_only" model={model}
    options={models.slice(1).map((selection) => ({ selection, name: selection.modelId, selectable: true,
      unavailableReason: null, rating: null, catalogVersion: 1 }))}
    configuration={configuration} disabled={false} onChange={vi.fn()} />);
  fireEvent.click(screen.getByText("Role requirements"));
  fireEvent.click(screen.getByRole("button", { name: "Find matching profiles" }));
  await screen.findByText("Verified match");
  expect(screen.getByText("verified-attempt")).toBeTruthy();
  expect(screen.getByText("Reasoning verification has been superseded.")).toBeTruthy();
  expect(screen.getByText("No saved assessment for the ninth profile.")).toBeTruthy();
  expect(invoke.mock.calls).toEqual([models.slice(0, 8), models.slice(8)].map((batch) =>
    ["workflow_stage_matching_profiles", { projectId: "project", checkoutId: "worktree",
      models: batch, mode: "read_only", effort: null, configuration }]));
});
