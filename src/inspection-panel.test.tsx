// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { RunInspector } from "@/inspection-panel";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("only reads command output when its tab is activated, under the inspected project and attempt", async () => {
  const runId = "attempt-2";
  const { projectId, checkoutId } = exampleCheckout;
  const readOutput = vi.fn(() => ({ stdout: "Command finished", stderr: "", truncated: false, process: null }));
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "command_job_run": return { id: runId, projectId, checkoutId, turnId: "turn-2", status: "succeeded" };
      case "run_orchestration": return null;
      case "command_job_output":
        expect(args).toEqual({ projectId, runId });
        return readOutput();
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const user = userEvent.setup();
  render(<QueryClientProvider client={client}>
    <RunInspector checkout={exampleCheckout} simulation={null} runId={runId} onClose={vi.fn()}
      startSimulation={vi.fn()} openWorkspace={vi.fn()} openChanges={vi.fn()} openActivity={vi.fn()} />
  </QueryClientProvider>);

  const inspector = within(await screen.findByRole("region", { name: "Command activity" }));
  const overview = inspector.getByRole("tab", { name: "Overview" });
  const output = inspector.getByRole("tab", { name: "Command output" });
  await user.click(overview);
  await user.keyboard("{ArrowRight}");
  await waitFor(() => expect(document.activeElement).toBe(output));
  expect(output.getAttribute("aria-selected")).toBe("false");
  expect(readOutput).not.toHaveBeenCalled();

  await user.keyboard("{Enter}");
  const panel = inspector.getByRole("tabpanel", { name: "Command output" });
  expect(await within(panel).findByText("Command finished")).toBeTruthy();
  expect(readOutput).toHaveBeenCalledTimes(1);
});
