// @vitest-environment jsdom
import { invoke } from "@tauri-apps/api/core";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { downloadArtifact } from "@/download-artifact";
import { SavedGitDiffs } from "@/git-saved-diffs";
import { loadMonaco } from "@/ide-monaco";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/download-artifact", async (original) => ({
  ...await original<typeof import("@/download-artifact")>(), downloadArtifact: vi.fn(),
}));
vi.mock("@/ide-monaco", () => ({
  loadMonaco: vi.fn(() => Promise.reject(new Error("No Monaco in jsdom"))), monacoTheme: () => "vs-dark",
}));
vi.mock("@/git-diff-view", () => ({
  GitDiffFrame: ({ children }: { children: ReactNode }) => <>{children}</>,
  MonacoComparison: ({ file }: { file: { original: string; modified: string } }) => <>
    <pre aria-label="Base patch">{file.original}</pre><pre aria-label="Compared patch">{file.modified}</pre>
  </>,
}));

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.resetAllMocks(); });

it("copies and downloads the exact saved patch and compares it with the same part of another capture", async () => {
  const projectId = "project";
  const first = "older-run";
  const current = "current-run";
  const firstPatch = "diff --git a/file.txt b/file.txt\n-old\n+first\n";
  const currentPatch = "diff --git a/file.txt b/file.txt\r\n-old\r\n+changed\r\n";
  const attempts = [
    { runId: first, checkoutId: "original-worktree", label: "First attempt", conversationId: "original-conversation" },
    { runId: current, checkoutId: "current-worktree", label: "Current attempt", conversationId: "current-conversation" },
  ];
  vi.mocked(invoke).mockImplementation(async (command, payload) => {
    if (command === "git_saved_attempts") return attempts;
    if (command === "attempt_worktree_evidence") {
      const { runId } = payload as { runId: string };
      return { kind: "saved", artifact: {
        revision: `capture-${runId}`, start: { worktree: { branch: "main" }, starting: { head: "base-head" } },
        candidate: { head: `head-${runId}` }, committedPatchBase64: btoa(runId === first ? firstPatch : currentPatch),
      } };
    }
    throw new Error(command);
  });
  const activity = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}>
    <SavedGitDiffs projectId={projectId} target={`attempt:${current}`} openActivity={activity} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  const clipboard = vi.spyOn(navigator.clipboard, "writeText");
  const open = async (option: string) => {
    fireEvent.contextMenu(await screen.findByLabelText("Recorded diff source"));
    await user.click(await screen.findByRole("menuitem", { name: option }));
  };
  await open("Copy saved patch");
  await waitFor(() => expect(clipboard).toHaveBeenCalledWith(currentPatch));
  await open("Download saved patch");
  expect(downloadArtifact).toHaveBeenCalledWith(`${current}-committed.patch`, expect.objectContaining({ size: currentPatch.length, type: "text/x-diff" }));

  await open("Compare with another saved attempt…");
  const dialog = within(await screen.findByRole("dialog", { name: "Compare saved Git patches" }));
  expect((await dialog.findByLabelText("Base patch")).textContent).toBe(firstPatch);
  expect((await dialog.findByLabelText("Compared patch")).textContent).toBe(currentPatch);
  expect(dialog.getByText(`capture-${first}`)).toBeTruthy();
  expect(dialog.getByText(`capture-${current}`)).toBeTruthy();
  await user.click(dialog.getByRole("button", { name: "Open base attempt" }));
  expect(activity).toHaveBeenCalledWith(first, "original-conversation");
});

