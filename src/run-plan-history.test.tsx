// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { RunPlanHistory } from "@/run-plan-history";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const goToDiff = vi.hoisted(() => vi.fn());
vi.mock("@/git-diff-view", async () => {
  const { useEffect } = await import("react");
  return { MonacoComparison: ({ file, layout, ready }: { file: { original: string; modified: string }; layout: string;
    ready?: (instance: { goToDiff: typeof goToDiff } | null) => void }) => {
    useEffect(() => { ready?.({ goToDiff }); return () => ready?.(null); }, [ready]);
    return <div data-layout={layout}><pre aria-label="Original text">{file.original}</pre><pre aria-label="Modified text">{file.modified}</pre></div>;
  } };
});

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

function queryClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
}

it("opens frozen plan versions, revision feedback, and the prior release in the run inspector", async () => {
  const projectId = "project";
  const runId = "revision-attempt";
  const openActivity = vi.fn();
  vi.mocked(invoke).mockResolvedValue({
    request: { baseRevisionId: "version-one", feedback: "Require a smoke test" },
    versions: [
      { revisionId: "version-one", sourceRunId: "original-planner", eventSequence: "26", text: "Original frozen plan",
        current: false, approvalArtifactId: "approval-one", releaseId: "release-one", releaseRevoked: true },
      { revisionId: "version-two", sourceRunId: runId, eventSequence: "53", text: "Revised frozen plan",
        current: true, approvalArtifactId: null, releaseId: null, releaseRevoked: null },
    ],
  });
  render(<QueryClientProvider client={queryClient()}><RunPlanHistory {...{ projectId, runId, openActivity }} /></QueryClientProvider>);

  expect(invoke).not.toHaveBeenCalled();
  const user = userEvent.setup();
  await user.click(screen.getByText("Plan revision history"));
  expect(await screen.findByText("Require a smoke test")).toBeTruthy();
  const original = await screen.findByText("Proposal 26");
  const revised = screen.getByText("Proposal 53 · Selected plan");
  fireEvent.contextMenu(original);
  await user.click(screen.getByRole("menuitem", { name: "Copy plan text" }));
  expect(await navigator.clipboard.readText()).toBe("Original frozen plan");
  await user.click(original);
  expect(screen.getByText("Original frozen plan")).toBeTruthy();
  expect(screen.getByText("approval-one")).toBeTruthy();
  expect(screen.getByText("Revoked release")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Open plan attempt" }));
  expect(openActivity).toHaveBeenCalledWith("original-planner");
  await user.click(revised);
  expect(screen.getByText("Revised frozen plan")).toBeTruthy();
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("repository_plan_history", { projectId, runId }));

  fireEvent.contextMenu(revised);
  await user.click(screen.getByRole("menuitem", { name: "Compare saved plans…" }));
  const comparison = within(await screen.findByRole("dialog", { name: "Compare saved plans" }));
  expect((await comparison.findByLabelText("Original text")).textContent).toBe("Original frozen plan");
  expect(comparison.getByLabelText("Modified text").textContent).toBe("Revised frozen plan");
  expect(within(comparison.getByRole("region", { name: "Base attempt provenance" })).getByText("version-one")).toBeTruthy();
  expect(within(comparison.getByRole("region", { name: "Compared attempt provenance" })).getByText("version-two")).toBeTruthy();
  await user.click(comparison.getByRole("button", { name: "Next change" }));
  await user.click(comparison.getByRole("button", { name: "Previous change" }));
  expect(goToDiff.mock.calls).toEqual([["next"], ["previous"]]);
  await user.selectOptions(comparison.getByRole("combobox", { name: "Diff layout" }), "unified");
  expect(comparison.getByLabelText("Original text").parentElement?.dataset.layout).toBe("unified");
  await user.click(comparison.getByRole("button", { name: "Swap attempts" }));
  expect((await comparison.findByLabelText("Original text")).textContent).toBe("Revised frozen plan");
  await user.click(comparison.getByRole("button", { name: "Open compared attempt" }));
  expect(openActivity).toHaveBeenLastCalledWith("original-planner");
});

it("prepares feedback about only the selected lines of a saved plan revision", async () => {
  const projectId = "project";
  const runId = "current-attempt";
  const sourceRunId = "prior-attempt";
  const checkoutId = "linked-worktree";
  const text = "Keep earlier text private\r\nReview this exact step.\r\nRun the current fixture.\r\nUnselected final text.";
  const excerpt = "Review this exact step.\r\nRun the current fixture.";
  const open = vi.fn();
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "repository_plan_history") return { request: null, versions: [{ revisionId: "old-plan", sourceRunId, eventSequence: "17", text }] };
    if (command === "list_conversations") return [{ id: "target-chat", title: "Plan review" }];
    if (command === "append_checkout_context_to_conversation") return;
    throw new Error(command);
  });
  const user = userEvent.setup();
  render(<QueryClientProvider client={queryClient()}>
    <RunPlanHistory projectId={projectId} runId={runId} openActivity={vi.fn()} draftDestination={{ checkoutId, open }} />
  </QueryClientProvider>);
  await user.click(screen.getByText("Plan revision history"));
  await user.click(await screen.findByText("Proposal 17"));
  const pre = screen.getByText(/Keep earlier text private/, { selector: "pre" });
  const selection = document.getSelection()!;
  const range = document.createRange();
  const start = text.indexOf(excerpt);
  range.setStart(pre.firstChild!, start);
  range.setEnd(pre.firstChild!, start + excerpt.length);
  selection.removeAllRanges();
  selection.addRange(range);
  fireEvent.contextMenu(pre);
  await user.click(await screen.findByRole("menuitem", { name: "Review selected plan excerpt…" }));
  selection.removeAllRanges();
  const dialog = within(await screen.findByRole("dialog", { name: "Add checkout context" }));
  expect(dialog.getByText("Plan revision old-plan · Run prior-attempt · Proposal 17 · Lines 2–3")).toBeTruthy();
  expect(dialog.getByLabelText("Selected text").textContent).toBe(excerpt);
  await user.selectOptions(dialog.getByLabelText("Conversation"), "target-chat");
  await user.click(dialog.getByRole("button", { name: "Add to draft" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("append_checkout_context_to_conversation", {
    checkoutId, conversationId: "target-chat", context: expect.stringContaining(`\n\n\`\`\`markdown\n${excerpt}\n\`\`\``),
  }));
  expect(open).toHaveBeenCalledWith("target-chat");
});
