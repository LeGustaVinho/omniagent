// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { AgentRoleFields } from "@/workflow-stage-model";
import type { WorkflowNodeConfiguration } from "@/workflow-graph-model";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => []) }));
afterEach(cleanup);

const requirements = { stage: { capabilities: [{ kind: "protocol", capability: "fresh_session" }] } };

function mount(nodeKind: "plan" | "implement", fixedModel = false) {
  const initial = { kind: "agent_role", role: "Researcher", requirements };
  function Stage() {
    const [configuration, setConfiguration] = useState<WorkflowNodeConfiguration>(initial);
    return <>
      <AgentRoleFields projectId="project" nodeId="agent" nodeKind={nodeKind} configuration={configuration}
        disabled={false} fixedModel={fixedModel} mode={nodeKind === "plan" ? "plan" : "approved_write"} onChange={setConfiguration} />
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
