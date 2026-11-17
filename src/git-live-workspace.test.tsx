// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import ChangesPanel from "@/changes-panel";
import GitWorkspace from "@/git-workspace";
import { exampleCheckout, type WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/ide-monaco", () => ({
  loadMonaco: () => Promise.reject(new Error("No Monaco worker in jsdom")),
  monacoTheme: () => "vs-dark",
}));

const checkout = { ...exampleCheckout, checkoutId: "local-checkout", checkoutRoot: "C:/local" };
const originalRevision = { branch: "refs/heads/main", head: "current-main", index: "old-index" };
const worktrees = [
  { checkoutId: checkout.checkoutId, root: checkout.checkoutRoot, branch: "main", head: "current-main", managed: false },
  { checkoutId: "linked-task", root: "C:/linked", branch: "feature", head: "current-feature", managed: true },
  { checkoutId: null, root: "C:/external", branch: "external", head: "external-head", managed: false },
];
const branches = [
  { name: "main", reference: "refs/heads/main", sha: "current-main" },
  { name: "feature", reference: "refs/heads/feature", sha: "current-feature" },
];

function openGit(overrides: Partial<WorkspaceProps> = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const openActivity = vi.fn();
  render(<QueryClientProvider client={client}><GitWorkspace
    checkout={checkout}
    simulation={null}
    startSimulation={() => {}}
    openWorkspace={() => {}}
    openChanges={() => {}}
    openActivity={openActivity}
    {...overrides}
  /></QueryClientProvider>);
  return { openActivity };
}

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

it("compares live paths and sends the current index, branch and HEAD for each staging change", async () => {
  let staged = false;
  vi.mocked(invoke).mockImplementation(async (command, payload) => {
    const args = payload as Record<string, unknown> | undefined;
    if (command === "git_checkout_review") {
      const isLinked = args?.checkoutId === "linked-task";
      return {
        checkoutId: args?.checkoutId, worktrees, branches,
        root: isLinked ? "C:/linked" : checkout.checkoutRoot, managed: isLinked,
        revision: isLinked ? { branch: "refs/heads/feature", head: "current-feature", index: "linked-index" }
          : { ...originalRevision, index: staged ? "new-index" : originalRevision.index },
        changes: isLinked ? [{ path: "linked.txt", working: "?", staged: null }]
          : [{ path: "main.txt", working: staged ? null : "M", staged: staged ? "M" : null }],
      };
    }
    if (command === "git_checkout_diff") return {
      path: args?.path, source: args?.source,
      original: { status: "text", content: "baseline" },
      modified: { status: "text", content: args?.source === "staged" ? "staged revision" : "working revision" },
    };
    if (command === "set_git_path_staged") { staged = Boolean(args?.staged); return; }
    if (command === "git_checkout_history") return [{
      sha: "current-feature", author: "Person", committedAt: "2026-11-16T12:00:00Z", subject: "Linked history",
    }];
    throw new Error(`Unexpected ${command}`);
  });
  const user = userEvent.setup();
  openGit();
  expect(within(await screen.findByRole("region", { name: "Revised file" })).getByText("working revision")).toBeDefined();
  await user.click(screen.getByRole("button", { name: "Stage main.txt" }));
  expect(invoke).toHaveBeenCalledWith("set_git_path_staged", { checkoutId: checkout.checkoutId, path: "main.txt", staged: true, expected: originalRevision });
  await screen.findByText("No working changes.");
  await user.selectOptions(screen.getByLabelText("Diff source"), "staged");
  expect(within(await screen.findByRole("region", { name: "Revised file" })).getByText("staged revision")).toBeDefined();
  await user.click(screen.getByRole("button", { name: "Unstage main.txt" }));
  expect(invoke).toHaveBeenCalledWith("set_git_path_staged", {
    checkoutId: checkout.checkoutId, path: "main.txt", staged: false, expected: { ...originalRevision, index: "new-index" },
  });
  await user.click(screen.getByRole("button", { name: "Worktrees" }));
  const table = screen.getByRole("table", { name: "Git worktrees" });
  const external = within(table).getByRole("row", { name: /C:\/external/ });
  expect((within(external).getByRole("button") as HTMLButtonElement).disabled).toBe(true);
  const linked = within(table).getByRole("row", { name: /C:\/linked/ });
  await user.click(within(linked).getByRole("button", { name: "Use worktree" }));
  await user.click(screen.getByRole("button", { name: "History" }));
  await user.selectOptions(await screen.findByLabelText("History branch"), "refs/heads/feature");
  await screen.findByText("Linked history");
  expect(invoke).toHaveBeenCalledWith("git_checkout_history", { checkoutId: "linked-task", branch: "refs/heads/feature" });
  await user.click(screen.getByRole("button", { name: "Changes" }));
  await within(screen.getByRole("region", { name: "Changed files" })).findByRole("button", { name: /linked\.txt.*Untracked/ });
  expect(screen.queryByRole("button", { name: "Open file" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Open changes panel" })).toBeNull();
});

it("loads the active checkout's diff in the Changes side panel and opens its real file", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "git_checkout_review") return {
      checkoutId: checkout.checkoutId, worktrees, branches, root: checkout.checkoutRoot, managed: false,
      revision: originalRevision, changes: [{ path: "notes/real.md", working: "M", staged: null }],
    };
    if (command === "git_checkout_diff") return {
      path: "notes/real.md", source: "working", original: { status: "text", content: "on the index" },
      modified: { status: "text", content: "on the real worktree" },
    };
    throw new Error(`Unexpected ${command}`);
  });
  const user = userEvent.setup();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const openWorkspace = vi.fn();
  render(<QueryClientProvider client={client}><ChangesPanel
    checkout={checkout} simulation={null} target="working" startSimulation={() => {}}
    openWorkspace={openWorkspace} openChanges={() => {}} openActivity={() => {}}
  /></QueryClientProvider>);
  expect(await screen.findByText("on the real worktree")).toBeDefined();
  expect(screen.getByLabelText("Comparison revisions").textContent).toBe("Index→Worktree");
  expect(invoke).toHaveBeenCalledWith("git_checkout_review", { checkoutId: checkout.checkoutId });
  expect(invoke).toHaveBeenCalledWith("git_checkout_diff", { checkoutId: checkout.checkoutId, path: "notes/real.md", source: "working" });
  expect(screen.queryByRole("button", { name: "Stage notes/real.md" })).toBeNull();
  expect(screen.queryByText("docs/release.md")).toBeNull();
  await user.click(screen.getByRole("button", { name: "Open file" }));
  expect(openWorkspace).toHaveBeenCalledWith("knowledge", "notes/real.md");
});

