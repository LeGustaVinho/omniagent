// @vitest-environment jsdom

import type { ComponentType } from "react";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { WorkflowSimulationCanvas, type WorkflowSimulationGraph, type WorkflowSimulationStep } from "@/workflow-simulation-canvas";

vi.mock("@xyflow/react", async (original) => {
  type CanvasNode = { id: string; data: unknown; ariaLabel: string };
  return {
    ...await original<typeof import("@xyflow/react")>(),
    Handle: ({ id, isConnectable }: { id: string; isConnectable: boolean }) => <span data-port={id} data-connectable={isConnectable} />,
    Controls: () => null,
    Background: () => null,
    ReactFlow: ({ nodes, edges, nodeTypes, onNodeClick, nodesDraggable, nodesConnectable, edgesReconnectable, deleteKeyCode }: {
      nodes: CanvasNode[]; edges: { id: string; ariaLabel: string; data: { state: string } }[];
      nodeTypes: { simulation: ComponentType<CanvasNode> }; onNodeClick: (event: unknown, node: CanvasNode) => void;
      nodesDraggable: boolean; nodesConnectable: boolean; edgesReconnectable: boolean; deleteKeyCode: string | null;
    }) => <div data-testid="trace-flow" data-editable={nodesDraggable || nodesConnectable || edgesReconnectable || deleteKeyCode !== null}>
      {nodes.map((node) => <div key={node.id}>
        <button type="button" aria-label={node.ariaLabel} onClick={() => onNodeClick(undefined, node)}>Inspect</button>
        <nodeTypes.simulation {...node} />
      </div>)}
      <ul aria-label="Simulated edges">{edges.map((edge) => <li key={edge.id} data-signal={edge.data.state}>{edge.ariaLabel}</li>)}</ul>
    </div>,
  };
});

afterEach(cleanup);

it("keeps the saved simulation graph read-only and highlights only the recorded endpoint signal", async () => {
  const port = (name: string) => ({ name, value: { kind: "control" as const } });
  const graph: WorkflowSimulationGraph = {
    nodes: [
      { id: "branch", title: "Choose path", inputs: [], outputs: [port("true"), port("false")] },
      { id: "checks::verify", title: "Verify", inputs: [port("ready")], outputs: [port("done")] },
      { id: "checks::recheck", title: "Recheck", inputs: [port("ready")], outputs: [] },
    ],
    wires: [
      { from: { node: "branch", port: "true" }, to: { node: "checks::verify", port: "ready" } },
      { from: { node: "branch", port: "false" }, to: { node: "checks::recheck", port: "ready" } },
    ],
  };
  const steps: WorkflowSimulationStep[] = [
    { nodeId: "branch", state: "completed", inputs: [] },
    { nodeId: "checks::verify", state: "waiting", reason: "Needs the sample check.",
      inputs: [{ port: "ready", from: { node: "branch", port: "true" }, state: "selected" }] },
    { nodeId: "checks::recheck", state: "skipped",
      inputs: [{ port: "ready", from: { node: "branch", port: "false" }, state: "skipped" }] },
  ];
  render(<WorkflowSimulationCanvas graph={graph} steps={steps} renderStep={(id) => <p>Evidence for {id}</p>} />);
  const canvas = within(screen.getByRole("region", { name: "Simulated workflow graph" }));
  const flow = canvas.getByTestId("trace-flow");
  expect(flow.getAttribute("data-editable")).toBe("false");
  expect(flow.querySelectorAll("[data-port]").length).toBeGreaterThan(0);
  expect(flow.querySelectorAll('[data-connectable="true"]')).toHaveLength(0);
  expect(canvas.getByText("branch.true to checks::verify.ready · Selected path").getAttribute("data-signal")).toBe("selected");
  expect(canvas.getByText("branch.false to checks::recheck.ready · Skipped").getAttribute("data-signal")).toBe("skipped");

  const user = userEvent.setup();
  const inspector = within(screen.getByRole("complementary", { name: "Simulation node evidence" }));
  await user.click(canvas.getByRole("button", { name: "Verify · checks::verify · Waiting" }));
  expect(inspector.getByRole("heading", { name: "Verify" })).toBeTruthy();
  expect(inspector.getByText("Evidence for checks::verify")).toBeTruthy();
  await user.selectOptions(inspector.getByRole("combobox", { name: "Inspect simulated stage" }), "checks::recheck");
  expect(inspector.getByText("Evidence for checks::recheck")).toBeTruthy();
});
