// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { AgentRoleFields } from "@/workflow-stage-model";
import type { WorkflowNodeConfiguration } from "@/workflow-graph-model";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => []) }));
afterEach(() => { cleanup(); vi.mocked(invoke).mockReset(); });

const requirements = { stage: { capabilities: [{ kind: "protocol", capability: "fresh_session" }] } };

function mount(nodeKind: "plan" | "implement" | "agent", fixedModel = false,
  initial: WorkflowNodeConfiguration = { kind: "agent_role", role: "Researcher", requirements }) {
  function Stage() {
    const [configuration, setConfiguration] = useState<WorkflowNodeConfiguration>(initial);
    return <>
      <AgentRoleFields projectId="project" checkoutId="checkout" nodeId="agent" nodeKind={nodeKind} configuration={configuration}
        disabled={false} fixedModel={fixedModel} mode={nodeKind === "plan" ? "plan" : nodeKind === "agent" ? "read_only" : "approved_write"}
        onChange={setConfiguration} />
      <output aria-label="Stage configuration">{JSON.stringify(configuration)}</output>
    </>;
  }
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <Stage />
  </QueryClientProvider>);
  return initial;
}

function configuration() {
  return JSON.parse(screen.getByLabelText("Stage configuration").textContent ?? "{}");
}

it("edits the question route and subagent cap on a Plan stage and preserves the other requirements", async () => {
  const initial = mount("plan");
  const user = userEvent.setup();
  const route = screen.getByLabelText("Planning questions");
  await user.selectOptions(route, "structured_turn");
  const cap = screen.getByRole("spinbutton", { name: "Maximum subagents" });
  fireEvent.change(cap, { target: { value: "2" } });
  expect(configuration()).toEqual({ ...initial, requirements: { ...requirements,
    plan: { questionRoute: "structured_turn", maxSubagents: 2 } } });
  fireEvent.change(cap, { target: { value: "33" } });
  expect(configuration().requirements.plan.maxSubagents).toBe(2);
  await user.selectOptions(route, "native");
  expect(configuration().requirements.plan).toEqual({ questionRoute: "native", maxSubagents: 2 });
  await user.selectOptions(route, "default");
  expect(configuration()).toEqual(initial);
});

it("leaves writing stages outside the Plan Mode controls and keeps pinned presets read-only", () => {
  mount("implement");
  expect(screen.queryByLabelText("Planning questions")).toBeNull();
  cleanup();
  mount("plan", true);
  expect(screen.getByRole<HTMLFieldSetElement>("group", { name: "Plan Mode" }).disabled).toBe(true);
});

it("configures generic Agent planning and lets the operator turn it off before enabling Goal Mode", async () => {
  const initial = mount("agent");
  const user = userEvent.setup();
  const route = screen.getByRole<HTMLSelectElement>("combobox", { name: "Planning questions" });
  expect(route.value).toBe("default");
  await user.selectOptions(route, "structured_turn");
  fireEvent.change(screen.getByRole("spinbutton", { name: "Maximum subagents" }), { target: { value: "4" } });
  expect(configuration()).toEqual({ ...initial, requirements: { ...requirements,
    plan: { questionRoute: "structured_turn", maxSubagents: 4 } } });

  await user.click(screen.getByText("Goal Mode"));
  const goal = screen.getByRole<HTMLInputElement>("checkbox", { name: "Use Goal Mode for this stage" });
  expect(goal.disabled).toBe(true);
  await user.selectOptions(route, "default");
  expect(configuration()).toEqual(initial);
  expect(screen.queryByRole("spinbutton", { name: "Maximum subagents" })).toBeNull();
  await user.click(goal);
  expect(configuration().requirements.goal).toBeDefined();
  expect(configuration().requirements.plan).toBeUndefined();
  expect(route.disabled).toBe(true);
  await user.click(goal);
  expect(configuration()).toEqual(initial);
  expect(route.disabled).toBe(false);
});

it("checks a generic native Plan Mode profile with the pinned question route", async () => {
  const model = { provider: "codex", profileId: "planning-profile", modelId: "planning-model" };
  vi.mocked(invoke).mockImplementation(async (name, args) => {
    if (name === "conversation_models") return (args as { provider: string }).provider === "codex"
      ? [{ option: { selection: model, name: "Planning model", selectable: true, rating: null } }] : [];
    if (name === "list_conversation_templates") return [];
    if (name === "workflow_stage_matching_profiles") return [{ selection: model, blockers: [] }];
    throw Error(String(name));
  });
  const initial = mount("agent", false, { kind: "agent_role", role: "Researcher", model });
  const user = userEvent.setup();
  await user.selectOptions(screen.getByRole("combobox", { name: "Planning questions" }), "native");
  const plan = { questionRoute: "native", maxSubagents: 0 };
  expect(configuration()).toEqual({ ...initial, requirements: { plan } });
  await user.click(screen.getByText("Role and stage requirements"));
  await user.click(screen.getByRole("button", { name: "Find matching profiles" }));
  expect(await screen.findByText("Verified match")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("workflow_stage_matching_profiles", {
    projectId: "project", checkoutId: "checkout", models: [model], effort: null, mode: "plan", configuration: { plan },
  });
});
