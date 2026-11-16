// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { SimulatedDeliveryGitSettings } from "@/delivery-git-settings";
import { GitSimulationProvider, useGitSimulation } from "@/git-simulation";
import PipelinesWorkspace from "@/pipelines-workspace";
import { deliveryCommitSha } from "@/tasks-demo-data";
import { exampleCheckout, nextDeliveryStep, type DeliveryStep } from "@/workbench-contract";

vi.mock("@xyflow/react", async (importOriginal) => ({
  ...await importOriginal<typeof import("@xyflow/react")>(), ReactFlow: () => null,
}));

function PipelineScenario() {
  const git = useGitSimulation();
  const [delivery, setDelivery] = useState<DeliveryStep>("note");
  return <>
    <SimulatedDeliveryGitSettings />
    <button type="button" onClick={() => git.setMergeRequired(true)}>Require verified merge</button>
    <PipelinesWorkspace checkout={exampleCheckout} simulation={{
      scenario: "pipelines", epoch: 1, delivery,
      advanceDelivery: (action) => setDelivery((step) => nextDeliveryStep(step, action)),
    }} startSimulation={() => {}} openWorkspace={() => {}} openChanges={() => {}} openActivity={() => {}} />
  </>;
}

async function progressUntil<T>(get: () => T | null): Promise<T> {
  for (let step = 0; step < 25; step++) {
    const result = get();
    if (result !== null) return result;
    await act(() => vi.advanceTimersByTimeAsync(650));
  }
  throw new Error("The workflow did not reach the expected state");
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

it.each([false, true])("uses the Settings policy in the pipeline simulation, manual = %s", async (manual) => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  render(<GitSimulationProvider epoch={1}><PipelineScenario /></GitSimulationProvider>);
  const policy = screen.getByLabelText("After accepting a diff") as HTMLSelectElement;
  expect(policy.value).toBe("automatic_commit_user_merge");
  if (manual) fireEvent.change(policy, { target: { value: "user_commits_and_merges" } });
  fireEvent.click(screen.getByRole("button", { name: "Require verified merge" }));
  fireEvent.click(screen.getByRole("button", { name: "Run pipeline" }));
  const inspector = within(screen.getByRole("complementary", { name: "Stage inspector" }));
  const inspect = (name: string) => fireEvent.change(inspector.getByLabelText("Inspect stage"), { target: { value: name } });

  fireEvent.click(await progressUntil(() => inspector.queryByRole("button", { name: "Answer question" })));
  inspect("approval");
  fireEvent.click(await progressUntil(() => inspector.queryByRole("button", { name: "Approve implementation" })));
  inspect("verify");
  fireEvent.click(await progressUntil(() => inspector.queryByRole("button", { name: "Retry stage" })));
  inspect("accept");
  fireEvent.click(await progressUntil(() => inspector.queryByRole("button", { name: "Accept diff" })));
  inspect("commit");

  if (manual) {
    expect(await progressUntil(() => inspector.queryByText("Waiting for commit", { selector: ".pipeline-stack > p" }))).toBeDefined();
    expect(inspector.queryByText(deliveryCommitSha)).toBeNull();
  } else {
    expect(await progressUntil(() => inspector.queryByText(deliveryCommitSha))).toBeDefined();
    inspect("merge");
    expect(await progressUntil(() => inspector.queryByText("Waiting for merge", { selector: ".pipeline-stack > p" }))).toBeDefined();
  }
});
