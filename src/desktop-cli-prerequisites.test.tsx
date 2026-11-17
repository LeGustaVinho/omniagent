// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { Provider } from "@/bindings/Provider";
import { DesktopCliPrerequisites, providerExecutableLabels } from "@/desktop-cli-prerequisites";
import { ProviderSettings } from "@/provider-settings";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("reports prerequisite discovery and persists exact Windows provider paths through Settings", async () => {
  const detected = {
    codex: "C:\\Program Files\\Codex\\codex.exe",
    claude: "C:\\Program Files\\Claude Code\\claude.exe",
    antigravity: null,
  };
  const names = { codex: "codex.exe", claude: "claude.exe", antigravity: "agy.exe" };
  const providers: Provider[] = ["codex", "claude", "antigravity"];
  const saved = new Map<Provider, string>();
  const profiles = () => providers.map((provider) => ({ provider, executable: saved.get(provider) ?? null }));
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "cli_executables") return profiles();
    if (command === "cli_plugin_inventory") return null;
    if (command === "desktop_cli_prerequisites") return {
      git: { executable: "C:\\Program Files\\Git\\cmd\\git.exe", version: "2.55.0.windows.3", issue: null },
      providers: profiles().map(({ provider, executable }) => {
        const path = executable ?? detected[provider];
        return { provider, configuredExecutable: executable,
          status: { executable: path, version: path ? "1.2.3" : null, issue: path ? null : "agy.exe is not installed on PATH" },
        };
      }),
    };
    if (command === "set_cli_executable") {
      const { provider, executable } = args as { provider: Provider; executable: string | null };
      if (executable) saved.set(provider, executable);
      else saved.delete(provider);
      return { provider, executable };
    }
    throw new Error(`Unexpected command ${command}`);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><DesktopCliPrerequisites /><ProviderSettings /></QueryClientProvider>);
  const inventory = within(screen.getByRole("region", { name: "Desktop CLI installation" }));
  expect(await inventory.findByText("2.55.0.windows.3")).toBeTruthy();
  expect(inventory.getByText("Gemini via agy")).toBeTruthy();
  expect(inventory.getByText("agy.exe is not installed on PATH")).toBeTruthy();
  const user = userEvent.setup();
  for (const provider of providers) {
    const card = within(await screen.findByRole("region", { name: providerExecutableLabels[provider] }));
    const executable = `D:\\My utilities\\${names[provider]}`;
    fireEvent.change(card.getByLabelText("Executable path"), { target: { value: `"${executable}"` } });
    await user.click(card.getByRole("button", { name: "Save" }));
    expect(await card.findByText(executable)).toBeTruthy();
    expect(invoke).toHaveBeenCalledWith("set_cli_executable", { provider, executable });
  }
  const gemini = within(screen.getByRole("region", { name: "Gemini via agy" }));
  await user.click(gemini.getByRole("button", { name: "Use PATH" }));
  expect(await gemini.findByText("agy.exe is not installed on PATH")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("set_cli_executable", { provider: "antigravity", executable: null });
  expect(vi.mocked(invoke).mock.calls.every(([command]) => command !== "probe_cli_executable")).toBe(true);
});