it("reports unavailable saved evidence in the side panel for the exact requested attempt", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "git_saved_attempts") return [{ runId: "newer-run", checkoutId: checkout.checkoutId, label: "Later attempt" }];
    if (command === "attempt_worktree_evidence") return null;
    throw new Error(`Unexpected ${command}`);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><ChangesPanel
    checkout={checkout} simulation={null} target="attempt:unavailable-run" startSimulation={() => {}}
    openWorkspace={() => {}} openChanges={() => {}} openActivity={() => {}}
  /></QueryClientProvider>);
  expect(await screen.findByText("Saved Git evidence is unavailable for this attempt.")).toBeDefined();
  expect(invoke).toHaveBeenCalledWith("attempt_worktree_evidence", { projectId: checkout.projectId, runId: "unavailable-run" });
  expect(invoke).not.toHaveBeenCalledWith("attempt_worktree_evidence", { projectId: checkout.projectId, runId: "newer-run" });
  expect(invoke).not.toHaveBeenCalledWith("git_checkout_review", expect.anything());
  expect(screen.queryByRole("button", { name: "View attempt" })).toBeNull();
});

it("opens the requested attempt's saved diff without loading live checkout content", async () => {
  const savedPatch = "@@ -1 +1 @@\n-before the attempt\n+captured candidate\n";
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "git_saved_attempts") return [
      { runId: "newer-run", checkoutId: checkout.checkoutId, label: "Later attempt" },
      { runId: "recorded-run", checkoutId: "linked-task", label: "Recorded workflow" },
    ];
    if (command === "attempt_worktree_evidence") return { kind: "saved", artifact: {
      revision: "immutable-evidence", start: { worktree: { branch: "feature" }, starting: { head: "start-head" } },
      candidate: { head: "captured-head" }, committedPatchBase64: btoa(savedPatch),
    } };
    throw new Error(`Unexpected ${command}`);
  });
  const user = userEvent.setup();
  const { openActivity } = openGit({ target: "attempt:recorded-run" });
  expect(await screen.findByText("immutable-evidence")).toBeDefined();
  await waitFor(() => expect(screen.getByLabelText("Commits during attempt").textContent).toBe(savedPatch));
  expect(invoke).toHaveBeenCalledWith("attempt_worktree_evidence", { projectId: checkout.projectId, runId: "recorded-run" });
  expect(vi.mocked(invoke).mock.calls.map(([command]) => command).sort()).toEqual(["attempt_worktree_evidence", "git_saved_attempts"]);
  await user.click(screen.getByRole("button", { name: "View attempt" }));
  expect(openActivity).toHaveBeenCalledWith("recorded-run");
});

