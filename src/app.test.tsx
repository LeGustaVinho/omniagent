// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { App } from "@/app";
import { useWorkspace } from "@/workspace-state";

const invoke = vi.hoisted(() => vi.fn());
const isTauri = vi.hoisted(() => vi.fn());
const listen = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke, isTauri }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
vi.mock("@/settings-appearance", () => ({ useAppAppearance: () => undefined }));
vi.mock("@/workbench", () => ({ default: ({ fileTarget }: { fileTarget?: { path: string; line: number | null } }) =>
  <><p>Repository workspace</p>{fileTarget && <p>{fileTarget.path}:{fileTarget.line}</p>}</> }));

function renderApp() {
  const query = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={query}><App /></QueryClientProvider>);
}

afterEach(() => {
  cleanup();
  useWorkspace.getState().selectCheckout(null);
  invoke.mockReset();
  isTauri.mockReset();
  listen.mockReset();
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

it("opens startup and forwarded file links in their registered checkout without writing the selection", async () => {
  const root = "D:/Projects/Example";
  const previous = { projectId: "project", repositoryRoot: root, checkoutId: "checkout", checkoutRoot: root };
  const linked = { ...previous, checkoutId: "linked", checkoutRoot: `${root}-linked` };
  let notify: (() => void) | undefined;
  isTauri.mockReturnValue(true);
  listen.mockImplementation(async (_name, callback) => { notify = callback; return () => {}; });
  invoke.mockResolvedValueOnce(previous)
    .mockResolvedValueOnce({ checkout: linked, path: "docs/note.md", line: 4, column: null })
    .mockResolvedValueOnce({ checkout: previous, path: "src/index.ts", line: 7, column: 3 });
  renderApp();
  expect(await screen.findByText("docs/note.md:4")).toBeTruthy();
  expect(useWorkspace.getState().checkout).toEqual(linked);
  await act(async () => notify?.());
  expect(await screen.findByText("src/index.ts:7")).toBeTruthy();
  expect(useWorkspace.getState().checkout).toEqual(previous);
  expect(invoke.mock.calls).toEqual([["active_checkout"], ["take_pending_file_link"], ["take_pending_file_link"]]);
});
