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
      case "desktop_cli_prerequisites": return {
        git: { executable: null, version: null, issue: null },
        providers: [{ provider: "claude", configuredExecutable: null,
          status: { executable: "C:\\Tools\\claude.exe", version: "2.1.296", issue: null } }],
      };
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

it("stages the verified backup only after confirmation and reports the pending restart", async () => {
  const archive = "C:\\Backups\\OmniAgent checkpoint";
  const receipt = { checkpointId: "checkpoint-123", schemaBefore: 111, schemaAfter: 112 };
  let pending: typeof receipt | null = null;
  getVersion.mockResolvedValue("0.1.0");
  invoke.mockImplementation(async (command: string) => {
    switch (command) {
      case "appearance_settings": return defaultAppearance;
      case "runtime_status": return "Desktop runtime is ready.";
      case "ripgrep_status": return { configuredExecutable: null, executable: null, version: null };
      case "desktop_cli_prerequisites": return { git: {}, providers: [] };
      case "signed_update_status": return { configured: false, latest: null };
      case "application_restore_status": return { active: null, pending, rejection: null };
      case "preview_application_restore": return {
        archive, preview: { checkpointId: receipt.checkpointId, schemaVersion: 111, manifestSha256: "a".repeat(64), packageCount: 2 },
      };
      case "stage_application_restore": return pending = receipt;
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  openSettings({ initialPage: "system", simulation: null });
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Preview restore" }));
  expect(await screen.findByText(archive)).toBeTruthy();
  expect(invoke.mock.calls.some(([command]) => command === "stage_application_restore")).toBe(false);
  await user.click(screen.getByRole("button", { name: "Stage restore for next restart" }));
  expect(invoke).toHaveBeenCalledWith("stage_application_restore", { archive, manifestSha256: "a".repeat(64) });
  expect(await screen.findByText(/Restore staged\. Quit OmniAgent/)).toBeTruthy();
});

it("saves Git automation independently of its completion milestone and destination drift policy", async () => {
  invoke.mockImplementation(async (command: string, args?: { policy?: unknown }) => {
    switch (command) {
      case "appearance_settings": return defaultAppearance;
      case "candidate_gate_policy": return { revision: "fixture", gates: [] };
      case "delivery_git_policy": return "automatic_commit_user_merge";
      case "set_delivery_git_policy": return args?.policy;
      case "delivery_milestone_policy": return { milestone: "verified_commit", destinationBranch: null };
      case "delivery_destination_drift_policy": return "pause";
      case "set_delivery_milestone_policy":
      case "set_delivery_destination_drift_policy": return args?.policy;
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  openSettings({ initialPage: "git", simulation: null });
  const policy = screen.getByLabelText("After accepting a diff") as HTMLSelectElement;
  const user = userEvent.setup();
  await waitFor(() => expect(policy.value).toBe("automatic_commit_user_merge"));
  await user.selectOptions(policy, "user_commits_and_merges");
  expect(invoke).toHaveBeenCalledWith("set_delivery_git_policy", {
    projectId: exampleCheckout.projectId, policy: "user_commits_and_merges",
  });
  await waitFor(() => expect(policy.value).toBe("user_commits_and_merges"));
  await user.selectOptions(policy, "automatic_local_commit_and_merge");
  await waitFor(() => expect(policy.value).toBe("automatic_local_commit_and_merge"));

  const completion = screen.getByLabelText<HTMLSelectElement>("Complete repository work at");
  expect(completion.value).toBe("verified_commit");
  await user.selectOptions(completion, "verified_merge");
  const save = screen.getByRole<HTMLButtonElement>("button", { name: "Save completion settings" });
  expect(save.disabled).toBe(true);
  await user.type(screen.getByLabelText("Merge destination"), "release/integration");
  await user.click(save);
  expect(invoke).toHaveBeenCalledWith("set_delivery_milestone_policy", {
    projectId: exampleCheckout.projectId, policy: { milestone: "verified_merge", destinationBranch: "release/integration" },
  });
  const drift = screen.getByLabelText<HTMLSelectElement>("When the destination branch advances");
  expect(drift.value).toBe("pause");
  await user.selectOptions(drift, "prepare_and_recheck");
  await waitFor(() => expect(drift.value).toBe("prepare_and_recheck"));
  expect(invoke).toHaveBeenCalledWith("set_delivery_destination_drift_policy", {
    projectId: exampleCheckout.projectId, policy: "prepare_and_recheck",
  });
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
