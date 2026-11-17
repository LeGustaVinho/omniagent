// @vitest-environment jsdom
import { invoke } from "@tauri-apps/api/core";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { downloadArtifact } from "@/download-artifact";
import { SavedGitDiffs } from "@/git-saved-diffs";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/download-artifact", async (original) => ({
  ...await original<typeof import("@/download-artifact")>(), downloadArtifact: vi.fn(),
}));
vi.mock("@/ide-monaco", () => ({
  loadMonaco: () => Promise.reject(new Error("No Monaco in jsdom")), monacoTheme: () => "vs-dark",
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
