// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { ClaudePluginManagement } from "@/cli-plugin-management";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

beforeEach(() => invoke.mockReset());

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
  render(<QueryClientProvider client={client}><ClaudePluginManagement disabled={false} /></QueryClientProvider>);

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
