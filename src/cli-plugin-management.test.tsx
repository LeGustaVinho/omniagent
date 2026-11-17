// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { UserPluginManagement } from "@/cli-plugin-management";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

beforeEach(() => invoke.mockReset());
afterEach(cleanup);

it("shows the exact CLI target and refreshes inventory only after confirmation", async () => {
  const after = { id: "inventory-after", report: { provider: "claude" } };
  invoke.mockResolvedValueOnce({
    id: "preview-token",
    action: "disable",
    pluginId: "formatter@local",
    configurationRoot: "C:/Users/Fixture/.claude",
    executable: "C:/Tools/claude.exe",
    cliVersion: "2.1.296",
    executionScope: "user",
    persists: true,
  }).mockResolvedValueOnce({ status: "applied", inventoryAfter: after });
  const client = new QueryClient();
  render(<QueryClientProvider client={client}><UserPluginManagement provider="claude" disabled={false} /></QueryClientProvider>);

  fireEvent.click(screen.getByText("Manage user plugins"));
  fireEvent.change(screen.getByLabelText("Action"), { target: { value: "disable" } });
  fireEvent.change(screen.getByLabelText("Plugin ID"), { target: { value: "formatter@local" } });
  fireEvent.click(screen.getByRole("button", { name: "Review change" }));
  const confirm = await screen.findByRole("button", { name: "Confirm disable" });
  expect(screen.getByText("Claude Code · 2.1.296")).toBeTruthy();
  expect(screen.getByText("C:/Tools/claude.exe")).toBeTruthy();
  expect(screen.getByText("C:/Users/Fixture/.claude")).toBeTruthy();
  expect(screen.getByText("User · persists across sessions")).toBeTruthy();
  expect(invoke.mock.calls).toEqual([[
    "preview_claude_user_plugin_action", { request: { action: "disable", pluginId: "formatter@local" } },
  ]]);

  fireEvent.click(confirm);
  expect(await screen.findByText("Change confirmed in the refreshed inventory.")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("apply_claude_user_plugin_action", { previewId: "preview-token" });
  expect(client.getQueryData(["cli-plugin-inventory", "claude"])).toEqual(after);
});

it("reviews an Antigravity package folder and keeps the confirmed receipt in its provider inventory", async () => {
  const inventory = { id: "agy-after", report: { provider: "antigravity" } };
  const folder = "C:\\Plugins\\formatter";
  invoke.mockResolvedValueOnce({
    id: "agy-preview",
    action: "install",
    pluginId: "formatter",
    executable: "C:/Tools/agy.exe",
    cliVersion: "1.5.9",
    configurationRoot: "C:/Users/Fixture/.gemini/config",
    executionScope: "user",
    persists: true,
    source: folder,
    version: "1.0.0",
  }).mockResolvedValueOnce({ status: "applied", inventoryAfter: inventory });
  const client = new QueryClient();
  render(<QueryClientProvider client={client}><UserPluginManagement provider="antigravity" disabled={false} /></QueryClientProvider>);

  fireEvent.click(screen.getByText("Manage user plugins"));
  fireEvent.change(screen.getByLabelText("Local plugin folder"), { target: { value: folder } });
  fireEvent.click(screen.getByRole("button", { name: "Review change" }));
  const confirmation = await screen.findByRole("button", { name: "Confirm install" });
  expect(screen.getByText("Antigravity · 1.5.9")).toBeTruthy();
  expect(screen.getByText("C:/Users/Fixture/.gemini/config")).toBeTruthy();
  expect(invoke.mock.calls).toEqual([[
    "preview_native_user_plugin_action", { request: { provider: "antigravity", action: "install", target: folder } },
  ]]);

  fireEvent.click(confirmation);
  expect(await screen.findByText("Change confirmed in the refreshed inventory.")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("apply_native_user_plugin_action", { previewId: "agy-preview" });
  expect(client.getQueryData(["cli-plugin-inventory", "antigravity"])).toEqual(inventory);
});

it("selects an exact Codex local catalog entry and only offers native installation", async () => {
  const entry = { pluginId: "formatter@local", version: "2.1.0", source: "D:/Local/formatter" };
  invoke.mockResolvedValueOnce([entry]).mockResolvedValueOnce({
    ...entry,
    id: "codex-preview",
    action: "install",
    configurationRoot: "C:/Users/Fixture/.codex",
    executable: "C:/Tools/codex.exe",
    cliVersion: "0.169.0-alpha.11.1",
    executionScope: "user",
    persists: true,
    marketplace: "local",
  });
  const client = new QueryClient();
  render(<QueryClientProvider client={client}><UserPluginManagement provider="codex" disabled={false} /></QueryClientProvider>);

  fireEvent.click(screen.getByText("Manage user plugins"));
  expect(screen.queryByLabelText("Action")).toBeNull();
  fireEvent.change(screen.getByLabelText("Configured marketplace"), { target: { value: "local" } });
  fireEvent.click(screen.getByRole("button", { name: "Load catalog" }));
  await screen.findByRole("option", { name: "formatter@local · 2.1.0" });
  fireEvent.change(screen.getByLabelText("Plugin"), { target: { value: "formatter@local" } });
  fireEvent.click(screen.getByRole("button", { name: "Review change" }));
  await screen.findByRole("button", { name: "Confirm install" });
  expect(screen.getByText("D:/Local/formatter")).toBeTruthy();
  expect(invoke.mock.calls).toEqual([
    ["native_codex_plugin_catalog", { marketplace: "local" }],
    ["preview_native_user_plugin_action", { request: { provider: "codex", action: "install", target: "formatter@local" } }],
  ]);
});
