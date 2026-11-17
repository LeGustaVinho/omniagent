// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { App } from "@/app";
import { useWorkspace } from "@/workspace-state";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@/settings-appearance", () => ({ useAppAppearance: () => undefined }));
vi.mock("@/workbench", () => ({ default: () => <p>Repository workspace</p> }));

function renderApp() {
  const query = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={query}><App /></QueryClientProvider>);
}

afterEach(() => {
  cleanup();
  useWorkspace.getState().selectCheckout(null);
  invoke.mockReset();
});

it("requires a registered checkout and surfaces a Git validation error", async () => {
  invoke.mockResolvedValueOnce(null).mockResolvedValueOnce("C:\\Temp\\Sem repositório").mockRejectedValueOnce("not a git repository");
  const user = userEvent.setup();
  renderApp();
  expect(screen.queryByRole("button", { name: "Start fixture" })).toBeNull();
  await user.click(await screen.findByRole("button", { name: "Open repository" }));
  expect((await screen.findByRole("alert")).textContent).toContain("not a git repository");
  expect(invoke.mock.calls).toEqual([
    ["active_checkout"],
    ["choose_repository"],
    ["register_project", { directory: "C:\\Temp\\Sem repositório" }],
  ]);
  expect(useWorkspace.getState().checkout).toBeNull();
});

it("registers the selected folder and activates the checkout returned by Git", async () => {
  const folder = "C:\\Temp\\Repositório\\docs";
  const root = "C:\\Temp\\Repositório";
  const checkout = { projectId: "project", repositoryRoot: root, checkoutId: "checkout", checkoutRoot: root };
  invoke.mockResolvedValueOnce(null).mockResolvedValueOnce(folder).mockResolvedValueOnce(checkout).mockResolvedValueOnce(undefined);
  renderApp();
  await userEvent.setup().click(await screen.findByRole("button", { name: "Open repository" }));
  expect(await screen.findByText("Repository workspace")).toBeTruthy();
  expect(invoke.mock.calls).toEqual([
    ["active_checkout"], ["choose_repository"], ["register_project", { directory: folder }],
    ["select_active_checkout", { checkoutId: "checkout" }],
  ]);
  expect(useWorkspace.getState().checkout).toEqual(checkout);
});


it("reopens a registered checkout from startup without registering it again", async () => {
  const root = "D:/Projects/Example";
  const checkout = { projectId: "project", repositoryRoot: root, checkoutId: "checkout", checkoutRoot: root };
  invoke.mockResolvedValueOnce(null).mockResolvedValueOnce([checkout]).mockResolvedValueOnce(undefined);
  const user = userEvent.setup();
  renderApp();
  await user.click(await screen.findByRole("button", { name: "Registered repositories" }));
  await user.click(await screen.findByRole("button", { name: /^Example/ }));
  expect(await screen.findByText("Repository workspace")).toBeTruthy();
  expect(invoke.mock.calls).toEqual([
    ["active_checkout"], ["registered_checkouts"], ["select_active_checkout", { checkoutId: "checkout" }],
  ]);
});
