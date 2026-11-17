// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { PluginInventory } from "@/bindings/PluginInventory";
import { ProviderInventory } from "@/provider-inventory";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("searches one service's models, partial plugins and checkout skills while keeping qualifications scoped", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "provider_model_inventory") return {
      executable: "C:/bin/agy.exe", version: "1.2.3",
      models: [{ option: { name: "Gemini example", selection: { modelId: "gemini-example", profileId: "desktop-direct:antigravity" },
        selectable: false, unavailableReason: "Unavailable in this CLI session" },
      inputModalities: ["text", "image"], outputModalities: null, efforts: [] }],
      transportReport: { protocol: "ACP fixture", capabilities: ["native_plan_mode"], promptFormats: { image: true, audio: false, embeddedContext: null },
        sessionModel: null, commands: null, tools: [] },
    };
    if (command === "provider_skill_inventory") return { checkout: "C:/checkout",
      catalog: { entries: [{ id: "skill-1", name: "Review source", source: "filesystem", path: "C:/checkout/.agents/skills/review/SKILL.md",
        scope: "repo", enabled: null, revision: "1234567890123456" }], issues: [] } };
    if (command === "review_checkout_customizations") return { workingDirectory: "C:/checkout", entries: [], instructions: [], unresolvedSources: [] };
    if (command === "provider_capability_qualifications") return { records: [], nextCursor: null };
    throw new Error(`Unexpected command ${command}`);
  });
  const inventory: PluginInventory = { id: "partial-report", checkedAt: "2026-04-01T12:00:00Z", report: {
    provider: "antigravity", executable: "C:/bin/agy.exe", version: "1.2.3", status: "partial", loadTelemetryReported: false,
    plugins: [{ id: "sample", name: "Sample extension", version: "2.1.0", enabled: false, scope: "user", marketplace: null,
      source: "local", sourceLocation: "C:/plugins/sample", installPath: null, skills: ["Analyze output"], hooks: [], mcpServers: null }],
  } };
  const refreshPlugins = vi.fn(async () => {});
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><ProviderInventory provider="antigravity" checkoutId="checkout"
    inventory={inventory} refreshPlugins={refreshPlugins} pluginsRefreshing={false} /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(await screen.findByText("Gemini example"));
  expect(screen.getByText("text, image").closest("details")?.open).toBe(true);
  expect(screen.getByText("Unavailable in this CLI session")).toBeTruthy();
  await user.click(screen.getByText("Qualification matrix"));
  await screen.findByText("No submitted dispatch profiles have been recorded.");
  expect(invoke).toHaveBeenCalledWith("provider_capability_qualifications", { cursor: null, provider: "antigravity", checkoutId: "checkout" });
  await user.click(screen.getByRole("button", { name: "Refresh inventory" }));
  expect(refreshPlugins).toHaveBeenCalledOnce();
  expect(invoke).toHaveBeenCalledWith("provider_skill_inventory", { provider: "antigravity", checkoutId: "checkout" });

  const search = screen.getByRole("searchbox", { name: "Search provider inventory" });
  await user.type(search, "analyze disabled");
  await waitFor(() => expect(screen.getByText("Sample extension").closest("details")?.open).toBe(true));
  expect(screen.getByText(/Partial inventory/)).toBeTruthy();
  expect(screen.getByText("Disabled")).toBeTruthy();
  expect(screen.queryByText("Gemini example")).toBeNull();
  await user.clear(search);
  await user.type(search, "review source");
  expect(screen.getByText("Review source")).toBeTruthy();
  expect(screen.getByText("Filesystem · repo · 123456789012")).toBeTruthy();
  expect(screen.queryByText("Sample extension")).toBeNull();
});
