// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { CheckoutSelector } from "@/checkout-selector";
import { readTerminalFolderTarget, terminalFolderTarget } from "@/file-context-actions";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); localStorage.clear(); });

it("opens the clicked worktree folder from the chooser without switching projects", async () => {
  const clicked = { ...exampleCheckout, checkoutId: "linked-checkout", checkoutRoot: "C:\\worktrees\\review" };
  vi.mocked(invoke).mockImplementation(async (command) => command === "registered_checkouts" ? [exampleCheckout, clicked] : undefined);
  const onSelect = vi.fn(async () => undefined);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(<QueryClientProvider client={client}><CheckoutSelector checkout={exampleCheckout} onSelect={onSelect} /></QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Switch repository or worktree" }));
  const chooser = within(await screen.findByRole("dialog", { name: "Registered checkouts" }));
  fireEvent.contextMenu(await chooser.findByText(clicked.checkoutRoot));
  await user.click(await screen.findByRole("menuitem", { name: "Show in File Explorer" }));
  expect(invoke).toHaveBeenCalledWith("open_checkout_item_in_shell", { checkoutId: clicked.checkoutId, relativePath: "", action: "reveal" });
  expect(onSelect).not.toHaveBeenCalled();
});

it("opens the scoped terminal in the clicked checkout and gives repeated requests distinct targets", async () => {
  const clicked = { ...exampleCheckout, checkoutId: "linked-checkout", checkoutRoot: "C:\\worktrees\\review" };
  vi.mocked(invoke).mockResolvedValue([exampleCheckout, clicked]);
  const onSelect = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><CheckoutSelector checkout={exampleCheckout} onSelect={onSelect} /></QueryClientProvider>);
  const user = userEvent.setup();
  for (let attempt = 0; attempt < 2; attempt += 1) {
    await user.click(screen.getByRole("button", { name: "Switch repository or worktree" }));
    fireEvent.contextMenu(await screen.findByText(clicked.checkoutRoot));
    await user.click(await screen.findByRole("menuitem", { name: "Open terminal here" }));
  }
  const routes = onSelect.mock.calls.map(([checkout, destination]) => [checkout, destination.workspace, readTerminalFolderTarget(destination.target)]);
  expect(routes).toEqual([[clicked, "ide", ""], [clicked, "ide", ""]]);
  expect(onSelect.mock.calls[0]![1].target).not.toBe(onSelect.mock.calls[1]![1].target);
  expect(readTerminalFolderTarget(terminalFolderTarget("docs/[drafts]"))).toBe("docs/[drafts]");
  expect(readTerminalFolderTarget("omni-terminal-folder:[drafts]")).toBe("[drafts]");
});

it("favorites the exact project from a linked worktree and keeps it first across mounts", async () => {
  const primary = { ...exampleCheckout, repositoryRoot: "C:/Projects/Primary", checkoutRoot: "C:/Projects/Primary" };
  const other = { projectId: "other-project", repositoryRoot: "C:/Projects/Other", checkoutId: "other-root", checkoutRoot: "C:/Projects/Other" };
  const linked = { ...other, checkoutId: "other-linked", checkoutRoot: "C:/Worktrees/Other linked" };
  vi.mocked(invoke).mockResolvedValue([primary, other, linked]);
  const onSelect = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const chooser = () => <QueryClientProvider client={client}><CheckoutSelector checkout={primary} onSelect={onSelect} /></QueryClientProvider>;
  const user = userEvent.setup();
  const open = async () => {
    await user.click(screen.getByRole("button", { name: "Switch repository or worktree" }));
    const dialog = within(await screen.findByRole("dialog", { name: "Registered checkouts" }));
    const rows = await dialog.findAllByRole("listitem");
    return rows.map((row) => within(row).getByRole("button", { name: /^(Primary|Other) / }).getAttribute("title"));
  };
  const original = render(chooser());
  expect(await open()).toEqual([primary.checkoutRoot, other.checkoutRoot, linked.checkoutRoot]);
  fireEvent.contextMenu(screen.getByText(linked.checkoutRoot));
  await user.click(await screen.findByRole("menuitem", { name: "Favorite project" }));
  expect(await screen.findByText("Added Other to project favorites.")).toBeTruthy();
  expect(onSelect).not.toHaveBeenCalled();
  original.unmount();
  render(chooser());
  expect(await open()).toEqual([other.checkoutRoot, linked.checkoutRoot, primary.checkoutRoot]);
  fireEvent.contextMenu(screen.getByText(linked.checkoutRoot));
  await user.click(await screen.findByRole("menuitem", { name: "Remove project from favorites" }));
  expect(await screen.findByText("Removed Other from project favorites.")).toBeTruthy();
});
