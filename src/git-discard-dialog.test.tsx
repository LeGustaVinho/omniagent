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

it("reviews and recycles the clicked untracked file without using the active diff path", async () => {
  const checkoutId = "selected-worktree";
  const active = "tracked.txt";
  const clicked = "notes/[temporary].bin";
  const revision = { branch: "refs/heads/main", head: "head", index: "index" };
  const review: CheckoutReview = { checkoutId, root: "C:/temp/checkout", revision, managed: false, branches: [], worktrees: [],
    changes: [{ path: active, working: "M", staged: "M" }, { path: clicked, working: "?", staged: null }] };
  const args = { checkoutId, path: clicked, hunkId: null, untracked: true };
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    if (command === "git_checkout_diff") return { path: active, revision, original: { status: "text", content: "base" },
      modified: { status: "text", content: "edited" }, hunks: [] };
    if (command === "git_review_comments") return [];
    if (command === "git_discard_preview") {
      expect(input).toEqual(args);
      return { path: clicked, snapshot: "untracked-snapshot", hunk: null,
        before: { status: "bytes", size: 4, sha256: "abc123", reason: "The file is not UTF-8 text." }, after: { status: "missing" } };
    }
    if (command === "git_discard_changes") { expect(input).toEqual({ ...args, snapshot: "untracked-snapshot" }); return; }
    throw new Error("Unexpected " + command);
  });
  const updated = vi.fn();
  window.addEventListener("omniagent:checkout-items-changed", updated);
  try {
    const user = userEvent.setup();
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(<QueryClientProvider client={client}><GitCheckoutChanges review={review} indexActions /></QueryClientProvider>);
    const files = within(screen.getByRole("region", { name: "Changed files" }));
    fireEvent.contextMenu(files.getByRole("button", { name: /\[temporary\]\.bin\s*Untracked/ }));
    await user.click(await screen.findByRole("menuitem", { name: "Move untracked file to Recycle Bin…" }));
    const dialog = within(await screen.findByRole("dialog", { name: "Move untracked file to Recycle Bin" }));
    expect(await dialog.findByText("SHA-256 abc123")).toBeTruthy();
    expect(dialog.getByText("File absent")).toBeTruthy();
    expect(invoke).not.toHaveBeenCalledWith("git_discard_changes", expect.anything());
    await user.click(dialog.getByRole("button", { name: "Move reviewed file to Recycle Bin" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(invoke).toHaveBeenCalledWith("git_discard_changes", { ...args, snapshot: "untracked-snapshot" });
    expect(updated).toHaveBeenCalledWith(expect.objectContaining({
      detail: expect.objectContaining({ checkoutId, operation: "recycle", completed: [{ source: clicked, directory: false, destination: null }] }),
    }));
    expect(screen.getByText("Moved " + clicked + " to the Recycle Bin.")).toBeTruthy();
  } finally {
    window.removeEventListener("omniagent:checkout-items-changed", updated);
  }
});
