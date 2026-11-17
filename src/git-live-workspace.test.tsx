// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import ChangesPanel from "@/changes-panel";
import GitWorkspace from "@/git-workspace";
import { fileHistoryTarget } from "@/file-context-actions";
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

it("stages and unstages the exact checked files with a single reviewed index mutation", async () => {
  const selected = ["notes/one.md", "src/two.rs"];
  const other = "untouched.txt";
  let staged = false;
  const revision = () => staged ? { ...originalRevision, index: "updated-index" } : originalRevision;
  vi.mocked(invoke).mockImplementation(async (command, payload) => {
    if (command === "git_checkout_review") return {
      checkoutId: checkout.checkoutId, worktrees, branches, root: checkout.checkoutRoot, managed: false, revision: revision(),
      changes: [...selected, other].map((path) => ({ path, working: "M", staged: staged && selected.includes(path) ? "M" : null })),
    };
    if (command === "git_checkout_diff") return {
      path: (payload as { path: string }).path, revision: revision(),
      original: { status: "text", content: "base\n" }, modified: { status: "text", content: "modified\n" },
    };
    if (command === "set_git_paths_staged") { staged = (payload as { staged: boolean }).staged; return; }
    throw new Error(command);
  });
  const user = userEvent.setup();
  openGit();
  let files = within(await screen.findByRole("region", { name: "Changed files" }));
  for (const path of selected) await user.click(files.getByRole("checkbox", { name: `Select ${path} for batch actions` }));
  expect(files.getByRole("status").textContent).toContain("2 files selected");
  fireEvent.contextMenu(files.getByRole("button", { name: /^untouched\.txt/ }));
  expect(await screen.findByRole("menuitem", { name: "Stage file" })).toBeTruthy();
  await user.keyboard("{Escape}");
  fireEvent.keyDown(files.getByRole("button", { name: /^notes\/one\.md/ }), { key: "F10", shiftKey: true });
  await user.click(await screen.findByRole("menuitem", { name: "Stage 2 selected files" }));
  expect(invoke).toHaveBeenCalledWith("set_git_paths_staged", {
    checkoutId: checkout.checkoutId, paths: selected, staged: true, expected: originalRevision,
  });
  await user.selectOptions(screen.getByLabelText("Diff source"), "staged");
  files = within(await screen.findByRole("region", { name: "Changed files" }));
  for (const path of selected) await user.click(await files.findByRole("checkbox", { name: `Select ${path} for batch actions` }));
  fireEvent.contextMenu(files.getByRole("button", { name: /^src\/two\.rs/ }));
  await user.click(await screen.findByRole("menuitem", { name: "Unstage 2 selected files" }));
  expect(invoke).toHaveBeenCalledWith("set_git_paths_staged", {
    checkoutId: checkout.checkoutId, paths: selected, staged: false, expected: { ...originalRevision, index: "updated-index" },
  });
});

