// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { QueueWriterReconciliation } from "@/queue-writer-reconciliation";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("requires a restored checkpoint and explicit review before confirming an interrupted write", async () => {
  const scope = { projectId: "project", jobId: "job", runId: "interrupted-run" };
  const receipt = { sourceRunId: scope.runId, reviewRevision: "restored-revision", reviewedAt: "2026-11-16" };
  let restored = false;
  let recorded = false;
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "interrupted_writer_review") return {
      sourceRunId: scope.runId, checkoutId: "checkout", revision: restored ? "restored-revision" : "dirty-revision",
      startingHead: "start-sha", observedHead: "start-sha", observedStatus: restored ? [] : [" M main.rs"],
      indexRestored: true, worktreeRestored: restored, launchRequested: true,
      dispatches: [{ id: "saved-dispatch", ordinal: 1, delivery: "uncertain" }], receipt: recorded ? receipt : null,
    };
    if (command === "reconcile_interrupted_writer") {
      recorded = true;
      return receipt;
    }
    throw new Error(command);
  });
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <QueueWriterReconciliation {...scope} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  await user.click(screen.getByText("Review interrupted write"));
  expect(await screen.findByText("Differs from checkpoint")).toBeTruthy();
  expect(screen.getByText("Prompt 1 · uncertain")).toBeTruthy();
  const confirm = screen.getByRole<HTMLButtonElement>("button", { name: "Confirm reconciliation" });
  expect(confirm.disabled).toBe(true);
  restored = true;
  await user.click(screen.getByRole("button", { name: "Refresh worktree evidence" }));
  expect(await screen.findAllByText("Matches checkpoint")).toHaveLength(2);
  expect(confirm.disabled).toBe(true);
  await user.click(screen.getByRole("checkbox", { name: /reconciled its effects outside this worktree/ }));
  await user.click(confirm);
  expect(await screen.findByText(/Write reconciliation recorded/)).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("reconcile_interrupted_writer", {
    ...scope, expectedRevision: "restored-revision", externalEffectsReconciled: true,
  });
  expect(vi.mocked(invoke).mock.calls.every(([command]) =>
    command === "interrupted_writer_review" || command === "reconcile_interrupted_writer")).toBe(true);
});
