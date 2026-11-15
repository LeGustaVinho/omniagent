// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { defaultAppearance, useAppAppearance } from "@/settings-appearance";
import SettingsDialog from "@/settings-dialog";
import { exampleCheckout, type SimulationSession } from "@/workbench-contract";

const { invoke, getVersion } = vi.hoisted(() => ({ invoke: vi.fn(), getVersion: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion }));

const unusedAction = () => undefined;

function AppearanceRoot() {
  useAppAppearance();
  return null;
}

function openSettings(options: { initialPage?: string; simulation: SimulationSession | null }) {
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    {!options.simulation && <AppearanceRoot />}
    <SettingsDialog checkout={exampleCheckout} open onOpenChange={unusedAction}
      openActivity={unusedAction} openChanges={unusedAction} openWorkspace={unusedAction}
      startSimulation={unusedAction} {...options} />
  </QueryClientProvider>);
}

afterEach(() => {
  cleanup();
  invoke.mockReset();
  getVersion.mockReset();
  delete document.documentElement.dataset.appTheme;
  delete document.documentElement.dataset.appMotion;
  document.documentElement.removeAttribute("style");
});

it("restores the last saved theme if the appearance write fails", async () => {
  invoke.mockImplementation((command: string) => command === "appearance_settings"
    ? Promise.resolve(defaultAppearance)
    : Promise.reject(new Error("Database unavailable")));
  const user = userEvent.setup();
  openSettings({ simulation: null });

  const light = screen.getByRole("button", { name: "Light" });
  await waitFor(() => expect((light.closest("fieldset") as HTMLFieldSetElement).disabled).toBe(false));
  await user.click(light);
  await screen.findByRole("alert");
  expect(invoke).toHaveBeenCalledWith("save_appearance_settings", {
    settings: { ...defaultAppearance, theme: "light" },
  });
  expect(document.documentElement.dataset.appTheme).toBe("dark");
  expect(screen.getByRole("button", { name: "Dark" }).getAttribute("aria-pressed")).toBe("true");
});

it("shows reported CLI permissions independently of Windows sandbox support", async () => {
  invoke.mockImplementation(async (command: string) => {
    switch (command) {
      case "appearance_settings": return defaultAppearance;
      case "cli_executables": return [{ provider: "claude", executable: null }];
      case "cli_plugin_inventory": return null;
      case "probe_cli_executable": return {
        provider: "claude", version: "2.1.296", protocol: "verified", authentication: "authenticated",
        effectivePermissions: { status: "mode_reported", mode: "plan" }, osSandboxSupport: "unsupported",
      };
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  openSettings({ initialPage: "providers", simulation: null });
  await userEvent.setup().click(await screen.findByRole("button", { name: "Check CLI" }));
  expect((await screen.findByText("Effective CLI permissions")).nextElementSibling?.textContent).toContain("plan");
  expect(screen.getByText("Windows command sandbox").nextElementSibling?.textContent).toBe("Unsupported for this CLI on the Windows host");
});

it("checks a backup profile and recovers a failed simulated update without desktop commands", async () => {
  const user = userEvent.setup();
  openSettings({
    initialPage: "system",
    simulation: { epoch: 1, scenario: "windows", delivery: "note", advanceDelivery: unusedAction },
  });

  await user.selectOptions(screen.getByLabelText("Archive source"), "different-profile");
  await user.click(screen.getByRole("button", { name: "Preview restore" }));
  expect(screen.getByRole("alert").textContent).toContain("another Windows user profile");
  expect(screen.queryByRole("button", { name: "Restore selected backup" })).toBeNull();

  await user.selectOptions(screen.getByLabelText("Update example"), "launch-failed");
  await user.click(screen.getByRole("button", { name: "Check for updates" }));
  await user.click(screen.getByRole("button", { name: "Install signed build" }));
  for (let index = 0; index < 4; index++) {
    await user.click(screen.getByRole("button", { name: "Continue" }));
  }
  await user.click(screen.getByRole("button", { name: "Finish" }));
  expect(screen.getByText(/The candidate failed its restart check/).getAttribute("role")).toBe("alert");
  await user.click(screen.getByRole("button", { name: "Restore previous build" }));
  await user.click(screen.getByRole("button", { name: "Continue" }));
  await user.click(screen.getByRole("button", { name: "Continue" }));
  await user.click(screen.getByRole("button", { name: "Finish" }));
  expect(screen.getByRole("status").textContent).toContain("Restored the previous build");
  expect(invoke).not.toHaveBeenCalled();
  expect(getVersion).not.toHaveBeenCalled();
});