it("compares two tracked refs in the shared text diff without reading the worktree", async () => {
  const revisions = { base: "current-main", head: "current-feature", mergeBase: "common-ancestor" };
  const range = { baseReference: "refs/heads/main", headReference: "refs/heads/feature" };
  vi.mocked(invoke).mockImplementation(async (command, payload) => {
    const args = payload as { range?: typeof range } | undefined;
    if (command === "git_checkout_review") return {
      checkoutId: checkout.checkoutId, worktrees, branches, root: checkout.checkoutRoot, managed: false,
      revision: originalRevision, changes: [],
    };
    if (command === "git_branch_comparison") {
      return args?.range?.headReference
        ? { revisions, changes: [{ path: "notes.md", status: "M" }] }
        : { revisions, changes: [] };
    }
    if (command === "git_branch_comparison_diff") return {
      path: "notes.md", original: { status: "text", content: "merge base text" }, modified: { status: "text", content: "feature text" },
    };
    throw new Error(`Unexpected ${command}`);
  });
  const user = userEvent.setup();
  openGit();
  await user.selectOptions(await screen.findByLabelText("Diff source"), "branch");
  await user.selectOptions(screen.getByLabelText("Base reference"), range.baseReference);
  await user.selectOptions(screen.getByLabelText("Compare reference"), range.headReference);
  const original = within(await screen.findByRole("region", { name: "Base file" }));
  const modified = within(await screen.findByRole("region", { name: "Revised file" }));
  expect(original.getByText("merge base text")).toBeTruthy();
  expect(modified.getByText("feature text")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("git_branch_comparison_diff", {
    checkoutId: checkout.checkoutId, range, expected: revisions, path: "notes.md",
  });
  expect(invoke).not.toHaveBeenCalledWith("git_checkout_diff", expect.anything());
});

it("opens file history at the selected worktree and compares a renamed file to its former path", async () => {
  const path = "notes/current.md";
  const previousPath = "notes/old.md";
  const renamed = { sha: "a".repeat(40), parent: "b".repeat(40), path, previousPath, status: "R", subject: "Rename note",
    author: "Author", committedAt: "2026-11-17T11:15:52-03:00" };
  const original = { ...renamed, sha: renamed.parent, parent: null, path: previousPath, previousPath: null, status: "A", subject: "Create note" };
  const openWorkspace = vi.fn();
  vi.mocked(invoke).mockImplementation(async (command, payload) => {
    const args = payload as { offset?: number; base?: { path?: string }; revised?: { kind?: string } };
    if (command === "git_checkout_review") return { checkoutId: checkout.checkoutId, worktrees, branches, root: checkout.checkoutRoot,
      managed: false, revision: { ...originalRevision, head: renamed.sha }, changes: [] };
    if (command === "git_checkout_file_history") return { head: renamed.sha, entries: args.offset ? [original] : [renamed], hasMore: !args.offset };
    if (command === "git_compare_file_versions") return {
      original: args.base?.path === previousPath ? { status: "text", content: "old path content" } : { status: "missing" },
      modified: { status: "text", content: args.revised?.kind === "index" ? "staged content" : "saved commit content" },
    };
    throw new Error(command);
  });
  const user = userEvent.setup();
  openGit({ target: fileHistoryTarget(path), openWorkspace });
  const history = within(await screen.findByRole("region", { name: "File history" }));
  expect((await history.findByRole("region", { name: "Base file" })).textContent).toContain("old path content");
  expect(invoke).toHaveBeenCalledWith("git_compare_file_versions", { checkoutId: checkout.checkoutId, path,
    base: { path: previousPath, sha: renamed.parent, kind: "commit" }, revised: { path, sha: renamed.sha, kind: "commit" } });
  fireEvent.contextMenu(history.getByRole("button", { name: /^Rename note/ }));
  await user.click(await screen.findByRole("menuitem", { name: "Compare with staged index" }));
  expect(await history.findByText("staged content")).toBeTruthy();
  await user.click(history.getByRole("button", { name: "Load older revisions" }));
  expect(await history.findByRole("button", { name: /^Create note/ })).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("git_checkout_file_history", { checkoutId: checkout.checkoutId, path, branch: null, offset: 1, pinnedHead: renamed.sha });
  await user.click(history.getByRole("button", { name: "Open working file" }));
  expect(openWorkspace).toHaveBeenCalledWith("knowledge", path);
});

it("anchors a comment to the clicked hunk line and adds the retained feedback to an unsent draft", async () => {
  const path = "src/example.txt";
  const body = "Check how this handles a missing value.";
  const excerpt = "reviewed text\r\n";
  const scope = {
    snapshot: "recorded-comparison",
    source: { kind: "checkout", source: "working", revision: originalRevision },
    original: { path, sha256: "original-digest", lineCount: 2 },
    modified: { path, sha256: "modified-digest", lineCount: 2 },
  };
  const comment = { id: "comment-1", path, scope, side: "modified", startLine: 2, endLine: 2,
    excerpt, body, createdAt: 0, resolved: false };
  let saved = false;
  const openWorkspace = vi.fn();
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "git_checkout_review") return {
      checkoutId: checkout.checkoutId, root: checkout.checkoutRoot, worktrees, branches, managed: false,
      revision: originalRevision, changes: [{ path, working: "M", staged: null }],
    };
    if (command === "git_checkout_diff") return {
      path, revision: originalRevision, commentScope: scope,
      original: { status: "text", content: "unchanged\nbase text\n" },
      modified: { status: "text", content: "unchanged\r\nreviewed text\r\n" },
      hunks: [{ id: "example-hunk", header: "@@ -1,2 +1,2 @@", originalLine: 1, modifiedLine: 1,
        patch: "diff --git a/src/example.txt b/src/example.txt\n@@ -1,2 +1,2 @@\n unchanged\n-base text\n+reviewed text\r\n" }],
    };
    if (command === "git_review_comments") return saved ? [comment] : [];
    if (command === "git_add_review_comment") { saved = true; return comment; }
    if (command === "list_conversations") return [{ id: "draft-id", checkoutId: checkout.checkoutId, title: "Existing conversation" }];
    if (command === "append_checkout_context_to_conversation") return;
    throw new Error(`Unexpected ${command}`);
  });
  openGit({ openWorkspace });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Review hunks" }));
  fireEvent.contextMenu(await screen.findByText("+reviewed text"));
  const menu = within(await screen.findByRole("menu"));
  expect(menu.queryByRole("menuitem", { name: "Add review comment on base line…" })).toBeNull();
  await user.click(menu.getByRole("menuitem", { name: "Add review comment on revised line…" }));
  const add = within(await screen.findByRole("dialog", { name: "Add review comment" }));
  expect(add.getByText("src/example.txt:2 · Revised")).toBeTruthy();
  await user.type(add.getByRole("textbox", { name: "Comment" }), body);
  expect(invoke).not.toHaveBeenCalledWith("git_add_review_comment", expect.anything());
  await user.click(add.getByRole("button", { name: "Save comment" }));
  expect(invoke).toHaveBeenCalledWith("git_add_review_comment", { checkoutId: checkout.checkoutId, request: {
    path, source: scope.source, snapshot: scope.snapshot, side: "modified", startLine: 2, endLine: 2, body,
  } });
  fireEvent.contextMenu(await screen.findByText(body));
  await user.click(await screen.findByRole("menuitem", { name: "Add review comment to conversation…" }));
  const append = within(await screen.findByRole("dialog", { name: "Add checkout context" }));
  expect(append.getByLabelText("Selected text").textContent).toBe(excerpt);
  await append.findByRole("option", { name: "Existing conversation" });
  await user.selectOptions(append.getByRole("combobox", { name: "Conversation" }), "draft-id");
  expect(invoke).not.toHaveBeenCalledWith("append_checkout_context_to_conversation", expect.anything());
  await user.click(append.getByRole("button", { name: "Add to draft" }));
  await waitFor(() => expect(openWorkspace).toHaveBeenCalledWith("conversations", "draft-id"));
  expect(invoke).toHaveBeenCalledWith("append_checkout_context_to_conversation", { checkoutId: checkout.checkoutId, conversationId: "draft-id",
    context: expect.stringContaining(`${body}\n\nSource: src/example.txt:2 · Revised · Index → worktree · HEAD current-main · Index old-index`) });
  expect(invoke).not.toHaveBeenCalledWith("start_conversation_turn", expect.anything());
});