it.each(["Monaco", "fallback"])("reviews only the selected saved patch lines in %s with their captured worktree and revision", async (surface) => {
  const patch = "diff --git a/file.txt b/file.txt\r\n-previous line\r\n+selected line\n+untouched line\r\n";
  const excerpt = "previous line\r\n+selected";
  const projectId = "project";
  const origin = { projectId, repositoryRoot: "D:/root", checkoutId: "source-worktree", checkoutRoot: "D:/linked" };
  const active = { ...origin, checkoutId: "active-worktree", checkoutRoot: origin.repositoryRoot };
  const openCheckoutWorkspace = vi.fn(async () => {});
  const openWorkspace = vi.fn();
  let review: (() => void) | undefined;
  let revision = "capture-id";
  if (surface === "Monaco") {
    vi.mocked(loadMonaco).mockResolvedValue({ editor: {
      setTheme: () => {}, createModel: () => ({ dispose() {} }),
      create: () => ({ dispose() {}, onMouseDown() {}, onKeyDown() {}, onContextMenu() {},
        addAction: (action: { id: string; run: () => void }) => { if (action.id === "omniagent.review-saved-patch-selection") review = action.run; },
        getSelection: () => ({ startLineNumber: 2, startColumn: 2, endLineNumber: 3, endColumn: 10 }),
      }),
    } } as unknown as Awaited<ReturnType<typeof loadMonaco>>);
  }
  vi.mocked(invoke).mockImplementation(async (command, payload) => {
    switch (command) {
      case "git_saved_attempts": return [{ runId: "run-id", checkoutId: origin.checkoutId, label: "Saved attempt" }];
      case "attempt_worktree_evidence": return { kind: "saved", artifact: {
        revision, start: { worktree: { branch: "feature", checkoutId: origin.checkoutId }, starting: { head: "start-head" } },
        candidate: { head: "captured-head" }, committedPatchBase64: btoa(patch),
      } };
      case "registered_checkouts": return [{ ...origin, projectId: "foreign-project" }, active, origin];
      case "list_conversations": return (payload as { checkoutId: string }).checkoutId === origin.checkoutId
        ? [{ id: "review-conversation", title: "Review migration" }] : [];
      case "append_checkout_context_to_conversation": return;
      default: throw new Error(command);
    }
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}>
    <SavedGitDiffs projectId={projectId} openActivity={vi.fn()} workspace={{ checkout: active, openWorkspace, openCheckoutWorkspace }} />
  </QueryClientProvider>);
  const user = userEvent.setup();
  if (surface === "Monaco") {
    await waitFor(() => expect(review).toBeTypeOf("function"));
    act(() => review?.());
  } else {
    const pre = await screen.findByLabelText("Commits during attempt", { selector: "pre" });
    const start = patch.indexOf(excerpt);
    const selection = document.getSelection()!;
    const range = document.createRange();
    range.setStart(pre.firstChild!, start);
    range.setEnd(pre.firstChild!, start + excerpt.length);
    selection.removeAllRanges();
    selection.addRange(range);
    fireEvent.contextMenu(pre);
    await user.click(await screen.findByRole("menuitem", { name: "Review selected patch excerpt…" }));
    selection.removeAllRanges();
  }
  const dialog = within(await screen.findByRole("dialog", { name: "Add checkout context" }));
  expect(dialog.getByLabelText("Selected text").textContent).toBe(excerpt);
  expect(dialog.getByText(/patch lines 2–3 · Run run-id · Capture capture-id · Worktree source-worktree/)).toBeTruthy();
  await dialog.findByRole("option", { name: "Review migration" });
  await user.selectOptions(dialog.getByLabelText("Conversation"), "review-conversation");
  expect(invoke).not.toHaveBeenCalledWith("append_checkout_context_to_conversation", expect.anything());
  if (surface === "fallback") {
    revision = "changed-capture";
    await user.click(dialog.getByRole("button", { name: "Add to draft" }));
    expect((await dialog.findByRole("alert")).textContent).toContain("This saved patch changed or was removed");
    expect(invoke).not.toHaveBeenCalledWith("append_checkout_context_to_conversation", expect.anything());
    return;
  }
  await user.click(dialog.getByRole("button", { name: "Add to draft" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("append_checkout_context_to_conversation", {
    checkoutId: origin.checkoutId, conversationId: "review-conversation",
    context: expect.stringContaining("```diff\n" + excerpt + "\n```"),
  }));
  await waitFor(() => expect(openCheckoutWorkspace).toHaveBeenCalledExactlyOnceWith(origin, { workspace: "conversations", target: "review-conversation" }));
  expect(openWorkspace).not.toHaveBeenCalled();
});
