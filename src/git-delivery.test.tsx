// @vitest-environment jsdom

import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import ChangesPanel from "@/changes-panel";
import { GitSimulationProvider } from "@/git-simulation";
import GitWorkspace from "@/git-workspace";
import QueuePanel from "@/queue-panel";
import { exampleCheckout, nextDeliveryStep, type DeliveryStep, type WorkspaceProps } from "@/workbench-contract";

vi.mock("@/ide-monaco", () => ({
  loadMonaco: () => new Promise(() => {}),
  monacoTheme: () => "vs-dark",
}));

const defaultProps = {
  checkout: exampleCheckout,
  startSimulation: () => {},
  openWorkspace: () => {},
  openChanges: () => {},
  openActivity: () => {},
} satisfies Omit<WorkspaceProps, "simulation">;

function QueueHarness() {
  const [delivery, setDelivery] = useState<DeliveryStep>("queued");
  return <QueuePanel {...defaultProps} simulation={{
    scenario: "delivery", epoch: 1, delivery,
    advanceDelivery: (action) => setDelivery((step) => nextDeliveryStep(step, action)),
  }} />;
}

function GitHarness({ start }: { start: DeliveryStep }) {
  const [delivery, setDelivery] = useState<DeliveryStep>(start);
  const simulation: WorkspaceProps["simulation"] = {
    scenario: "delivery", epoch: 1, delivery,
    advanceDelivery: (action) => setDelivery((step) => nextDeliveryStep(step, action)),
  };
  return (
    <GitSimulationProvider epoch={1}>
      <GitWorkspace {...defaultProps} simulation={simulation} target="saved" />
    </GitSimulationProvider>
  );
}

afterEach(cleanup);

describe("delivery review UI", () => {
  it("requires verification again after the destination changes", async () => {
    const user = userEvent.setup();
    render(<GitHarness start="verified" />);
    expect(screen.getByRole("button", { name: "Accept diff" })).toBeDefined();

    await user.click(screen.getByRole("button", { name: "Simulate destination change" }));
    expect(screen.queryByRole("button", { name: "Accept diff" })).toBeNull();
    expect((screen.getByRole("button", { name: "Run verification" }) as HTMLButtonElement).disabled).toBe(true);

    await user.click(screen.getByRole("button", { name: "Refresh destination" }));
    await user.click(screen.getByRole("button", { name: "Run verification" }));
    await user.click(screen.getByRole("button", { name: "Accept diff" }));
    const merge = screen.getByRole("button", { name: "Merge into destination" }) as HTMLButtonElement;
    expect(merge.disabled).toBe(true);

    await user.click(screen.getByRole("button", { name: "Commit accepted candidate" }));
    expect(merge.disabled).toBe(false);
    await user.click(merge);
    expect(screen.getByText("Merged into destination")).toBeDefined();
  });

  it("keeps a saved candidate visible when selecting another worktree", async () => {
    const user = userEvent.setup();
    render(<GitHarness start="review" />);
    const files = () => within(screen.getByRole("region", { name: "Changed files" }));

    expect(files().getByText("docs/release.md")).toBeDefined();
    await user.selectOptions(screen.getByLabelText("Git worktree"), "main");
    expect(files().getByText("docs/release.md")).toBeDefined();
    await user.selectOptions(screen.getByLabelText("Diff source"), "working");
    expect(screen.getByText("No changes")).toBeDefined();
  });

  it("applies the configured automatic commit and merge policy on approval", async () => {
    const user = userEvent.setup();
    render(<GitHarness start="verified" />);
    await user.selectOptions(screen.getByLabelText("After diff acceptance"), "merge");
    await user.click(screen.getByRole("button", { name: "Accept diff" }));
    expect(screen.getByText("Merged into destination")).toBeDefined();
    expect(screen.queryByRole("button", { name: "Commit accepted candidate" })).toBeNull();
  });

  it("blocks verification during a merge conflict and retains the saved patch", async () => {
    const user = userEvent.setup();
    render(<GitHarness start="verified" />);
    await user.click(screen.getByRole("button", { name: "Simulate merge conflict" }));
    expect(screen.getByText("Merge conflict. Resolve it in Git and run verification again.")).toBeDefined();
    expect((screen.getByRole("button", { name: "Run verification" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(screen.getByRole("button", { name: "View saved patch" }));
    expect(screen.getByText("+## Recovery")).toBeDefined();
    await user.click(screen.getByRole("button", { name: "Resolve with candidate edit" }));
    await user.click(screen.getByRole("button", { name: "Run verification" }));
    expect(screen.getByRole("button", { name: "Accept diff" })).toBeDefined();
  });

  it("labels unavailable diffs and enters simulation only on request", async () => {
    const user = userEvent.setup();
    const startSimulation = vi.fn();
    render(<ChangesPanel {...defaultProps} simulation={null} startSimulation={startSimulation} kind="saved" />);
    expect(screen.getByText("Diff integration pending")).toBeDefined();
    expect(screen.queryByText("docs/release.md")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Simulate" }));
    expect(startSimulation).toHaveBeenCalledOnce();
  });
});

describe("execution queue UI", () => {
  it("pauses dispatch, recovers an interrupted job and captures a new candidate", async () => {
    const user = userEvent.setup();
    render(<QueueHarness />);

    await user.click(screen.getByRole("button", { name: "Pause queue" }));
    expect((screen.getByRole("button", { name: "Start attempt" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(screen.getByRole("button", { name: "Resume queue" }));
    await user.click(screen.getByRole("button", { name: "Start attempt" }));
    await user.click(screen.getByRole("button", { name: "Interrupt" }));
    await user.click(screen.getByRole("button", { name: "Resume from checkpoint" }));
    await user.click(screen.getByRole("button", { name: "Start attempt" }));
    await user.click(screen.getByRole("button", { name: "Capture candidate" }));
    expect(within(screen.getByRole("complementary", { name: "Selected job" })).getByText("Candidate captured for verification")).toBeDefined();
  });
});
