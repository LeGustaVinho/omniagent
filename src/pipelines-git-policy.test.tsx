// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { GitSimulationProvider } from "@/git-simulation";
import PipelinesWorkspace from "@/pipelines-workspace";
import { exampleCheckout, nextDeliveryStep, type DeliveryStep } from "@/workbench-contract";

vi.mock("@xyflow/react", async (importOriginal) => ({
  ...await importOriginal<typeof import("@xyflow/react")>(), ReactFlow: () => null,
}));

function PipelineScenario() {
  const [delivery, setDelivery] = useState<DeliveryStep>("note");
  return <>
    <PipelinesWorkspace checkout={exampleCheckout} simulation={{
      scenario: "pipelines", epoch: 1, delivery,
      advanceDelivery: (action) => setDelivery((step) => nextDeliveryStep(step, action)),
    }} startSimulation={() => {}} openWorkspace={() => {}} openChanges={() => {}} openActivity={() => {}} />
  </>;
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

it("opens a clean simulation canvas without any starter nodes or templates", () => {
  render(<GitSimulationProvider epoch={1}><PipelineScenario /></GitSimulationProvider>);
  expect(screen.getByRole("textbox", { name: "Pipeline name" })).toHaveProperty("value", "Untitled pipeline");
  expect(screen.getByRole("region", { name: "Simulation graph canvas" })).toBeTruthy();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Run pipeline" }).disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Library" }));
  fireEvent.click(within(screen.getByRole("navigation", { name: "Library views" })).getByRole("button", { name: "Roles" }));
  expect(screen.queryByText("Saved templates")).toBeNull();
});
