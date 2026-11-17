// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { CheckoutSelector } from "@/checkout-selector";
import { exampleCheckout } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

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