it("deletes a workflow attempt's recorded diff using the owning chat and the preview revision", async () => {
  let deleted = false;
  const target = { kind: "attempt_diff", conversationId: "workflow-chat", runId: "recorded-run" };
  const preview = {
    revision: "reviewed-diff", messageRevisionIds: [], artifactIds: [], dispatchIds: [], referenceIds: [],
    attemptDiffIds: ["recorded-run"], candidateIds: [], sideRequestIds: [],
    nativeSessions: 0, retainedRuns: 1, affectedJobs: 0, unsettledRunIds: [],
  };
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "git_saved_attempts") return [{
      runId: "recorded-run", checkoutId: "linked-task", label: "Workflow attempt",
      conversationId: "workflow-chat", historyDeleted: deleted,
    }];
    if (command === "attempt_worktree_evidence") return deleted
      ? { kind: "unavailable", reason: "The recorded diff was removed from history." }
      : { kind: "saved", artifact: {
        revision: "immutable-evidence", start: { worktree: { branch: "feature" }, starting: { head: "start-head" } },
        candidate: { head: "captured-head" }, committedPatchBase64: btoa("private captured patch"),
      } };
    if (command === "preview_conversation_history_deletion") return preview;
    if (command === "confirm_conversation_history_deletion") { deleted = true; return preview; }
    throw new Error(`Unexpected ${command}`);
  });
  openGit({ target: "attempt:recorded-run" });
  fireEvent.click(await screen.findByText("Delete recorded diff"));
  fireEvent.click(screen.getByRole("button", { name: "Review deletion impact" }));
  fireEvent.click(await screen.findByRole("button", { name: "Confirm deletion" }));
  expect(await screen.findByText("The recorded diff was removed from history.")).toBeTruthy();
  expect(screen.queryByText("private captured patch")).toBeNull();
  expect(invoke).toHaveBeenCalledWith("confirm_conversation_history_deletion", {
    target, mode: "selected", expectedRevision: "reviewed-diff",
  });
  expect(screen.getByRole("button", { name: "View attempt" })).toBeTruthy();
});

it("requires explicit review when the staged revision changes with a commit message entered", async () => {
  let index = "reviewed-index";
  let head = "current-main";
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "git_checkout_review") return {
      checkoutId: checkout.checkoutId, worktrees, branches, root: checkout.checkoutRoot, managed: false,
      revision: { ...originalRevision, head, index },
      changes: head === "current-main" ? [{ path: "main.txt", working: null, staged: "M" }] : [],
    };
    if (command === "commit_git_index") { head = "new-commit-sha"; return head; }
    throw new Error(`Unexpected ${command}`);
  });
  const user = userEvent.setup();
  openGit();
  await user.type(await screen.findByLabelText("Commit message"), "Describe the staged changes");
  index = "new-index";
  await user.click(screen.getByRole("button", { name: "Refresh Git review" }));
  const commit = screen.getByRole("button", { name: "Commit staged changes" }) as HTMLButtonElement;
  await waitFor(() => expect(commit.disabled).toBe(true));
  expect(invoke).not.toHaveBeenCalledWith("commit_git_index", expect.anything());
  await user.click(screen.getByRole("button", { name: "Use current index" }));
  await user.click(commit);
  expect(invoke).toHaveBeenCalledWith("commit_git_index", {
    checkoutId: checkout.checkoutId, message: "Describe the staged changes", expected: { ...originalRevision, index: "new-index" },
  });
  expect((await screen.findByText("Commit saved", { exact: false })).textContent).toContain("new-commit-sha");
});
