// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { GitCheckoutChanges, type CheckoutReview } from "@/git-checkout-changes";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/ide-monaco", () => ({
  loadMonaco: () => Promise.reject(new Error("No Monaco worker in jsdom")), monacoTheme: () => "vs-dark",
}));

afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("previews only the clicked hunk or inactive file and applies the confirmed snapshot", async () => {
  const active = "src/first.txt";
  const clicked = "src/deleted.txt";
  const checkoutId = "scope";
  const revision = { branch: "refs/heads/main", head: "head", index: "index" };
  const review: CheckoutReview = { checkoutId, root: "C:/temp/checkout", revision, managed: false, branches: [], worktrees: [],
    changes: [{ path: active, working: "M", staged: "M" }, { path: clicked, working: "D", staged: null }] };
  const hunkId = "reviewed-hunk";
  const hunk = "@@ -1,1 +1,1 @@";
  const text = (content: string) => ({ status: "text", content });
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "git_checkout_diff") return { path: active, revision, original: text("base\n"), modified: text("edit\n"),
      hunks: [{ id: hunkId, header: hunk, originalLine: 1, modifiedLine: 1, patch: `diff --git a/first b/first\n${hunk}\n-base\n+edit\n` }] };
    if (command === "git_review_comments") return [];
    if (command === "git_discard_preview") return (args as { path?: string } | undefined)?.path === active
      ? { path: active, snapshot: "hunk-snapshot", hunk, before: text("edit\n"), after: text("base\n") }
      : { path: clicked, snapshot: "file-snapshot", hunk: null, before: { status: "missing" }, after: text("staged contents\n") };
    if (command === "git_discard_changes") return;
    throw new Error(`Unexpected ${command}`);
  });
  const user = userEvent.setup();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><GitCheckoutChanges review={review} indexActions /></QueryClientProvider>);
  await user.click(await screen.findByRole("button", { name: "Review hunks" }));
  fireEvent.contextMenu(await screen.findByRole("button", { name: hunk }));
  await user.click(await screen.findByRole("menuitem", { name: "Discard hunk…" }));
  const discard = within(await screen.findByRole("dialog", { name: "Discard hunk" }));
  expect(await discard.findByLabelText("Discard preview")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("git_discard_preview", { checkoutId, path: active, hunkId });
  expect(invoke).not.toHaveBeenCalledWith("git_discard_changes", expect.anything());
  await user.click(discard.getByRole("button", { name: "Cancel" }));

  fireEvent.contextMenu(within(screen.getByRole("region", { name: "Changed files" })).getByRole("button", { name: `${clicked} · Deleted` }));
  await user.click(await screen.findByRole("menuitem", { name: "Restore deleted file…" }));
  const restore = within(await screen.findByRole("dialog", { name: "Restore deleted file" }));
  expect(invoke).toHaveBeenCalledWith("git_discard_preview", { checkoutId, path: clicked, hunkId: null });
  await user.click(await restore.findByRole("button", { name: "Restore reviewed file" }));
  expect(invoke).toHaveBeenCalledWith("git_discard_changes", { checkoutId, path: clicked, hunkId: null, snapshot: "file-snapshot" });
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});
