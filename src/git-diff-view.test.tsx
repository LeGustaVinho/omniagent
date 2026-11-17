// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { GitDiffReview, MonacoComparison, type GitTextComparison } from "@/git-diff-view";
import { loadMonaco } from "@/ide-monaco";

vi.mock("@/ide-monaco", () => ({ loadMonaco: vi.fn(), monacoTheme: () => "vs-dark" }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const comparison: GitTextComparison = {
  path: "docs/current.txt", originalPath: "docs/before rename.txt", language: "plaintext",
  original: "header\r\nremoved line\r\nfooter\r\n", modified: "header\r\nadded line\r\nfooter\r\n",
  hunks: [{ id: "exact", header: "@@ -2,1 +2,1 @@", originalLine: 2, modifiedLine: 2,
    patch: "diff --git a/docs/before.txt b/docs/current.txt\n@@ -2,1 +2,1 @@\n-removed line\n+added line\n" }],
};

it("opens the exact read-only base line of a removed hunk using its historical path", async () => {
  vi.mocked(loadMonaco).mockRejectedValue(new Error("Fallback for jsdom"));
  const user = userEvent.setup();
  const openWorking = vi.fn();
  render(<GitDiffReview source="working" title="Working changes" ranges={{ base: "Index abc", revised: "Worktree" }}
    files={[{ path: comparison.path, previousPath: comparison.originalPath, kind: "Renamed" }]} selectedPath={comparison.path}
    select={vi.fn()} comparison={comparison} openLine={openWorking} empty={null} />);
  await user.click(screen.getByRole("button", { name: "Review hunks" }));
  fireEvent.contextMenu(screen.getByText("-removed line"));
  expect(screen.queryByRole("menuitem", { name: /Open revised version at line/ })).toBeNull();
  await user.click(await screen.findByRole("menuitem", { name: "Open base version at line 2" }));
  const readOnly = within(await screen.findByRole("dialog", { name: "Base version" }));
  expect(readOnly.getByText("docs/before rename.txt:2")).toBeTruthy();
  expect(readOnly.getByText("Index abc")).toBeTruthy();
  expect(readOnly.getByLabelText("Selected line 2").textContent).toBe("removed line");
  expect(readOnly.getByLabelText("Version contents").textContent).toBe(comparison.original);
  expect(openWorking).not.toHaveBeenCalled();
  await user.click(readOnly.getByRole("button", { name: "Close dialog" }));
  fireEvent.contextMenu(screen.getByText("-removed line"));
  await user.click(await screen.findByRole("menuitem", { name: "Copy base line 2 reference" }));
  expect(await navigator.clipboard.readText()).toBe("docs/before rename.txt:2 · Index abc");

  const added = screen.getByText("+added line");
  fireEvent.keyDown(added, { key: "F10", shiftKey: true });
  await user.click(await screen.findByRole("menuitem", { name: "Open revised version at line 2" }));
  const revised = within(await screen.findByRole("dialog", { name: "Revised version" }));
  expect(revised.getByText("docs/current.txt:2")).toBeTruthy();
  expect(revised.getByLabelText("Selected line 2").textContent).toBe("added line");
});

it("uses the clicked Monaco position independently of the opposite side and current cursor", async () => {
  type MonacoMenuEvent = { target: { position: { lineNumber: number; column: number } } };
  function mockSide() {
    const actions = new Map<string, () => void>();
    let context: ((event: MonacoMenuEvent) => void) | undefined;
    const side = { addAction: ({ id, run }: { id: string; run: () => void }) => actions.set(id, run),
      onContextMenu: (handler: (event: MonacoMenuEvent) => void) => { context = handler; },
      onMouseDown: vi.fn(), onKeyDown: vi.fn(), getPosition: () => ({ lineNumber: 1, column: 1 }),
      getSelection: () => ({ startLineNumber: 1, endLineNumber: 1, startColumn: 1, endColumn: 4 }),
      getSelections: () => [{ containsPosition: (position: { lineNumber: number }) => position.lineNumber === 1 }],
      getModel: () => ({ getValueInRange: () => "" }),
      createDecorationsCollection: () => ({ clear: vi.fn() }) };
    return { side, actions, click: (lineNumber: number) => context?.({ target: { position: { lineNumber, column: 3 } } }) };
  }
  const base = mockSide();
  const revised = mockSide();
  vi.mocked(loadMonaco).mockResolvedValue({ editor: {
    setTheme: vi.fn(), createModel: () => ({ dispose: vi.fn() }), createDiffEditor: () => ({ setModel: vi.fn(),
      getOriginalEditor: () => base.side, getModifiedEditor: () => revised.side, dispose: vi.fn(), updateOptions: vi.fn() }),
  } } as unknown as Awaited<ReturnType<typeof loadMonaco>>);
  const user = userEvent.setup();
  const openWorking = vi.fn();
  render(<MonacoComparison file={comparison} layout="split" revisions={{ original: "Previous commit", modified: "Working copy" }} openLine={openWorking} />);
  await waitFor(() => expect(base.actions.has("omniagent.open-original-diff-version-line")).toBe(true));
  act(() => { base.click(2); base.actions.get("omniagent.open-original-diff-version-line")?.(); });
  const readOnly = within(await screen.findByRole("dialog", { name: "Base version" }));
  expect(readOnly.getByLabelText("Selected line 2").textContent).toBe("removed line");
  expect(readOnly.getByText("Previous commit")).toBeTruthy();
  await user.click(readOnly.getByRole("button", { name: "Close dialog" }));
  await act(async () => { base.click(2); base.actions.get("omniagent.copy-original-diff-line")?.(); });
  expect(await navigator.clipboard.readText()).toBe("docs/before rename.txt:2 · Previous commit");
  act(() => { revised.click(3); revised.actions.get("omniagent.open-working-diff-line")?.(); });
  expect(openWorking).toHaveBeenCalledExactlyOnceWith(3);
});