it("reviews conflict stages from the clicked path and stages only the explicitly reviewed working snapshot", async () => {
  const path = "src/conflicted.ts";
  const openWorkspace = vi.fn();
  let snapshot = "first-reviewed-result";
  let staged = false;
  vi.mocked(invoke).mockImplementation(async (command, payload) => {
    if (command === "git_checkout_review") return {
      checkoutId: checkout.checkoutId, root: checkout.checkoutRoot, worktrees, branches, managed: false,
      revision: originalRevision, changes: staged ? [] : [{ path, working: "U", staged: "D" }],
    };
    if (command === "git_review_comments") return [];
    if (command === "git_conflict_review") return {
      path, revision: originalRevision, snapshot, resolutionBlocked: null,
      base: { objectId: "base-blob", mode: "100644", file: { status: "text", content: "original version" } },
      local: { objectId: "local-blob", mode: "100644", file: { status: "text", content: "locally changed version" } },
      incoming: null,
      working: { status: "text", content: snapshot === "first-reviewed-result" ? "reviewed result" : "updated result" },
    };
    if (command === "git_mark_conflict_resolved") {
      if ((payload as { expected: string }).expected !== snapshot) throw new Error("The working result changed.");
      staged = true;
      return;
    }
    throw new Error(`Unexpected ${command}`);
  });
  openGit({ openWorkspace });
  const user = userEvent.setup();
  const files = within(await screen.findByRole("region", { name: "Changed files" }));
  async function openConflict() {
    fireEvent.contextMenu(await files.findByRole("button", { name: /conflicted\.ts\s*Unmerged/ }));
    const menu = within(await screen.findByRole("menu"));
    expect(menu.queryByRole("menuitem", { name: "Stage file" })).toBeNull();
    expect(menu.queryByRole("menuitem", { name: "Unstage file" })).toBeNull();
    await user.click(menu.getByRole("menuitem", { name: "Review conflict…" }));
    return within(await screen.findByRole("dialog", { name: "Review Git conflict" }));
  }
  const review = await openConflict();
  expect(await review.findByText("reviewed result")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("git_conflict_review", { checkoutId: checkout.checkoutId, path });
  await user.click(review.getByRole("button", { name: "Base · stage 1" }));
  expect(review.getByText("original version")).toBeTruthy();
  await user.click(review.getByRole("button", { name: "Local · stage 2" }));
  expect(review.getByText("locally changed version")).toBeTruthy();
  await user.click(review.getByRole("button", { name: "Incoming · stage 3" }));
  expect(review.getByText("File absent in this version.")).toBeTruthy();
  await user.click(review.getByRole("button", { name: "Open working result in editor" }));
  expect(openWorkspace).toHaveBeenCalledWith("ide", path);
  expect(invoke).not.toHaveBeenCalledWith("git_mark_conflict_resolved", expect.anything());
  expect(invoke).not.toHaveBeenCalledWith("git_checkout_diff", expect.anything());

  const resolution = await openConflict();
  const stage = await resolution.findByRole("button", { name: "Stage reviewed result" });
  expect(stage).toHaveProperty("disabled", true);
  await user.click(await resolution.findByRole("checkbox", { name: /I reviewed this working result/ }));
  snapshot = "updated-reviewed-result";
  await user.click(stage);
  expect(await resolution.findByText(/The working result changed/)).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("git_mark_conflict_resolved", { checkoutId: checkout.checkoutId, path, expected: "first-reviewed-result" });
  await user.click(resolution.getByRole("button", { name: "Refresh review" }));
  expect(await resolution.findByText("updated result")).toBeTruthy();
  expect(stage).toHaveProperty("disabled", true);
  await user.click(resolution.getByRole("checkbox", { name: /I reviewed this working result/ }));
  await user.click(stage);
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Review Git conflict" })).toBeNull());
  expect(invoke).toHaveBeenCalledWith("git_mark_conflict_resolved", { checkoutId: checkout.checkoutId, path, expected: snapshot });
  expect(invoke).not.toHaveBeenCalledWith("set_git_path_staged", expect.anything());
});

it("previews a chosen side and an edited combination before writing a working result without staging", async () => {
  const path = "src/merge.ts";
  const local = "export const first = 1;\r\n";
  const incoming = "export const second = 2;\n";
  let working: string | undefined;
  let version = 0;
  vi.mocked(invoke).mockImplementation(async (command, payload) => {
    if (command === "git_checkout_review") return {
      checkoutId: checkout.checkoutId, root: checkout.checkoutRoot, worktrees, branches, managed: false,
      revision: originalRevision, changes: [{ path, working: "U", staged: "U" }],
    };
    if (command === "git_review_comments") return [];
    if (command === "git_conflict_review") return {
      path, revision: originalRevision, snapshot: `confirmed-snapshot-${version}`, proposalBlocked: null,
      resolutionBlocked: working === undefined ? "Create the working result before staging." : null,
      base: null,
      local: { objectId: "local-blob", mode: "100644", file: { status: "text", content: local } },
      incoming: { objectId: "incoming-blob", mode: "100644", file: { status: "text", content: incoming } },
      working: working === undefined ? { status: "missing" } : { status: "text", content: working },
    };
    if (command === "git_apply_conflict_resolution") {
      const { proposal } = payload as { proposal: { kind: string; content: string } };
      working = proposal.kind === "local" ? local : proposal.content;
      version += 1;
      return;
    }
    throw new Error(`Unexpected ${command}`);
  });
  const user = userEvent.setup();
  openGit();
  await user.click(await screen.findByRole("button", { name: "Review conflict" }));
  const review = within(await screen.findByRole("dialog", { name: "Review Git conflict" }));
  expect(await review.findByText("File absent in this version.")).toBeTruthy();
  await user.click(review.getByRole("button", { name: "Local · stage 2" }));
  await user.click(review.getByRole("button", { name: "Use local version…" }));
  const create = review.getByRole("button", { name: "Create working result" });
  expect(create).toHaveProperty("disabled", true);
  expect(review.getByLabelText("Proposed working result").textContent).toBe(local);
  await user.click(review.getByRole("checkbox", { name: /I reviewed the proposed contents and want to create/ }));
  expect(invoke).not.toHaveBeenCalledWith("git_apply_conflict_resolution", expect.anything());
  await user.click(create);
  expect(await review.findByText("Working result saved. Review it to stage the resolution.")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("git_apply_conflict_resolution", {
    checkoutId: checkout.checkoutId, path, expected: "confirmed-snapshot-0", proposal: { kind: "local", objectId: "local-blob" },
  });

  await user.click(review.getByRole("button", { name: "Incoming · stage 3" }));
  await user.click(review.getByRole("button", { name: "Combine manually…" }));
  const result = review.getByRole("textbox", { name: "Proposed working result" });
  expect(result).toHaveProperty("value", incoming);
  const confirmed = review.getByRole("checkbox", { name: /I reviewed the proposed contents and want to replace/ });
  const replace = review.getByRole("button", { name: "Replace working result" });
  await user.click(confirmed);
  const combined = local.replace(/\r\n/g, "\n") + incoming;
  fireEvent.change(result, { target: { value: combined } });
  expect(confirmed).toHaveProperty("checked", false);
  expect(replace).toHaveProperty("disabled", true);
  await user.click(confirmed);
  await user.click(replace);
  expect(await review.findByText("Working result saved. Review it to stage the resolution.")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("git_apply_conflict_resolution", {
    checkoutId: checkout.checkoutId, path, expected: "confirmed-snapshot-1", proposal: { kind: "combined", content: combined },
  });
  expect(invoke).not.toHaveBeenCalledWith("git_mark_conflict_resolved", expect.anything());
  expect(invoke).not.toHaveBeenCalledWith("set_git_path_staged", expect.anything());
});

it("registers a manual worktree from the shared dialog and selects the returned checkout", async () => {
  const tree = { checkoutId: "manually-created", root: "D:/worktrees/new-local", branch: "new-local", head: "current-main", managed: false };
  let created = false;
  vi.mocked(invoke).mockImplementation(async (command, payload) => {
    const args = payload as { checkoutId?: string } | undefined;
    if (command === "git_checkout_review") {
      const selected = args?.checkoutId === tree.checkoutId;
      return {
        checkoutId: args?.checkoutId, worktrees: created ? [...worktrees, tree] : worktrees,
        branches, root: selected ? tree.root : checkout.checkoutRoot, managed: false,
        revision: selected ? { ...originalRevision, branch: "refs/heads/new-local" } : originalRevision,
        changes: [],
      };
    }
    if (command === "git_checkout_history") return [];
    if (command === "create_git_worktree") {
      created = true;
      return { ...checkout, checkoutId: tree.checkoutId, checkoutRoot: tree.root };
    }
    throw new Error(`Unexpected ${command}`);
  });
  const user = userEvent.setup();
  openGit({ target: "worktrees" });
  await waitFor(() => expect((screen.getByRole("button", { name: "New worktree" }) as HTMLButtonElement).disabled).toBe(false));
  await user.click(screen.getByRole("button", { name: "New worktree" }));
  const dialog = within(screen.getByRole("dialog", { name: "New worktree" }));
  await user.type(dialog.getByLabelText("Branch", { exact: true }), "new-local");
  await user.type(dialog.getByLabelText("Worktree path"), tree.root);
  await user.click(dialog.getByRole("button", { name: "Create worktree" }));
  expect(invoke).toHaveBeenCalledWith("create_git_worktree", {
    projectId: checkout.projectId, checkoutId: checkout.checkoutId,
    request: { branch: "new-local", existing: false, baseReference: null, expectedHead: "current-main", path: tree.root },
  });
  const row = await screen.findByRole("row", { name: /new-local/ });
  expect(within(row).getByRole("button", { name: "Selected" })).toBeTruthy();
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
  expect(within(external).getByRole<HTMLButtonElement>("button", { name: "Use worktree" }).disabled).toBe(true);
  const linked = within(table).getByRole("row", { name: /C:\/linked/ });
  fireEvent.contextMenu(within(linked).getByText("C:/linked"));
  await user.click(await screen.findByRole("menuitem", { name: "Open worktree history" }));
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

it("stages the exact hunk selected in the live comparison", async () => {
  const path = "example.txt";
  const hunk = { id: "reviewed-hunk", header: "@@ -12,3 +12,3 @@", originalLine: 12, modifiedLine: 12,
    patch: "diff --git a/example.txt b/example.txt\n--- a/example.txt\n+++ b/example.txt\n@@ -12,3 +12,3 @@\n-old\n+changed\n" };
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "git_checkout_review") return { checkoutId: checkout.checkoutId, worktrees, branches, root: checkout.checkoutRoot,
      managed: false, revision: originalRevision, changes: [{ path, working: "M", staged: null }] };
    if (command === "git_checkout_diff") return { path, source: "working", revision: originalRevision, hunks: [hunk],
      original: { status: "text", content: "old" }, modified: { status: "text", content: "changed" } };
    if (command === "set_git_hunk_staged") return;
    throw new Error(`Unexpected ${command}`);
  });
  const user = userEvent.setup();
  openGit();
  await user.click(await screen.findByRole("button", { name: "Review hunks" }));
  fireEvent.contextMenu(within(screen.getByRole("region", { name: hunk.header })).getByText("+changed"));
  await user.click(within(await screen.findByRole("menu", { name: hunk.header })).getByRole("menuitem", { name: "Stage hunk" }));
  expect(invoke).toHaveBeenCalledWith("set_git_hunk_staged", {
    checkoutId: checkout.checkoutId, path, source: "working", expected: originalRevision, hunkId: hunk.id,
  });
  expect(invoke).not.toHaveBeenCalledWith("set_git_path_staged", expect.anything());
});

it("marks and unmarks the clicked hunk on its exact Git diff without staging it", async () => {
  const path = "example.txt";
  const hunk = { id: "picked-hunk", header: "@@ -12 +12 @@", originalLine: 12, modifiedLine: 12,
    patch: "diff --git a/example.txt b/example.txt\n--- a/example.txt\n+++ b/example.txt\n@@ -12 +12 @@\n-old\n+changed\n" };
  const base = { path, sha256: "original-text", lineCount: 12 };
  const snapshot = "selected-checkout-diff";
  let reviewed = false;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "git_checkout_review") return { checkoutId: checkout.checkoutId, worktrees, branches, root: checkout.checkoutRoot,
      managed: false, revision: originalRevision, changes: [{ path, working: "M", staged: null }] };
    if (command === "git_checkout_diff") return { path, source: "working", revision: originalRevision, hunks: [{ ...hunk, reviewed }],
      original: { status: "text", content: "old" }, modified: { status: "text", content: "changed" },
      commentScope: { snapshot, source: { kind: "checkout", source: "working", revision: originalRevision },
        original: base, modified: { ...base, sha256: "changed-text" } } };
    if (command === "git_review_comments") return [];
    if (command === "set_git_hunk_reviewed") {
      reviewed = (args as { request: { reviewed: boolean } }).request.reviewed;
      return;
    }
    throw new Error(`Unexpected ${command}`);
  });
  const user = userEvent.setup();
  openGit();
  await user.click(await screen.findByRole("button", { name: "Review hunks" }));
  const region = within(screen.getByRole("region", { name: hunk.header }));
  fireEvent.contextMenu(region.getByRole("button", { name: hunk.header }));
  await user.click(within(await screen.findByRole("menu", { name: hunk.header })).getByRole("menuitem", { name: "Mark hunk as reviewed" }));
  expect(invoke).toHaveBeenCalledWith("set_git_hunk_reviewed", { checkoutId: checkout.checkoutId,
    request: { path, source: "working", snapshot, hunkId: hunk.id, reviewed: true } });
  await region.findByText("Reviewed");
  expect(screen.getByText("1/1 hunks reviewed")).toBeTruthy();

  fireEvent.contextMenu(region.getByText("+changed"));
  const lineMenu = await screen.findByRole("menu", { name: "Revised line 12" });
  await user.click(within(lineMenu).getByRole("menuitem", { name: "Mark hunk as unreviewed" }));
  expect(invoke).toHaveBeenCalledWith("set_git_hunk_reviewed", { checkoutId: checkout.checkoutId,
    request: { path, source: "working", snapshot, hunkId: hunk.id, reviewed: false } });
  await waitFor(() => expect(region.queryByText("Reviewed")).toBeNull());
  expect(invoke).not.toHaveBeenCalledWith("set_git_hunk_staged", expect.anything());
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

it("requires a destination and a fresh reviewed comparison before applying saved evidence", async () => {
  let previews = 0;
  const destination = "D:\\fixture\\alternate";
  const request = { projectId: checkout.projectId, runId: "patch-attempt", captureRevision: "saved-capture", checkoutId: "alternate", part: "committed" };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "git_saved_attempts") return [{ runId: request.runId, checkoutId: checkout.checkoutId, label: "Saved attempt" }];
    if (command === "attempt_worktree_evidence") return { kind: "saved", artifact: { revision: request.captureRevision,
      start: { worktree: { branch: "attempt" }, starting: { head: "base" } }, candidate: { head: "candidate" }, committedPatchBase64: btoa("patch") } };
    if (command === "registered_checkouts") return [checkout, { ...checkout, checkoutId: request.checkoutId, checkoutRoot: destination },
      { ...checkout, projectId: "other-project", checkoutId: "other", checkoutRoot: "Unrelated worktree" }];
    if (command === "preview_saved_git_patch") return { snapshot: `snapshot-${++previews}`, destination, revision: originalRevision,
      files: [{ path: "example.txt", before: { status: "text", content: "Original contents" }, after: { status: "text", content: "Result contents" } }] };
    if (command === "apply_saved_git_patch") {
      if ((args as { snapshot: string }).snapshot === "snapshot-1") throw "The reviewed destination changed.";
      return;
    }
    throw new Error(`Unexpected ${command}`);
  });
  const user = userEvent.setup();
  openGit({ target: "attempt:patch-attempt" });
  await user.click(await screen.findByRole("button", { name: "Apply saved patch…" }));
  const dialog = within(await screen.findByRole("dialog", { name: "Apply saved patch" }));
  const target = await dialog.findByRole<HTMLSelectElement>("combobox", { name: "Destination worktree" });
  const confirm = dialog.getByRole<HTMLButtonElement>("button", { name: "Apply reviewed patch" });
  expect(target.value).toBe("");
  expect(dialog.queryByRole("option", { name: "Unrelated worktree" })).toBeNull();
  expect(confirm.disabled).toBe(true);
  await user.selectOptions(target, request.checkoutId);
  await user.click(dialog.getByRole("button", { name: "Review patch" }));
  expect(await dialog.findByText("Result contents")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("preview_saved_git_patch", { request });
  await user.click(confirm);
  expect(await dialog.findByText("The reviewed destination changed.")).toBeTruthy();
  expect(confirm.disabled).toBe(true);
  await user.click(dialog.getByRole("button", { name: "Review patch" }));
  await waitFor(() => expect(confirm.disabled).toBe(false));
  await user.click(confirm);
  expect(await screen.findByText(`Saved patch applied to ${destination}.`)).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("apply_saved_git_patch", { request, snapshot: "snapshot-2" });
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
