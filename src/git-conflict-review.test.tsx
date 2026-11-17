// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { GitConflictReview } from "@/git-conflict-review";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it.each(["local", "incoming"] as const)("previews the %s deletion and stages only the confirmed absent result", async (side) => {
  const checkoutId = "checkout";
  const path = "dir/removed [literal].txt";
  const observedText = "exact working α\r\n";
  let version = 1;
  let working: string | undefined = observedText;
  const snapshot = () => `review-${version}`;
  const stage = { objectId: "existing-blob", mode: "100644", file: { status: "text", content: "Recorded text" } };
  const close = vi.fn();
  const openFile = vi.fn();
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "git_conflict_review") return {
      path, revision: { branch: "main", head: "head", index: "unchanged-conflict-index" },
      snapshot: snapshot(), deletedSide: side, resolutionBlocked: null, proposalBlocked: null,
      base: stage, local: side === "local" ? null : stage, incoming: side === "incoming" ? null : stage,
      working: working === undefined ? { status: "missing" } : { status: "text", content: working },
    };
    if (command === "git_apply_conflict_resolution") { working = undefined; version++; return; }
    if (command === "git_mark_conflict_resolved") return;
    throw new Error(command);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><GitConflictReview {...{ checkoutId, checkoutRoot: "D:\\repo", path, close, openFile }} /></QueryClientProvider>);
  const review = within(await screen.findByRole("dialog", { name: "Review Git conflict" }));
  const user = userEvent.setup();
  const requestedSide = side === "local" ? "Local · stage 2" : "Incoming · stage 3";
  await user.click(await review.findByRole("button", { name: requestedSide }));
  fireEvent.contextMenu(review.getByText("This side deleted the file."));
  await user.click(screen.getByRole("menuitem", { name: `Use ${side} deletion…` }));
  expect(review.getByLabelText("File contents to remove").textContent).toBe(observedText);
  const remove = review.getByRole("button", { name: "Remove working file" });
  expect(remove).toHaveProperty("disabled", true);

  // A refresh after an external write must invalidate the exact removal preview.
  working = "Externally updated working file\n";
  version++;
  await user.click(review.getByRole("button", { name: "Refresh review" }));
  expect(await review.findByText("The conflict changed. Review the current versions before removing the working file.")).toBeTruthy();
  expect(remove).toHaveProperty("disabled", true);
  expect(invoke).not.toHaveBeenCalledWith("git_apply_conflict_resolution", expect.anything());
  await user.click(review.getByRole("button", { name: "Cancel proposal" }));
  await user.click(review.getByRole("button", { name: `Use ${side} deletion…` }));
  expect(review.getByLabelText("File contents to remove").textContent).toBe(working);
  await user.click(review.getByRole("checkbox", { name: "I reviewed the working contents and want to move this file to the Recycle Bin." }));
  await user.click(review.getByRole("button", { name: "Remove working file" }));
  expect(await review.findByText(/Working file moved to the Recycle Bin/)).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("git_apply_conflict_resolution", { checkoutId, path, expected: "review-2", proposal: { kind: "deletion", side } });
  expect(invoke).not.toHaveBeenCalledWith("git_mark_conflict_resolved", expect.anything());
  expect(review.queryByRole("button", { name: "Open working result in editor" })).toBeNull();
  const stageDeletion = review.getByRole("button", { name: "Stage reviewed deletion" });
  expect(stageDeletion).toHaveProperty("disabled", true);
  await user.click(review.getByRole("checkbox", { name: "I reviewed the absent working file and want to stage its deletion." }));
  await user.click(stageDeletion);
  await waitFor(() => expect(close).toHaveBeenCalledTimes(1));
  expect(invoke).toHaveBeenCalledWith("git_mark_conflict_resolved", { checkoutId, path, expected: "review-3" });
});
