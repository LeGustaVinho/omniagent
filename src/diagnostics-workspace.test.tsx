// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { RealDiagnostics } from "@/desktop-dock";
import { SimulationDiagnostics } from "@/inspection-panel";
import { exampleCheckout, type WorkspaceProps } from "@/workbench-contract";

const { getVersion, invoke } = vi.hoisted(() => ({ getVersion: vi.fn(), invoke: vi.fn() }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

afterEach(() => {
  cleanup();
  getVersion.mockReset();
  invoke.mockReset();
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
  await user.click(screen.getByRole("button", { name: "Desktop" }));
  expect(screen.queryByRole("region", { name: "Process fixture" })).toBeNull();
  await user.click(fixtureTab);
  expect(within(screen.getByRole("region", { name: "Process fixture" })).getByText("Input received: Boundary check")).toBeTruthy();
  expect(invoke).not.toHaveBeenCalled();
  expect(getVersion).not.toHaveBeenCalled();
});
