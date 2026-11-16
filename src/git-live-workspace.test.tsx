// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import GitWorkspace from "@/git-workspace";
import { exampleCheckout, type WorkspaceProps } from "@/workbench-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/changes-panel", () => ({
  default: () => null,
  MonacoComparison: ({ file }: { file: { original: string; modified: string } }) => (
    <section aria-label="Monaco comparison"><pre>{file.original}</pre><pre>{file.modified}</pre></section>
  ),
}));
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
  expect(within(await screen.findByRole("region", { name: "Monaco comparison" })).getByText("working revision")).toBeDefined();
  await user.click(screen.getByRole("button", { name: "Stage main.txt" }));
  expect(invoke).toHaveBeenCalledWith("set_git_path_staged", { checkoutId: checkout.checkoutId, path: "main.txt", staged: true, expected: originalRevision });
  await screen.findByText("No working changes.");
  await user.selectOptions(screen.getByLabelText("Diff source"), "staged");
  expect(within(await screen.findByRole("region", { name: "Monaco comparison" })).getByText("staged revision")).toBeDefined();
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
  expect(await screen.findByText("new-commit-sha", { selector: "code" })).toBeDefined();
});
