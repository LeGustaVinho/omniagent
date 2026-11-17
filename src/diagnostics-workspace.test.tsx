// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { RealDiagnostics } from "@/desktop-dock";
import { FixtureRunPanel } from "@/fixture-panels";
import { SimulationDiagnostics } from "@/inspection-panel";
import { exampleCheckout, type WorkspaceProps } from "@/workbench-contract";
import { useWorkspace } from "@/workspace-state";

const { getVersion, invoke } = vi.hoisted(() => ({ getVersion: vi.fn(), invoke: vi.fn() }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

afterEach(() => {
  cleanup();
  getVersion.mockReset();
  invoke.mockReset();
  useWorkspace.setState({ checkout: null });
});

it("reads actual desktop and CLI status and exposes the device settings link", async () => {
  getVersion.mockResolvedValue("0.1.0");
  invoke.mockImplementation(async (command: string) => {
    switch (command) {
      case "runtime_status": return "Desktop runtime is ready.";
      case "desktop_cli_prerequisites": return {
        git: { executable: "C:\\Git\\git.exe", version: "git version fixture", issue: null },
        providers: [],
      };
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  const openSystem = vi.fn();
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <RealDiagnostics checkout={exampleCheckout} openSystem={openSystem} />
  </QueryClientProvider>);

  const desktop = screen.getByRole("region", { name: "Current installation" });
  expect(await within(desktop).findByText("Desktop runtime is ready.")).toBeTruthy();
  expect(await within(desktop).findByText("0.1.0")).toBeTruthy();
  const executables = screen.getByRole("region", { name: "Installed executables" });
  expect(await within(executables).findByText("git version fixture")).toBeTruthy();
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "System settings" }));
  expect(openSystem).toHaveBeenCalledOnce();
  fireEvent.contextMenu(within(desktop).getByText("Desktop runtime is ready."));
  await user.click(screen.getByRole("menuitem", { name: "Copy installation summary" }));
  expect(await navigator.clipboard.readText()).toBe("OmniAgent 0.1.0\nDesktop runtime is ready.");
  await user.click(screen.getByRole("button", { name: "Check installed executables" }));
  await waitFor(() => expect(invoke.mock.calls.filter(([command]) => command === "desktop_cli_prerequisites")).toHaveLength(2));
  expect(invoke.mock.calls.every(([command]) => ["runtime_status", "desktop_cli_prerequisites"].includes(command))).toBe(true);
});

it("uses the same view navigation for a disposable fixture without invoking the runtime", async () => {
  const ctx: WorkspaceProps = {
    checkout: exampleCheckout,
    simulation: { epoch: 1, scenario: "windows", delivery: "note", advanceDelivery: vi.fn() },
    openWorkspace: vi.fn(), openActivity: vi.fn(), openChanges: vi.fn(), startSimulation: vi.fn(),
  };
  render(<SimulationDiagnostics {...ctx} />);
  const user = userEvent.setup();
  const fixtureTab = screen.getByRole("button", { name: "Fixture runs" });
  fixtureTab.focus();
  await user.keyboard("{Enter}");
  expect(fixtureTab.getAttribute("aria-current")).toBe("page");
  await user.click(screen.getByRole("button", { name: "Start fixture" }));
  await user.type(screen.getByLabelText("Fixture input"), "Boundary check");
  await user.click(screen.getByRole("button", { name: "Send input" }));
  expect(screen.getByText("Input received: Boundary check")).toBeTruthy();
  fireEvent.contextMenu(screen.getByText("Input received: Boundary check"));
  await user.click(screen.getByRole("menuitem", { name: "Copy event text" }));
  expect(await navigator.clipboard.readText()).toBe("Input received: Boundary check");
  fireEvent.contextMenu(screen.getByRole("heading", { name: "Process fixture" }));
  await user.click(screen.getByRole("menuitem", { name: "Copy recent fixture events" }));
  expect(await navigator.clipboard.readText()).toBe("[1 state] Waiting for input\n[2 stdout] Input received: Boundary check\n[3 state] Succeeded");
  await user.click(screen.getByRole("button", { name: "Desktop" }));
  expect(screen.queryByRole("region", { name: "Process fixture" })).toBeNull();
  await user.click(fixtureTab);
  expect(within(screen.getByRole("region", { name: "Process fixture" })).getByText("Input received: Boundary check")).toBeTruthy();
  expect(invoke).not.toHaveBeenCalled();
  expect(getVersion).not.toHaveBeenCalled();
});

it("copies only the selected fixture's events and keeps protected input display scoped to its run", async () => {
  useWorkspace.setState({ checkout: exampleCheckout });
  const runs = ["first-fixture", "second-fixture"].map((id) => ({
    id, projectId: exampleCheckout.projectId, checkoutId: exampleCheckout.checkoutId, turnId: id, status: "succeeded",
  }));
  invoke.mockImplementation(async (command: string, { runId } = { runId: "" }) => {
    if (command === "run_snapshot") return { cursor: "2", runs, recentEvents: runs.map(({ id, turnId }, index) => ({
      seq: String(index + 1), turnId, runId: id, data: { type: "output", stream: "stdout", preview: `Output for ${id}` },
    })) };
    if (command === "fixture_input_evidence") return { sent: true, text: `Protected input for ${runId}` };
    throw new Error(`Unexpected command ${command}`);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const panelFor = (runId: string) => <QueryClientProvider client={client}>
    <FixtureRunPanel {...({ params: { runId } } as Parameters<typeof FixtureRunPanel>[0])} />
  </QueryClientProvider>;
  const { rerender } = render(panelFor("first-fixture"));
  const user = userEvent.setup();
  const first = await screen.findByRole("region", { name: "Run first-fi" });
  await within(first).findByText("Output for first-fixture");
  fireEvent.contextMenu(within(first).getByRole("heading"));
  await user.click(screen.getByRole("menuitem", { name: "Copy recent fixture events" }));
  expect(await navigator.clipboard.readText()).toBe("[1 stdout] Output for first-fixture");
  expect(invoke.mock.calls.some(([command]) => command === "fixture_input_evidence")).toBe(false);
  await user.click(within(first).getByRole("button", { name: "Inspect protected input" }));
  expect(await within(first).findByText("Protected input for first-fixture")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("fixture_input_evidence", { checkoutId: exampleCheckout.checkoutId, runId: "first-fixture" });

  rerender(panelFor("second-fixture"));
  expect(await screen.findByText("Output for second-fixture")).toBeTruthy();
  expect(screen.queryByText("Protected input for first-fixture")).toBeNull();
  expect(invoke).not.toHaveBeenCalledWith("fixture_input_evidence", { checkoutId: exampleCheckout.checkoutId, runId: "second-fixture" });
});
